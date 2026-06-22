use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
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

use crate::graph::{
    Definition, DefinitionKind, Edge, EdgeKind, FindingKind, FixPlan, Fragment, Span,
    VisibilityReduction,
};

pub fn is_wrapper_invocation(args: &[String]) -> bool {
    env::var_os("HAWK_OUTPUT_DIR").is_some()
        && env::var_os("HAWK_ROOT_CRATE").is_some()
        && args.get(1).is_some()
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
        // A Hawk visibility fix can make an import unused in one consumer
        // mode while it remains required by another mode.
        args.push("--allow".to_owned());
        args.push("unused_imports".to_owned());
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
        if definition_kind == DefinitionKind::Reexport && !is_named_reexport(tcx, def_id) {
            continue;
        }
        let visibility_span = visibility_span(tcx, def_id);
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
            visibility_fixes.push((
                field.vis_span,
                planned_fix(tcx, field.def_id, DefinitionKind::Field, &fix_plan.targets),
            ));
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

    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let mut derive_fixes: Vec<(rustc_span::Span, HashSet<String>, Vec<rustc_span::Span>)> =
        Vec::new();
    for owner in crate_items.owners() {
        let impl_def_id = owner.def_id;
        let Some(definition) = derived_trait_definition(tcx, impl_def_id, &crate_name) else {
            continue;
        };
        if !fix_plan.targets.iter().any(|target| {
            target.kind == FindingKind::UnnecessaryDerive
                && fix_target_matches_definition(target, &definition)
        }) {
            continue;
        }
        let Some(trait_span) = derived_trait_span(tcx, impl_def_id) else {
            continue;
        };
        let Some(attribute_span) = derive_attribute_span(tcx, trait_span) else {
            continue;
        };
        let trait_name = definition
            .name
            .rsplit_once(" as ")
            .map_or(definition.name.as_str(), |(_, trait_name)| trait_name)
            .to_owned();
        let related_attribute_spans = if trait_name == "Default" {
            default_variant_attribute_spans(tcx, impl_def_id)
        } else {
            Vec::new()
        };
        if let Some((_, traits, related_spans)) = derive_fixes
            .iter_mut()
            .find(|(span, _, _)| *span == attribute_span)
        {
            traits.insert(trait_name);
            related_spans.extend(related_attribute_spans);
        } else {
            derive_fixes.push((
                attribute_span,
                HashSet::from([trait_name]),
                related_attribute_spans,
            ));
        }
    }
    for (attribute_span, traits, related_attribute_spans) in derive_fixes {
        let Some(replacement) = derive_attribute_replacement(tcx, attribute_span, &traits) else {
            continue;
        };
        let mut diagnostic = tcx.dcx().struct_span_warn(
            attribute_span,
            "derived trait implementation is unnecessary",
        );
        diagnostic.is_lint(FindingKind::UnnecessaryDerive.code().to_owned(), false);
        if related_attribute_spans.is_empty() {
            diagnostic.span_suggestion(
                attribute_span,
                "remove the unnecessary derive",
                replacement,
                Applicability::MachineApplicable,
            );
        } else {
            let mut replacements = vec![(attribute_span, replacement)];
            replacements.extend(
                related_attribute_spans
                    .into_iter()
                    .map(|span| (span, String::new())),
            );
            diagnostic.multipart_suggestion(
                "remove the unnecessary derive",
                replacements,
                Applicability::MachineApplicable,
            );
        }
        diagnostic.emit();
    }
}

fn default_variant_attribute_spans(
    tcx: TyCtxt<'_>,
    impl_def_id: LocalDefId,
) -> Vec<rustc_span::Span> {
    let trait_ref = tcx.impl_trait_ref(impl_def_id).instantiate_identity();
    let ty::Adt(adt, _) = trait_ref.self_ty().kind() else {
        return Vec::new();
    };
    let Some(adt_def_id) = adt.did().as_local() else {
        return Vec::new();
    };
    let hir::ItemKind::Enum(_, _, enumeration) = tcx.hir_expect_item(adt_def_id).kind else {
        return Vec::new();
    };
    enumeration
        .variants
        .iter()
        .flat_map(|variant| tcx.hir_attrs(variant.hir_id))
        .filter(|attribute| attribute.has_name(Symbol::intern("default")))
        .map(hir::Attribute::span)
        .collect()
}

fn planned_fix(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    definition_kind: DefinitionKind,
    targets: &[crate::graph::FixTarget],
) -> Option<(FindingKind, VisibilityReduction)> {
    let id = id(tcx, def_id.to_def_id());
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let name = definition_name(tcx, def_id, definition_kind);
    let definition_span = span(tcx, def_id);
    targets
        .iter()
        .find(|target| {
            target.id == id
                || (target.crate_name == crate_name
                    && target.name == name
                    && target.definition_kind == definition_kind
                    && match (&target.span, &definition_span) {
                        (Some(target), Some(definition)) => {
                            target.file == definition.file
                                && target.line == definition.line
                                && target.column == definition.column
                        }
                        _ => false,
                    })
        })
        .and_then(|target| {
            target
                .replacement
                .map(|replacement| (target.kind, replacement))
        })
}

fn fix_target_matches_definition(
    target: &crate::graph::FixTarget,
    definition: &Definition,
) -> bool {
    if target.definition_kind == DefinitionKind::DerivedTrait
        && definition.kind == DefinitionKind::DerivedTrait
    {
        return target.crate_name == definition.crate_name && target.name == definition.name;
    }
    target.id == definition.id
        || (target.crate_name == definition.crate_name
            && target.name == definition.name
            && target.definition_kind == definition.kind
            && match (&target.span, &definition.span) {
                (Some(target), Some(definition)) => {
                    target.file == definition.file
                        && target.line == definition.line
                        && target.column == definition.column
                }
                _ => false,
            })
}

fn derive_attribute_span(
    tcx: TyCtxt<'_>,
    trait_span: rustc_span::Span,
) -> Option<rustc_span::Span> {
    let source_map = tcx.sess.source_map();
    let previous = source_map.span_to_prev_source(trait_span).ok()?;
    let attribute_start = previous.rfind("#[derive")?;
    let prefix = &previous[attribute_start..];
    let open = prefix.find('(')?;
    if prefix[open + 1..].contains(']') {
        return None;
    }
    let next = source_map.span_to_next_source(trait_span).ok()?;
    let attribute_end = next.find(']')? + 1;
    let span = trait_span
        .with_lo(trait_span.lo() - BytePos((previous.len() - attribute_start) as u32))
        .with_hi(trait_span.hi() + BytePos(attribute_end as u32));
    source_map
        .span_to_snippet(span)
        .ok()
        .filter(|snippet| snippet.starts_with("#[derive"))?;
    Some(span)
}

fn derive_attribute_replacement(
    tcx: TyCtxt<'_>,
    attribute_span: rustc_span::Span,
    removed_traits: &HashSet<String>,
) -> Option<String> {
    let source = tcx.sess.source_map().span_to_snippet(attribute_span).ok()?;
    let mut parser = match rustc_parse::new_parser_from_source_str(
        &tcx.sess.psess,
        FileName::Custom(format!(
            "hawk derive attribute {}:{}",
            attribute_span.lo().to_u32(),
            attribute_span.hi().to_u32()
        )),
        format!("{source}\nstruct __HawkDeriveFix;"),
        StripTokens::Nothing,
    ) {
        Ok(parser) => parser,
        Err(errors) => {
            for error in errors {
                error.cancel();
            }
            return None;
        }
    };
    let item = match parser.parse_item(ForceCollect::No, AllowConstBlockItems::Yes) {
        Ok(Some(item)) => item,
        Ok(None) => return None,
        Err(error) => {
            error.cancel();
            return None;
        }
    };
    let attribute = item
        .attrs
        .iter()
        .find(|attribute| attribute.has_name(Symbol::intern("derive")))?;
    let attribute_start = attribute.span.lo().to_u32();
    let entries = attribute.meta_item_list()?;
    let mut parsed_entries = Vec::with_capacity(entries.len());
    let mut matched_traits = HashSet::new();
    for entry in entries {
        let ast::MetaItemInner::MetaItem(meta) = entry else {
            return None;
        };
        if !meta.is_word() {
            return None;
        }
        let name = meta.path.segments.last()?.ident.name.to_string();
        let start = (meta.span.lo().to_u32() - attribute_start) as usize;
        let end = (meta.span.hi().to_u32() - attribute_start) as usize;
        let removed = removed_traits.contains(&name);
        if removed {
            matched_traits.insert(name);
        }
        parsed_entries.push((start, end, removed));
    }
    if matched_traits.len() != removed_traits.len() {
        return None;
    }
    if parsed_entries.iter().all(|(_, _, removed)| *removed) {
        return Some(String::new());
    }

    let mut ranges = Vec::new();
    let mut index = 0;
    while index < parsed_entries.len() {
        if !parsed_entries[index].2 {
            index += 1;
            continue;
        }
        let run_start = index;
        while index < parsed_entries.len() && parsed_entries[index].2 {
            index += 1;
        }
        if index < parsed_entries.len() {
            ranges.push(parsed_entries[run_start].0..parsed_entries[index].0);
        } else if run_start > 0 {
            ranges.push(parsed_entries[run_start - 1].1..parsed_entries[index - 1].1);
        }
    }

    let mut replacement = source;
    for range in ranges.into_iter().rev() {
        replacement.replace_range(range, "");
    }
    Some(replacement)
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

fn emit_fragment(tcx: TyCtxt<'_>, root_crate: &str, output_dir: &Path) -> Result<()> {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let crate_id = id(tcx, CRATE_DEF_ID.to_def_id());
    let is_non_production = env::var("HAWK_CONSUMER_MODE").as_deref() == Ok("non-production");
    let collect_unnecessary_derives = env::var_os("HAWK_UNNECESSARY_DERIVE").is_some();
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
        .filter(|character| character.is_ascii_alphanumeric())
        .collect();
    let fragment = collect_fragment(
        tcx,
        crate_name.clone(),
        crate_id,
        is_product_root,
        test_surface,
        collect_unnecessary_derives,
    );
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
    crate_id: String,
    is_product_root: bool,
    test_surface: bool,
    collect_unnecessary_derives: bool,
) -> Fragment {
    let mut definitions = Vec::new();
    let mut defined = HashSet::new();
    let mut adt_members = Vec::new();
    let mut source_item_fields = Vec::new();
    let mut generated_fields = Vec::new();
    let crate_items = tcx.hir_crate_items(());
    let is_proc_macro_crate = tcx.crate_types().contains(&CrateType::ProcMacro);
    let supported_derive_traits: Vec<_> = if collect_unnecessary_derives {
        crate_items
            .owners()
            .filter_map(|owner| {
                let def_id = owner.def_id;
                (matches!(tcx.def_kind(def_id), DefKind::Impl { of_trait: true })
                    && tcx.is_builtin_derived(def_id.to_def_id()))
                .then(|| tcx.impl_trait_ref(def_id).instantiate_identity().def_id)
                .filter(|trait_def_id| supported_derive_trait_name(tcx, *trait_def_id).is_some())
            })
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        Vec::new()
    };

    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        if collect_unnecessary_derives
            && let Some(definition) = derived_trait_definition(tcx, def_id, &crate_name)
        {
            definitions.push(definition);
            defined.insert(def_id);
            continue;
        }
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
                let uniform_field_group = if source_fields_have_uniform_visibility(tcx, item.span) {
                    span(tcx, item.owner_id.def_id)
                } else {
                    None
                };
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
                    let mut field_definition = definition(
                        tcx,
                        field.def_id,
                        &crate_name,
                        DefinitionKind::Field,
                        is_public_candidate(tcx, field.def_id, test_surface),
                    );
                    field_definition.uniform_field_group = uniform_field_group.clone();
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
    if collect_unnecessary_derives {
        for owner in crate_items.owners() {
            let impl_def_id = owner.def_id;
            if !matches!(tcx.def_kind(impl_def_id), DefKind::Impl { of_trait: true })
                || !tcx.is_builtin_derived(impl_def_id.to_def_id())
            {
                continue;
            }
            let trait_ref = tcx.impl_trait_ref(impl_def_id).instantiate_identity();
            if supported_derive_trait_name(tcx, trait_ref.def_id).is_none() {
                continue;
            }
            let ty::Adt(adt, args) = trait_ref.self_ty().kind() else {
                continue;
            };
            for field in adt.all_fields() {
                edges.extend(
                    derived_impl_ids_for_requirement(tcx, trait_ref.def_id, field.ty(tcx, args))
                        .into_iter()
                        .map(|target| Edge {
                            from: id(tcx, impl_def_id.to_def_id()),
                            to: target,
                            kind: EdgeKind::TraitRequirement,
                        }),
                );
            }
        }
    }
    for def_id in tcx.hir_body_owners() {
        let body = tcx.hir_body_owned_by(def_id);
        let parent = tcx.local_parent(def_id);
        let derive_source = collect_unnecessary_derives
            .then_some(())
            .filter(|_| matches!(tcx.def_kind(parent), DefKind::Impl { of_trait: true }))
            .map(|()| parent.to_def_id())
            .filter(|parent| tcx.is_builtin_derived(*parent))
            .map(|parent| id(tcx, parent));
        let mut visitor = ReferenceVisitor {
            tcx,
            source: id(tcx, def_id.to_def_id()),
            derive_source,
            edge_kind: EdgeKind::Body,
            typeck_results: Some(tcx.typeck_body(body.id())),
            typing_env: Some(ty::TypingEnv::post_analysis(tcx, def_id.to_def_id())),
            collect_derive_requirements: collect_unnecessary_derives,
            supported_derive_traits: &supported_derive_traits,
            traverse_bodies: true,
            edges: &mut edges,
        };
        visitor.visit_body(body);
    }
    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let edge_start = edges.len();
        let mut visitor = ReferenceVisitor {
            tcx,
            source: id(tcx, def_id.to_def_id()),
            derive_source: None,
            edge_kind: if tcx.def_kind(def_id) == DefKind::Use {
                EdgeKind::Reexport
            } else {
                EdgeKind::Interface
            },
            typeck_results: None,
            typing_env: None,
            collect_derive_requirements: false,
            supported_derive_traits: &supported_derive_traits,
            traverse_bodies: false,
            edges: &mut edges,
        };
        visitor.visit_node(tcx.hir_node_by_def_id(def_id));
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
                derive_source: None,
                edge_kind: EdgeKind::Interface,
                typeck_results: None,
                typing_env: None,
                collect_derive_requirements: false,
                supported_derive_traits: &supported_derive_traits,
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
    let mut conservative_roots: Vec<String> = tcx
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
        .chain(
            definitions
                .iter()
                .filter(|definition| definition.dead_code_allowed)
                .map(|definition| definition.id.clone()),
        )
        .collect();
    if collect_unnecessary_derives {
        for owner in crate_items.owners() {
            let def_id = owner.def_id;
            if matches!(
                tcx.def_kind(def_id),
                DefKind::Fn
                    | DefKind::AssocFn
                    | DefKind::Trait
                    | DefKind::Impl { .. }
                    | DefKind::TyAlias
                    | DefKind::AssocTy
                    | DefKind::Struct
                    | DefKind::Enum
                    | DefKind::Union
            ) {
                for (predicate, _) in tcx.predicates_of(def_id).instantiate_identity(tcx) {
                    if let ty::ClauseKind::Trait(trait_predicate) = predicate.kind().skip_binder() {
                        conservative_roots.extend(derived_impl_ids_for_requirement(
                            tcx,
                            trait_predicate.trait_ref.def_id,
                            trait_predicate.self_ty(),
                        ));
                    }
                }
            }

            if matches!(
                tcx.def_kind(def_id),
                DefKind::Fn
                    | DefKind::AssocFn
                    | DefKind::Const
                    | DefKind::AssocConst
                    | DefKind::Static { .. }
            ) && tcx.hir_maybe_body_owned_by(def_id).is_some()
            {
                for opaque_def_id in tcx.opaque_types_defined_by(def_id) {
                    let hidden_ty = tcx
                        .type_of_opaque_hir_typeck(opaque_def_id)
                        .instantiate_identity();
                    for (predicate, _) in tcx
                        .explicit_item_bounds(opaque_def_id.to_def_id())
                        .iter_identity_copied()
                    {
                        if let ty::ClauseKind::Trait(trait_predicate) =
                            predicate.kind().skip_binder()
                        {
                            conservative_roots.extend(derived_impl_ids_for_requirement(
                                tcx,
                                trait_predicate.trait_ref.def_id,
                                hidden_ty,
                            ));
                        }
                    }
                }
            }

            if tcx.def_kind(def_id) == DefKind::AssocTy
                && let Some(trait_item_def_id) = tcx.trait_item_of(def_id.to_def_id())
            {
                let associated_ty = tcx.type_of(def_id).instantiate_identity();
                for (predicate, _) in tcx
                    .explicit_item_bounds(trait_item_def_id)
                    .iter_identity_copied()
                {
                    if let ty::ClauseKind::Trait(trait_predicate) = predicate.kind().skip_binder() {
                        conservative_roots.extend(derived_impl_ids_for_requirement(
                            tcx,
                            trait_predicate.trait_ref.def_id,
                            associated_ty,
                        ));
                    }
                }
            }

            if matches!(tcx.def_kind(def_id), DefKind::Impl { of_trait: true }) {
                let trait_ref = tcx.impl_trait_ref(def_id).instantiate_identity();
                let bound_trait_ref = ty::Binder::dummy(trait_ref);
                for (predicate, _) in tcx
                    .explicit_super_predicates_of(trait_ref.def_id)
                    .iter_identity_copied()
                {
                    let predicate = predicate.instantiate_supertrait(tcx, bound_trait_ref);
                    if let ty::ClauseKind::Trait(trait_predicate) = predicate.kind().skip_binder() {
                        conservative_roots.extend(derived_impl_ids_for_requirement(
                            tcx,
                            trait_predicate.trait_ref.def_id,
                            trait_predicate.self_ty(),
                        ));
                    }
                }
            }
        }
    }
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

fn is_public_candidate(tcx: TyCtxt<'_>, def_id: LocalDefId, test_surface: bool) -> bool {
    !tcx.def_span(def_id).from_expansion()
        && has_visibility_modifier(tcx, def_id, "pub")
        && tcx.local_visibility(def_id).is_public()
        && (test_surface || tcx.effective_visibilities(()).is_exported(def_id))
}

fn has_visibility_modifier(tcx: TyCtxt<'_>, def_id: LocalDefId, expected: &str) -> bool {
    visibility_modifier(tcx, def_id).as_deref() == Some(expected)
}

fn visibility_modifier(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<String> {
    visibility_span(tcx, def_id)
        .and_then(|span| tcx.sess.source_map().span_to_snippet(span).ok())
        .and_then(|visibility| compact_visibility_modifier(&visibility))
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
    let visibility = visibility_modifier(tcx, def_id);
    let has_explicit_visibility = visibility
        .as_deref()
        .is_some_and(|visibility| visibility.starts_with("pub"));
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
            && visibility.as_deref() == Some("pub(crate)")
            && restricted_visibility == Some(ty::Visibility::Restricted(CRATE_DEF_ID)),
        visible_reexport_api: kind == DefinitionKind::Reexport && has_explicit_visibility,
        module_scope: module_scope(tcx, def_id),
        uniform_field_group: None,
        dead_code_allowed: tcx
            .lint_level_at_node(DEAD_CODE, tcx.local_def_id_to_hir_id(def_id))
            .level
            == Level::Allow,
    }
}

fn derived_trait_definition(
    tcx: TyCtxt<'_>,
    impl_def_id: LocalDefId,
    crate_name: &str,
) -> Option<Definition> {
    if !matches!(tcx.def_kind(impl_def_id), DefKind::Impl { of_trait: true })
        || !tcx.is_builtin_derived(impl_def_id.to_def_id())
    {
        return None;
    }

    let trait_ref = tcx.impl_trait_ref(impl_def_id).instantiate_identity();
    let trait_name = supported_derive_trait_name(tcx, trait_ref.def_id)?;
    let ty::Adt(adt, _) = trait_ref.self_ty().kind() else {
        return None;
    };
    let type_def_id = adt.did().as_local()?;
    if tcx.def_span(type_def_id).from_expansion() {
        return None;
    }

    let derive_span = tcx
        .def_span(impl_def_id)
        .ctxt()
        .outer_expn_data()
        .call_site
        .source_callsite();
    let type_name = tcx.def_path_str(type_def_id.to_def_id());
    Some(Definition {
        id: id(tcx, impl_def_id.to_def_id()),
        crate_name: crate_name.into(),
        name: format!("{type_name} as {trait_name}"),
        kind: DefinitionKind::DerivedTrait,
        span: source_span(tcx, derive_span).or_else(|| span(tcx, type_def_id)),
        public_api: false,
        restricted_visible_api: false,
        crate_visible_api: false,
        visible_reexport_api: false,
        module_scope: module_scope(tcx, type_def_id),
        uniform_field_group: None,
        dead_code_allowed: false,
    })
}

fn derived_trait_span(tcx: TyCtxt<'_>, impl_def_id: LocalDefId) -> Option<rustc_span::Span> {
    if !matches!(tcx.def_kind(impl_def_id), DefKind::Impl { of_trait: true })
        || !tcx.is_builtin_derived(impl_def_id.to_def_id())
    {
        return None;
    }
    let trait_ref = tcx.impl_trait_ref(impl_def_id).instantiate_identity();
    supported_derive_trait_name(tcx, trait_ref.def_id)?;
    let ty::Adt(adt, _) = trait_ref.self_ty().kind() else {
        return None;
    };
    let type_def_id = adt.did().as_local()?;
    if tcx.def_span(type_def_id).from_expansion() {
        return None;
    }
    Some(
        tcx.def_span(impl_def_id)
            .ctxt()
            .outer_expn_data()
            .call_site
            .source_callsite(),
    )
}

fn supported_derive_trait_name(tcx: TyCtxt<'_>, trait_def_id: DefId) -> Option<&'static str> {
    match tcx.item_name(trait_def_id).as_str() {
        "Clone" => Some("Clone"),
        "Debug" => Some("Debug"),
        "Default" => Some("Default"),
        "Hash" => Some("Hash"),
        "PartialEq" => Some("PartialEq"),
        "Eq" => Some("Eq"),
        "PartialOrd" => Some("PartialOrd"),
        "Ord" => Some("Ord"),
        _ => None,
    }
}

fn derived_impl_ids_for_requirement<'tcx>(
    tcx: TyCtxt<'tcx>,
    trait_def_id: DefId,
    self_ty: ty::Ty<'tcx>,
) -> Vec<String> {
    if supported_derive_trait_name(tcx, trait_def_id).is_none() {
        return Vec::new();
    }
    let mut impls: Vec<_> = self_ty
        .walk()
        .filter_map(|argument| argument.as_type())
        .flat_map(|ty| tcx.non_blanket_impls_for_ty(trait_def_id, ty))
        .map(|impl_def_id| id(tcx, impl_def_id))
        .collect();
    impls.sort();
    impls.dedup();
    impls
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
    source_span(tcx, tcx.def_span(def_id))
}

fn source_span(tcx: TyCtxt<'_>, span: rustc_span::Span) -> Option<Span> {
    if span.from_expansion() {
        return None;
    }
    let location = tcx.sess.source_map().lookup_char_pos(span.lo());
    Some(Span {
        file: normalize_source_path(
            location
                .file
                .name
                .prefer_local_unconditionally()
                .to_string(),
        ),
        line: location.line,
        column: location.col.to_usize() + 1,
    })
}

fn normalize_source_path(path: String) -> String {
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

struct ReferenceVisitor<'tcx, 'edges> {
    tcx: TyCtxt<'tcx>,
    source: String,
    derive_source: Option<String>,
    edge_kind: EdgeKind,
    typeck_results: Option<&'tcx ty::TypeckResults<'tcx>>,
    typing_env: Option<ty::TypingEnv<'tcx>>,
    collect_derive_requirements: bool,
    supported_derive_traits: &'edges [DefId],
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

    fn record_trait_requirement(&mut self, trait_def_id: DefId, self_ty: ty::Ty<'tcx>) {
        for impl_id in derived_impl_ids_for_requirement(self.tcx, trait_def_id, self_ty) {
            self.edges.push(Edge {
                from: self
                    .derive_source
                    .clone()
                    .unwrap_or_else(|| self.source.clone()),
                to: impl_id,
                kind: EdgeKind::TraitRequirement,
            });
        }
    }

    fn record_derived_impl(&mut self, impl_def_id: DefId) {
        self.edges.push(Edge {
            from: self
                .derive_source
                .clone()
                .unwrap_or_else(|| self.source.clone()),
            to: id(self.tcx, impl_def_id),
            kind: EdgeKind::TraitRequirement,
        });
    }

    fn record_callee(&mut self, callee_def_id: DefId, args: ty::GenericArgsRef<'tcx>) {
        let Some(typing_env) = self.typing_env else {
            return;
        };
        let args = self.tcx.normalize_erasing_regions(typing_env, args);

        if let Some(trait_def_id) = self.tcx.trait_of_assoc(callee_def_id)
            && let Some(self_ty) = args.types().next()
        {
            self.record_trait_requirement(trait_def_id, self_ty);
        }

        if matches!(
            self.tcx.def_kind(callee_def_id),
            DefKind::Fn | DefKind::AssocFn
        ) && let Ok(Some(instance)) =
            ty::Instance::try_resolve(self.tcx, typing_env, callee_def_id, args)
            && let Some(impl_def_id) = self.tcx.trait_impl_of_assoc(instance.def_id())
        {
            self.record_derived_impl(impl_def_id);
        }

        self.record_predicate_requirements(callee_def_id, args);
    }

    fn record_predicate_requirements(&mut self, definition: DefId, args: ty::GenericArgsRef<'tcx>) {
        if args.len() != self.tcx.generics_of(definition).count() {
            return;
        }
        let predicates = self
            .tcx
            .predicates_of(definition)
            .instantiate(self.tcx, args);
        for (predicate, _) in predicates {
            if let ty::ClauseKind::Trait(trait_predicate) = predicate.kind().skip_binder() {
                let trait_def_id = trait_predicate.trait_ref.def_id;
                let self_ty = trait_predicate.self_ty();
                self.record_trait_requirement(trait_def_id, self_ty);
                if matches!(
                    self.tcx.item_name(trait_def_id).as_str(),
                    "FromIterator" | "Extend"
                ) {
                    for index in 0..self.supported_derive_traits.len() {
                        self.record_trait_requirement(self.supported_derive_traits[index], self_ty);
                    }
                }
            }
        }
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
            if self.collect_derive_requirements {
                let source_ty = typeck_results.expr_ty(expression);
                let target_ty = typeck_results.expr_ty_adjusted(expression);
                for target in target_ty.walk().filter_map(|argument| argument.as_type()) {
                    if let ty::Dynamic(predicates, ..) = target.kind()
                        && let Some(principal) = predicates.principal()
                    {
                        self.record_trait_requirement(principal.def_id(), source_ty);
                    }
                }
                if let Some(def_id) = typeck_results.type_dependent_def_id(expression.hir_id) {
                    self.record_callee(def_id, typeck_results.node_args(expression.hir_id));
                }
                if let ty::FnDef(def_id, args) = *typeck_results.expr_ty(expression).kind() {
                    self.record_callee(def_id, args);
                }
            }
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

    fn visit_ty(&mut self, hir_ty: &'tcx hir::Ty<'tcx, hir::AmbigArg>) {
        if self.collect_derive_requirements
            && let Some(typeck_results) = self.typeck_results
            && let Some(ty) = typeck_results.node_type_opt(hir_ty.hir_id)
            && let ty::Adt(adt, args) = ty.kind()
        {
            self.record_predicate_requirements(adt.did(), args);
        }
        intravisit::walk_ty(self, hir_ty);
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
        if let Some(typeck_results) = self.typeck_results {
            if self.collect_derive_requirements
                && let Some(partial_eq) = self.tcx.lang_items().eq_trait()
                && let Some(pattern_ty) = typeck_results.node_type_opt(expression.hir_id)
            {
                self.record_trait_requirement(partial_eq, pattern_ty);
            }
            if let hir::PatExprKind::Path(ref qpath @ hir::QPath::TypeRelative(..)) =
                expression.kind
            {
                self.record(typeck_results.qpath_res(qpath, expression.hir_id));
            }
        }
        intravisit::walk_pat_expr(self, expression);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::path::Path;

    use super::{
        compact_visibility_modifier, fix_target_matches_definition, normalize_source_path,
        write_fragment,
    };
    use crate::graph::{Definition, DefinitionKind, FindingKind, FixTarget, Fragment, Span};

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
            normalize_source_path("library/tests/../src/shared.rs".into()),
            "library/src/shared.rs"
        );
        assert_eq!(normalize_source_path("../shared.rs".into()), "../shared.rs");
    }

    #[test]
    fn derive_fix_targets_survive_source_location_and_id_changes() {
        let target = FixTarget {
            id: "old-id".into(),
            crate_name: "library".into(),
            name: "module::Type as Debug".into(),
            definition_kind: DefinitionKind::DerivedTrait,
            span: Some(Span {
                file: "src/lib.rs".into(),
                line: 20,
                column: 10,
            }),
            kind: FindingKind::UnnecessaryDerive,
            replacement: None,
        };
        let definition = Definition {
            id: "new-id".into(),
            crate_name: "library".into(),
            name: "module::Type as Debug".into(),
            kind: DefinitionKind::DerivedTrait,
            span: Some(Span {
                file: "src/lib.rs".into(),
                line: 18,
                column: 10,
            }),
            public_api: false,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: Vec::new(),
            uniform_field_group: None,
            dead_code_allowed: false,
        };

        assert!(fix_target_matches_definition(&target, &definition));
    }
}
