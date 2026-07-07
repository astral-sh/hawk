use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::protocol::ProtocolVersion;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CollectionOptions {
    preserve_uniform_field_visibility: bool,
}

impl CollectionOptions {
    const DEFAULT: &'static str = "default";
    const PRESERVE_UNIFORM_FIELD_VISIBILITY: &'static str = "preserve-uniform-field-visibility";

    pub const fn new(preserve_uniform_field_visibility: bool) -> Self {
        Self {
            preserve_uniform_field_visibility,
        }
    }

    pub const fn preserve_uniform_field_visibility(self) -> bool {
        self.preserve_uniform_field_visibility
    }

    pub const fn as_env_value(self) -> &'static str {
        if self.preserve_uniform_field_visibility {
            Self::PRESERVE_UNIFORM_FIELD_VISIBILITY
        } else {
            Self::DEFAULT
        }
    }

    pub fn from_env_value(value: Option<&str>) -> Option<Self> {
        match value {
            None | Some(Self::DEFAULT) => Some(Self::default()),
            Some(Self::PRESERVE_UNIFORM_FIELD_VISIBILITY) => Some(Self::new(true)),
            Some(_) => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Fragment {
    pub protocol_version: ProtocolVersion,
    pub package_name: String,
    pub crate_name: String,
    pub crate_id: String,
    pub is_product_root: bool,
    pub test_surface: bool,
    pub definitions: Vec<Definition>,
    pub edges: Vec<Edge>,
    pub roots: Vec<String>,
    pub conservative_roots: Vec<String>,
    pub required_public_roots: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixPlan {
    pub protocol_version: ProtocolVersion,
    pub targets: Vec<FixTarget>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FixTarget {
    pub id: String,
    pub crate_name: String,
    pub name: String,
    pub definition_kind: DefinitionKind,
    pub span: Option<Span>,
    pub kind: FindingKind,
    pub replacement: VisibilityReduction,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub crate_name: String,
    pub name: String,
    pub kind: DefinitionKind,
    pub span: Option<Span>,
    pub public_api: bool,
    pub restricted_visible_api: bool,
    pub crate_visible_api: bool,
    pub visible_reexport_api: bool,
    pub module_scope: Vec<String>,
    pub uniform_field_group: Option<Span>,
    pub dead_code_allowed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Body,
    Interface,
    Reexport,
    VisibilityParent,
    VisibilityRequirement,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionKind {
    Function,
    InherentMethod,
    InherentAssociatedConstant,
    Trait,
    Struct,
    Enum,
    Union,
    TypeAlias,
    Constant,
    Static,
    Field,
    EnumVariant,
    Reexport,
    Module,
    Other,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    DeadPublic,
    UnnecessaryPublic,
    UnnecessaryRestrictedVisibility,
    UnnecessaryCrateVisibility,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityReduction {
    Crate,
    Super,
    Private,
}

impl VisibilityReduction {
    pub fn replacement(self) -> &'static str {
        match self {
            Self::Crate => "pub(crate)",
            Self::Super => "pub(super)",
            Self::Private => "",
        }
    }
}

impl FindingKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::DeadPublic => "hawk::dead_public",
            Self::UnnecessaryPublic => "hawk::unnecessary_public",
            Self::UnnecessaryRestrictedVisibility => "hawk::unnecessary_restricted_visibility",
            Self::UnnecessaryCrateVisibility => "hawk::unnecessary_crate_visibility",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "hawk::dead_public" => Some(Self::DeadPublic),
            "hawk::unnecessary_public" => Some(Self::UnnecessaryPublic),
            "hawk::unnecessary_restricted_visibility" => {
                Some(Self::UnnecessaryRestrictedVisibility)
            }
            "hawk::unnecessary_crate_visibility" => Some(Self::UnnecessaryCrateVisibility),
            _ => None,
        }
    }

    pub const fn visibility_reduction(self) -> Option<VisibilityReduction> {
        match self {
            Self::DeadPublic => None,
            Self::UnnecessaryPublic => Some(VisibilityReduction::Crate),
            Self::UnnecessaryRestrictedVisibility => Some(VisibilityReduction::Private),
            Self::UnnecessaryCrateVisibility => Some(VisibilityReduction::Super),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Finding<'a> {
    pub kind: FindingKind,
    pub definition: &'a Definition,
    pub test_only: bool,
    pub test_compiled_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DefinitionIdentity<'a> {
    crate_name: &'a str,
    name: &'a str,
    kind: DefinitionKind,
    file: Option<&'a str>,
    line: Option<usize>,
    column: Option<usize>,
}

impl<'a> DefinitionIdentity<'a> {
    pub fn new(
        crate_name: &'a str,
        name: &'a str,
        kind: DefinitionKind,
        span: Option<&'a Span>,
    ) -> Self {
        Self {
            crate_name,
            name,
            kind,
            file: span.map(|span| span.file.as_str()),
            line: span.map(|span| span.line),
            column: span.map(|span| span.column),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SourceDefinitionIdentity<'a> {
    name: Option<&'a str>,
    kind: DefinitionKind,
    file: Option<&'a str>,
    line: Option<usize>,
    column: Option<usize>,
}

pub fn analyze<'a>(
    production_fragments: &'a [Fragment],
    test_fragments: &'a [Fragment],
    candidate_crates: &HashSet<String>,
    excluded_crates: &HashSet<String>,
) -> Vec<Finding<'a>> {
    analyze_with_options(
        production_fragments,
        test_fragments,
        candidate_crates,
        excluded_crates,
        false,
    )
}

pub fn analyze_with_options<'a>(
    production_fragments: &'a [Fragment],
    test_fragments: &'a [Fragment],
    candidate_crates: &HashSet<String>,
    excluded_crates: &HashSet<String>,
    preserve_uniform_field_visibility: bool,
) -> Vec<Finding<'a>> {
    let observed_definitions: Vec<&Definition> = production_fragments
        .iter()
        .chain(test_fragments)
        .flat_map(|fragment| &fragment.definitions)
        .collect();
    let definitions: HashMap<&str, &Definition> = observed_definitions
        .iter()
        .copied()
        .map(|definition| (definition.id.as_str(), definition))
        .collect();
    let definition_crate_ids: HashMap<&str, &str> = production_fragments
        .iter()
        .chain(test_fragments)
        .flat_map(|fragment| {
            fragment
                .definitions
                .iter()
                .map(|definition| (definition.id.as_str(), fragment.crate_id.as_str()))
        })
        .collect();
    let definition_compilation_ids: HashMap<&str, usize> = production_fragments
        .iter()
        .chain(test_fragments)
        .enumerate()
        .flat_map(|(compilation_id, fragment)| {
            fragment
                .definitions
                .iter()
                .map(move |definition| (definition.id.as_str(), compilation_id))
        })
        .collect();
    let production_edges: Vec<&Edge> = production_fragments
        .iter()
        .flat_map(|fragment| &fragment.edges)
        .collect();
    let test_edges: Vec<&Edge> = test_fragments
        .iter()
        .flat_map(|fragment| &fragment.edges)
        .collect();
    let edges: Vec<&Edge> = production_edges
        .iter()
        .chain(&test_edges)
        .copied()
        .collect();
    let equivalents = equivalent_definitions(&definitions, &definition_compilation_ids);
    let required_scopes = required_scopes(&definitions, &edges, &equivalents);
    let production_definition_ids: HashSet<&str> = production_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .map(|definition| definition.id.as_str())
        .collect();
    let test_definition_ids: HashSet<&str> = test_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .map(|definition| definition.id.as_str())
        .collect();
    let production_adjacency =
        adjacency(&production_edges, &equivalents, &production_definition_ids);
    let test_adjacency = adjacency(&test_edges, &equivalents, &test_definition_ids);

    let production_roots = production_fragments
        .iter()
        .filter(|fragment| fragment.is_product_root)
        .flat_map(|fragment| fragment.roots.iter().map(String::as_str))
        .chain(
            production_fragments
                .iter()
                .flat_map(|fragment| fragment.conservative_roots.iter().map(String::as_str)),
        );
    let production = reachable(production_roots, &production_adjacency);
    let test_roots = test_fragments
        .iter()
        .filter(|fragment| fragment.is_product_root)
        .flat_map(|fragment| fragment.roots.iter().map(String::as_str))
        .chain(
            test_fragments
                .iter()
                .flat_map(|fragment| fragment.conservative_roots.iter().map(String::as_str)),
        );
    let tests = reachable(test_roots, &test_adjacency);

    let mut explicitly_required: HashSet<&str> = production_fragments
        .iter()
        .chain(test_fragments)
        .flat_map(|fragment| fragment.required_public_roots.iter().map(String::as_str))
        .collect();
    let no_explicitly_required = HashSet::new();
    let externally_required_visibility = required_public_visibility(
        &definitions,
        &definition_crate_ids,
        &edges,
        &equivalents,
        &no_explicitly_required,
    );
    for definition in definitions
        .values()
        .filter(|definition| definition.public_api && definition.kind == DefinitionKind::Reexport)
    {
        let targets = reexport_targets(definition.id.as_str(), &edges);
        if !is_analyzable_reexport(&targets, &definitions)
            || targets
                .iter()
                .any(|target| externally_required_visibility.contains(target))
        {
            explicitly_required.insert(definition.id.as_str());
        }
    }
    let required_public_visibility = required_public_visibility(
        &definitions,
        &definition_crate_ids,
        &edges,
        &equivalents,
        &explicitly_required,
    );

    let mut findings = Vec::new();
    let mut reported = HashSet::new();
    let production_definitions: HashSet<_> = production_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .map(definition_identity)
        .collect();
    let production_candidates: HashSet<_> = production_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .filter(|definition| definition.public_api)
        .map(definition_identity)
        .collect();
    let production_restricted_visible_candidates: HashSet<_> = production_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .filter(|definition| definition.restricted_visible_api)
        .map(definition_identity)
        .collect();
    let production_root_definitions: HashSet<_> = production_fragments
        .iter()
        .filter(|fragment| fragment.is_product_root)
        .flat_map(|fragment| &fragment.definitions)
        .map(definition_identity)
        .collect();
    let non_production_root_definitions: HashSet<_> = test_fragments
        .iter()
        .filter(|fragment| fragment.is_product_root && !fragment.test_surface)
        .flat_map(|fragment| &fragment.definitions)
        .map(definition_identity)
        .collect();
    for definition in production_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .chain(
            test_fragments
                .iter()
                .flat_map(|fragment| &fragment.definitions),
        )
    {
        let identity = definition_identity(definition);
        if !definition.public_api
            || definition.dead_code_allowed
            || (production_definitions.contains(&identity)
                && !production_candidates.contains(&identity))
            || !candidate_crates.contains(&definition.crate_name)
            || excluded_crates.contains(&definition.crate_name)
            || production_root_definitions.contains(&identity)
        {
            continue;
        }

        if required_public_visibility.contains(definition.id.as_str()) {
            continue;
        }

        if !reported.insert(identity) {
            continue;
        }

        let test_compiled_only = !production_definitions.contains(&identity);
        let is_production_live = is_live(definition, &production_edges, &production, &equivalents);
        let is_test_live = is_live(definition, &test_edges, &tests, &equivalents);
        if !is_production_live && !is_test_live {
            findings.push(Finding {
                kind: FindingKind::DeadPublic,
                definition,
                test_only: false,
                test_compiled_only,
            });
            continue;
        }

        if definition.kind == DefinitionKind::EnumVariant {
            continue;
        }

        findings.push(Finding {
            kind: FindingKind::UnnecessaryPublic,
            definition,
            test_only: !is_production_live && is_test_live,
            test_compiled_only,
        });
    }

    for definition in production_fragments
        .iter()
        .flat_map(|fragment| &fragment.definitions)
        .chain(
            test_fragments
                .iter()
                .flat_map(|fragment| &fragment.definitions),
        )
    {
        let identity = definition_identity(definition);
        if !definition.restricted_visible_api
            || definition.dead_code_allowed
            || (production_definitions.contains(&identity)
                && !production_restricted_visible_candidates.contains(&identity))
            || !candidate_crates.contains(&definition.crate_name)
            || excluded_crates.contains(&definition.crate_name)
            || production_root_definitions.contains(&identity)
            || (!production_definitions.contains(&identity)
                && non_production_root_definitions.contains(&identity))
            || reported.contains(&identity)
        {
            continue;
        }

        let Some(kind) =
            restricted_visibility_finding_kind(definition, &required_scopes, &equivalents)
        else {
            continue;
        };
        reported.insert(identity);
        let test_compiled_only = !production_definitions.contains(&identity);
        let is_production_live = is_live(definition, &production_edges, &production, &equivalents);
        let is_test_live = is_live(definition, &test_edges, &tests, &equivalents);
        findings.push(Finding {
            kind,
            definition,
            test_only: !is_production_live && is_test_live,
            test_compiled_only,
        });
    }

    if preserve_uniform_field_visibility {
        suppress_uniform_field_visibility_findings(
            &mut findings,
            &observed_definitions,
            &required_public_visibility,
            &required_scopes,
            &equivalents,
        );
    }

    findings.sort_by_key(|finding| {
        let span = finding.definition.span.as_ref();
        (
            span.map(|span| span.file.as_str()).unwrap_or(""),
            span.map(|span| span.line).unwrap_or(0),
            span.map(|span| span.column).unwrap_or(0),
            finding.definition.crate_name.as_str(),
            finding.definition.name.as_str(),
            finding.definition.kind,
            finding.kind,
        )
    });
    findings
}

fn field_group_identity(definition: &Definition) -> Option<(&str, &Span)> {
    Some((
        definition.crate_name.as_str(),
        definition.uniform_field_group.as_ref()?,
    ))
}

fn suppress_uniform_field_visibility_findings<'a>(
    findings: &mut Vec<Finding<'a>>,
    definitions: &[&'a Definition],
    required_public_visibility: &HashSet<&str>,
    required_scopes: &HashMap<&str, RequiredScope>,
    equivalents: &HashMap<&str, Vec<&str>>,
) {
    let protected_groups: HashSet<_> = definitions
        .iter()
        .filter_map(|definition| {
            let identity = field_group_identity(definition)?;
            let required = if definition.public_api {
                required_public_visibility.contains(definition.id.as_str())
            } else if definition.restricted_visible_api {
                has_known_restricted_visibility_requirement(
                    definition,
                    required_scopes,
                    equivalents,
                )
            } else {
                false
            };
            required.then_some(identity)
        })
        .collect();

    findings.retain(|finding| {
        if finding.kind == FindingKind::DeadPublic {
            return true;
        }
        field_group_identity(finding.definition)
            .is_none_or(|identity| !protected_groups.contains(&identity))
    });
}

fn has_known_restricted_visibility_requirement(
    definition: &Definition,
    required_scopes: &HashMap<&str, RequiredScope>,
    equivalents: &HashMap<&str, Vec<&str>>,
) -> bool {
    let required_scope = merged_required_scope(definition, required_scopes, equivalents);
    matches!(
        &required_scope,
        RequiredScope::Known { crate_name, .. } if crate_name == &definition.crate_name
    ) && restricted_visibility_finding_kind(definition, required_scopes, equivalents).is_none()
}

fn restricted_visibility_finding_kind(
    definition: &Definition,
    required_scopes: &HashMap<&str, RequiredScope>,
    equivalents: &HashMap<&str, Vec<&str>>,
) -> Option<FindingKind> {
    if matches!(
        definition.kind,
        DefinitionKind::EnumVariant | DefinitionKind::Reexport
    ) {
        return None;
    }

    match merged_required_scope(definition, required_scopes, equivalents) {
        RequiredScope::Bottom => Some(FindingKind::UnnecessaryRestrictedVisibility),
        RequiredScope::Unknown => None,
        RequiredScope::Known {
            crate_name,
            module_scope,
        } => {
            if crate_name != definition.crate_name {
                return None;
            }
            if module_scope.starts_with(&definition.module_scope) {
                return Some(FindingKind::UnnecessaryRestrictedVisibility);
            }
            if definition.crate_visible_api {
                let parent_scope = definition.module_scope.split_last()?.1;
                return module_scope
                    .starts_with(parent_scope)
                    .then_some(FindingKind::UnnecessaryCrateVisibility);
            }
            None
        }
    }
}

fn merged_required_scope(
    definition: &Definition,
    required_scopes: &HashMap<&str, RequiredScope>,
    equivalents: &HashMap<&str, Vec<&str>>,
) -> RequiredScope {
    let mut required_scope = RequiredScope::default();
    for id in std::iter::once(definition.id.as_str()).chain(
        equivalents
            .get(definition.id.as_str())
            .into_iter()
            .flatten()
            .copied(),
    ) {
        if let Some(scope) = required_scopes.get(id) {
            required_scope.merge(scope);
        }
    }
    required_scope
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum RequiredScope {
    #[default]
    Bottom,
    Known {
        crate_name: String,
        module_scope: Vec<String>,
    },
    Unknown,
}

impl RequiredScope {
    fn for_definition(definition: &Definition) -> Self {
        Self::Known {
            crate_name: definition.crate_name.clone(),
            module_scope: definition.module_scope.clone(),
        }
    }

    fn unknown() -> Self {
        Self::Unknown
    }

    fn merge(&mut self, other: &Self) -> bool {
        match other {
            Self::Bottom => false,
            Self::Unknown => {
                if matches!(self, Self::Unknown) {
                    false
                } else {
                    *self = Self::Unknown;
                    true
                }
            }
            Self::Known {
                crate_name: other_crate,
                module_scope: other_scope,
            } => match self {
                Self::Bottom => {
                    *self = other.clone();
                    true
                }
                Self::Unknown => false,
                Self::Known {
                    crate_name,
                    module_scope,
                } if crate_name == other_crate => {
                    let shared = module_scope
                        .iter()
                        .zip(other_scope)
                        .take_while(|(left, right)| left == right)
                        .count();
                    if shared == module_scope.len() {
                        false
                    } else {
                        module_scope.truncate(shared);
                        true
                    }
                }
                Self::Known { .. } => {
                    *self = Self::Unknown;
                    true
                }
            },
        }
    }
}

fn required_scopes<'a>(
    definitions: &HashMap<&'a str, &'a Definition>,
    edges: &[&'a Edge],
    equivalents: &HashMap<&'a str, Vec<&'a str>>,
) -> HashMap<&'a str, RequiredScope> {
    let mut required_scopes: HashMap<&str, RequiredScope> = HashMap::new();
    let mut propagation: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut pending = VecDeque::new();
    for edge in edges {
        if edge.from == edge.to || !definitions.contains_key(edge.to.as_str()) {
            continue;
        }
        let source = definitions.get(edge.from.as_str());
        let requirement = if edge.kind == EdgeKind::Reexport
            || (edge.kind == EdgeKind::VisibilityParent
                && source.is_some_and(|source| source.visible_reexport_api))
        {
            RequiredScope::unknown()
        } else {
            source.map_or_else(RequiredScope::unknown, |source| {
                RequiredScope::for_definition(source)
            })
        };
        if required_scopes
            .entry(edge.to.as_str())
            .or_default()
            .merge(&requirement)
        {
            pending.push_back(edge.to.as_str());
        }
        if propagates_visibility_requirement(edge.kind) {
            propagation
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    for (source, targets) in equivalents {
        propagation
            .entry(source)
            .or_default()
            .extend(targets.iter().copied());
    }
    while let Some(source) = pending.pop_front() {
        let Some(required_scope) = required_scopes.get(source).cloned() else {
            continue;
        };
        for target in propagation.get(source).into_iter().flatten() {
            if required_scopes
                .entry(target)
                .or_default()
                .merge(&required_scope)
            {
                pending.push_back(target);
            }
        }
    }
    required_scopes
}

fn propagates_visibility_requirement(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Interface
            | EdgeKind::Reexport
            | EdgeKind::VisibilityParent
            | EdgeKind::VisibilityRequirement
    )
}

fn is_live(
    definition: &Definition,
    edges: &[&Edge],
    reachable: &HashSet<&str>,
    equivalents: &HashMap<&str, Vec<&str>>,
) -> bool {
    let equivalent_ids = equivalents
        .get(definition.id.as_str())
        .into_iter()
        .flatten()
        .copied();
    let ids = std::iter::once(definition.id.as_str()).chain(equivalent_ids);
    if definition.kind == DefinitionKind::Reexport {
        ids.flat_map(|id| reexport_targets(id, edges))
            .any(|target| reachable.contains(target))
    } else {
        ids.into_iter().any(|id| reachable.contains(id))
    }
}

fn required_public_visibility<'a>(
    definitions: &HashMap<&'a str, &'a Definition>,
    definition_crate_ids: &HashMap<&'a str, &'a str>,
    edges: &[&'a Edge],
    equivalents: &HashMap<&'a str, Vec<&'a str>>,
    explicitly_required: &HashSet<&'a str>,
) -> HashSet<&'a str> {
    let mut required = explicitly_required.clone();
    // Rust privacy-checks every compiled item, including items outside the
    // selected product's runtime reachability graph.
    required.extend(edges.iter().filter_map(|edge| {
        let from = definition_crate_ids.get(edge.from.as_str())?;
        let to = definition_crate_ids.get(edge.to.as_str())?;
        (from != to).then_some(edge.to.as_str())
    }));

    let mut interface_edges: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if propagates_visibility_requirement(edge.kind)
            && definitions.contains_key(edge.to.as_str())
        {
            interface_edges
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    for (source, targets) in equivalents {
        interface_edges
            .entry(source)
            .or_default()
            .extend(targets.iter().copied());
    }

    let mut pending: Vec<&str> = required.iter().copied().collect();
    while let Some(from) = pending.pop() {
        if let Some(targets) = interface_edges.get(from) {
            for target in targets {
                if required.insert(target) {
                    pending.push(target);
                }
            }
        }
    }

    required
}

fn reexport_targets<'a>(source: &str, edges: &'a [&Edge]) -> Vec<&'a str> {
    edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Reexport && edge.from == source)
        .map(|edge| edge.to.as_str())
        .collect()
}

fn is_analyzable_reexport(targets: &[&str], definitions: &HashMap<&str, &Definition>) -> bool {
    !targets.is_empty()
        && targets.iter().all(|target| {
            definitions.get(target).is_some_and(|definition| {
                matches!(
                    definition.kind,
                    DefinitionKind::Function
                        | DefinitionKind::InherentMethod
                        | DefinitionKind::Trait
                        | DefinitionKind::Struct
                        | DefinitionKind::Enum
                        | DefinitionKind::Union
                        | DefinitionKind::TypeAlias
                        | DefinitionKind::Constant
                        | DefinitionKind::Static
                )
            })
        })
}

fn adjacency<'a>(
    edges: &'a [&Edge],
    equivalents: &HashMap<&'a str, Vec<&'a str>>,
    definition_ids: &HashSet<&str>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if edge.kind == EdgeKind::VisibilityRequirement {
            continue;
        }
        adjacency
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
    }
    for (source, targets) in equivalents {
        if definition_ids.contains(source) {
            adjacency.entry(source).or_default().extend(
                targets
                    .iter()
                    .copied()
                    .filter(|target| definition_ids.contains(target)),
            );
        }
    }
    adjacency
}

fn equivalent_definitions<'a>(
    definitions: &HashMap<&'a str, &'a Definition>,
    definition_compilation_ids: &HashMap<&'a str, usize>,
) -> HashMap<&'a str, Vec<&'a str>> {
    let mut groups: HashMap<SourceDefinitionIdentity<'a>, Vec<(&'a str, usize)>> = HashMap::new();
    for definition in definitions.values() {
        groups
            .entry(source_definition_identity(definition))
            .or_default()
            .push((
                definition.id.as_str(),
                definition_compilation_ids[definition.id.as_str()],
            ));
    }

    let mut equivalents: HashMap<&str, Vec<&str>> = HashMap::new();
    for group in groups.values().filter(|group| {
        group.len() > 1
            && group
                .iter()
                .map(|(_, compilation_id)| compilation_id)
                .collect::<HashSet<_>>()
                .len()
                == group.len()
    }) {
        for source in group {
            equivalents.entry(source.0).or_default().extend(
                group
                    .iter()
                    .map(|target| target.0)
                    .filter(|target| target != &source.0),
            );
        }
    }
    equivalents
}

fn definition_identity<'a>(definition: &'a Definition) -> DefinitionIdentity<'a> {
    DefinitionIdentity::new(
        &definition.crate_name,
        &definition.name,
        definition.kind,
        definition.span.as_ref(),
    )
}

fn source_definition_identity<'a>(definition: &'a Definition) -> SourceDefinitionIdentity<'a> {
    SourceDefinitionIdentity {
        name: definition
            .span
            .is_none()
            .then_some(definition.name.as_str()),
        kind: definition.kind,
        file: definition.span.as_ref().map(|span| span.file.as_str()),
        line: definition.span.as_ref().map(|span| span.line),
        column: definition.span.as_ref().map(|span| span.column),
    }
}

fn reachable<'a>(
    roots: impl IntoIterator<Item = &'a str>,
    adjacency: &HashMap<&'a str, Vec<&'a str>>,
) -> HashSet<&'a str> {
    let mut live = HashSet::new();
    let mut pending: Vec<&str> = roots.into_iter().collect();
    while let Some(id) = pending.pop() {
        if live.insert(id)
            && let Some(next) = adjacency.get(id)
        {
            pending.extend(next.iter().copied());
        }
    }
    live
}

#[cfg(test)]
mod tests {
    use super::{
        Definition, DefinitionKind, Edge, EdgeKind, Finding, FindingKind, Fragment, RequiredScope,
        Span, VisibilityReduction, analyze as analyze_with_tests, analyze_with_options,
    };
    use crate::protocol::ProtocolVersion;
    use std::collections::HashSet;

    fn analyze<'a>(
        fragments: &'a [Fragment],
        excluded_crates: &HashSet<String>,
    ) -> Vec<Finding<'a>> {
        analyze_with_tests(fragments, &[], &candidate_crates(), excluded_crates)
    }

    fn analyze_preserving_uniform_fields<'a>(fragments: &'a [Fragment]) -> Vec<Finding<'a>> {
        analyze_with_options(fragments, &[], &candidate_crates(), &HashSet::new(), true)
    }

    fn candidate_crates() -> HashSet<String> {
        ["lib", "test_support"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn finding_summaries(findings: Vec<Finding<'_>>) -> Vec<(FindingKind, String, bool, bool)> {
        findings
            .into_iter()
            .map(|finding| {
                (
                    finding.kind,
                    finding.definition.name.clone(),
                    finding.test_only,
                    finding.test_compiled_only,
                )
            })
            .collect()
    }

    #[test]
    fn finding_kind_determines_visibility_reduction() {
        assert_eq!(FindingKind::DeadPublic.visibility_reduction(), None);
        assert_eq!(
            FindingKind::UnnecessaryPublic.visibility_reduction(),
            Some(VisibilityReduction::Crate)
        );
        assert_eq!(
            FindingKind::UnnecessaryRestrictedVisibility.visibility_reduction(),
            Some(VisibilityReduction::Private)
        );
        assert_eq!(
            FindingKind::UnnecessaryCrateVisibility.visibility_reduction(),
            Some(VisibilityReduction::Super)
        );
    }

    #[test]
    fn required_scope_merge_follows_its_lattice() {
        let mut scope = RequiredScope::Bottom;
        assert!(scope.merge(&RequiredScope::Known {
            crate_name: "lib".into(),
            module_scope: vec!["api".into(), "nested".into()],
        }));
        assert!(scope.merge(&RequiredScope::Known {
            crate_name: "lib".into(),
            module_scope: vec!["api".into(), "sibling".into()],
        }));
        assert_eq!(
            scope,
            RequiredScope::Known {
                crate_name: "lib".into(),
                module_scope: vec!["api".into()],
            }
        );
        assert!(scope.merge(&RequiredScope::Known {
            crate_name: "other".into(),
            module_scope: vec![],
        }));
        assert_eq!(scope, RequiredScope::Unknown);
        assert!(!scope.merge(&RequiredScope::Bottom));
        assert!(!scope.merge(&RequiredScope::Known {
            crate_name: "lib".into(),
            module_scope: vec![],
        }));
    }

    fn node(id: &str, crate_name: &str, public_api: bool) -> Definition {
        Definition {
            id: id.into(),
            crate_name: crate_name.into(),
            name: id.into(),
            kind: DefinitionKind::Function,
            span: None,
            public_api,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec![],
            uniform_field_group: None,
            dead_code_allowed: false,
        }
    }

    fn typed_node(
        id: &str,
        crate_name: &str,
        public_api: bool,
        kind: DefinitionKind,
    ) -> Definition {
        let mut definition = node(id, crate_name, public_api);
        definition.kind = kind;
        definition
    }

    fn crate_visible_node(id: &str, module_scope: &[&str]) -> Definition {
        let mut definition = restricted_visible_node(id, module_scope);
        definition.crate_visible_api = true;
        definition
    }

    fn restricted_visible_node(id: &str, module_scope: &[&str]) -> Definition {
        let mut definition = node(id, "lib", false);
        definition.restricted_visible_api = true;
        definition.module_scope = module_scope.iter().map(|module| (*module).into()).collect();
        definition
    }

    fn scoped_node(id: &str, module_scope: &[&str]) -> Definition {
        let mut definition = node(id, "lib", false);
        definition.module_scope = module_scope.iter().map(|module| (*module).into()).collect();
        definition
    }

    fn field(mut definition: Definition) -> Definition {
        definition.kind = DefinitionKind::Field;
        definition
    }

    fn uniform_field(definition: Definition) -> Definition {
        uniform_field_at(definition, 1)
    }

    fn uniform_field_at(mut definition: Definition, line: usize) -> Definition {
        definition = field(definition);
        definition.uniform_field_group = Some(Span {
            file: "lib.rs".into(),
            line,
            column: 1,
        });
        definition
    }

    fn fragments(definitions: Vec<Definition>, edges: Vec<Edge>) -> Vec<Fragment> {
        vec![
            Fragment {
                protocol_version: ProtocolVersion,
                package_name: "app".into(),
                crate_name: "app".into(),
                crate_id: "app".into(),
                is_product_root: true,
                test_surface: false,
                definitions: vec![node("main", "app", false)],
                edges: vec![],
                roots: vec!["main".into()],
                conservative_roots: vec![],
                required_public_roots: vec![],
            },
            Fragment {
                protocol_version: ProtocolVersion,
                package_name: "lib".into(),
                crate_name: "lib".into(),
                crate_id: "lib".into(),
                is_product_root: false,
                test_surface: false,
                definitions,
                edges,
                roots: vec![],
                conservative_roots: vec![],
                required_public_roots: vec![],
            },
        ]
    }

    fn test_fragments(definitions: Vec<Definition>, edges: Vec<Edge>) -> Vec<Fragment> {
        vec![
            Fragment {
                protocol_version: ProtocolVersion,
                package_name: "integration_test".into(),
                crate_name: "integration_test".into(),
                crate_id: "integration_test".into(),
                is_product_root: true,
                test_surface: true,
                definitions: vec![node("test_main", "integration_test", false)],
                edges: vec![Edge {
                    from: "test_main".into(),
                    to: "test_entry".into(),
                    kind: EdgeKind::Body,
                }],
                roots: vec!["test_main".into()],
                conservative_roots: vec![],
                required_public_roots: vec![],
            },
            Fragment {
                protocol_version: ProtocolVersion,
                package_name: "lib".into(),
                crate_name: "lib".into(),
                crate_id: "lib".into(),
                is_product_root: false,
                test_surface: false,
                definitions,
                edges,
                roots: vec![],
                conservative_roots: vec![],
                required_public_roots: vec![],
            },
        ]
    }

    #[test]
    fn serialized_fragments_require_complete_protocol_fields() {
        let mut fragment =
            serde_json::to_value(&fragments(vec![], vec![])[0]).expect("serialize fragment");
        fragment
            .as_object_mut()
            .expect("fragment is a JSON object")
            .remove("test_surface");

        let error = serde_json::from_value::<Fragment>(fragment)
            .expect_err("missing protocol field should fail");

        assert_eq!(error.to_string(), "missing field `test_surface`");
    }

    #[test]
    fn fragment_order_does_not_change_findings() {
        let mut production = fragments(
            vec![
                node("production_entry", "lib", true),
                node("production_helper", "lib", true),
                node("unused", "lib", true),
            ],
            vec![Edge {
                from: "production_entry".into(),
                to: "production_helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        production[0].edges.push(Edge {
            from: "main".into(),
            to: "production_entry".into(),
            kind: EdgeKind::Body,
        });
        let mut test_entry = node("test_entry", "lib", true);
        test_entry.name = "production_entry".into();
        let mut test_helper = node("test_helper", "lib", true);
        test_helper.name = "production_helper".into();
        let mut tests = test_fragments(
            vec![test_entry, test_helper],
            vec![Edge {
                from: "test_entry".into(),
                to: "test_helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        let expected = finding_summaries(analyze_with_tests(
            &production,
            &tests,
            &candidate_crates(),
            &HashSet::new(),
        ));

        production.reverse();
        tests.reverse();
        let actual = finding_summaries(analyze_with_tests(
            &production,
            &tests,
            &candidate_crates(),
            &HashSet::new(),
        ));

        assert_eq!(actual, expected);
    }

    #[test]
    fn dead_public_chain_is_not_kept_alive_by_internal_references() {
        let input = fragments(
            vec![node("unused", "lib", true), node("helper", "lib", true)],
            vec![Edge {
                from: "unused".into(),
                to: "helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .all(|finding| finding.kind == FindingKind::DeadPublic)
        );
    }

    #[test]
    fn product_root_named_like_a_library_only_excludes_its_own_declarations() {
        let mut input = fragments(
            vec![node("entry", "lib", true), node("unused", "lib", true)],
            vec![],
        );
        input[0].crate_name = "lib".into();
        input[0].definitions[0].crate_name = "lib".into();
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DeadPublic);
        assert_eq!(findings[0].definition.id, "unused");
    }

    #[test]
    fn live_internal_public_helper_can_be_narrowed() {
        let mut input = fragments(
            vec![node("entry", "lib", true), node("helper", "lib", true)],
            vec![Edge {
                from: "entry".into(),
                to: "helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "helper");
        assert!(!findings[0].test_only);
        assert!(!findings[0].test_compiled_only);
    }

    #[test]
    fn uniform_public_field_visibility_is_preserved_when_enabled() {
        let mut input = fragments(
            vec![
                uniform_field(node("required", "lib", true)),
                uniform_field(node("internal", "lib", true)),
                node("entry", "lib", false),
            ],
            vec![Edge {
                from: "entry".into(),
                to: "internal".into(),
                kind: EdgeKind::Body,
            }],
        );
        input[0].edges.extend([
            Edge {
                from: "main".into(),
                to: "required".into(),
                kind: EdgeKind::Body,
            },
            Edge {
                from: "main".into(),
                to: "entry".into(),
                kind: EdgeKind::Body,
            },
        ]);

        let findings = analyze(&input, &HashSet::new());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].definition.id, "internal");

        assert!(analyze_preserving_uniform_fields(&input).is_empty());
    }

    #[test]
    fn uniform_field_visibility_does_not_suppress_dead_public() {
        let mut input = fragments(
            vec![
                uniform_field(node("required", "lib", true)),
                uniform_field(node("dead", "lib", true)),
            ],
            vec![],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "required".into(),
            kind: EdgeKind::Body,
        });

        let findings = analyze_preserving_uniform_fields(&input);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DeadPublic);
        assert_eq!(findings[0].definition.id, "dead");
    }

    #[test]
    fn fields_without_a_uniform_group_do_not_preserve_visibility() {
        let mut input = fragments(
            vec![
                field(node("required", "lib", true)),
                field(node("internal", "lib", true)),
                node("entry", "lib", false),
            ],
            vec![Edge {
                from: "entry".into(),
                to: "internal".into(),
                kind: EdgeKind::Body,
            }],
        );
        input[0].edges.extend([
            Edge {
                from: "main".into(),
                to: "required".into(),
                kind: EdgeKind::Body,
            },
            Edge {
                from: "main".into(),
                to: "entry".into(),
                kind: EdgeKind::Body,
            },
        ]);

        let findings = analyze_preserving_uniform_fields(&input);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].definition.id, "internal");
    }

    #[test]
    fn test_requirement_preserves_uniform_production_field_visibility() {
        let mut production_input = fragments(
            vec![
                uniform_field(node("production_required", "lib", true)),
                uniform_field(node("internal", "lib", true)),
                node("entry", "lib", false),
            ],
            vec![Edge {
                from: "entry".into(),
                to: "internal".into(),
                kind: EdgeKind::Body,
            }],
        );
        production_input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });
        let mut test_required = uniform_field(node("test_required", "lib", true));
        test_required.name = "production_required".into();
        let mut test_input = test_fragments(vec![test_required], vec![]);
        test_input[0].edges[0].to = "test_required".into();

        let findings = analyze_with_options(
            &production_input,
            &test_input,
            &candidate_crates(),
            &HashSet::new(),
            true,
        );

        assert!(findings.is_empty());
    }

    #[test]
    fn cfg_alternative_declarations_do_not_share_field_visibility_requirements() {
        let mut production_input = fragments(
            vec![uniform_field_at(
                node("production_required", "lib", true),
                1,
            )],
            vec![],
        );
        production_input[0].edges.push(Edge {
            from: "main".into(),
            to: "production_required".into(),
            kind: EdgeKind::Body,
        });
        let test_input = test_fragments(
            vec![
                node("test_entry", "lib", true),
                uniform_field_at(node("test_internal", "lib", true), 10),
            ],
            vec![Edge {
                from: "test_entry".into(),
                to: "test_internal".into(),
                kind: EdgeKind::Body,
            }],
        );

        let findings = analyze_with_options(
            &production_input,
            &test_input,
            &candidate_crates(),
            &HashSet::new(),
            true,
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].definition.id, "test_internal");
    }

    #[test]
    fn crate_visible_helper_used_within_its_module_can_be_private() {
        let input = fragments(
            vec![
                scoped_node("scoped::entry", &["scoped"]),
                crate_visible_node("scoped::helper", &["scoped"]),
            ],
            vec![Edge {
                from: "scoped::entry".into(),
                to: "scoped::helper".into(),
                kind: EdgeKind::Body,
            }],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            FindingKind::UnnecessaryRestrictedVisibility
        );
        assert_eq!(findings[0].definition.id, "scoped::helper");
    }

    #[test]
    fn unused_restricted_item_can_be_private() {
        let input = fragments(
            vec![restricted_visible_node("scoped::unused", &["scoped"])],
            vec![],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            FindingKind::UnnecessaryRestrictedVisibility
        );
        assert_eq!(findings[0].definition.id, "scoped::unused");
    }

    #[test]
    fn uniform_crate_visible_field_visibility_is_preserved_when_required() {
        let required = uniform_field(crate_visible_node(
            "scoped::nested::required",
            &["scoped", "nested"],
        ));
        let internal = uniform_field(crate_visible_node(
            "scoped::nested::internal",
            &["scoped", "nested"],
        ));
        let input = fragments(
            vec![
                required,
                internal,
                scoped_node("outside::entry", &["outside"]),
                scoped_node("scoped::nested::entry", &["scoped", "nested"]),
            ],
            vec![
                Edge {
                    from: "outside::entry".into(),
                    to: "scoped::nested::required".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "scoped::nested::entry".into(),
                    to: "scoped::nested::internal".into(),
                    kind: EdgeKind::Body,
                },
            ],
        );

        let findings = analyze(&input, &HashSet::new());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].definition.id, "scoped::nested::internal");

        assert!(analyze_preserving_uniform_fields(&input).is_empty());
    }

    #[test]
    fn uniform_restricted_field_visibility_is_preserved_when_required() {
        let required = uniform_field(restricted_visible_node(
            "scoped::nested::required",
            &["scoped", "nested"],
        ));
        let internal = uniform_field(restricted_visible_node(
            "scoped::nested::internal",
            &["scoped", "nested"],
        ));
        let input = fragments(
            vec![
                required,
                internal,
                scoped_node("scoped::sibling::entry", &["scoped", "sibling"]),
                scoped_node("scoped::nested::entry", &["scoped", "nested"]),
            ],
            vec![
                Edge {
                    from: "scoped::sibling::entry".into(),
                    to: "scoped::nested::required".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "scoped::nested::entry".into(),
                    to: "scoped::nested::internal".into(),
                    kind: EdgeKind::Body,
                },
            ],
        );

        assert!(analyze_preserving_uniform_fields(&input).is_empty());
    }

    #[test]
    fn reducible_sibling_does_not_preserve_uniform_field_visibility() {
        let parent_visible = uniform_field(crate_visible_node(
            "scoped::nested::parent_visible",
            &["scoped", "nested"],
        ));
        let private = uniform_field(crate_visible_node(
            "scoped::nested::private",
            &["scoped", "nested"],
        ));
        let input = fragments(
            vec![
                parent_visible,
                private,
                scoped_node("scoped::sibling::entry", &["scoped", "sibling"]),
                scoped_node("scoped::nested::entry", &["scoped", "nested"]),
            ],
            vec![
                Edge {
                    from: "scoped::sibling::entry".into(),
                    to: "scoped::nested::parent_visible".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "scoped::nested::entry".into(),
                    to: "scoped::nested::private".into(),
                    kind: EdgeKind::Body,
                },
            ],
        );

        let findings = analyze_preserving_uniform_fields(&input);

        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == FindingKind::UnnecessaryCrateVisibility)
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.kind == FindingKind::UnnecessaryRestrictedVisibility)
        );
    }

    #[test]
    fn parent_visible_helper_used_within_its_module_can_be_private() {
        let input = fragments(
            vec![
                scoped_node("scoped::entry", &["scoped"]),
                restricted_visible_node("scoped::helper", &["scoped"]),
            ],
            vec![Edge {
                from: "scoped::entry".into(),
                to: "scoped::helper".into(),
                kind: EdgeKind::Body,
            }],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].kind,
            FindingKind::UnnecessaryRestrictedVisibility
        );
        assert_eq!(findings[0].definition.id, "scoped::helper");
    }

    #[test]
    fn crate_visible_helper_used_by_a_sibling_can_be_visible_to_its_parent() {
        let input = fragments(
            vec![
                scoped_node("scoped::sibling::entry", &["scoped", "sibling"]),
                crate_visible_node("scoped::nested::helper", &["scoped", "nested"]),
            ],
            vec![Edge {
                from: "scoped::sibling::entry".into(),
                to: "scoped::nested::helper".into(),
                kind: EdgeKind::Body,
            }],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryCrateVisibility);
        assert_eq!(findings[0].definition.id, "scoped::nested::helper");
    }

    #[test]
    fn crate_visible_helper_used_outside_its_parent_is_not_reported() {
        let input = fragments(
            vec![
                scoped_node("outside::entry", &["outside"]),
                crate_visible_node("scoped::nested::helper", &["scoped", "nested"]),
            ],
            vec![Edge {
                from: "outside::entry".into(),
                to: "scoped::nested::helper".into(),
                kind: EdgeKind::Body,
            }],
        );

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn crate_visible_module_accounts_for_uses_of_its_descendants() {
        let mut module = crate_visible_node("scoped::nested", &["scoped"]);
        module.kind = DefinitionKind::Module;
        let mut descendant = node("scoped::nested::helper", "lib", false);
        descendant.module_scope = vec!["scoped".into(), "nested".into()];
        let input = fragments(
            vec![node("entry", "lib", false), module, descendant],
            vec![
                Edge {
                    from: "entry".into(),
                    to: "scoped::nested::helper".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "scoped::nested::helper".into(),
                    to: "scoped::nested".into(),
                    kind: EdgeKind::VisibilityParent,
                },
            ],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryCrateVisibility);
        assert_eq!(findings[0].definition.id, "scoped::nested");
    }

    #[test]
    fn crate_visible_enum_accounts_for_uses_of_its_variants() {
        let mut enumeration =
            crate_visible_node("scoped::nested::ErrorKind", &["scoped", "nested"]);
        enumeration.kind = DefinitionKind::Enum;
        let mut variant = typed_node(
            "scoped::nested::ErrorKind::UnexpectedEnd",
            "lib",
            false,
            DefinitionKind::EnumVariant,
        );
        variant.module_scope = vec!["scoped".into(), "nested".into()];
        let input = fragments(
            vec![
                scoped_node("scoped::sibling::entry", &["scoped", "sibling"]),
                enumeration,
                variant,
            ],
            vec![
                Edge {
                    from: "scoped::sibling::entry".into(),
                    to: "scoped::nested::ErrorKind::UnexpectedEnd".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "scoped::nested::ErrorKind::UnexpectedEnd".into(),
                    to: "scoped::nested::ErrorKind".into(),
                    kind: EdgeKind::Interface,
                },
            ],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryCrateVisibility);
        assert_eq!(findings[0].definition.id, "scoped::nested::ErrorKind");
    }

    #[test]
    fn crate_visible_type_accounts_for_consumers_of_its_interface() {
        let function = crate_visible_node("scoped::nested::read", &["scoped", "nested"]);
        let mut error = crate_visible_node("scoped::nested::Error", &["scoped", "nested"]);
        error.kind = DefinitionKind::Enum;
        let input = fragments(
            vec![
                scoped_node("scoped::sibling::entry", &["scoped", "sibling"]),
                function,
                error,
            ],
            vec![
                Edge {
                    from: "scoped::sibling::entry".into(),
                    to: "scoped::nested::read".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "scoped::nested::read".into(),
                    to: "scoped::nested::Error".into(),
                    kind: EdgeKind::Interface,
                },
            ],
        );

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .all(|finding| finding.kind == FindingKind::UnnecessaryCrateVisibility)
        );
    }

    #[test]
    fn crate_visible_reexport_target_is_not_narrowed() {
        let mut reexport = crate_visible_node("scoped::TargetExport", &["scoped"]);
        reexport.kind = DefinitionKind::Reexport;
        let input = fragments(
            vec![reexport, crate_visible_node("scoped::Target", &["scoped"])],
            vec![Edge {
                from: "scoped::TargetExport".into(),
                to: "scoped::Target".into(),
                kind: EdgeKind::Reexport,
            }],
        );

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn namespace_containing_visible_reexport_is_not_narrowed() {
        let mut namespace = restricted_visible_node("wrapper::api", &["wrapper"]);
        namespace.kind = DefinitionKind::Module;
        let mut reexport = node("wrapper::api::f", "lib", false);
        reexport.kind = DefinitionKind::Reexport;
        reexport.visible_reexport_api = true;
        reexport.module_scope = vec!["wrapper".into(), "api".into()];
        let input = fragments(
            vec![
                namespace,
                reexport,
                node("sibling::call", "lib", false),
                node("target::f", "lib", false),
            ],
            vec![
                Edge {
                    from: "wrapper::api::f".into(),
                    to: "target::f".into(),
                    kind: EdgeKind::Reexport,
                },
                Edge {
                    from: "wrapper::api::f".into(),
                    to: "wrapper::api".into(),
                    kind: EdgeKind::VisibilityParent,
                },
                Edge {
                    from: "sibling::call".into(),
                    to: "target::f".into(),
                    kind: EdgeKind::Body,
                },
            ],
        );

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn integration_test_api_is_public_while_its_helper_can_be_narrowed() {
        let input = fragments(
            vec![node("entry", "lib", true), node("helper", "lib", true)],
            vec![Edge {
                from: "entry".into(),
                to: "helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        let mut test_entry = node("test_entry", "lib", true);
        test_entry.name = "entry".into();
        let mut test_helper = node("test_helper", "lib", true);
        test_helper.name = "helper".into();
        let test_input = test_fragments(
            vec![test_entry, test_helper],
            vec![Edge {
                from: "test_entry".into(),
                to: "test_helper".into(),
                kind: EdgeKind::Body,
            }],
        );

        let findings =
            analyze_with_tests(&input, &test_input, &candidate_crates(), &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "helper");
        assert!(findings[0].test_only);
        assert!(!findings[0].test_compiled_only);
    }

    #[test]
    fn production_reachability_does_not_follow_test_only_edges() {
        let mut production_input = fragments(
            vec![
                node("production_entry", "lib", true),
                node("production_helper", "lib", true),
            ],
            vec![],
        );
        production_input[0].edges.push(Edge {
            from: "main".into(),
            to: "production_entry".into(),
            kind: EdgeKind::Body,
        });

        let mut test_entry = node("test_entry", "lib", true);
        test_entry.name = "production_entry".into();
        let mut test_helper = node("test_helper", "lib", true);
        test_helper.name = "production_helper".into();
        let test_input = test_fragments(
            vec![test_entry, test_helper],
            vec![Edge {
                from: "test_entry".into(),
                to: "test_helper".into(),
                kind: EdgeKind::Body,
            }],
        );

        let findings = analyze_with_tests(
            &production_input,
            &test_input,
            &candidate_crates(),
            &HashSet::new(),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "production_helper");
        assert!(findings[0].test_only);
        assert!(!findings[0].test_compiled_only);
    }

    #[test]
    fn public_surface_compiled_only_for_tests_is_analyzed() {
        let production_input = fragments(vec![], vec![]);
        let mut test_input = test_fragments(
            vec![
                node("test_entry", "test_support", true),
                node("test_helper", "test_support", true),
            ],
            vec![Edge {
                from: "test_entry".into(),
                to: "test_helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        test_input[1].crate_name = "test_support".into();

        let findings = analyze_with_tests(
            &production_input,
            &test_input,
            &candidate_crates(),
            &HashSet::new(),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "test_helper");
        assert!(findings[0].test_only);
        assert!(findings[0].test_compiled_only);
    }

    #[test]
    fn dead_public_surface_compiled_only_for_tests_is_reported() {
        let production_input = fragments(vec![], vec![]);
        let mut test_input = test_fragments(vec![node("unused", "test_support", true)], vec![]);
        test_input[1].crate_name = "test_support".into();

        let findings = analyze_with_tests(
            &production_input,
            &test_input,
            &candidate_crates(),
            &HashSet::new(),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DeadPublic);
        assert_eq!(findings[0].definition.id, "unused");
        assert!(!findings[0].test_only);
        assert!(findings[0].test_compiled_only);
    }

    #[test]
    fn public_declarations_in_test_binary_targets_are_not_candidates() {
        let production_input = fragments(vec![], vec![]);
        let mut test_input = test_fragments(vec![], vec![]);
        test_input[0]
            .definitions
            .push(node("public_test_helper", "integration_test", true));

        assert!(
            analyze_with_tests(
                &production_input,
                &test_input,
                &candidate_crates(),
                &HashSet::new(),
            )
            .is_empty()
        );
    }

    #[test]
    fn crate_visible_declarations_in_test_binary_targets_named_like_library_are_not_candidates() {
        let production_input = fragments(vec![], vec![]);
        let mut test_input = test_fragments(vec![], vec![]);
        test_input[0].crate_name = "lib".into();
        test_input[0].test_surface = false;
        test_input[0].definitions[0].crate_name = "lib".into();
        test_input[0]
            .definitions
            .push(crate_visible_node("resolver::resolve", &["resolver"]));
        test_input[0].edges.push(Edge {
            from: "test_main".into(),
            to: "resolver::resolve".into(),
            kind: EdgeKind::Body,
        });

        assert!(
            analyze_with_tests(
                &production_input,
                &test_input,
                &candidate_crates(),
                &HashSet::new(),
            )
            .is_empty()
        );
    }

    #[test]
    fn test_harness_does_not_expand_existing_production_candidate_surface() {
        let production_input = fragments(vec![node("production_hidden", "lib", false)], vec![]);
        let mut test_hidden = node("test_hidden", "lib", true);
        test_hidden.name = "production_hidden".into();
        let test_input = test_fragments(vec![test_hidden], vec![]);

        assert!(
            analyze_with_tests(
                &production_input,
                &test_input,
                &candidate_crates(),
                &HashSet::new(),
            )
            .is_empty()
        );
    }

    #[test]
    fn public_entry_needed_across_crates_is_clean() {
        let mut input = fragments(vec![node("entry", "lib", true)], vec![]);
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn cross_crate_variant_use_requires_its_parent_enum_to_remain_public() {
        let mut input = fragments(
            vec![
                typed_node("api_enum", "lib", true, DefinitionKind::Enum),
                typed_node("api_enum::used", "lib", true, DefinitionKind::EnumVariant),
            ],
            vec![Edge {
                from: "api_enum::used".into(),
                to: "api_enum".into(),
                kind: EdgeKind::Interface,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "api_enum::used".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn internally_used_variant_of_required_public_enum_is_not_reported() {
        let mut input = fragments(
            vec![
                typed_node("entry", "lib", true, DefinitionKind::Function),
                typed_node("api_enum", "lib", true, DefinitionKind::Enum),
                typed_node(
                    "api_enum::internal",
                    "lib",
                    true,
                    DefinitionKind::EnumVariant,
                ),
            ],
            vec![
                Edge {
                    from: "entry".into(),
                    to: "api_enum".into(),
                    kind: EdgeKind::Interface,
                },
                Edge {
                    from: "entry".into(),
                    to: "api_enum::internal".into(),
                    kind: EdgeKind::Body,
                },
            ],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn cross_crate_field_use_requires_its_public_payload_type_to_remain_public() {
        let mut input = fragments(
            vec![
                typed_node("api_field", "lib", true, DefinitionKind::Field),
                typed_node("payload", "lib", true, DefinitionKind::Struct),
            ],
            vec![Edge {
                from: "api_field".into(),
                to: "payload".into(),
                kind: EdgeKind::Interface,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "api_field".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn cross_crate_generated_field_use_preserves_its_source_field_visibility() {
        let mut input = fragments(
            vec![
                typed_node("source_field", "lib", true, DefinitionKind::Field),
                typed_node("generated_field", "lib", false, DefinitionKind::Field),
            ],
            vec![Edge {
                from: "generated_field".into(),
                to: "source_field".into(),
                kind: EdgeKind::VisibilityRequirement,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "generated_field".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn internal_generated_field_use_does_not_make_its_source_field_live() {
        let mut input = fragments(
            vec![
                typed_node("entry", "lib", true, DefinitionKind::Function),
                typed_node("source_field", "lib", true, DefinitionKind::Field),
                typed_node("generated_field", "lib", false, DefinitionKind::Field),
            ],
            vec![
                Edge {
                    from: "entry".into(),
                    to: "generated_field".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "generated_field".into(),
                    to: "source_field".into(),
                    kind: EdgeKind::VisibilityRequirement,
                },
            ],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DeadPublic);
        assert_eq!(findings[0].definition.id, "source_field");
    }

    #[test]
    fn typechecked_cross_crate_reference_requires_public_visibility() {
        let mut input = fragments(vec![node("entry", "lib", true)], vec![]);
        input[0]
            .definitions
            .push(node("unreachable_helper", "app", false));
        input[0].edges.push(Edge {
            from: "unreachable_helper".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn duplicate_compilation_units_share_liveness() {
        let mut input = fragments(
            vec![
                node("duplicate_a", "lib", true),
                node("duplicate_b", "lib", true),
            ],
            vec![],
        );
        input[1].definitions[0].name = "duplicate".into();
        input[1].definitions[1].name = "duplicate".into();
        let duplicate = input[1].definitions.pop().unwrap();
        input.push(Fragment {
            protocol_version: ProtocolVersion,
            package_name: "lib".into(),
            crate_name: "lib".into(),
            crate_id: "lib-test".into(),
            is_product_root: false,
            test_surface: false,
            definitions: vec![duplicate],
            edges: vec![],
            roots: vec![],
            conservative_roots: vec![],
            required_public_roots: vec![],
        });
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "duplicate_b".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn same_span_declarations_in_one_compilation_unit_do_not_share_liveness() {
        let mut input = fragments(
            vec![node("first", "lib", true), node("second", "lib", true)],
            vec![],
        );
        for definition in &mut input[1].definitions {
            definition.span = Some(Span {
                file: "shared.rs".into(),
                line: 1,
                column: 1,
            });
        }
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "first".into(),
            kind: EdgeKind::Body,
        });

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].definition.id, "second");
        assert_eq!(findings[0].kind, FindingKind::DeadPublic);
    }

    #[test]
    fn unreachable_public_reference_does_not_keep_a_helper_alive() {
        let input = fragments(
            vec![
                node("debug_entry", "lib", true),
                node("helper", "lib", true),
            ],
            vec![Edge {
                from: "debug_entry".into(),
                to: "helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .all(|finding| finding.kind == FindingKind::DeadPublic)
        );
    }

    #[test]
    fn dead_code_allow_suppresses_an_entry_but_preserves_its_body() {
        let mut allowed_entry = node("allowed_entry", "lib", true);
        allowed_entry.dead_code_allowed = true;
        let mut input = fragments(
            vec![allowed_entry, node("helper", "lib", true)],
            vec![Edge {
                from: "allowed_entry".into(),
                to: "helper".into(),
                kind: EdgeKind::Body,
            }],
        );
        input[1].conservative_roots.push("allowed_entry".into());

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].definition.id, "helper");
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
    }

    #[test]
    fn public_signature_type_of_a_cross_crate_entry_stays_public() {
        let mut input = fragments(
            vec![
                node("factory", "lib", true),
                node("return_type", "lib", true),
            ],
            vec![Edge {
                from: "factory".into(),
                to: "return_type".into(),
                kind: EdgeKind::Interface,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "factory".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn trait_interface_type_required_by_rust_visibility_is_clean() {
        let mut input = fragments(vec![node("options", "lib", true)], vec![]);
        input[1].required_public_roots.push("options".into());

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn public_reexport_target_required_by_rust_visibility_is_clean() {
        let mut input = fragments(vec![node("reexported", "lib", true)], vec![]);
        input[1].required_public_roots.push("reexported".into());

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn dead_public_reexport_is_reported_without_narrowing_its_target() {
        let mut input = fragments(
            vec![
                typed_node("alias", "lib", true, DefinitionKind::Reexport),
                node("target", "lib", true),
            ],
            vec![Edge {
                from: "alias".into(),
                to: "target".into(),
                kind: EdgeKind::Reexport,
            }],
        );
        input[1].required_public_roots.push("target".into());

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::DeadPublic);
        assert_eq!(findings[0].definition.id, "alias");
    }

    #[test]
    fn locally_used_public_reexport_can_be_narrowed() {
        let mut input = fragments(
            vec![
                typed_node("alias", "lib", true, DefinitionKind::Reexport),
                node("entry", "lib", true),
                node("target", "lib", true),
            ],
            vec![
                Edge {
                    from: "alias".into(),
                    to: "target".into(),
                    kind: EdgeKind::Reexport,
                },
                Edge {
                    from: "entry".into(),
                    to: "target".into(),
                    kind: EdgeKind::Body,
                },
            ],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });
        input[1].required_public_roots.push("target".into());

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "alias");
    }

    #[test]
    fn possible_cross_crate_consumer_preserves_public_reexport() {
        let mut input = fragments(
            vec![
                typed_node("alias", "lib", true, DefinitionKind::Reexport),
                node("target", "lib", true),
            ],
            vec![Edge {
                from: "alias".into(),
                to: "target".into(),
                kind: EdgeKind::Reexport,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "target".into(),
            kind: EdgeKind::Body,
        });
        input[1].required_public_roots.push("target".into());

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn internally_used_public_module_can_be_narrowed() {
        let mut input = fragments(
            vec![
                typed_node("namespace", "lib", true, DefinitionKind::Module),
                node("entry", "lib", true),
                node("child", "lib", false),
            ],
            vec![
                Edge {
                    from: "entry".into(),
                    to: "child".into(),
                    kind: EdgeKind::Body,
                },
                Edge {
                    from: "child".into(),
                    to: "namespace".into(),
                    kind: EdgeKind::VisibilityParent,
                },
            ],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "entry".into(),
            kind: EdgeKind::Body,
        });

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "namespace");
    }

    #[test]
    fn cross_crate_descendant_preserves_public_module_path() {
        let mut input = fragments(
            vec![
                typed_node("namespace", "lib", true, DefinitionKind::Module),
                node("child", "lib", true),
            ],
            vec![Edge {
                from: "child".into(),
                to: "namespace".into(),
                kind: EdgeKind::VisibilityParent,
            }],
        );
        input[0].edges.push(Edge {
            from: "main".into(),
            to: "child".into(),
            kind: EdgeKind::Body,
        });

        assert!(analyze(&input, &HashSet::new()).is_empty());
    }

    #[test]
    fn live_trait_item_keeps_containing_trait_live() {
        let mut input = fragments(
            vec![node("extension_trait", "lib", true)],
            vec![Edge {
                from: "extension_method".into(),
                to: "extension_trait".into(),
                kind: EdgeKind::Interface,
            }],
        );
        input[1]
            .definitions
            .push(node("extension_method", "lib", false));
        input[1].conservative_roots.push("extension_method".into());

        let findings = analyze(&input, &HashSet::new());

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, FindingKind::UnnecessaryPublic);
        assert_eq!(findings[0].definition.id, "extension_trait");
    }
}
