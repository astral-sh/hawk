use std::collections::HashSet;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use rustc_driver::{Callbacks, Compilation};
use rustc_hir as hir;
use rustc_hir::Node;
use rustc_hir::def::{CtorOf, DefKind, Res};
use rustc_hir::def_id::{CRATE_DEF_ID, DefId, LocalDefId};
use rustc_hir::intravisit::{self, Visitor};
use rustc_interface::interface;
use rustc_lint_defs::Level;
use rustc_middle::ty::{self, TyCtxt};
use rustc_session::config::CrateType;
use rustc_session::lint::builtin::DEAD_CODE;
use rustc_span::Pos;
use rustc_span::Symbol;
use rustc_span::def_id::LOCAL_CRATE;

use crate::graph::{Definition, DefinitionKind, Edge, EdgeKind, Fragment, Span};

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
    let mut callbacks = HawkCallbacks {
        output_dir,
        root_crate,
    };

    rustc_driver::catch_with_exit_code(move || {
        rustc_driver::run_compiler(&args, &mut callbacks);
    })
}

struct HawkCallbacks {
    output_dir: PathBuf,
    root_crate: String,
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
        if let Err(error) = emit_fragment(tcx, &self.root_crate, &self.output_dir) {
            tcx.dcx()
                .fatal(format!("hawk could not emit analysis graph: {error:#}"));
        }
        Compilation::Continue
    }
}

fn emit_fragment(tcx: TyCtxt<'_>, root_crate: &str, output_dir: &Path) -> Result<()> {
    let crate_name = tcx.crate_name(LOCAL_CRATE).to_string();
    let is_product_root = crate_name == root_crate && tcx.entry_fn(()).is_some();
    let fragment = collect_fragment(tcx, crate_name.clone(), is_product_root);
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

fn collect_fragment(tcx: TyCtxt<'_>, crate_name: String, is_product_root: bool) -> Fragment {
    let mut definitions = Vec::new();
    let mut defined = HashSet::new();
    let crate_items = tcx.hir_crate_items(());
    let is_proc_macro_crate = tcx.crate_types().contains(&CrateType::ProcMacro);

    for owner in crate_items.owners() {
        let def_id = owner.def_id;
        let kind = diagnostic_kind(tcx, def_id);
        let public_api = kind.is_some_and(|kind| kind != DefinitionKind::Reexport)
            && is_publicly_exported(tcx, def_id);
        definitions.push(definition(
            tcx,
            def_id,
            &crate_name,
            kind.unwrap_or(DefinitionKind::Other),
            public_api,
        ));
        defined.insert(def_id);
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
        if diagnostic_kind(tcx, def_id) == Some(DefinitionKind::InherentMethod)
            && let ty::Adt(adt, _) = tcx
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
    let public_reexport_sources: HashSet<String> = crate_items
        .owners()
        .map(|owner| owner.def_id)
        .filter(|def_id| {
            tcx.def_kind(*def_id) == DefKind::Use && is_publicly_exported(tcx, *def_id)
        })
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

fn is_publicly_exported(tcx: TyCtxt<'_>, def_id: LocalDefId) -> bool {
    !tcx.def_span(def_id).from_expansion()
        && tcx.local_visibility(def_id).is_public()
        && tcx.effective_visibilities(()).is_exported(def_id)
}

fn definition(
    tcx: TyCtxt<'_>,
    def_id: LocalDefId,
    crate_name: &str,
    kind: DefinitionKind,
    public_api: bool,
) -> Definition {
    let hir_id = tcx.local_def_id_to_hir_id(def_id);
    let allow_dead_code = matches!(
        tcx.lint_level_at_node(DEAD_CODE, hir_id).level,
        Level::Allow | Level::Expect
    );
    Definition {
        id: id(tcx, def_id.to_def_id()),
        crate_name: crate_name.into(),
        name: tcx.def_path_str(def_id.to_def_id()),
        kind,
        span: span(tcx, def_id),
        public_api,
        allow_dead_code,
    }
}

fn diagnostic_kind(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<DefinitionKind> {
    match tcx.def_kind(def_id) {
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
        _ => None,
    }
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
                self.record_def(self.tcx.parent(constructor));
            }
            Res::Def(DefKind::Ctor(CtorOf::Variant, ..), constructor) => {
                self.record_def(self.tcx.parent(self.tcx.parent(constructor)));
            }
            Res::Def(DefKind::Variant, variant) => {
                self.record_def(self.tcx.parent(variant));
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
                hir::ExprKind::Struct(qpath @ hir::QPath::TypeRelative(..), ..) => {
                    self.record(typeck_results.qpath_res(qpath, expression.hir_id));
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

        assert!(error.to_string().contains("flush fragment.json"));
    }
}
