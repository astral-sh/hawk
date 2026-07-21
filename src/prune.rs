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
        production_fragments: &'a [Fragment],
        test_fragments: &'a [Fragment],
    ) -> Self {
        let mut unsupported_definitions = Vec::new();
        let mut incomplete_span = 0;
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
                incomplete_span += 1;
                continue;
            };
            candidates.push(PruneCandidate { definition, span });
        }

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

        let mut outermost = Vec::new();
        let mut contained = 0;
        for candidate in candidates {
            if outermost
                .iter()
                .any(|parent: &PruneCandidate<'_>| contains(parent.span, candidate.span))
            {
                contained += 1;
            } else {
                outermost.push(candidate);
            }
        }
        let mut unsupported = 0;
        for definition in unsupported_definitions {
            let is_contained = definition.span.as_ref().is_some_and(|span| {
                outermost
                    .iter()
                    .any(|candidate| contains_location(candidate.span, span))
            });
            if is_contained {
                contained += 1;
            } else {
                unsupported += 1;
            }
        }

        let candidate_by_identity: HashMap<_, _> = outermost
            .iter()
            .enumerate()
            .map(|(index, candidate)| (identity(candidate.definition), index))
            .collect();
        let mut candidates_by_file: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, candidate) in outermost.iter().enumerate() {
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

        let mut dependencies = vec![Vec::new(); outermost.len()];
        let mut blocked = vec![false; outermost.len()];
        let mut queue = VecDeque::new();
        let mut source_owner_cache = HashMap::new();
        for edge in fragments.clone().flat_map(|fragment| &fragment.edges) {
            let Some(&target) = candidate_by_id.get(&edge.to) else {
                continue;
            };
            let source = *source_owner_cache.entry(edge.from).or_insert_with(|| {
                source_owner(
                    edge.from,
                    &outermost,
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
        let candidates = outermost
            .into_iter()
            .zip(blocked)
            .filter_map(|(candidate, blocked)| (!blocked).then_some(candidate))
            .collect();
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
                "hawk: prune preview: skipped {skipped} finding(s) ({} contained, {} field/variant/re-export, {} without a complete source range, {} with remaining uses)",
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

fn source_owner(
    source: DefinitionId,
    candidates: &[PruneCandidate<'_>],
    candidate_by_id: &HashMap<DefinitionId, usize>,
    definition_by_id: &HashMap<DefinitionId, &Definition>,
    candidates_by_file: &HashMap<&str, Vec<usize>>,
) -> Option<usize> {
    if let Some(index) = candidate_by_id.get(&source) {
        return Some(*index);
    }
    let span = definition_by_id
        .get(&source)
        .and_then(|definition| definition.span.as_ref())?;
    candidates_by_file
        .get(span.file.as_str())?
        .iter()
        .copied()
        .find(|index| contains_location(candidates[*index].span, span))
}

fn contains(parent: &DeclarationSpan, child: &DeclarationSpan) -> bool {
    parent.file == child.file
        && (parent.start_line, parent.start_column) <= (child.start_line, child.start_column)
        && (parent.end_line, parent.end_column) >= (child.end_line, child.end_column)
}

fn contains_location(parent: &DeclarationSpan, child: &Span) -> bool {
    parent.file == child.file
        && (parent.start_line, parent.start_column) <= (child.line, child.column)
        && (parent.end_line, parent.end_column) >= (child.line, child.column)
}
