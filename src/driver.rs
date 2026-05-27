use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use rustc_driver::{Callbacks, Compilation};
use rustc_errors::Applicability;
use rustc_hir as hir;
use rustc_hir::Node;
use rustc_hir::def::{CtorOf, DefKind, Res};
use rustc_hir::def_id::{CRATE_DEF_ID, DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_interface::interface;
use rustc_middle::ty::{self, TyCtxt};
use rustc_session::config::CrateType;
use rustc_span::Pos;
use rustc_span::Symbol;
use rustc_span::def_id::LOCAL_CRATE;
use rustc_span::hygiene::{ExpnKind, MacroKind};

use crate::graph::{
    Definition, DefinitionKind, Edge, EdgeKind, FindingKind, FixPlan, Fragment, Span,
};

pub fn is_wrapper_invocation(args: &[String]) -> bool {
    env::var_os("HAWK_OUTPUT_DIR").is_some()
        && args
            .get(1)
            .and_then(|arg| Path::new(arg).file_stem())
            .is_some_and(|stem| stem == "rustc")
}

pub fn run_wrapper(mut args: Vec<String>) -> ExitCode {
    args.remove(1);
    let output_dir = PathBuf::from(env::var_os("HAWK_OUTPUT_DIR").expect("HAWK_OUTPUT_DIR set"));
    let root_crate = env::var("HAWK_ROOT_CRATE").expect("HAWK_ROOT_CRATE set");
    let fix_plan = match env::var_os("HAWK_FIX_PLAN")
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
    }
    let mut callbacks = HawkCallbacks {
        output_dir,
        root_crate,
        fix_plan,
    };

    rustc_driver::catch_with_exit_code(move || {
        rustc_driver::run_compiler(&args, &mut callbacks);
    })
}

struct HawkCallbacks {
    output_dir: PathBuf,
    root_crate: String,
    fix_plan: Option<FixPlan>,
}

impl Callbacks for HawkCallbacks {
    fn config(&mut self, config: &mut interface::Config) {
        let run_id = env::var("HAWK_RUN_ID").ok();
        config.psess_created = Some(Box::new(move |parse_session| {
            parse_session.env_depinfo.get_mut().insert((
                Symbol::intern("HAWK_RUN_ID"),
                run_id.as_deref().map(Symbol::intern),
            ));
        }));
    }

    fn after_analysis<'tcx>(
        &mut self,
        _compiler: &interface::Compiler,
        tcx: TyCtxt<'tcx>,
    ) -> Compilation {
        if let Some(fix_plan) = &self.fix_plan {
            emit_fixes(tcx, fix_plan);
        } else if let Err(error) = emit_fragment(tcx, &self.root_crate, &self.output_dir) {
            tcx.dcx()
                .fatal(format!("hawk could not emit analysis graph: {error:#}"));
        }
        Compilation::Continue
    }
}

fn read_fix_plan(path: &Path) -> Result<FixPlan> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(file).with_context(|| format!("deserialize {}", path.display()))
}

fn emit_fixes(tcx: TyCtxt<'_>, fix_plan: &FixPlan) {
    let crate_items = tcx.hir_crate_items(());
    let mut visibility_fixes = Vec::new();
    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let Some(definition_kind) = diagnostic_kind(tcx, def_id) else {
            continue;
        };
        if !tcx.local_visibility(def_id).is_public() {
            continue;
        }
        let visibility_span = match tcx.hir_node_by_def_id(def_id) {
            Node::Item(item) => Some(item.vis_span),
            Node::ImplItem(item) => item.vis_span(),
            _ => None,
        };
        if let Some(visibility_span) = visibility_span {
            visibility_fixes.push((
                visibility_span,
                planned_fix(tcx, def_id, definition_kind, &fix_plan.targets),
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
            if tcx.local_visibility(field.def_id).is_public() {
                visibility_fixes.push((
                    field.vis_span,
                    planned_fix(tcx, field.def_id, DefinitionKind::Field, &fix_plan.targets),
                ));
            }
        }
    }

    let mut emitted_spans = Vec::new();
    for (span, kind) in &visibility_fixes {
        let Some(kind) = kind else {
            continue;
        };
        if emitted_spans.contains(span)
            || visibility_fixes
                .iter()
                .any(|(other_span, kind)| other_span == span && kind.is_none())
        {
            continue;
        }
        emit_fix(tcx, *span, *kind);
        emitted_spans.push(*span);
    }
}

fn planned_fix(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    definition_kind: DefinitionKind,
    targets: &[crate::graph::FixTarget],
) -> Option<FindingKind> {
    let id = id(tcx, def_id.to_def_id());
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let name = definition_name(tcx, def_id, definition_kind);
    targets
        .iter()
        .find(|target| {
            target.id == id
                || (target.crate_name == crate_name
                    && target.name == name
                    && target.definition_kind == definition_kind)
        })
        .map(|target| target.kind)
}

fn emit_fix(tcx: TyCtxt<'_>, visibility_span: rustc_span::Span, kind: FindingKind) {
    let mut diagnostic = tcx.dcx().struct_span_warn(
        visibility_span,
        "public visibility can be restricted for the selected Hawk product",
    );
    diagnostic.is_lint(kind.code().to_owned(), false);
    diagnostic.span_suggestion(
        visibility_span,
        "change this visibility to",
        "pub(crate)",
        Applicability::MachineApplicable,
    );
    diagnostic.emit();
}

fn emit_fragment(tcx: TyCtxt<'_>, root_crate: &str, output_dir: &Path) -> Result<()> {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let is_non_production = env::var("HAWK_CONSUMER_MODE").as_deref() == Ok("non-production");
    let test_surface = is_non_production && tcx.sess.opts.test;
    let is_product_root = if is_non_production {
        test_surface && tcx.entry_fn(()).is_some()
    } else {
        crate_name == root_crate && tcx.entry_fn(()).is_some()
    };
    let fragment = collect_fragment(tcx, crate_name.clone(), is_product_root, test_surface);
    let crate_id = id(tcx, CRATE_DEF_ID.to_def_id());
    let suffix: String = crate_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    let path = output_dir.join(format!("{crate_name}-{suffix}.json"));
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    write_fragment(file, &fragment, &path)
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
    crate_name: String,
    is_product_root: bool,
    test_surface: bool,
) -> Fragment {
    let mut definitions = Vec::new();
    let mut defined = HashSet::new();
    let mut adt_members = Vec::new();
    let mut source_item_fields = Vec::new();
    let mut generated_fields = Vec::new();
    let crate_items = tcx.hir_crate_items(());
    let is_proc_macro_crate = tcx.crate_types().contains(&CrateType::ProcMacro);

    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let kind = diagnostic_kind(tcx, def_id);
        let public_api = kind
            .is_some_and(|kind| kind != DefinitionKind::Reexport || is_named_reexport(tcx, def_id))
            && is_public_candidate(tcx, def_id, test_surface);
        definitions.push(definition(
            tcx,
            def_id,
            &crate_name,
            kind.unwrap_or(DefinitionKind::Other),
            public_api,
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
                for field in data.fields() {
                    let field_span = tcx.def_span(field.def_id);
                    if let Some(index) = source_item_index
                        && is_public_candidate(tcx, field.def_id, test_surface)
                    {
                        source_item_fields[index]
                            .2
                            .push((tcx.item_name(field.def_id.to_def_id()), field.def_id));
                    }
                    if field_span.from_expansion() {
                        generated_fields.push(field.def_id);
                    }
                    definitions.push(definition(
                        tcx,
                        field.def_id,
                        &crate_name,
                        DefinitionKind::Field,
                        is_public_candidate(tcx, field.def_id, test_surface),
                    ));
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
            ));
        }
    }

    let mut edges = Vec::new();
    for def_id in tcx.hir_body_owners() {
        let body = tcx.hir_body_owned_by(def_id);
        let mut visitor = ReferenceVisitor {
            tcx,
            source: id(tcx, def_id.to_def_id()),
            edge_kind: EdgeKind::Body,
            typeck_results: Some(tcx.typeck_body(body.id())),
            traverse_bodies: true,
            edges: &mut edges,
        };
        visitor.visit_body(body);
    }
    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let mut visitor = ReferenceVisitor {
            tcx,
            source: id(tcx, def_id.to_def_id()),
            edge_kind: if tcx.def_kind(def_id) == DefKind::Use {
                EdgeKind::Reexport
            } else {
                EdgeKind::Interface
            },
            typeck_results: None,
            traverse_bodies: false,
            edges: &mut edges,
        };
        visitor.visit_node(tcx.hir_node_by_def_id(def_id));
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
            DefKind::AssocFn | DefKind::AssocConst | DefKind::AssocTy
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
        let data = match item.kind {
            hir::ItemKind::Struct(_, _, data) | hir::ItemKind::Union(_, _, data) => data,
            _ => continue,
        };
        for field in data.fields() {
            let mut visitor = ReferenceVisitor {
                tcx,
                source: id(tcx, field.def_id.to_def_id()),
                edge_kind: EdgeKind::Interface,
                typeck_results: None,
                traverse_bodies: false,
                edges: &mut edges,
            };
            visitor.visit_field_def(field);
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
        source_item_fields
            .iter()
            .find(|(file_start, item_start, _)| {
                *file_start == source_file && *item_start >= source_position
            })?
            .2
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
                DefKind::AssocFn | DefKind::AssocConst | DefKind::AssocTy
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
    let mut pending_required_public_roots: Vec<String> = edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::Interface && trait_impl_interface_sources.contains(&edge.from)
        })
        .map(|edge| edge.to.clone())
        .collect();
    let mut required_public_roots = Vec::new();
    let mut examined_required_public_roots = HashSet::new();
    while let Some(target) = pending_required_public_roots.pop() {
        if !examined_required_public_roots.insert(target.clone()) {
            continue;
        }
        if type_aliases.contains(target.as_str()) {
            pending_required_public_roots.extend(
                edges
                    .iter()
                    .filter(|edge| edge.kind == EdgeKind::Interface && edge.from == target)
                    .map(|edge| edge.to.clone()),
            );
        } else {
            required_public_roots.push(target);
        }
    }
    // Lowering the local target of a public reexport fails with E0365 while
    // the reexport remains part of the crate interface.
    let public_reexports: Vec<LocalDefId> = crate_items
        .owners()
        .map(|owner| owner.def_id)
        .filter(|def_id| {
            tcx.def_kind(*def_id) == DefKind::Use && is_public_candidate(tcx, *def_id, test_surface)
        })
        .collect();
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
    let conservative_roots = tcx
        .hir_body_owners()
        .filter(|def_id| {
            matches!(
                tcx.def_kind(*def_id),
                DefKind::AssocFn | DefKind::AssocConst
            ) && matches!(
                tcx.def_kind(tcx.local_parent(*def_id)),
                DefKind::Trait | DefKind::Impl { of_trait: true }
            )
        })
        .map(|def_id| id(tcx, def_id.to_def_id()))
        .collect();

    Fragment {
        crate_name,
        is_product_root,
        definitions,
        edges,
        roots,
        conservative_roots,
        required_public_roots,
    }
}

fn is_public_candidate(tcx: TyCtxt<'_>, def_id: LocalDefId, test_surface: bool) -> bool {
    !tcx.def_span(def_id).from_expansion()
        && tcx.local_visibility(def_id).is_public()
        && (test_surface || tcx.effective_visibilities(()).is_exported(def_id))
}

fn source_file_start(tcx: TyCtxt<'_>, span: rustc_span::Span) -> u32 {
    tcx.sess
        .source_map()
        .lookup_source_file(span.lo())
        .start_pos
        .to_u32()
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
) -> Definition {
    Definition {
        id: id(tcx, def_id.to_def_id()),
        crate_name: crate_name.into(),
        name: definition_name(tcx, def_id, kind),
        kind,
        span: span(tcx, def_id),
        public_api,
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
        DefKind::Const => Some(DefinitionKind::Constant),
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
        DefKind::AssocConst
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

fn id(tcx: TyCtxt<'_>, def_id: DefId) -> String {
    format!("{:?}", tcx.def_path_hash(def_id))
}

fn span(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<Span> {
    let span = tcx.def_span(def_id);
    if span.from_expansion() {
        return None;
    }
    let location = tcx.sess.source_map().lookup_char_pos(span.lo());
    Some(Span {
        file: location
            .file
            .name
            .prefer_local_unconditionally()
            .to_string(),
        line: location.line,
        column: location.col.to_usize() + 1,
    })
}

struct ReferenceVisitor<'tcx, 'edges> {
    tcx: TyCtxt<'tcx>,
    source: String,
    edge_kind: EdgeKind,
    typeck_results: Option<&'tcx ty::TypeckResults<'tcx>>,
    traverse_bodies: bool,
    edges: &'edges mut Vec<Edge>,
}

impl<'tcx> ReferenceVisitor<'tcx, '_> {
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
        self.edges.push(Edge {
            from: self.source.clone(),
            to: id(self.tcx, def_id),
            kind: self.edge_kind,
        });
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

impl<'tcx> Visitor<'tcx> for ReferenceVisitor<'tcx, '_> {
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
    use std::io::{self, Write};
    use std::path::Path;

    use super::write_fragment;
    use crate::graph::Fragment;

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
    fn fragment_emission_reports_buffered_write_failures() {
        let fragment = Fragment {
            crate_name: "library".into(),
            is_product_root: false,
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
}
