use std::collections::HashSet;
use std::env;
use std::fmt::{Display, Formatter, Write as _};
use std::fs::{self, File};
use std::io::{BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anstyle::{AnsiColor, Style};
use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};

use crate::config::{AnalysisTarget, Config, ConfigDiagnostic, ConfigDiagnosticKind};
use crate::graph::{
    DefinitionKind, Finding, FindingKind, FixPlan, FixTarget, Fragment, Span, analyze,
};

#[derive(Debug, Parser)]
#[command(
    name = "cargo hawk",
    about = "Find unnecessary public surface in a Cargo binary product"
)]
struct Args {
    /// Path to the workspace manifest.
    #[arg(long, default_value = "Cargo.toml")]
    manifest_path: PathBuf,

    /// Package containing the selected binary product.
    #[arg(short = 'p', long)]
    package: String,

    /// Binary target that defines production reachability.
    #[arg(long)]
    bin: String,

    /// Compilation target triple to analyze; defaults to the host target.
    #[arg(long, value_name = "TRIPLE")]
    target: Option<String>,

    /// Workspace library crate whose API is an external boundary.
    #[arg(long = "exclude-crate")]
    excluded_crates: Vec<String>,

    /// Reusable Cargo target directory for the instrumented build.
    #[arg(long)]
    target_dir: Option<PathBuf>,

    /// Preserve serialized compiler fragments at this directory.
    #[arg(long)]
    graph_dir: Option<PathBuf>,

    /// Path to Hawk lint overrides; defaults to hawk.toml in the workspace root.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Suppress a Hawk diagnostic or warning group.
    #[arg(short = 'A', long = "allow", value_name = "LINT")]
    allow: Vec<String>,

    /// Emit a Hawk diagnostic or warning group without failing.
    #[arg(short = 'W', long = "warn", value_name = "LINT")]
    warn: Vec<String>,

    /// Emit a Hawk diagnostic or warning group as an error.
    #[arg(short = 'D', long = "deny", value_name = "LINT")]
    deny: Vec<String>,

    /// Automatically apply machine-applicable visibility fixes.
    #[arg(long)]
    fix: bool,

    /// Apply fixes despite uncommitted changes in the workspace.
    #[arg(long, requires = "fix")]
    allow_dirty: bool,

    /// Apply fixes despite staged changes in the workspace.
    #[arg(long, requires = "fix")]
    allow_staged: bool,

    /// Apply fixes when the workspace is not under version control.
    #[arg(long, requires = "fix")]
    allow_no_vcs: bool,

    /// Control when colored output is used.
    #[arg(long, value_enum, default_value_t, value_name = "WHEN")]
    color: TerminalColor,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum LintLevel {
    /// Do not emit a diagnostic.
    Allow,

    /// Report a diagnostic without failing.
    #[default]
    Warn,

    /// Report a diagnostic as an error and fail.
    Deny,
}

impl LintLevel {
    fn severity(self) -> &'static str {
        match self {
            Self::Allow => unreachable!("allowed diagnostics are not rendered"),
            Self::Warn => "warning",
            Self::Deny => "error",
        }
    }

    fn style(self) -> Style {
        match self {
            Self::Allow => unreachable!("allowed diagnostics are not rendered"),
            Self::Warn => WARNING,
            Self::Deny => ERROR,
        }
    }

    fn is_emitted(self) -> bool {
        self != Self::Allow
    }
}

#[derive(Debug, Default)]
struct LintLevels {
    overrides: Vec<(String, LintLevel)>,
}

impl LintLevels {
    fn from_matches(matches: &ArgMatches) -> Result<Self> {
        let mut indexed_overrides = Vec::new();
        for (argument, level) in [
            ("allow", LintLevel::Allow),
            ("warn", LintLevel::Warn),
            ("deny", LintLevel::Deny),
        ] {
            let Some(values) = matches.get_many::<String>(argument) else {
                continue;
            };
            let indices = matches
                .indices_of(argument)
                .expect("present lint-level values have argument indices");
            for (index, selector) in indices.zip(values) {
                Self::validate_selector(selector)?;
                indexed_overrides.push((index, selector.clone(), level));
            }
        }
        indexed_overrides.sort_unstable_by_key(|(index, _, _)| *index);
        Ok(Self {
            overrides: indexed_overrides
                .into_iter()
                .map(|(_, selector, level)| (selector, level))
                .collect(),
        })
    }

    fn validate_selector(selector: &str) -> Result<()> {
        if matches!(
            selector,
            "warnings"
                | "hawk::dead_public"
                | "hawk::unnecessary_public"
                | "hawk::unknown_item"
                | "hawk::unfulfilled_expectation"
        ) {
            return Ok(());
        }
        bail!(
            "unknown lint selector `{selector}`; expected `warnings` or a `hawk::...` diagnostic name"
        );
    }

    fn level(&self, code: &str) -> LintLevel {
        self.overrides
            .iter()
            .filter(|(selector, _)| selector == "warnings" || selector == code)
            .map(|(_, level)| *level)
            .next_back()
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum TerminalColor {
    /// Display colors if the output goes to an interactive terminal.
    #[default]
    Auto,

    /// Always display colors.
    Always,

    /// Never display colors.
    Never,
}

impl From<TerminalColor> for anstream::ColorChoice {
    fn from(color: TerminalColor) -> Self {
        match color {
            TerminalColor::Auto => Self::Auto,
            TerminalColor::Always => Self::Always,
            TerminalColor::Never => Self::Never,
        }
    }
}

pub fn run(mut raw_args: Vec<String>) -> Result<ExitCode> {
    if raw_args.get(1).is_some_and(|argument| argument == "hawk") {
        raw_args.remove(1);
    }
    let matches = match Args::command().try_get_matches_from(&raw_args) {
        Ok(matches) => matches,
        Err(error) => {
            let exit_code = error.exit_code();
            error.print().context("print command-line help")?;
            return Ok(ExitCode::from(exit_code as u8));
        }
    };
    let lint_levels = LintLevels::from_matches(&matches)?;
    let args = Args::from_arg_matches(&matches).context("read command-line arguments")?;
    debug_assert_eq!(
        lint_levels.overrides.len(),
        args.allow.len() + args.warn.len() + args.deny.len()
    );
    let metadata = MetadataCommand::new()
        .manifest_path(&args.manifest_path)
        .no_deps()
        .exec()
        .with_context(|| format!("read Cargo metadata from {}", args.manifest_path.display()))?;
    validate_product(&metadata, &args.package, &args.bin)?;

    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let config = Config::load(&workspace_root, args.config.as_deref())?;
    let manifest_path = args
        .manifest_path
        .canonicalize()
        .with_context(|| format!("resolve manifest path for {}", args.manifest_path.display()))?;
    let crate_name = args.bin.replace('-', "_");
    let target_dir = args
        .target_dir
        .clone()
        .unwrap_or_else(|| default_target_dir(&workspace_root, &args.package, &args.bin));
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("create target directory {}", target_dir.display()))?;

    let temporary_graph_dir;
    let graph_dir = match &args.graph_dir {
        Some(path) => {
            fs::create_dir_all(path)
                .with_context(|| format!("create graph directory {}", path.display()))?;
            tempfile::Builder::new()
                .prefix("run-")
                .tempdir_in(path)
                .with_context(|| format!("create graph run directory {}", path.display()))?
                .keep()
        }
        None => {
            temporary_graph_dir =
                tempfile::tempdir().context("create temporary graph directory")?;
            temporary_graph_dir.path().to_path_buf()
        }
    };
    let run_id = graph_dir
        .file_name()
        .unwrap_or(graph_dir.as_os_str())
        .to_string_lossy()
        .into_owned();

    let executable = env::current_exe().context("locate hawk executable")?;
    let cargo = InstrumentedCargo {
        args: &args,
        workspace_root: &workspace_root,
        manifest_path: &manifest_path,
        target_dir: &target_dir,
        graph_dir: &graph_dir,
        crate_name: &crate_name,
        executable: &executable,
    };
    cargo.run("check", &run_id, None)?;

    let mut fragments = read_fragments(&graph_dir)?;
    if !fragments.iter().any(|fragment| fragment.is_product_root) {
        bail!(
            "no instrumented fragment was emitted for binary `{}`; rerun with a fresh --target-dir",
            args.bin
        );
    }
    let analysis_target = AnalysisTarget::from_rustc(args.target.as_deref())?;
    let excluded: HashSet<String> = args.excluded_crates.iter().cloned().collect();
    if args.fix {
        let initial_findings =
            config.apply(&analysis_target, &fragments, analyze(&fragments, &excluded));
        let fix_plan = FixPlan {
            targets: initial_findings
                .findings
                .iter()
                .filter(|finding| lint_levels.level(finding.kind.code()).is_emitted())
                .filter(|finding| finding.definition.kind != DefinitionKind::EnumVariant)
                .map(|finding| FixTarget {
                    id: finding.definition.id.clone(),
                    crate_name: finding.definition.crate_name.clone(),
                    kind: finding.kind,
                })
                .collect(),
        };
        if !fix_plan.targets.is_empty() {
            let fix_packages = fix_packages(&metadata, &fix_plan)?;
            let fix_plan_path = graph_dir.join("fix-plan");
            write_fix_plan(&fix_plan_path, &fix_plan)?;
            cargo.run(
                "fix",
                &format!("{run_id}-fix"),
                Some(FixRequest {
                    plan: &fix_plan_path,
                    packages: &fix_packages,
                }),
            )?;
            cargo.run("check", &format!("{run_id}-post-fix"), None)?;
            fragments = read_fragments(&graph_dir)?;
        }
    }
    let findings = config.apply(&analysis_target, &fragments, analyze(&fragments, &excluded));
    let mut diagnostics = String::new();
    let mut diagnostic_count = 0;
    let mut has_denied_diagnostic = false;
    for finding in &findings.findings {
        let level = lint_levels.level(finding.kind.code());
        if level.is_emitted() {
            diagnostic_count += 1;
            has_denied_diagnostic |= level == LintLevel::Deny;
            write_diagnostic(&mut diagnostics, finding, &args.bin, &workspace_root, level)
                .expect("formatting diagnostics into a string cannot fail");
        }
    }
    for diagnostic in &findings.config_diagnostics {
        let level = lint_levels.level(config_diagnostic_code(diagnostic.kind));
        if level.is_emitted() {
            diagnostic_count += 1;
            has_denied_diagnostic |= level == LintLevel::Deny;
            write_config_diagnostic(
                &mut diagnostics,
                diagnostic,
                &config,
                &workspace_root,
                level,
            )
            .expect("formatting diagnostics into a string cannot fail");
        }
    }
    let compilation_target = args.target.as_deref().map_or_else(
        || "the host target".to_owned(),
        |target| format!("target `{target}`"),
    );
    writeln!(
        diagnostics,
        "hawk: {} finding(s) for `{} --bin {} --all-features` on {}",
        diagnostic_count, args.package, args.bin, compilation_target
    )
    .expect("formatting diagnostics into a string cannot fail");
    anstream::AutoStream::new(std::io::stdout(), args.color.into())
        .write_all(diagnostics.as_bytes())
        .context("write diagnostic output")?;
    Ok(if has_denied_diagnostic {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

struct InstrumentedCargo<'a> {
    args: &'a Args,
    workspace_root: &'a Path,
    manifest_path: &'a Path,
    target_dir: &'a Path,
    graph_dir: &'a Path,
    crate_name: &'a str,
    executable: &'a Path,
}

#[derive(Clone, Copy)]
struct FixRequest<'a> {
    plan: &'a Path,
    packages: &'a [String],
}

impl InstrumentedCargo<'_> {
    fn run(
        &self,
        subcommand: &str,
        run_id: &str,
        fix_request: Option<FixRequest<'_>>,
    ) -> Result<()> {
        let mut command = Command::new("cargo");
        command
            .current_dir(self.workspace_root)
            .arg(subcommand)
            .arg("--manifest-path")
            .arg(self.manifest_path)
            .arg("--all-features")
            .arg("--locked")
            .arg("--target-dir")
            .arg(self.target_dir);
        if let Some(fix_request) = fix_request {
            for package in fix_request.packages {
                command.arg("--package").arg(package);
            }
            command.arg("--lib");
        } else {
            command
                .arg("--package")
                .arg(&self.args.package)
                .arg("--bin")
                .arg(&self.args.bin);
        }
        if let Some(target) = &self.args.target {
            command.arg("--target").arg(target);
        }
        if fix_request.is_some() {
            if self.args.allow_dirty {
                command.arg("--allow-dirty");
            }
            if self.args.allow_staged {
                command.arg("--allow-staged");
            }
            if self.args.allow_no_vcs {
                command.arg("--allow-no-vcs");
            }
        }
        command
            .env("RUSTC_WORKSPACE_WRAPPER", self.executable)
            .env("HAWK_OUTPUT_DIR", self.graph_dir)
            .env("HAWK_ROOT_CRATE", self.crate_name)
            .env("HAWK_RUN_ID", run_id);
        if let Some(fix_request) = fix_request {
            command.env("HAWK_FIX_PLAN", fix_request.plan);
        }
        let status = command
            .status()
            .with_context(|| format!("run instrumented Cargo {subcommand}"))?;
        if !status.success() {
            bail!("instrumented Cargo {subcommand} failed with {status}");
        }
        Ok(())
    }
}

fn write_fix_plan(path: &Path, fix_plan: &FixPlan) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer(file, fix_plan).with_context(|| format!("serialize {}", path.display()))
}

fn fix_packages(metadata: &cargo_metadata::Metadata, fix_plan: &FixPlan) -> Result<Vec<String>> {
    let mut remaining: std::collections::BTreeSet<String> = fix_plan
        .targets
        .iter()
        .map(|target| target.crate_name.clone())
        .collect();
    let mut packages = Vec::new();
    for package in &metadata.packages {
        for target in &package.targets {
            if target.kind.contains(&TargetKind::Lib)
                && remaining.remove(&target.name.replace('-', "_"))
            {
                packages.push(package.name.to_string());
            }
        }
    }
    if !remaining.is_empty() {
        bail!(
            "could not identify Cargo library package(s) for fixes in crate(s): {}",
            remaining.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(packages)
}

const WARNING: Style = AnsiColor::Yellow.on_default().bold();
const ERROR: Style = AnsiColor::Red.on_default().bold();
const LOCATION: Style = AnsiColor::BrightBlue.on_default().bold();
const SEPARATOR: Style = AnsiColor::Cyan.on_default();
const HELP: Style = AnsiColor::BrightCyan.on_default().bold();
const EMPHASIS: Style = Style::new().bold();

fn write_diagnostic(
    output: &mut String,
    finding: &Finding<'_>,
    binary: &str,
    workspace_root: &Path,
    level: LintLevel,
) -> std::fmt::Result {
    let (message, help, marker) = match (finding.kind, finding.definition.kind) {
        (FindingKind::DeadPublic, DefinitionKind::EnumVariant) => (
            format!(
                "`{}` is a public enum variant but is not reachable from binary `{binary}`",
                finding.definition.name
            ),
            "remove this variant",
            "public enum variant",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::EnumVariant) => {
            unreachable!("live enum variants do not have actionable visibility findings")
        }
        (FindingKind::DeadPublic, DefinitionKind::Reexport) => (
            format!(
                "public re-export `{}` has no target reachable from binary `{binary}`",
                finding.definition.name
            ),
            "consider restricting this re-export's visibility or removing it",
            "public re-export",
        ),
        (FindingKind::DeadPublic, DefinitionKind::Module) => (
            format!(
                "public module `{}` has no declaration reachable from binary `{binary}`",
                finding.definition.name
            ),
            "consider restricting this module's visibility or removing it",
            "public module",
        ),
        (FindingKind::DeadPublic, _) => (
            format!(
                "`{}` is public but is not reachable from binary `{binary}`",
                finding.definition.name
            ),
            "consider restricting this declaration's visibility or removing it",
            "public declaration",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::Reexport) => (
            format!(
                "public re-export `{}` is not required by any compiled cross-crate use; it can be `pub(crate)`",
                finding.definition.name
            ),
            "change this re-export to `pub(crate) use`",
            "public re-export",
        ),
        (FindingKind::UnnecessaryPublic, DefinitionKind::Module) => (
            format!(
                "public module `{}` is used only within `{}`; it can be `pub(crate)`",
                finding.definition.name, finding.definition.crate_name
            ),
            "change this module to `pub(crate) mod`",
            "public module",
        ),
        (FindingKind::UnnecessaryPublic, _) => (
            format!(
                "`{}` is public but all reachable uses are within `{}`; it can be `pub(crate)`",
                finding.definition.name, finding.definition.crate_name
            ),
            "change this declaration to `pub(crate)`",
            "public declaration",
        ),
    };
    write_diagnostic_header(output, finding.kind.code(), message, level)?;

    if let Some(span) = &finding.definition.span {
        let source_line = source_line(workspace_root, span);
        let width = write_annotated_location(
            output,
            &span.file,
            span.line,
            span.column,
            source_line.as_deref(),
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
        ConfigDiagnosticKind::UnfulfilledExpectation => (
            format!(
                "expected `{}` for `{item}`, but no finding was produced",
                entry.lint.code()
            ),
            "unfulfilled expectation",
            "remove this expectation or update its `lint` selector",
        ),
    };
    write_diagnostic_header(
        output,
        config_diagnostic_code(diagnostic.kind),
        message,
        level,
    )?;

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

fn config_diagnostic_code(kind: ConfigDiagnosticKind) -> &'static str {
    match kind {
        ConfigDiagnosticKind::UnknownItem => "hawk::unknown_item",
        ConfigDiagnosticKind::UnfulfilledExpectation => "hawk::unfulfilled_expectation",
    }
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

fn styled(content: impl Display, style: Style) -> impl Display {
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

fn source_line(workspace_root: &Path, span: &Span) -> Option<String> {
    let source_path = Path::new(&span.file);
    let source_path = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        workspace_root.join(source_path)
    };
    fs::read_to_string(source_path)
        .ok()?
        .lines()
        .nth(span.line.checked_sub(1)?)
        .map(str::to_owned)
}

fn marker_indent(source_line: &str, column: usize) -> String {
    source_line
        .chars()
        .take(column.saturating_sub(1))
        .map(|character| if character == '\t' { '\t' } else { ' ' })
        .collect()
}

fn validate_product(
    metadata: &cargo_metadata::Metadata,
    package: &str,
    binary: &str,
) -> Result<()> {
    let Some(package) = metadata
        .packages
        .iter()
        .find(|candidate| candidate.name.as_str() == package)
    else {
        bail!("package `{package}` is not in the selected workspace");
    };
    if !package
        .targets
        .iter()
        .any(|target| target.name == binary && target.kind.contains(&TargetKind::Bin))
    {
        bail!("package `{}` has no binary target `{binary}`", package.name);
    }
    Ok(())
}

fn default_target_dir(workspace_root: &Path, package: &str, binary: &str) -> PathBuf {
    let workspace = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    PathBuf::from("/private/tmp/codex-hawk-target").join(format!("{workspace}-{package}-{binary}"))
}

fn read_fragments(graph_dir: &Path) -> Result<Vec<Fragment>> {
    let mut fragments = Vec::new();
    for entry in fs::read_dir(graph_dir)
        .with_context(|| format!("read graph directory {}", graph_dir.display()))?
    {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let file =
                File::open(&path).with_context(|| format!("open fragment {}", path.display()))?;
            fragments.push(
                serde_json::from_reader(BufReader::new(file))
                    .with_context(|| format!("deserialize fragment {}", path.display()))?,
            );
        }
    }
    Ok(fragments)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::CommandFactory;

    use crate::graph::{Definition, DefinitionKind, Finding, FindingKind, Span};

    use super::{Args, LintLevel, LintLevels, write_diagnostic};

    #[test]
    fn diagnostic_rendering_includes_terminal_styles() {
        let definition = Definition {
            id: "internal_helper".into(),
            crate_name: "library".into(),
            name: "internal_helper".into(),
            kind: DefinitionKind::Function,
            span: Some(Span {
                file: "tests/fixtures/basic/library/src/lib.rs".into(),
                line: 5,
                column: 1,
            }),
            public_api: true,
        };
        let finding = Finding {
            kind: FindingKind::UnnecessaryPublic,
            definition: &definition,
        };
        let mut output = String::new();

        write_diagnostic(
            &mut output,
            &finding,
            "app",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            LintLevel::Warn,
        )
        .expect("render diagnostic");

        assert!(output.contains('\u{1b}'));
        let output = anstream::adapter::strip_str(&output);
        insta::assert_snapshot!(output, @r###"
        warning[hawk::unnecessary_public]: `internal_helper` is public but all reachable uses are within `library`; it can be `pub(crate)`
          --> tests/fixtures/basic/library/src/lib.rs:5:1
          |
        5 | pub fn internal_helper() {}
          | ^^^ public declaration
          = help: change this declaration to `pub(crate)`

        "###);
    }

    #[test]
    fn dead_enum_variant_diagnostic_suggests_a_valid_remediation() {
        let definition = Definition {
            id: "InternalState::Active".into(),
            crate_name: "library".into(),
            name: "InternalState::Active".into(),
            kind: DefinitionKind::EnumVariant,
            span: None,
            public_api: true,
        };
        let finding = Finding {
            kind: FindingKind::DeadPublic,
            definition: &definition,
        };
        let mut output = String::new();

        write_diagnostic(
            &mut output,
            &finding,
            "app",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            LintLevel::Warn,
        )
        .expect("render diagnostic");

        let output = anstream::adapter::strip_str(&output).to_string();
        insta::assert_snapshot!(output, @r###"
        warning[hawk::dead_public]: `InternalState::Active` is a public enum variant but is not reachable from binary `app`
          = note: declaration in crate `library`
          = help: remove this variant

        "###);
        assert!(!output.contains("pub(crate)"));
    }

    #[test]
    fn later_lint_levels_override_the_warnings_group() {
        let matches = Args::command()
            .try_get_matches_from([
                "cargo-hawk",
                "--package",
                "app",
                "--bin",
                "app",
                "-Dwarnings",
                "--warn",
                "hawk::unnecessary_public",
                "-A",
                "hawk::unknown_item",
            ])
            .expect("parse lint-level arguments");
        let levels = LintLevels::from_matches(&matches).expect("valid lint selectors");

        assert_eq!(levels.level("hawk::dead_public"), LintLevel::Deny);
        assert_eq!(levels.level("hawk::unnecessary_public"), LintLevel::Warn);
        assert_eq!(levels.level("hawk::unknown_item"), LintLevel::Allow);
        assert_eq!(
            levels.level("hawk::unfulfilled_expectation"),
            LintLevel::Deny
        );
    }
}
