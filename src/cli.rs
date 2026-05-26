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
use clap::{Parser, ValueEnum};

use crate::config::{Config, ConfigDiagnostic, ConfigDiagnosticKind};
use crate::graph::{Finding, FindingKind, Fragment, Span, analyze};

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

    /// Control when colored output is used.
    #[arg(long, value_enum, default_value_t, value_name = "WHEN")]
    color: TerminalColor,
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
    let args = match Args::try_parse_from(raw_args) {
        Ok(args) => args,
        Err(error) => {
            let exit_code = error.exit_code();
            error.print().context("print command-line help")?;
            return Ok(ExitCode::from(exit_code as u8));
        }
    };
    let metadata = MetadataCommand::new()
        .manifest_path(&args.manifest_path)
        .no_deps()
        .exec()
        .with_context(|| format!("read Cargo metadata from {}", args.manifest_path.display()))?;
    validate_product(&metadata, &args.package, &args.bin)?;

    let workspace_root = metadata.workspace_root.into_std_path_buf();
    let config = Config::load(&workspace_root, args.config.as_deref())?;
    let manifest_path = args
        .manifest_path
        .canonicalize()
        .with_context(|| format!("resolve manifest path for {}", args.manifest_path.display()))?;
    let crate_name = args.bin.replace('-', "_");
    let target_dir = args
        .target_dir
        .unwrap_or_else(|| default_target_dir(&workspace_root, &args.package, &args.bin));
    fs::create_dir_all(&target_dir)
        .with_context(|| format!("create target directory {}", target_dir.display()))?;

    let temporary_graph_dir;
    let graph_dir = match args.graph_dir {
        Some(path) => {
            fs::create_dir_all(&path)
                .with_context(|| format!("create graph directory {}", path.display()))?;
            tempfile::Builder::new()
                .prefix("run-")
                .tempdir_in(&path)
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
    let status = Command::new("cargo")
        .current_dir(&workspace_root)
        .arg("check")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("--package")
        .arg(&args.package)
        .arg("--bin")
        .arg(&args.bin)
        .arg("--all-features")
        .arg("--locked")
        .arg("--target-dir")
        .arg(&target_dir)
        .env("RUSTC_WORKSPACE_WRAPPER", executable)
        .env("HAWK_OUTPUT_DIR", &graph_dir)
        .env("HAWK_ROOT_CRATE", &crate_name)
        .env("HAWK_RUN_ID", run_id)
        .status()
        .context("run instrumented Cargo check")?;
    if !status.success() {
        bail!("instrumented Cargo check failed with {status}");
    }

    let fragments = read_fragments(&graph_dir)?;
    if !fragments.iter().any(|fragment| fragment.is_product_root) {
        bail!(
            "no instrumented fragment was emitted for binary `{}`; rerun with a fresh --target-dir",
            args.bin
        );
    }
    let excluded: HashSet<String> = args.excluded_crates.into_iter().collect();
    let findings = config.apply(&fragments, analyze(&fragments, &excluded));
    let mut diagnostics = String::new();
    for finding in &findings.findings {
        write_diagnostic(&mut diagnostics, finding, &args.bin, &workspace_root)
            .expect("formatting diagnostics into a string cannot fail");
    }
    for diagnostic in &findings.config_diagnostics {
        write_config_diagnostic(&mut diagnostics, diagnostic, &config, &workspace_root)
            .expect("formatting diagnostics into a string cannot fail");
    }
    writeln!(
        diagnostics,
        "hawk: {} finding(s) for `{} --bin {} --all-features` on the host target",
        findings.findings.len() + findings.config_diagnostics.len(),
        args.package,
        args.bin
    )
    .expect("formatting diagnostics into a string cannot fail");
    anstream::AutoStream::new(std::io::stdout(), args.color.into())
        .write_all(diagnostics.as_bytes())
        .context("write diagnostic output")?;
    Ok(ExitCode::SUCCESS)
}

const WARNING: Style = AnsiColor::Yellow.on_default().bold();
const LOCATION: Style = AnsiColor::BrightBlue.on_default().bold();
const SEPARATOR: Style = AnsiColor::Cyan.on_default();
const HELP: Style = AnsiColor::BrightCyan.on_default().bold();
const EMPHASIS: Style = Style::new().bold();

fn write_diagnostic(
    output: &mut String,
    finding: &Finding<'_>,
    binary: &str,
    workspace_root: &Path,
) -> std::fmt::Result {
    let (message, help) = match finding.kind {
        FindingKind::DeadPublic => (
            format!(
                "`{}` is public but is not reachable from binary `{binary}`",
                finding.definition.name
            ),
            "consider restricting this declaration's visibility or removing it",
        ),
        FindingKind::UnnecessaryPublic => (
            format!(
                "`{}` is public but all reachable uses are within `{}`; it can be `pub(crate)`",
                finding.definition.name, finding.definition.crate_name
            ),
            "change this declaration to `pub(crate)`",
        ),
    };
    write_warning_header(output, finding.kind.code(), message)?;

    if let Some(span) = &finding.definition.span {
        let source_line = source_line(workspace_root, span);
        let width = write_annotated_location(
            output,
            &span.file,
            span.line,
            span.column,
            source_line.as_deref(),
            "public declaration",
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
) -> std::fmt::Result {
    let entry = diagnostic.entry;
    let item = format!("{}::{}", entry.crate_name, entry.item);
    let (code, message, marker, help) = match diagnostic.kind {
        ConfigDiagnosticKind::UnknownItem => (
            "hawk::unknown_item",
            format!(
                "override for `{}` references unknown item `{item}`",
                entry.lint.code()
            ),
            "no matching item was found",
            "remove this override or update its `crate` and `item` selectors",
        ),
        ConfigDiagnosticKind::UnfulfilledExpectation => (
            "hawk::unfulfilled_expectation",
            format!(
                "expected `{}` for `{item}`, but no finding was produced",
                entry.lint.code()
            ),
            "unfulfilled expectation",
            "remove this expectation or update its `lint` selector",
        ),
    };
    write_warning_header(output, code, message)?;

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

fn write_warning_header(
    output: &mut String,
    code: &str,
    message: impl Display,
) -> std::fmt::Result {
    writeln!(
        output,
        "{}: {}",
        styled(format_args!("warning[{code}]"), WARNING),
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
                WARNING
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

    use crate::graph::{Definition, DefinitionKind, Finding, FindingKind, Span};

    use super::write_diagnostic;

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
}
