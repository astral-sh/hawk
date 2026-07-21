use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;

use cargo_hawk_internal::graph::{
    DeclarationSpan, Definition, DefinitionId, DefinitionIdentity, DefinitionKind, Finding,
    Fragment, Span,
};

pub(crate) struct PrunePlan<'a> {
    candidates: Vec<PruneCandidate<'a>>,
    contained: usize,
    unsupported: usize,
    incomplete_span: usize,
    remaining_uses: usize,
}

struct PruneCandidate<'a> {
    definition: &'a Definition,
    span: &'a DeclarationSpan,
}

impl<'a> PrunePlan<'a> {
    pub(crate) fn build(
        findings: &[&Finding<'a>],
        retained_dead_definitions: &[&'a Definition],
        production_fragments: &'a [Fragment],
        test_fragments: &'a [Fragment],
    ) -> Self {
        let mut unsupported_definitions = Vec::new();
        let mut incomplete_definitions = Vec::new();
        let mut candidates = Vec::new();
        for finding in findings {
            let definition = finding.definition;
            if matches!(
                definition.kind,
                DefinitionKind::Field | DefinitionKind::EnumVariant | DefinitionKind::Reexport
            ) {
                unsupported_definitions.push(definition);
                continue;
            }
            let Some(span) = definition.declaration_span.as_ref() else {
                incomplete_definitions.push(definition);
                continue;
            };
            candidates.push(PruneCandidate { definition, span });
        }

        let incomplete_modules: Vec<_> = incomplete_definitions
            .iter()
            .copied()
            .filter(|definition| definition.kind == DefinitionKind::Module)
            .collect();
        let mut incomplete_span = incomplete_definitions.len();
        candidates.retain(|candidate| {
            let contained = incomplete_modules
                .iter()
                .any(|module| module_contains(module, candidate.definition));
            incomplete_span += usize::from(contained);
            !contained
        });

        candidates.sort_unstable_by(|left, right| {
            left.span
                .file
                .cmp(&right.span.file)
                .then_with(|| left.span.start_line.cmp(&right.span.start_line))
                .then_with(|| left.span.start_column.cmp(&right.span.start_column))
                .then_with(|| right.span.end_line.cmp(&left.span.end_line))
                .then_with(|| right.span.end_column.cmp(&left.span.end_column))
                .then_with(|| left.definition.crate_name.cmp(&right.definition.crate_name))
                .then_with(|| left.definition.name.cmp(&right.definition.name))
        });

        let candidate_by_identity: HashMap<_, _> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| (identity(candidate.definition), index))
            .collect();
        let mut candidates_by_file: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, candidate) in candidates.iter().enumerate() {
            candidates_by_file
                .entry(&candidate.span.file)
                .or_default()
                .push(index);
        }
        let fragments = production_fragments.iter().chain(test_fragments);
        let mut definition_by_id = HashMap::new();
        let mut candidate_by_id = HashMap::new();
        for fragment in fragments.clone() {
            for definition in &fragment.definitions {
                definition_by_id.entry(definition.id).or_insert(definition);
                if let Some(index) = candidate_by_identity.get(&identity(definition)) {
                    candidate_by_id.insert(definition.id, *index);
                }
            }
        }

        let mut dependencies = vec![Vec::new(); candidates.len()];
        for (child, candidate) in candidates.iter().enumerate() {
            dependencies[child].extend(
                candidates
                    .iter()
                    .enumerate()
                    .filter(|(parent, parent_candidate)| {
                        *parent != child && contains_candidate(parent_candidate, candidate)
                    })
                    .map(|(parent, _)| parent),
            );
        }
        let mut blocked = vec![false; candidates.len()];
        let mut queue = VecDeque::new();
        let mut owner_cache = HashMap::new();
        for definition in retained_dead_definitions {
            for (index, candidate) in candidates.iter().enumerate() {
                if contains_definition(candidate, definition) && !blocked[index] {
                    blocked[index] = true;
                    queue.push_back(index);
                }
            }
        }
        for definition in &incomplete_definitions {
            for (index, candidate) in candidates.iter().enumerate() {
                if contains_definition(candidate, definition) && !blocked[index] {
                    blocked[index] = true;
                    queue.push_back(index);
                }
            }
        }
        for root in fragments
            .clone()
            .flat_map(|fragment| &fragment.conservative_roots)
        {
            if let Some(index) = candidate_owner(
                *root,
                &candidates,
                &candidate_by_id,
                &definition_by_id,
                &candidates_by_file,
            ) && !blocked[index]
            {
                blocked[index] = true;
                queue.push_back(index);
            }
        }
        for edge in fragments.clone().flat_map(|fragment| &fragment.edges) {
            let target = *owner_cache.entry(edge.to).or_insert_with(|| {
                candidate_owner(
                    edge.to,
                    &candidates,
                    &candidate_by_id,
                    &definition_by_id,
                    &candidates_by_file,
                )
            });
            let Some(target) = target else {
                continue;
            };
            let source = *owner_cache.entry(edge.from).or_insert_with(|| {
                candidate_owner(
                    edge.from,
                    &candidates,
                    &candidate_by_id,
                    &definition_by_id,
                    &candidates_by_file,
                )
            });
            if let Some(source) = source {
                dependencies[source].push(target);
            } else if !blocked[target] {
                blocked[target] = true;
                queue.push_back(target);
            }
        }
        while let Some(source) = queue.pop_front() {
            for &target in &dependencies[source] {
                if !blocked[target] {
                    blocked[target] = true;
                    queue.push_back(target);
                }
            }
        }

        let remaining_uses = blocked.iter().filter(|blocked| **blocked).count();
        let unblocked = candidates
            .into_iter()
            .zip(blocked)
            .filter_map(|(candidate, blocked)| (!blocked).then_some(candidate))
            .collect::<Vec<_>>();
        let mut contained = 0;
        let candidates = unblocked
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let is_contained =
                    unblocked
                        .iter()
                        .enumerate()
                        .any(|(parent, parent_candidate)| {
                            parent != index
                                && contains_candidate(parent_candidate, candidate)
                                && (!contains_candidate(candidate, parent_candidate)
                                    || parent < index)
                        });
                contained += usize::from(is_contained);
                (!is_contained).then_some(PruneCandidate {
                    definition: candidate.definition,
                    span: candidate.span,
                })
            })
            .collect::<Vec<_>>();
        let mut unsupported = 0;
        for definition in unsupported_definitions {
            if candidates
                .iter()
                .any(|candidate| contains_definition(candidate, definition))
            {
                contained += 1;
            } else {
                unsupported += 1;
            }
        }
        Self {
            candidates,
            contained,
            unsupported,
            incomplete_span,
            remaining_uses,
        }
    }

    pub(crate) fn write_preview(&self, output: &mut String) -> std::fmt::Result {
        writeln!(
            output,
            "hawk: prune preview: {} removable declaration candidate(s)",
            self.candidates.len()
        )?;
        for candidate in &self.candidates {
            let span = candidate.span;
            writeln!(
                output,
                "  {}:{}:{}-{}:{}: `{}::{}`",
                span.file,
                span.start_line,
                span.start_column,
                span.end_line,
                span.end_column,
                candidate.definition.crate_name,
                candidate.definition.name
            )?;
        }
        let skipped =
            self.contained + self.unsupported + self.incomplete_span + self.remaining_uses;
        if skipped > 0 {
            writeln!(
                output,
                "hawk: prune preview: skipped {skipped} finding(s) ({} contained, {} field/variant/re-export, {} without a complete source range, {} with remaining uses or retained descendants)",
                self.contained, self.unsupported, self.incomplete_span, self.remaining_uses
            )?;
        }
        writeln!(output, "hawk: prune preview: no source files were modified")
    }
}

fn identity(definition: &Definition) -> DefinitionIdentity<'_> {
    DefinitionIdentity::new(
        &definition.crate_name,
        &definition.name,
        definition.kind,
        definition.span.as_ref(),
    )
}

fn candidate_owner(
    id: DefinitionId,
    candidates: &[PruneCandidate<'_>],
    candidate_by_id: &HashMap<DefinitionId, usize>,
    definition_by_id: &HashMap<DefinitionId, &Definition>,
    candidates_by_file: &HashMap<&str, Vec<usize>>,
) -> Option<usize> {
    if let Some(index) = candidate_by_id.get(&id) {
        return Some(*index);
    }
    let span = definition_by_id.get(&id).copied()?;
    let physical = span.span.as_ref().and_then(|span| {
        candidates_by_file
            .get(span.file.as_str())
            .into_iter()
            .flatten()
            .copied()
            .filter(|index| contains_location(candidates[*index].span, span))
            .max_by_key(|index| candidates[*index].definition.name.matches("::").count())
    });
    physical.or_else(|| {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| module_contains(candidate.definition, span))
            .max_by_key(|(_, candidate)| candidate.definition.name.matches("::").count())
            .map(|(index, _)| index)
    })
}

fn contains_candidate(parent: &PruneCandidate<'_>, child: &PruneCandidate<'_>) -> bool {
    contains(parent.span, child.span) || module_contains(parent.definition, child.definition)
}

fn contains_definition(parent: &PruneCandidate<'_>, child: &Definition) -> bool {
    child
        .span
        .as_ref()
        .is_some_and(|span| contains_location(parent.span, span))
        || module_contains(parent.definition, child)
}

fn module_contains(parent: &Definition, child: &Definition) -> bool {
    parent.kind == DefinitionKind::Module
        && parent.crate_name == child.crate_name
        && child
            .name
            .strip_prefix(&parent.name)
            .is_some_and(|suffix| suffix.starts_with("::"))
}

fn contains(parent: &DeclarationSpan, child: &DeclarationSpan) -> bool {
    parent.file == child.file
        && (parent.start_line, parent.start_column) <= (child.start_line, child.start_column)
        && (parent.end_line, parent.end_column) >= (child.end_line, child.end_column)
}

fn contains_location(parent: &DeclarationSpan, child: &Span) -> bool {
    parent.file == child.file
        && (parent.start_line, parent.start_column) <= (child.line, child.column)
        && (parent.end_line, parent.end_column) > (child.line, child.column)
}
