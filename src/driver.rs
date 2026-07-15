use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use rustc_ast as ast;
use rustc_driver::{Callbacks, Compilation};
use rustc_errors::Applicability;
use rustc_hir as hir;
use rustc_hir::Node;
use rustc_hir::def::{CtorOf, DefKind, Res};
use rustc_hir::def_id::{CRATE_DEF_ID, DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_interface::interface;
use rustc_lint_defs::builtin::DEAD_CODE;
use rustc_middle::middle::codegen_fn_attrs::CodegenFnAttrFlags;
use rustc_middle::ty::{self, TyCtxt};
use rustc_parse::lexer::StripTokens;
use rustc_parse::parser::{AllowConstBlockItems, ForceCollect};
use rustc_session::config::CrateType;
use rustc_session::lint::Level;
use rustc_span::Symbol;
use rustc_span::def_id::LOCAL_CRATE;
use rustc_span::hygiene::{ExpnKind, MacroKind};
use rustc_span::{BytePos, FileName, Pos};

use crate::protocol;
use cargo_hawk_internal::graph::{
    CollectionOptions, Definition, DefinitionIdentity, DefinitionKind, Edge, EdgeKind, FindingKind,
    FixPlan, FixTarget, Fragment, Span, VisibilityReduction,
};

pub(crate) fn is_protocol_version_query(args: &[String]) -> bool {
    args.get(1)
        .is_some_and(|argument| argument == protocol::VERSION_ARGUMENT)
        && args.len() == 2
}

pub(crate) fn print_protocol_version() -> ExitCode {
    println!("{}", protocol::VERSION);
    ExitCode::SUCCESS
}

pub(crate) fn is_wrapper_invocation(args: &[String]) -> bool {
    env::var_os(protocol::OUTPUT_DIR_ENV).is_some()
        && env::var_os(protocol::ROOT_CRATE_ENV).is_some()
        && args.get(1).is_some()
}

pub(crate) fn run_wrapper(mut args: Vec<String>) -> ExitCode {
    if let Err(error) = validate_frontend_protocol() {
        eprintln!("hawk: {error:#}");
        return ExitCode::FAILURE;
    }
    args.remove(1);
    let output_dir = PathBuf::from(
        env::var_os(protocol::OUTPUT_DIR_ENV).expect("HAWK_OUTPUT_DIR checked before dispatch"),
    );
    let root_crate =
        env::var(protocol::ROOT_CRATE_ENV).expect("HAWK_ROOT_CRATE checked before dispatch");
    let collection_options =
        match parse_collection_options(env::var_os(protocol::COLLECTION_OPTIONS_ENV).as_deref()) {
            Ok(collection_options) => collection_options,
            Err(error) => {
                eprintln!("hawk: invalid collection options: {error:#}");
                return ExitCode::FAILURE;
            }
        };
    let fix_plan = match env::var_os(protocol::FIX_PLAN_ENV)
        .map(PathBuf::from)
        .map(|path| read_fix_plan(&path))
        .transpose()
    {
        Ok(fix_plan) => fix_plan,
        Err(error) => {
            eprintln!("hawk: could not read fix plan: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    if fix_plan.is_some() {
        args.push("--cap-lints".to_owned());
        args.push("allow".to_owned());
        // A Hawk visibility fix can make an import unused in one consumer
        // mode while it remains required by another mode.
        args.push("--allow".to_owned());
        args.push("unused_imports".to_owned());
    }
    let mut callbacks = HawkCallbacks {
        output_dir,
        root_crate,
        collection_options,
        fix_plan,
    };

    rustc_driver::catch_with_exit_code(move || {
        rustc_driver::run_compiler(&args, &mut callbacks);
    })
}

fn validate_frontend_protocol() -> Result<()> {
    let version = env::var(protocol::VERSION_ENV)
        .context("Hawk frontend did not provide a compiler driver protocol version")?;
    validate_frontend_protocol_version(&version)
}

fn validate_frontend_protocol_version(version: &str) -> Result<()> {
    let version = version
        .parse::<u32>()
        .context("Hawk frontend provided an invalid compiler driver protocol version")?;
    if version != protocol::VERSION {
        bail!(
            "Hawk frontend uses compiler driver protocol {version}, but this driver uses protocol {}; install `cargo-hawk` and `cargo-hawk-driver` from the same release",
            protocol::VERSION
        );
    }
    Ok(())
}

struct HawkCallbacks {
    output_dir: PathBuf,
    root_crate: String,
    collection_options: CollectionOptions,
    fix_plan: Option<FixPlan>,
}

impl Callbacks for HawkCallbacks {
    fn config(&mut self, config: &mut interface::Config) {
        let run_id = env::var(protocol::RUN_ID_ENV).ok();
        let collection_options = self.collection_options.as_env_value();
        config.track_state = Some(Box::new(move |session| {
            let mut env_depinfo = session.env_depinfo.borrow_mut();
            env_depinfo.insert((
                Symbol::intern(protocol::RUN_ID_ENV),
                run_id.as_deref().map(Symbol::intern),
            ));
            env_depinfo.insert((
                Symbol::intern(protocol::COLLECTION_OPTIONS_ENV),
                Some(Symbol::intern(collection_options)),
            ));
        }));
    }

    fn after_analysis(&mut self, _compiler: &interface::Compiler, tcx: TyCtxt<'_>) -> Compilation {
        if let Some(fix_plan) = &self.fix_plan {
            emit_fixes(tcx, fix_plan);
        } else if let Err(error) = emit_fragment(
            tcx,
            &self.root_crate,
            &self.output_dir,
            self.collection_options,
        ) {
            tcx.dcx()
                .fatal(format!("hawk could not emit analysis graph: {error:#}"));
        }
        Compilation::Continue
    }
}

fn parse_collection_options(value: Option<&OsStr>) -> Result<CollectionOptions> {
    let Some(value) = value else {
        return Ok(CollectionOptions::default());
    };
    let Some(value) = value.to_str() else {
        bail!("{} must be valid UTF-8", protocol::COLLECTION_OPTIONS_ENV);
    };
    CollectionOptions::from_env_value(Some(value)).with_context(|| {
        format!(
            "unsupported {} value `{value}`",
            protocol::COLLECTION_OPTIONS_ENV
        )
    })
}

fn read_fix_plan(path: &Path) -> Result<FixPlan> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("deserialize {}", path.display()))
}

fn emit_fixes(tcx: TyCtxt<'_>, fix_plan: &FixPlan) {
    let fix_plan = FixPlanIndex::new(fix_plan);
    let crate_items = tcx.hir_crate_items(());
    let mut visibility_fixes = Vec::new();
    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let Some(definition_kind) = diagnostic_kind(tcx, def_id) else {
            continue;
        };
        if definition_kind == DefinitionKind::Reexport && !is_named_reexport(tcx, def_id) {
            continue;
        }
        let visibility_span = visibility_span(tcx, def_id);
        if let Some(visibility_span) = visibility_span {
            visibility_fixes.push((
                visibility_span,
                planned_fix(tcx, def_id, definition_kind, &fix_plan),
            ));
        }
    }
    for item_id in crate_items.free_items() {
        let item = tcx.hir_item(item_id);
        let fields = match item.kind {
            hir::ItemKind::Struct(_, _, data) | hir::ItemKind::Union(_, _, data) => data.fields(),
            _ => continue,
        };
        for field in fields {
            visibility_fixes.push((
                field.vis_span,
                planned_fix(tcx, field.def_id, DefinitionKind::Field, &fix_plan),
            ));
        }
    }

    let mut grouped_fixes = HashMap::new();
    for (span, kind) in visibility_fixes {
        grouped_fixes
            .entry((source_span(tcx, span), span.hi() - span.lo()))
            .and_modify(|(_, planned)| {
                if *planned != kind {
                    *planned = None;
                }
            })
            .or_insert((span, kind));
    }
    for (_, (span, kind)) in grouped_fixes {
        if let Some(kind) = kind {
            emit_fix(tcx, span, kind);
        }
    }
}

struct FixPlanIndex<'a> {
    by_id: HashMap<&'a str, &'a FixTarget>,
    by_identity: HashMap<DefinitionIdentity<'a>, &'a FixTarget>,
}

impl<'a> FixPlanIndex<'a> {
    fn new(fix_plan: &'a FixPlan) -> Self {
        Self {
            by_id: fix_plan
                .targets
                .iter()
                .map(|target| (target.id.as_str(), target))
                .collect(),
            by_identity: fix_plan
                .targets
                .iter()
                .map(|target| {
                    (
                        DefinitionIdentity::new(
                            &target.crate_name,
                            &target.name,
                            target.definition_kind,
                            target.span.as_ref(),
                        ),
                        target,
                    )
                })
                .collect(),
        }
    }

    fn get_by_id(&self, id: &str) -> Option<&'a FixTarget> {
        self.by_id.get(id).copied()
    }

    fn get_by_identity(&self, identity: &DefinitionIdentity<'_>) -> Option<&'a FixTarget> {
        self.by_identity.get(identity).copied()
    }
}

fn planned_fix(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    definition_kind: DefinitionKind,
    fix_plan: &FixPlanIndex<'_>,
) -> Option<(FindingKind, VisibilityReduction)> {
    let id = id(tcx, def_id.to_def_id());
    if let Some(target) = fix_plan.get_by_id(&id) {
        return Some((target.kind, target.replacement));
    }

    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let name = definition_name(tcx, def_id, definition_kind);
    let definition_span = span(tcx, def_id);
    fix_plan
        .get_by_identity(&DefinitionIdentity::new(
            &crate_name,
            &name,
            definition_kind,
            definition_span.as_ref(),
        ))
        .map(|target| (target.kind, target.replacement))
}

fn emit_fix(
    tcx: TyCtxt<'_>,
    mut visibility_span: rustc_span::Span,
    (kind, replacement): (FindingKind, VisibilityReduction),
) {
    if replacement == VisibilityReduction::Private {
        let extended = visibility_span.with_hi(visibility_span.hi() + BytePos(1));
        if tcx
            .sess
            .source_map()
            .span_to_snippet(extended)
            .is_ok_and(|snippet| matches!(snippet.as_bytes().last(), Some(b' ' | b'\t')))
        {
            visibility_span = extended;
        }
    }
    let mut diagnostic = tcx.dcx().struct_span_warn(
        visibility_span,
        "public visibility can be restricted for the selected Hawk product",
    );
    diagnostic.is_lint(kind.code().to_owned(), false);
    diagnostic.span_suggestion(
        visibility_span,
        "change this visibility to",
        replacement.replacement(),
        Applicability::MachineApplicable,
    );
    diagnostic.emit();
}

fn emit_fragment(
    tcx: TyCtxt<'_>,
    root_crate: &str,
    output_dir: &Path,
    collection_options: CollectionOptions,
) -> Result<()> {
    let package_name = env::var("CARGO_PKG_NAME").context("read Cargo package name")?;
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let crate_id = id(tcx, CRATE_DEF_ID.to_def_id());
    let is_non_production =
        env::var(protocol::CONSUMER_MODE_ENV).as_deref() == Ok("non-production");
    let test_surface = is_non_production && tcx.sess.opts.test;
    let is_product_root = if is_non_production {
        // Non-production executables, including custom tests and benchmarks,
        // can have entry points without `--test` but still consume APIs.
        tcx.entry_fn(()).is_some()
    } else {
        crate_name == root_crate && tcx.entry_fn(()).is_some()
    };
    let suffix: String = crate_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let fragment = collect_fragment(
        tcx,
        package_name,
        crate_name.clone(),
        crate_id,
        is_product_root,
        test_surface,
        collection_options,
    );
    let path = output_dir.join(format!("{crate_name}-{suffix}.json"));
    let mut file = tempfile::NamedTempFile::new_in(output_dir)
        .with_context(|| format!("create temporary fragment in {}", output_dir.display()))?;
    let temporary_path = file.path().to_path_buf();
    write_fragment(file.as_file_mut(), &fragment, &temporary_path)?;
    file.persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("persist fragment {}", path.display()))?;
    Ok(())
}

fn write_fragment(writer: impl Write, fragment: &Fragment, path: &Path) -> Result<()> {
    let mut writer = BufWriter::new(writer);
    serde_json::to_writer(&mut writer, fragment)
        .with_context(|| format!("serialize {}", path.display()))?;
    writer
        .flush()
        .with_context(|| format!("flush {}", path.display()))
}

fn collect_fragment(
    tcx: TyCtxt<'_>,
    package_name: String,
    crate_name: String,
    crate_id: String,
    is_product_root: bool,
    test_surface: bool,
    collection_options: CollectionOptions,
) -> Fragment {
    let mut definitions = Vec::new();
    let mut defined = HashSet::new();
    let mut adt_members = Vec::new();
    let mut source_item_fields = Vec::new();
    let mut generated_fields = Vec::new();
    let mut public_reexports = Vec::new();
    let crate_items = tcx.hir_crate_items(());
    let is_proc_macro_crate = tcx.crate_types().contains(&CrateType::ProcMacro);

    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let kind = diagnostic_kind(tcx, def_id);
        let visibility = visibility_modifier(tcx, def_id);
        let public_candidate = kind.is_some()
            && is_public_candidate_with_visibility(
                tcx,
                def_id,
                test_surface,
                visibility.as_deref(),
            );
        let public_api = kind
            .is_some_and(|kind| kind != DefinitionKind::Reexport || is_named_reexport(tcx, def_id))
            && public_candidate;
        if kind == Some(DefinitionKind::Reexport) && public_candidate {
            public_reexports.push(def_id);
        }
        definitions.push(definition(
            tcx,
            def_id,
            &crate_name,
            kind.unwrap_or(DefinitionKind::Other),
            public_api,
            visibility.as_deref(),
        ));
        defined.insert(def_id);
    }
    for item_id in crate_items.free_items() {
        let item = tcx.hir_item(item_id);
        let source_item_index = (!item.span.from_expansion()).then(|| {
            source_item_fields.push((
                source_file_start(tcx, item.span),
                item.span.lo().to_u32(),
                Vec::new(),
            ));
            source_item_fields.len() - 1
        });
        match item.kind {
            hir::ItemKind::Struct(_, _, data) | hir::ItemKind::Union(_, _, data) => {
                let uniform_field_group = uniform_field_group(
                    collection_options,
                    || source_fields_have_uniform_visibility(tcx, item.span),
                    || span(tcx, item.owner_id.def_id),
                );
                for field in data.fields() {
                    let field_span = tcx.def_span(field.def_id);
                    let visibility = visibility_modifier(tcx, field.def_id);
                    let public_api = is_public_candidate_with_visibility(
                        tcx,
                        field.def_id,
                        test_surface,
                        visibility.as_deref(),
                    );
                    if let Some(index) = source_item_index
                        && public_api
                    {
                        source_item_fields[index]
                            .2
                            .push((tcx.item_name(field.def_id.to_def_id()), field.def_id));
                    }
                    if field_span.from_expansion() {
                        generated_fields.push(field.def_id);
                    }
                    let mut field_definition = definition(
                        tcx,
                        field.def_id,
                        &crate_name,
                        DefinitionKind::Field,
                        public_api,
                        visibility.as_deref(),
                    );
                    field_definition
                        .uniform_field_group
                        .clone_from(&uniform_field_group);
                    definitions.push(field_definition);
                    defined.insert(field.def_id);
                    adt_members.push((field.def_id, item.owner_id.def_id));
                }
            }
            hir::ItemKind::Enum(_, _, enumeration) => {
                for variant in enumeration.variants {
                    definitions.push(definition(
                        tcx,
                        variant.def_id,
                        &crate_name,
                        DefinitionKind::EnumVariant,
                        is_public_variant(tcx, variant.def_id, test_surface),
                        visibility_modifier(tcx, variant.def_id).as_deref(),
                    ));
                    defined.insert(variant.def_id);
                    adt_members.push((variant.def_id, item.owner_id.def_id));
                }
            }
            _ => {}
        }
    }

    for def_id in tcx.hir_body_owners() {
        if defined.insert(def_id) {
            definitions.push(definition(
                tcx,
                def_id,
                &crate_name,
                DefinitionKind::Other,
                false,
                visibility_modifier(tcx, def_id).as_deref(),
            ));
        }
    }

    let mut edges = Vec::new();
    for def_id in tcx.hir_body_owners() {
        let body = tcx.hir_body_owned_by(def_id);
        let mut visitor = ReferenceVisitor::new(
            tcx,
            def_id.to_def_id(),
            EdgeKind::Body,
            Some(tcx.typeck_body(body.id())),
            true,
        );
        visitor.visit_body(body);
        visitor.finish(&mut edges);
    }
    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let edge_start = edges.len();
        let mut visitor = ReferenceVisitor::new(
            tcx,
            def_id.to_def_id(),
            if tcx.def_kind(def_id) == DefKind::Use {
                EdgeKind::Reexport
            } else {
                EdgeKind::Interface
            },
            None,
            false,
        );
        visitor.visit_node(tcx.hir_node_by_def_id(def_id));
        visitor.finish(&mut edges);
        if let Some(trait_item) = tcx.trait_item_of(def_id.to_def_id())
            && let Some(trait_def_id) = tcx.trait_of_assoc(trait_item)
        {
            let trait_id = id(tcx, trait_def_id);
            let exposed_types: Vec<_> = edges[edge_start..]
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Interface)
                .map(|edge| edge.to.clone())
                .collect();
            edges.extend(exposed_types.into_iter().map(|target| Edge {
                from: trait_id.clone(),
                to: target,
                kind: EdgeKind::VisibilityRequirement,
            }));
        }
        if let Some(parent) = enclosing_module(tcx, def_id) {
            edges.push(Edge {
                from: id(tcx, def_id.to_def_id()),
                to: id(tcx, parent.to_def_id()),
                kind: EdgeKind::VisibilityParent,
            });
        }
        if matches!(
            diagnostic_kind(tcx, def_id),
            Some(DefinitionKind::InherentMethod | DefinitionKind::InherentAssociatedConstant)
        ) && let ty::Adt(adt, _) = tcx
            .type_of(tcx.local_parent(def_id))
            .instantiate_identity()
            .skip_norm_wip()
            .kind()
        {
            edges.push(Edge {
                from: id(tcx, def_id.to_def_id()),
                to: id(tcx, adt.did()),
                kind: EdgeKind::Interface,
            });
        }
        if matches!(
            tcx.def_kind(def_id),
            DefKind::AssocFn | DefKind::AssocConst { .. } | DefKind::AssocTy
        ) && matches!(tcx.def_kind(tcx.local_parent(def_id)), DefKind::Trait)
        {
            edges.push(Edge {
                from: id(tcx, def_id.to_def_id()),
                to: id(tcx, tcx.local_parent(def_id).to_def_id()),
                kind: EdgeKind::Interface,
            });
        }
    }
    for item_id in crate_items.free_items() {
        let item = tcx.hir_item(item_id);
        let (hir::ItemKind::Struct(_, _, data) | hir::ItemKind::Union(_, _, data)) = item.kind
        else {
            continue;
        };
        for field in data.fields() {
            let mut visitor = ReferenceVisitor::new(
                tcx,
                field.def_id.to_def_id(),
                EdgeKind::Interface,
                None,
                false,
            );
            visitor.visit_field_def(field);
            visitor.finish(&mut edges);
        }
    }
    edges.extend(adt_members.into_iter().map(|(member, adt)| Edge {
        from: id(tcx, member.to_def_id()),
        to: id(tcx, adt.to_def_id()),
        kind: EdgeKind::Interface,
    }));
    source_item_fields.sort_by_key(|(file_start, item_start, _)| (*file_start, *item_start));
    // A derive can expose a generated field whose visibility is governed by a
    // source field, as `rkyv::Archived<T>` does. HIR cannot prove that macro
    // relationship, so conservatively retain same-named source visibility when
    // the expansion callsite identifies its decorated item.
    edges.extend(generated_fields.into_iter().filter_map(|field| {
        let span = tcx.def_span(field);
        if !matches!(
            span.ctxt().outer_expn_data().kind,
            ExpnKind::Macro(MacroKind::Derive, _)
        ) {
            return None;
        }
        let source_callsite = span.source_callsite();
        let source_file = source_file_start(tcx, source_callsite);
        let source_position = source_callsite.hi().to_u32();
        let name = tcx.item_name(field.to_def_id());
        source_item_at_or_after(&source_item_fields, source_file, source_position)?
            .iter()
            .find(|(source_name, _)| *source_name == name)
            .map(|(_, source_field)| Edge {
                from: id(tcx, field.to_def_id()),
                to: id(tcx, source_field.to_def_id()),
                kind: EdgeKind::VisibilityRequirement,
            })
    }));

    edges.sort_by(|left, right| {
        (&left.from, &left.to, left.kind as u8).cmp(&(&right.from, &right.to, right.kind as u8))
    });
    edges.dedup_by(|left, right| {
        left.from == right.from && left.to == right.to && left.kind == right.kind
    });
    // Lowering a type exposed by a public trait impl can fail privacy checking
    // even when the selected product does not otherwise reference that type.
    // This includes concrete types exposed by refined `impl Trait` methods.
    let trait_impl_interface_sources: HashSet<String> = crate_items
        .impl_items()
        .map(|item| item.owner_id.def_id)
        .filter(|def_id| {
            let impl_def_id = tcx.local_parent(*def_id);
            matches!(
                tcx.def_kind(*def_id),
                DefKind::AssocFn | DefKind::AssocConst { .. } | DefKind::AssocTy
            ) && matches!(tcx.def_kind(impl_def_id), DefKind::Impl { of_trait: true })
                && tcx.effective_visibilities(()).is_reachable(impl_def_id)
        })
        .map(|def_id| id(tcx, def_id.to_def_id()))
        .collect();
    // Type aliases are transparent for privacy: preserve their exposed target
    // types, but do not suppress a visibility finding for the alias itself.
    let type_aliases: HashSet<&str> = definitions
        .iter()
        .filter(|definition| definition.kind == DefinitionKind::TypeAlias)
        .map(|definition| definition.id.as_str())
        .collect();
    let interface_targets = type_alias_interface_targets(&edges, &type_aliases);
    let mut pending_required_public_roots: Vec<&str> = edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::Interface && trait_impl_interface_sources.contains(&edge.from)
        })
        .map(|edge| edge.to.as_str())
        .collect();
    let mut required_public_roots = Vec::new();
    let mut examined_required_public_roots = HashSet::new();
    while let Some(target) = pending_required_public_roots.pop() {
        if !examined_required_public_roots.insert(target) {
            continue;
        }
        if type_aliases.contains(target) {
            pending_required_public_roots
                .extend(interface_targets.get(target).into_iter().flatten().copied());
        } else {
            required_public_roots.push(target.to_owned());
        }
    }
    // Lowering the local target of a public reexport fails with E0365 while
    // the reexport remains part of the crate interface.
    let public_reexport_sources: HashSet<String> = public_reexports
        .iter()
        .map(|def_id| id(tcx, def_id.to_def_id()))
        .collect();
    required_public_roots.extend(
        edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::Reexport && public_reexport_sources.contains(&edge.from)
            })
            .map(|edge| edge.to.clone()),
    );
    // Consumer paths through a public reexport are erased to its declaration
    // target in HIR. A containing namespace cannot be narrowed soundly until
    // the exported path itself can be attributed to consumers.
    required_public_roots.extend(
        public_reexports
            .into_iter()
            .filter_map(|def_id| enclosing_module(tcx, def_id))
            .map(|def_id| id(tcx, def_id.to_def_id())),
    );
    if is_proc_macro_crate {
        // Public exports from a proc-macro crate can only be macro entry points.
        required_public_roots.extend(
            definitions
                .iter()
                .filter(|definition| definition.public_api)
                .map(|definition| definition.id.clone()),
        );
    }
    required_public_roots.sort();
    required_public_roots.dedup();
    let roots = tcx
        .entry_fn(())
        .filter(|_| is_product_root)
        .map(|(def_id, _)| vec![id(tcx, def_id)])
        .unwrap_or_default();
    let mut conservative_roots: Vec<String> = tcx
        .hir_body_owners()
        .filter(|def_id| {
            matches!(
                tcx.def_kind(*def_id),
                DefKind::AssocFn | DefKind::AssocConst { .. }
            ) && matches!(
                tcx.def_kind(tcx.local_parent(*def_id)),
                DefKind::Trait | DefKind::Impl { of_trait: true }
            )
        })
        .map(|def_id| id(tcx, def_id.to_def_id()))
        .chain(
            definitions
                .iter()
                .filter(|definition| definition.dead_code_allowed)
                .map(|definition| definition.id.clone()),
        )
        .collect();
    conservative_roots.extend(
        crate_items
            .owners()
            .map(|owner| owner.def_id)
            .filter(|def_id| {
                matches!(
                    tcx.def_kind(*def_id),
                    DefKind::Fn | DefKind::AssocFn | DefKind::Static { .. }
                )
            })
            .filter(|def_id| {
                let attrs = tcx.codegen_fn_attrs(def_id.to_def_id());
                attrs.flags.contains(CodegenFnAttrFlags::NO_MANGLE) || attrs.symbol_name.is_some()
            })
            .map(|def_id| id(tcx, def_id.to_def_id())),
    );
    conservative_roots.sort();
    conservative_roots.dedup();

    Fragment {
        protocol_version: crate::protocol::ProtocolVersion,
        package_name,
        crate_name,
        crate_id,
        is_product_root,
        test_surface,
        definitions,
        edges,
        roots,
        conservative_roots,
        required_public_roots,
    }
}

fn is_public_candidate_with_visibility(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    test_surface: bool,
    visibility: Option<&str>,
) -> bool {
    !tcx.def_span(def_id).from_expansion()
        && visibility == Some("pub")
        && tcx.local_visibility(def_id).is_public()
        && (test_surface || tcx.effective_visibilities(()).is_exported(def_id))
}

fn visibility_modifier(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<String> {
    visibility_span(tcx, def_id)
        .and_then(|span| tcx.sess.source_map().span_to_snippet(span).ok())
        .and_then(|visibility| compact_visibility_modifier(&visibility))
}

fn uniform_field_group<T>(
    collection_options: CollectionOptions,
    fields_have_uniform_visibility: impl FnOnce() -> bool,
    group: impl FnOnce() -> Option<T>,
) -> Option<T> {
    if collection_options.preserve_uniform_field_visibility() && fields_have_uniform_visibility() {
        group()
    } else {
        None
    }
}

// HIR omits cfg-stripped fields, so uniformity must come from the complete source declaration.
fn source_fields_have_uniform_visibility(tcx: TyCtxt<'_>, item_span: rustc_span::Span) -> bool {
    if item_span.from_expansion() {
        return false;
    }
    let Ok(source) = tcx.sess.source_map().span_to_snippet(item_span) else {
        return false;
    };
    let mut parser = match rustc_parse::new_parser_from_source_str(
        &tcx.sess.psess,
        // The source map otherwise reuses the first parsed snippet for later items.
        FileName::Custom(format!(
            "hawk field declaration {}:{}",
            item_span.lo().to_u32(),
            item_span.hi().to_u32()
        )),
        source,
        StripTokens::Nothing,
    ) {
        Ok(parser) => parser,
        Err(errors) => {
            for error in errors {
                error.cancel();
            }
            return false;
        }
    };
    let item = match parser.parse_item(ForceCollect::No, AllowConstBlockItems::Yes) {
        Ok(Some(item)) => item,
        Ok(None) => return false,
        Err(error) => {
            error.cancel();
            return false;
        }
    };
    let fields = match &item.kind {
        ast::ItemKind::Struct(_, _, data) | ast::ItemKind::Union(_, _, data) => data.fields(),
        _ => return false,
    };
    let mut visibilities = fields.iter().map(|field| match field.vis.kind {
        ast::VisibilityKind::Inherited => Some(String::new()),
        _ => tcx
            .sess
            .source_map()
            .span_to_snippet(field.vis.span)
            .ok()
            .and_then(|visibility| compact_visibility_modifier(&visibility)),
    });
    let Some(Some(first)) = visibilities.next() else {
        return false;
    };
    visibilities.all(|visibility| visibility.as_ref() == Some(&first))
}

fn compact_visibility_modifier(visibility: &str) -> Option<String> {
    let bytes = visibility.as_bytes();
    let mut compact = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if bytes[index..].starts_with(b"//") {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            index += 2;
            let mut depth = 1;
            while index < bytes.len() && depth > 0 {
                if bytes[index..].starts_with(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes[index..].starts_with(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            if depth > 0 {
                return None;
            }
            continue;
        }
        compact.push(bytes[index] as char);
        index += 1;
    }
    Some(compact)
}

fn visibility_span(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<rustc_span::Span> {
    match tcx.hir_node_by_def_id(def_id) {
        Node::Item(item) => Some(item.vis_span),
        Node::ImplItem(item) => item.vis_span(),
        Node::Field(field) => Some(field.vis_span),
        _ => None,
    }
}

fn source_file_start(tcx: TyCtxt<'_>, span: rustc_span::Span) -> u32 {
    tcx.sess
        .source_map()
        .lookup_source_file(span.lo())
        .start_pos
        .to_u32()
}

fn source_item_at_or_after<T>(
    source_items: &[(u32, u32, T)],
    source_file: u32,
    source_position: u32,
) -> Option<&T> {
    let index = source_items.partition_point(|(file_start, item_start, _)| {
        (*file_start, *item_start) < (source_file, source_position)
    });
    let (file_start, _, item) = source_items.get(index)?;
    (*file_start == source_file).then_some(item)
}

fn type_alias_interface_targets<'a>(
    edges: &'a [Edge],
    type_aliases: &HashSet<&str>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut targets = HashMap::new();
    if type_aliases.is_empty() {
        return targets;
    }
    for edge in edges {
        if edge.kind == EdgeKind::Interface && type_aliases.contains(edge.from.as_str()) {
            targets
                .entry(edge.from.as_str())
                .or_insert_with(Vec::new)
                .push(edge.to.as_str());
        }
    }
    targets
}

fn is_public_variant(tcx: TyCtxt<'_>, def_id: LocalDefId, test_surface: bool) -> bool {
    !tcx.def_span(def_id).from_expansion()
        && (tcx.effective_visibilities(()).is_exported(def_id)
            || (test_surface && tcx.local_visibility(tcx.local_parent(def_id)).is_public()))
}

fn is_named_reexport(tcx: TyCtxt<'_>, def_id: LocalDefId) -> bool {
    matches!(
        tcx.hir_node_by_def_id(def_id),
        Node::Item(item) if matches!(item.kind, hir::ItemKind::Use(_, hir::UseKind::Single(_)))
    )
}

fn definition(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    crate_name: &str,
    kind: DefinitionKind,
    public_api: bool,
    visibility: Option<&str>,
) -> Definition {
    let has_explicit_visibility =
        visibility.is_some_and(|visibility| visibility.starts_with("pub"));
    let restricted_visibility = (kind != DefinitionKind::Reexport
        && !tcx.def_span(def_id).from_expansion()
        && has_explicit_visibility)
        .then(|| tcx.local_visibility(def_id));
    let restricted_visible_api =
        matches!(restricted_visibility, Some(ty::Visibility::Restricted(_)));
    Definition {
        id: id(tcx, def_id.to_def_id()),
        crate_name: crate_name.into(),
        name: definition_name(tcx, def_id, kind),
        kind,
        span: span(tcx, def_id),
        public_api,
        restricted_visible_api,
        crate_visible_api: restricted_visible_api
            && visibility == Some("pub(crate)")
            && restricted_visibility == Some(ty::Visibility::Restricted(CRATE_DEF_ID)),
        visible_reexport_api: kind == DefinitionKind::Reexport && has_explicit_visibility,
        module_scope: module_scope(tcx, def_id),
        uniform_field_group: None,
        dead_code_allowed: tcx
            .lint_level_spec_at_node(DEAD_CODE, tcx.local_def_id_to_hir_id(def_id))
            .level()
            == Level::Allow,
    }
}

fn diagnostic_kind(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<DefinitionKind> {
    match tcx.def_kind(def_id) {
        DefKind::Mod if def_id != CRATE_DEF_ID => Some(DefinitionKind::Module),
        DefKind::Fn => Some(DefinitionKind::Function),
        DefKind::Trait => Some(DefinitionKind::Trait),
        DefKind::Struct => Some(DefinitionKind::Struct),
        DefKind::Enum => Some(DefinitionKind::Enum),
        DefKind::Union => Some(DefinitionKind::Union),
        DefKind::TyAlias => Some(DefinitionKind::TypeAlias),
        DefKind::Const { .. } => Some(DefinitionKind::Constant),
        DefKind::Static { .. } => Some(DefinitionKind::Static),
        DefKind::Use => Some(DefinitionKind::Reexport),
        DefKind::AssocFn
            if matches!(
                tcx.def_kind(tcx.local_parent(def_id)),
                DefKind::Impl { of_trait: false }
            ) =>
        {
            Some(DefinitionKind::InherentMethod)
        }
        DefKind::AssocConst { .. }
            if matches!(
                tcx.def_kind(tcx.local_parent(def_id)),
                DefKind::Impl { of_trait: false }
            ) =>
        {
            Some(DefinitionKind::InherentAssociatedConstant)
        }
        _ => None,
    }
}

fn definition_name(tcx: TyCtxt<'_>, def_id: LocalDefId, kind: DefinitionKind) -> String {
    if kind != DefinitionKind::Reexport {
        return tcx.def_path_str(def_id.to_def_id());
    }

    let Node::Item(item) = tcx.hir_node_by_def_id(def_id) else {
        return tcx.def_path_str(def_id.to_def_id());
    };
    let Some(ident) = item.kind.ident() else {
        return tcx.def_path_str(def_id.to_def_id());
    };
    let name = ident.to_string();
    let parent = tcx.local_parent(def_id);
    if parent == CRATE_DEF_ID {
        name
    } else {
        format!("{}::{name}", tcx.def_path_str(parent.to_def_id()))
    }
}

fn enclosing_module(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<LocalDefId> {
    if def_id == CRATE_DEF_ID {
        return None;
    }
    let parent = tcx.local_parent(def_id);
    (parent != CRATE_DEF_ID && tcx.def_kind(parent) == DefKind::Mod).then_some(parent)
}

fn module_scope(tcx: TyCtxt<'_>, mut def_id: LocalDefId) -> Vec<String> {
    let mut scope = Vec::new();
    while def_id != CRATE_DEF_ID {
        def_id = tcx.local_parent(def_id);
        if def_id != CRATE_DEF_ID && tcx.def_kind(def_id) == DefKind::Mod {
            scope.push(tcx.item_name(def_id.to_def_id()).to_string());
        }
    }
    scope.reverse();
    scope
}

fn id(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    format!("{:?}", tcx.def_path_hash(def_id))
}

fn span(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<Span> {
    let span = tcx.def_span(def_id);
    if span.from_expansion() {
        return None;
    }
    Some(source_span(tcx, span))
}

fn source_span(tcx: TyCtxt<'_>, span: rustc_span::Span) -> Span {
    let location = tcx.sess.source_map().lookup_char_pos(span.lo());
    Span {
        file: normalize_source_path(
            &location
                .file
                .name
                .prefer_local_unconditionally()
                .to_string(),
        ),
        line: location.line,
        column: location.col.to_usize() + 1,
    }
}

fn normalize_source_path(path: &str) -> String {
    let mut normalized = PathBuf::new();
    for component in Path::new(&path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match normalized.components().next_back() {
                Some(Component::Normal(_)) => {
                    normalized.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => normalized.push(component.as_os_str()),
            },
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized.to_string_lossy().into_owned()
}

struct ReferenceVisitor<'tcx> {
    tcx: TyCtxt<'tcx>,
    source: DefId,
    edge_kind: EdgeKind,
    typeck_results: Option<&'tcx ty::TypeckResults<'tcx>>,
    traverse_bodies: bool,
    targets: HashSet<DefId>,
}

impl<'tcx> ReferenceVisitor<'tcx> {
    fn new(
        tcx: TyCtxt<'tcx>,
        source: DefId,
        edge_kind: EdgeKind,
        typeck_results: Option<&'tcx ty::TypeckResults<'tcx>>,
        traverse_bodies: bool,
    ) -> Self {
        Self {
            tcx,
            source,
            edge_kind,
            typeck_results,
            traverse_bodies,
            targets: HashSet::new(),
        }
    }

    fn finish(self, edges: &mut Vec<Edge>) {
        if self.targets.is_empty() {
            return;
        }
        let source = id(self.tcx, self.source);
        edges.reserve(self.targets.len());
        edges.extend(self.targets.into_iter().map(|target| Edge {
            from: source.clone(),
            to: id(self.tcx, target),
            kind: self.edge_kind,
        }));
    }

    fn record(&mut self, resolution: Res) {
        match resolution {
            Res::Def(DefKind::Ctor(CtorOf::Struct, ..), constructor) => {
                let adt = self.tcx.parent(constructor);
                self.record_def(adt);
                for field in &self.tcx.adt_def(adt).non_enum_variant().fields {
                    self.record_def(field.did);
                }
            }
            Res::Def(DefKind::Ctor(CtorOf::Variant, ..), constructor) => {
                self.record_def(self.tcx.parent(constructor));
            }
            Res::Def(DefKind::Variant, variant) => {
                self.record_def(variant);
            }
            Res::Def(_, def_id)
            | Res::SelfTyParam { trait_: def_id }
            | Res::SelfTyAlias {
                alias_to: def_id, ..
            } => self.record_def(def_id),
            _ => {}
        }
    }

    fn record_def(&mut self, def_id: DefId) {
        self.targets.insert(def_id);
    }

    fn record_non_enum_field(&mut self, adt: ty::AdtDef<'tcx>, hir_id: hir::HirId) {
        if let Some(typeck_results) = self.typeck_results
            && let Some(index) = typeck_results.opt_field_index(hir_id)
        {
            self.record_def(adt.non_enum_variant().fields[index].did);
        }
    }

    fn visit_node(&mut self, node: Node<'tcx>) {
        match node {
            Node::Item(item) => self.visit_item(item),
            Node::ImplItem(item) => self.visit_impl_item(item),
            Node::TraitItem(item) => self.visit_trait_item(item),
            Node::ForeignItem(item) => self.visit_foreign_item(item),
            _ => {}
        }
    }
}

impl<'tcx> Visitor<'tcx> for ReferenceVisitor<'tcx> {
    fn visit_nested_body(&mut self, body_id: hir::BodyId) {
        if !self.traverse_bodies {
            return;
        }
        let previous = self.typeck_results.replace(self.tcx.typeck_body(body_id));
        self.visit_body(self.tcx.hir_body(body_id));
        self.typeck_results = previous;
    }

    fn visit_path(&mut self, path: &hir::Path<'tcx>, hir_id: hir::HirId) {
        self.record(path.res);
        intravisit::walk_path(self, path);
        let _ = hir_id;
    }

    fn visit_expr(&mut self, expression: &'tcx hir::Expr<'tcx>) {
        if let Some(typeck_results) = self.typeck_results {
            match expression.kind {
                hir::ExprKind::Path(ref qpath @ hir::QPath::TypeRelative(..)) => {
                    self.record(typeck_results.qpath_res(qpath, expression.hir_id));
                }
                hir::ExprKind::Struct(qpath, fields, tail) => {
                    let resolution = typeck_results.qpath_res(qpath, expression.hir_id);
                    if matches!(qpath, hir::QPath::TypeRelative(..)) {
                        self.record(resolution);
                    }
                    if let Some(adt) = typeck_results.expr_ty(expression).ty_adt_def()
                        && !adt.is_enum()
                    {
                        for field in fields {
                            self.record_non_enum_field(adt, field.hir_id);
                        }
                        if !matches!(tail, hir::StructTailExpr::None) {
                            for field in &adt.non_enum_variant().fields {
                                self.record_def(field.did);
                            }
                        }
                    }
                }
                hir::ExprKind::Field(base, _) => {
                    if let Some(adt) = typeck_results.expr_ty_adjusted(base).ty_adt_def()
                        && !adt.is_enum()
                    {
                        self.record_non_enum_field(adt, expression.hir_id);
                    }
                }
                hir::ExprKind::OffsetOf(..) => {
                    if let Some(fields) = typeck_results.offset_of_data().get(expression.hir_id) {
                        for (container, variant, field) in fields {
                            if let ty::Adt(adt, _) = container.kind()
                                && !adt.is_enum()
                            {
                                self.record_def(adt.variant(*variant).fields[*field].did);
                            }
                        }
                    }
                }
                hir::ExprKind::MethodCall(..) => {
                    if let Some(def_id) = typeck_results.type_dependent_def_id(expression.hir_id) {
                        self.record_def(def_id);
                    }
                }
                _ => {}
            }
        }
        intravisit::walk_expr(self, expression);
    }

    fn visit_pat(&mut self, pattern: &'tcx hir::Pat<'tcx>) {
        if let Some(typeck_results) = self.typeck_results {
            match pattern.kind {
                hir::PatKind::Struct(ref qpath, fields, _) => {
                    if matches!(qpath, hir::QPath::TypeRelative(..)) {
                        self.record(typeck_results.qpath_res(qpath, pattern.hir_id));
                    }
                    if let Some(adt) = typeck_results.pat_ty(pattern).ty_adt_def()
                        && !adt.is_enum()
                    {
                        for field in fields {
                            self.record_non_enum_field(adt, field.hir_id);
                        }
                    }
                }
                hir::PatKind::TupleStruct(ref qpath, ..)
                    if matches!(qpath, hir::QPath::TypeRelative(..)) =>
                {
                    self.record(typeck_results.qpath_res(qpath, pattern.hir_id));
                }
                _ => {}
            }
        }
        intravisit::walk_pat(self, pattern);
    }

    fn visit_pat_expr(&mut self, expression: &'tcx hir::PatExpr<'tcx>) {
        if let Some(typeck_results) = self.typeck_results
            && let hir::PatExprKind::Path(ref qpath @ hir::QPath::TypeRelative(..)) =
                expression.kind
        {
            self.record(typeck_results.qpath_res(qpath, expression.hir_id));
        }
        intravisit::walk_pat_expr(self, expression);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::HashSet;
    use std::ffi::OsStr;
    use std::io::{self, Write};
    use std::path::Path;

    use super::{
        compact_visibility_modifier, normalize_source_path, parse_collection_options,
        source_item_at_or_after, type_alias_interface_targets, uniform_field_group,
        validate_frontend_protocol_version, write_fragment,
    };
    use cargo_hawk_internal::graph::{CollectionOptions, Edge, EdgeKind, Fragment};

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("simulated write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("simulated flush failure"))
        }
    }

    #[test]
    fn rejects_mismatched_frontend_protocol() {
        let error = validate_frontend_protocol_version("1")
            .expect_err("mismatched frontend protocol should fail");

        assert_eq!(
            error.to_string(),
            "Hawk frontend uses compiler driver protocol 1, but this driver uses protocol 2; install `cargo-hawk` and `cargo-hawk-driver` from the same release"
        );
    }

    #[test]
    fn fragment_emission_reports_buffered_write_failures() {
        let fragment = Fragment {
            protocol_version: crate::protocol::ProtocolVersion,
            package_name: "library".into(),
            crate_name: "library".into(),
            crate_id: "library".into(),
            is_product_root: false,
            test_surface: false,
            definitions: vec![],
            edges: vec![],
            roots: vec![],
            conservative_roots: vec![],
            required_public_roots: vec![],
        };

        let error = write_fragment(FailingWriter, &fragment, Path::new("fragment.json"))
            .expect_err("buffer flush should report the underlying write failure");

        insta::assert_snapshot!(error.to_string(), @"flush fragment.json");
    }

    #[test]
    fn collection_options_are_validated() {
        assert_eq!(
            parse_collection_options(None).expect("default collection options"),
            CollectionOptions::default()
        );
        assert!(
            parse_collection_options(Some(OsStr::new(
                CollectionOptions::new(true).as_env_value()
            )))
            .expect("uniform field collection option")
            .preserve_uniform_field_visibility()
        );
        insta::assert_snapshot!(
            parse_collection_options(Some(OsStr::new("unknown")))
                .expect_err("unknown collection option")
                .to_string(),
            @"unsupported HAWK_COLLECTION_OPTIONS value `unknown`"
        );
    }

    #[test]
    fn uniform_field_collection_is_lazy() {
        let parse_count = Cell::new(0);
        let group_count = Cell::new(0);
        let collect = |options| {
            uniform_field_group(
                options,
                || {
                    parse_count.set(parse_count.get() + 1);
                    true
                },
                || {
                    group_count.set(group_count.get() + 1);
                    Some("group")
                },
            )
        };

        assert_eq!(collect(CollectionOptions::default()), None);
        assert_eq!(parse_count.get(), 0);
        assert_eq!(group_count.get(), 0);

        assert_eq!(collect(CollectionOptions::new(true)), Some("group"));
        assert_eq!(parse_count.get(), 1);
        assert_eq!(group_count.get(), 1);
    }

    #[test]
    fn source_item_lookup_stays_within_the_matching_file() {
        let items = [(10, 20, "first"), (10, 40, "second"), (50, 60, "third")];

        assert_eq!(source_item_at_or_after(&items, 10, 1), Some(&"first"));
        assert_eq!(source_item_at_or_after(&items, 10, 20), Some(&"first"));
        assert_eq!(source_item_at_or_after(&items, 10, 21), Some(&"second"));
        assert_eq!(source_item_at_or_after(&items, 10, 41), None);
        assert_eq!(source_item_at_or_after(&items, 20, 1), None);
        assert_eq!(source_item_at_or_after(&items, 50, 60), Some(&"third"));
        assert_eq!(source_item_at_or_after(&items, 50, 61), None);
    }

    #[test]
    fn interface_target_index_only_contains_type_alias_sources() {
        let edges = [
            Edge {
                from: "alias".into(),
                to: "target".into(),
                kind: EdgeKind::Interface,
            },
            Edge {
                from: "function".into(),
                to: "argument".into(),
                kind: EdgeKind::Interface,
            },
            Edge {
                from: "alias".into(),
                to: "body_target".into(),
                kind: EdgeKind::Body,
            },
        ];

        let targets = type_alias_interface_targets(&edges, &["alias"].into_iter().collect());
        assert_eq!(targets.len(), 1);
        assert_eq!(targets.get("alias"), Some(&vec!["target"]));
        assert!(type_alias_interface_targets(&edges, &HashSet::new()).is_empty());
    }

    #[test]
    fn visibility_modifier_compaction_ignores_whitespace_and_comments() {
        assert_eq!(
            compact_visibility_modifier("pub /* outer /* nested */ comment */ ( crate )"),
            Some("pub(crate)".into())
        );
        assert_eq!(
            compact_visibility_modifier("pub // comment\n ( super )"),
            Some("pub(super)".into())
        );
        assert_eq!(compact_visibility_modifier("pub /*"), None);
    }

    #[test]
    fn source_paths_are_lexically_normalized() {
        assert_eq!(
            normalize_source_path("library/tests/../src/shared.rs"),
            "library/src/shared.rs"
        );
        assert_eq!(normalize_source_path("../shared.rs"), "../shared.rs");
    }
}
