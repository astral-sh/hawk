use std::collections::{BTreeMap, HashMap};
use std::fmt::{Display, Formatter, Write as _};
use std::fs;
use std::path::{Path, PathBuf};

use anstyle::{AnsiColor, Style};

use crate::cli::LintLevel;
use crate::config::{Config, ConfigDiagnostic, ConfigDiagnosticKind};
use cargo_hawk_internal::graph::{DefinitionKind, Finding, FindingKind, Span};

pub(crate) const WARNING: Style = AnsiColor::Yellow.on_default().bold();
pub(crate) const ERROR: Style = AnsiColor::Red.on_default().bold();
const LOCATION: Style = AnsiColor::BrightBlue.on_default().bold();
const SEPARATOR: Style = AnsiColor::Cyan.on_default();
const HELP: Style = AnsiColor::BrightCyan.on_default().bold();
pub(crate) const EMPHASIS: Style = Style::new().bold();

type SourceLoader = fn(&Path) -> std::io::Result<String>;

fn load_source(path: &Path) -> std::io::Result<String> {
    fs::read_to_string(path)
}

struct CachedSource {
    source: String,
    line_starts: Vec<usize>,
}

impl CachedSource {
    fn new(source: String) -> Self {
        let line_starts = std::iter::once(0)
            .chain(
                source
                    .bytes()
                    .enumerate()
                    .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
            )
            .collect();
        Self {
            source,
            line_starts,
        }
    }

    fn line(&self, line: usize) -> Option<&str> {
        let index = line.checked_sub(1)?;
        let start = *self.line_starts.get(index)?;
        if start == self.source.len() {
            return None;
        }
        let Some(next_start) = self.line_starts.get(index + 1).copied() else {
            return Some(&self.source[start..]);
        };
        let line = &self.source[start..next_start - 1];
        Some(line.strip_suffix('\r').unwrap_or(line))
    }
}

pub(crate) struct DiagnosticRenderer<'a, L = SourceLoader> {
    workspace_root: &'a Path,
    sources: HashMap<PathBuf, Option<CachedSource>>,
    load_source: L,
    output: String,
}

impl<'a> DiagnosticRenderer<'a> {
    pub(crate) fn new(workspace_root: &'a Path) -> Self {
        Self::with_source_loader(workspace_root, load_source)
    }
}

impl<'a, L> DiagnosticRenderer<'a, L>
where
    L: FnMut(&Path) -> std::io::Result<String>,
{
    pub(crate) fn with_source_loader(workspace_root: &'a Path, load_source: L) -> Self {
        Self {
            workspace_root,
            sources: HashMap::new(),
            load_source,
            output: String::new(),
        }
    }

    pub(crate) fn write_diagnostic(
        &mut self,
        finding: &Finding<'_>,
        production_description: &str,
        level: LintLevel,
    ) -> std::fmt::Result {
        let source_line = finding
            .definition
            .span
            .as_ref()
            .and_then(|span| self.source_line(span))
            .map(str::to_owned);
        write_diagnostic(
            &mut self.output,
            finding,
            production_description,
            source_line.as_deref(),
            level,
        )
    }

    pub(crate) fn write_config_diagnostic(
        &mut self,
        diagnostic: &ConfigDiagnostic<'_>,
        config: &Config,
        level: LintLevel,
    ) -> std::fmt::Result {
        write_config_diagnostic(
            &mut self.output,
            diagnostic,
            config,
            self.workspace_root,
            level,
        )
    }

    pub(crate) fn write_summary(
        &mut self,
        diagnostic_count: usize,
        diagnostic_counts: &BTreeMap<&str, BTreeMap<&str, usize>>,
        production_summary: &str,
        compilation_target: &str,
    ) -> std::fmt::Result {
        writeln!(
            self.output,
            "hawk: {diagnostic_count} finding(s) for {production_summary} and workspace non-production targets on {compilation_target}"
        )?;
        for (lint, crates) in diagnostic_counts {
            let lint_count: usize = crates.values().sum();
            write!(self.output, "  {lint}: {lint_count} (")?;
            for (index, (crate_name, count)) in crates.iter().enumerate() {
                if index > 0 {
                    write!(self.output, ", ")?;
                }
                write!(self.output, "{crate_name}: {count}")?;
            }
            writeln!(self.output, ")")?;
        }
        Ok(())
    }

    pub(crate) fn source_line(&mut self, span: &Span) -> Option<&str> {
        let source_path = Path::new(&span.file);
        let source_path = if source_path.is_absolute() {
            source_path.to_path_buf()
        } else {
            self.workspace_root.join(source_path)
        };
        let source = self
            .sources
            .entry(source_path.clone())
            .or_insert_with(|| (self.load_source)(&source_path).ok().map(CachedSource::new));
        source.as_ref()?.line(span.line)
    }

    pub(crate) fn into_output(self) -> String {
        self.output
    }
}

fn write_diagnostic(
    output: &mut String,
    finding: &Finding<'_>,
    production_description: &str,
    source_line: Option<&str>,
    level: LintLevel,
) -> std::fmt::Result {
    let dead_reachability_source = if finding.test_compiled_only {
        "any workspace test"
    } else {
        production_description
    };
    let (message, help, marker) = match (finding.kind, finding.definition.kind, finding.test_only) {
        (FindingKind::DeadPublic, DefinitionKind::EnumVariant, _) => (
            format!(
                "`{}` is a public enum variant but is not reachable from {dead_reachability_source}",
                finding.definition.name
            ),
            "consider removing this variant and its remaining uses",
            "public enum variant",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::EnumVariant, _) => {
            unreachable!("live enum variants do not have actionable visibility findings")
        }
        (FindingKind::DeadPublic, DefinitionKind::Reexport, _) => (
            format!(
                "public re-export `{}` has no target reachable from {dead_reachability_source}",
                finding.definition.name
            ),
            "consider restricting this re-export's visibility or removing it",
            "public re-export",
        ),
        (FindingKind::DeadPublic, DefinitionKind::Module, _) => (
            format!(
                "public module `{}` has no declaration reachable from {dead_reachability_source}",
                finding.definition.name
            ),
            "consider restricting this module's visibility or removing it",
            "public module",
        ),
        (FindingKind::DeadPublic, _, _) => (
            format!(
                "`{}` is public but is not reachable from {dead_reachability_source}",
                finding.definition.name
            ),
            "consider restricting this declaration's visibility or removing it",
            "public declaration",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::Reexport, true) => (
            format!(
                "public re-export `{}` is needed only by tests; it can be `pub(crate)`",
                finding.definition.name
            ),
            "change this re-export to `pub(crate) use`",
            "public re-export",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::Reexport, false) => (
            format!(
                "public re-export `{}` is not required by any compiled cross-crate use; it can be `pub(crate)`",
                finding.definition.name
            ),
            "change this re-export to `pub(crate) use`",
            "public re-export",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::Module, true) => (
            format!(
                "public module `{}` is needed only by tests; it can be `pub(crate)`",
                finding.definition.name
            ),
            "change this module to `pub(crate) mod`",
            "public module",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::Module, false) => (
            format!(
                "public module `{}` is used only within `{}`; it can be `pub(crate)`",
                finding.definition.name, finding.definition.crate_name
            ),
            "change this module to `pub(crate) mod`",
            "public module",
        ),
        (FindingKind::UnnecessaryPublic, _, true) => (
            format!(
                "`{}` is public but is needed only by tests; it can be `pub(crate)`",
                finding.definition.name
            ),
            "change this declaration to `pub(crate)`",
            "public declaration",
        ),
        (FindingKind::UnnecessaryPublic, _, false) => (
            format!(
                "`{}` is public but all reachable uses are within `{}`; it can be `pub(crate)`",
                finding.definition.name, finding.definition.crate_name
            ),
            "change this declaration to `pub(crate)`",
            "public declaration",
        ),
        (
            FindingKind::UnnecessaryRestrictedVisibility | FindingKind::UnnecessaryCrateVisibility,
            DefinitionKind::EnumVariant | DefinitionKind::Reexport,
            _,
        ) => {
            unreachable!("restricted visibility findings exclude variants and re-exports")
        }
        (FindingKind::UnnecessaryRestrictedVisibility, definition_kind, _) => (
            format!(
                "`{}` has explicit restricted visibility but all compiled uses fit within the defining module; it can be private",
                finding.definition.name
            ),
            match definition_kind {
                DefinitionKind::Module => "remove this module's visibility modifier",
                _ => "remove this declaration's visibility modifier",
            },
            match definition_kind {
                DefinitionKind::Module => "restricted-visibility module",
                _ => "restricted-visibility declaration",
            },
        ),
        (FindingKind::UnnecessaryCrateVisibility, definition_kind, _) => (
            format!(
                "`{}` is visible throughout the crate but all compiled uses fit within the parent module; it can be `pub(super)`",
                finding.definition.name
            ),
            match definition_kind {
                DefinitionKind::Module => "change this module to `pub(super) mod`",
                _ => "change this declaration to `pub(super)`",
            },
            match definition_kind {
                DefinitionKind::Module => "crate-visible module",
                _ => "crate-visible declaration",
            },
        ),
    };
    write_diagnostic_header(output, finding.kind.code(), message, level)?;

    if let Some(span) = &finding.definition.span {
        let width = write_annotated_location(
            output,
            &span.file,
            span.line,
            span.column,
            source_line,
            marker,
            level.style(),
        )?;
        writeln!(
            output,
            "{empty:>width$} {} {}: {help}",
            styled("=", SEPARATOR),
            styled("help", HELP),
            empty = "",
            width = width
        )?;
    } else {
        writeln!(
            output,
            "  {} {}: declaration in crate `{}`",
            styled("=", SEPARATOR),
            styled("note", HELP),
            finding.definition.crate_name
        )?;
        writeln!(
            output,
            "  {} {}: {help}",
            styled("=", SEPARATOR),
            styled("help", HELP)
        )?;
    }
    writeln!(output)
}

fn write_config_diagnostic(
    output: &mut String,
    diagnostic: &ConfigDiagnostic<'_>,
    config: &Config,
    workspace_root: &Path,
    level: LintLevel,
) -> std::fmt::Result {
    let entry = diagnostic.entry;
    let item = format!("{}::{}", entry.crate_name, entry.item);
    let (message, marker, help) = match diagnostic.kind {
        ConfigDiagnosticKind::UnknownItem => (
            format!(
                "override for `{}` references unknown item `{item}`",
                entry.lint.code()
            ),
            "no matching item was found",
            "remove this override or update its `crate` and `item` selectors",
        ),
        ConfigDiagnosticKind::AmbiguousItem => (
            format!(
                "override for `{}` matches multiple items named `{item}`",
                entry.lint.code()
            ),
            "selector matches multiple items",
            "add a `kind` selector or otherwise disambiguate this override",
        ),
        ConfigDiagnosticKind::UnfulfilledExpectation => (
            format!(
                "expected `{}` for `{item}`, but no finding was produced",
                entry.lint.code()
            ),
            "unfulfilled expectation",
            "remove this expectation or update its `lint` selector",
        ),
    };
    write_diagnostic_header(output, diagnostic.kind.code(), message, level)?;

    let config_path = config.path().expect("diagnostic requires a loaded config");
    let display_path = config_path
        .strip_prefix(workspace_root)
        .unwrap_or(config_path)
        .display();
    write_annotated_location(
        output,
        display_path,
        entry.span.line,
        entry.span.column,
        config.source_line(entry.span.line),
        marker,
        level.style(),
    )?;
    writeln!(
        output,
        "  {} {}: reason: {}",
        styled("=", SEPARATOR),
        styled("note", HELP),
        entry.reason
    )?;
    writeln!(
        output,
        "  {} {}: {help}",
        styled("=", SEPARATOR),
        styled("help", HELP)
    )?;
    writeln!(output)
}

fn write_diagnostic_header(
    output: &mut String,
    code: &str,
    message: impl Display,
    level: LintLevel,
) -> std::fmt::Result {
    writeln!(
        output,
        "{}: {}",
        styled(format_args!("{}[{code}]", level.severity()), level.style()),
        styled(message, EMPHASIS)
    )
}

fn write_annotated_location(
    output: &mut String,
    file: impl Display,
    line: usize,
    column: usize,
    source_line: Option<&str>,
    marker: &str,
    marker_style: Style,
) -> Result<usize, std::fmt::Error> {
    writeln!(
        output,
        "  {} {file}:{line}:{column}",
        styled("-->", LOCATION)
    )?;
    let width = line.to_string().len();
    if let Some(source_line) = source_line {
        writeln!(
            output,
            "{empty:>width$} {}",
            styled("|", SEPARATOR),
            empty = "",
            width = width
        )?;
        writeln!(
            output,
            "{} {} {source_line}",
            styled(format!("{line:>width$}"), LOCATION),
            styled("|", SEPARATOR)
        )?;
        writeln!(
            output,
            "{empty:>width$} {} {}",
            styled("|", SEPARATOR),
            styled(
                format_args!("{}^^^ {marker}", marker_indent(source_line, column)),
                marker_style
            ),
            empty = "",
            width = width
        )?;
    }
    Ok(width)
}

pub(crate) fn styled(content: impl Display, style: Style) -> impl Display {
    struct Styled<T> {
        content: T,
        style: Style,
    }

    impl<T: Display> Display for Styled<T> {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            write!(
                formatter,
                "{}{}{}",
                self.style.render(),
                self.content,
                self.style.render_reset()
            )
        }
    }

    Styled { content, style }
}

fn marker_indent(source_line: &str, column: usize) -> String {
    source_line
        .chars()
        .take(column.saturating_sub(1))
        .map(|character| if character == '\t' { '\t' } else { ' ' })
        .collect()
}
