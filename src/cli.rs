use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
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
    Definition, DefinitionKind, Finding, FindingKind, FixPlan, FixTarget, Fragment, Span,
    analyze_with_options,
};

#[derive(Debug, Parser)]
#[command(
    name = "cargo hawk",
    about = "Find unnecessary public surface in a Cargo binary product",
    version
)]
struct Args {
    /// Path to the workspace manifest.
    #[arg(long, default_value = "Cargo.toml")]
    manifest_path: PathBuf,

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

    /// Path to Hawk configuration; defaults to hawk.toml in the workspace root.
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
                | "hawk::unnecessary_restricted_visibility"
                | "hawk::unnecessary_crate_visibility"
                | "hawk::unknown_item"
                | "hawk::ambiguous_item"
                | "hawk::unfulfilled_expectation"
        ) {
            return Ok(());
        }
        bail!(
            "unknown lint selector `{selector}`; expected `warnings` or a `hawk::...` diagnostic name"
        );
    }

    fn level(&self, code: &str) -> LintLevel {
        self.overrides.iter().fold(
            default_lint_level(code),
            |level, (selector, override_level)| {
                if selector == code || (selector == "warnings" && level.is_emitted()) {
                    *override_level
                } else {
                    level
                }
            },
        )
    }
}

fn default_lint_level(code: &str) -> LintLevel {
    if code == "hawk::unnecessary_crate_visibility" {
        LintLevel::Allow
    } else {
        LintLevel::default()
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

struct RustToolchain {
    rustc: OsString,
    sysroot: PathBuf,
}

impl RustToolchain {
    fn discover(workspace_root: &Path, manifest_path: &Path) -> Result<Self> {
        let rustc = cargo_rustc(workspace_root, manifest_path)?;
        let output = Command::new(&rustc)
            .current_dir(workspace_root)
            .arg("-vV")
            .output()
            .with_context(|| format!("query selected compiler `{}`", rustc.to_string_lossy()))?;
        if !output.status.success() {
            bail!(
                "query selected compiler `{}` failed with {}",
                rustc.to_string_lossy(),
                output.status
            );
        }
        let version =
            String::from_utf8(output.stdout).context("decode selected compiler version")?;
        let release = rustc_version_field(&version, "release")?;
        let commit_hash = rustc_version_field(&version, "commit-hash")?;
        let host = rustc_version_field(&version, "host")?;
        if release != env!("HAWK_RUSTC_RELEASE")
            || commit_hash != env!("HAWK_RUSTC_COMMIT_HASH")
            || host != env!("HAWK_RUSTC_HOST")
        {
            bail!(
                "Hawk was built for rustc {} ({}, {}), but the selected compiler is rustc {} ({}, {}); run Hawk with the matching toolchain, for example `cargo +{} hawk`",
                env!("HAWK_RUSTC_RELEASE"),
                env!("HAWK_RUSTC_COMMIT_HASH"),
                env!("HAWK_RUSTC_HOST"),
                release,
                commit_hash,
                host,
                env!("HAWK_RUSTC_RELEASE"),
            );
        }

        let output = Command::new(&rustc)
            .current_dir(workspace_root)
            .arg("--print=sysroot")
            .output()
            .with_context(|| {
                format!(
                    "query selected compiler `{}` sysroot",
                    rustc.to_string_lossy()
                )
            })?;
        if !output.status.success() {
            bail!(
                "query selected compiler `{}` sysroot failed with {}",
                rustc.to_string_lossy(),
                output.status
            );
        }
        let sysroot =
            String::from_utf8(output.stdout).context("decode selected compiler sysroot")?;
        let sysroot = PathBuf::from(sysroot.trim());
        let library_dir = driver_library_dir(&sysroot);
        if !library_dir.is_dir() {
            bail!(
                "selected compiler sysroot has no driver library directory at {}",
                library_dir.display()
            );
        }

        Ok(Self { rustc, sysroot })
    }

    fn rustc(&self) -> &OsStr {
        &self.rustc
    }

    fn configure_command(&self, command: &mut Command) -> Result<()> {
        let variable = driver_library_path_variable();
        let mut paths = vec![driver_library_dir(&self.sysroot)];
        if let Some(existing) = env::var_os(variable) {
            paths.extend(env::split_paths(&existing));
        }
        let value = env::join_paths(paths)
            .with_context(|| format!("construct {variable} for the Hawk compiler driver"))?;
        command.env(variable, value);
        Ok(())
    }
}

fn cargo_rustc(workspace_root: &Path, manifest_path: &Path) -> Result<OsString> {
    let probe_dir = tempfile::tempdir().context("create Cargo rustc probe directory")?;
    let output_path = probe_dir.path().join("rustc");
    let executable = env::current_exe().context("locate Hawk executable for Cargo rustc probe")?;
    let output = Command::new("cargo")
        .current_dir(workspace_root)
        .arg("check")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--workspace")
        .arg("--all-targets")
        .arg("--all-features")
        .arg("--locked")
        .arg("--quiet")
        .env("RUSTC_WORKSPACE_WRAPPER", executable)
        .env("HAWK_RUSTC_PROBE", &output_path)
        .output()
        .context("query Cargo's selected compiler")?;
    let rustc = fs::read_to_string(&output_path).with_context(|| {
        format!(
            "Cargo did not report its selected compiler: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    Ok(OsString::from(rustc.trim()))
}

pub fn run_rustc_probe(args: &[String]) -> Option<ExitCode> {
    let output_path = env::var_os("HAWK_RUSTC_PROBE")?;
    let rustc = args
        .get(1)
        .context("Cargo rustc probe omitted compiler path");
    match rustc.and_then(|rustc| {
        fs::write(&output_path, rustc).with_context(|| {
            format!(
                "write Cargo rustc probe result to {}",
                output_path.display()
            )
        })
    }) {
        Ok(()) => Some(ExitCode::FAILURE),
        Err(error) => {
            eprintln!("hawk: {error:#}");
            Some(ExitCode::FAILURE)
        }
    }
}

fn rustc_version_field<'a>(version: &'a str, field: &str) -> Result<&'a str> {
    version
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{field}: ")))
        .with_context(|| format!("selected compiler did not report {field}"))
}

#[cfg(windows)]
fn driver_library_dir(sysroot: &Path) -> PathBuf {
    sysroot.join("bin")
}

#[cfg(not(windows))]
fn driver_library_dir(sysroot: &Path) -> PathBuf {
    sysroot.join("lib")
}

#[cfg(target_os = "macos")]
fn driver_library_path_variable() -> &'static str {
    "DYLD_LIBRARY_PATH"
}

#[cfg(all(unix, not(target_os = "macos")))]
fn driver_library_path_variable() -> &'static str {
    "LD_LIBRARY_PATH"
}

#[cfg(windows)]
fn driver_library_path_variable() -> &'static str {
    "PATH"
}

fn driver_executable() -> Result<PathBuf> {
    let executable = env::current_exe().context("locate hawk executable")?;
    let driver = executable.with_file_name(format!("cargo-hawk-driver{}", env::consts::EXE_SUFFIX));
    if !driver.is_file() {
        bail!(
            "could not locate Hawk compiler driver at {}; install `cargo-hawk` and `cargo-hawk-driver` together",
            driver.display()
        );
    }
    Ok(driver)
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
    let candidate_crates = workspace_library_crates(&metadata);

    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let manifest_path = args
        .manifest_path
        .canonicalize()
        .with_context(|| format!("resolve manifest path for {}", args.manifest_path.display()))?;
    let toolchain = RustToolchain::discover(&workspace_root, &manifest_path)?;
    let config = Config::load(&workspace_root, args.config.as_deref())?;
    let analysis_target =
        AnalysisTarget::from_rustc(args.target.as_deref(), toolchain.rustc(), &workspace_root)?;
    let mut production_products: Vec<ProductionSelection<'_>> = Vec::new();
    for consumer in config.production_consumers(&analysis_target) {
        let config_path = config
            .path()
            .expect("configured production consumer has a configuration path");
        validate_product(&metadata, &consumer.package, &consumer.binary).with_context(|| {
            format!(
                "validate production consumer in {}:{}:{}: {}",
                config_path.display(),
                consumer.span.line,
                consumer.span.column,
                consumer.reason
            )
        })?;
        if !production_products
            .iter()
            .any(|product| product.package == consumer.package && product.binary == consumer.binary)
        {
            production_products.push(ProductionSelection {
                package: &consumer.package,
                binary: &consumer.binary,
            });
        }
    }
    if production_products.is_empty() {
        let config_path = config
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.join("hawk.toml"));
        bail!(
            "no applicable production binaries configured in {}; add a `[[production]]` entry",
            config_path.display()
        );
    }
    let target_dir = args
        .target_dir
        .clone()
        .unwrap_or_else(|| default_target_dir(&workspace_root));
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
    let production_graph_dir = graph_dir.join("production");
    let non_production_graph_dir = graph_dir.join("non-production");
    fs::create_dir_all(&production_graph_dir).with_context(|| {
        format!(
            "create production graph directory {}",
            production_graph_dir.display()
        )
    })?;
    fs::create_dir_all(&non_production_graph_dir).with_context(|| {
        format!(
            "create non-production graph directory {}",
            non_production_graph_dir.display()
        )
    })?;

    let driver = driver_executable()?;
    let cargo = InstrumentedCargo {
        args: &args,
        workspace_root: &workspace_root,
        manifest_path: &manifest_path,
        target_dir: &target_dir,
        driver: &driver,
        toolchain: &toolchain,
    };
    // Every production product uses the same compiler mode and feature set. Reuse one
    // dependency fingerprint across the product builds so Cargo can retain fragments from
    // shared dependencies instead of compiling them once per configured binary.
    let production_run_id = format!("{run_id}-production");
    for product in production_products.iter().copied() {
        cargo.run(
            "check",
            &production_run_id,
            &production_graph_dir,
            CargoSelection::Production(product),
        )?;
    }
    cargo.run(
        "check",
        &format!("{run_id}-non-production"),
        &non_production_graph_dir,
        CargoSelection::NonProduction,
    )?;
    cargo.run(
        "test",
        &format!("{run_id}-doctests"),
        &non_production_graph_dir,
        CargoSelection::Doctests,
    )?;

    let mut production_fragments = read_fragments(&production_graph_dir)?;
    let mut test_fragments = read_fragments(&non_production_graph_dir)?;
    if !production_fragments
        .iter()
        .any(|fragment| fragment.is_product_root)
    {
        bail!(
            "no instrumented fragment was emitted for a configured production binary; rerun with a fresh --target-dir"
        );
    }
    let excluded: HashSet<String> = args.excluded_crates.iter().cloned().collect();
    if args.fix {
        let mut fix_iteration = 0;
        loop {
            let initial_findings = config.apply(
                &analysis_target,
                &production_fragments,
                &test_fragments,
                analyze_with_options(
                    &production_fragments,
                    &test_fragments,
                    &candidate_crates,
                    &excluded,
                    config.preserve_uniform_field_visibility(),
                ),
            );
            let fixable_findings: Vec<_> = initial_findings
                .findings
                .iter()
                .filter(|finding| lint_levels.level(finding.kind.code()).is_emitted())
                // Restricting unreachable public surface to `pub(crate)` can
                // make rustc's ordinary `dead_code` lint start firing. Such
                // findings need coordinated removal rather than a
                // visibility-only fix.
                .filter(|finding| {
                    matches!(
                        finding.kind,
                        FindingKind::UnnecessaryRestrictedVisibility
                            | FindingKind::UnnecessaryCrateVisibility
                    ) || (fix_iteration == 0 && finding.kind == FindingKind::UnnecessaryPublic)
                })
                .collect();
            let production_fix_plan = fix_plan_for(
                fixable_findings
                    .iter()
                    .copied()
                    .filter(|finding| !finding.test_only && !finding.test_compiled_only),
                &production_fragments,
            );
            let test_fix_plan = fix_plan_for(
                fixable_findings
                    .iter()
                    .copied()
                    .filter(|finding| finding.test_only || finding.test_compiled_only),
                &test_fragments,
            );
            // A grouped `pub use` has one visibility span even when its aliases
            // are approved by different consumer modes. Project every approved
            // finding through each graph so fixes never name declarations
            // absent from that compilation mode.
            let production_emission_plan =
                fix_plan_for(fixable_findings.iter().copied(), &production_fragments);
            let test_emission_plan =
                fix_plan_for(fixable_findings.iter().copied(), &test_fragments);
            let mut applied_fixes = false;
            if !test_fix_plan.targets.is_empty() {
                let fix_packages = fix_packages(&metadata, &test_fix_plan)?;
                let fix_plan_path = graph_dir.join(format!("test-fix-plan-{fix_iteration}"));
                write_fix_plan(&fix_plan_path, &test_emission_plan)?;
                cargo.run(
                    "fix",
                    &format!("{run_id}-test-fix-{fix_iteration}"),
                    &non_production_graph_dir,
                    CargoSelection::FixNonProduction(FixRequest {
                        plan: &fix_plan_path,
                        packages: &fix_packages,
                        allow_dirty: fix_iteration > 0,
                    }),
                )?;
                applied_fixes = true;
            }
            if !production_fix_plan.targets.is_empty() {
                let fix_packages = fix_packages(&metadata, &production_fix_plan)?;
                let fix_plan_path = graph_dir.join(format!("production-fix-plan-{fix_iteration}"));
                write_fix_plan(&fix_plan_path, &production_emission_plan)?;
                cargo.run(
                    "fix",
                    &format!("{run_id}-production-fix-{fix_iteration}"),
                    &production_graph_dir,
                    CargoSelection::FixProduction(FixRequest {
                        plan: &fix_plan_path,
                        packages: &fix_packages,
                        allow_dirty: fix_iteration > 0 || applied_fixes,
                    }),
                )?;
                applied_fixes = true;
            }
            if !applied_fixes {
                break;
            }
            fix_iteration += 1;
            if fix_iteration > 3 {
                bail!("visibility fixes did not converge after {fix_iteration} iterations");
            }
            clear_fragments(&production_graph_dir)?;
            clear_fragments(&non_production_graph_dir)?;
            let production_run_id = format!("{run_id}-post-fix-{fix_iteration}-production");
            for product in production_products.iter().copied() {
                cargo.run(
                    "check",
                    &production_run_id,
                    &production_graph_dir,
                    CargoSelection::Production(product),
                )?;
            }
            cargo.run(
                "check",
                &format!("{run_id}-post-fix-{fix_iteration}-non-production"),
                &non_production_graph_dir,
                CargoSelection::NonProduction,
            )?;
            cargo.run(
                "test",
                &format!("{run_id}-post-fix-{fix_iteration}-doctests"),
                &non_production_graph_dir,
                CargoSelection::Doctests,
            )?;
            production_fragments = read_fragments(&production_graph_dir)?;
            test_fragments = read_fragments(&non_production_graph_dir)?;
        }
    }
    let findings = config.apply(
        &analysis_target,
        &production_fragments,
        &test_fragments,
        analyze_with_options(
            &production_fragments,
            &test_fragments,
            &candidate_crates,
            &excluded,
            config.preserve_uniform_field_visibility(),
        ),
    );
    let mut diagnostics = String::new();
    let mut diagnostic_count = 0;
    let mut has_denied_diagnostic = false;
    let production_description = if production_products.len() == 1 {
        format!("binary `{}`", production_products[0].binary)
    } else {
        "the configured production binaries".to_owned()
    };
    for finding in &findings.findings {
        let level = lint_levels.level(finding.kind.code());
        if level.is_emitted() {
            diagnostic_count += 1;
            has_denied_diagnostic |= level == LintLevel::Deny;
            write_diagnostic(
                &mut diagnostics,
                finding,
                &production_description,
                &workspace_root,
                level,
            )
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
    let production_summary = if production_products.len() == 1 {
        format!(
            "`{} --bin {} --all-features`",
            production_products[0].package, production_products[0].binary
        )
    } else {
        format!(
            "{} configured production binaries",
            production_products.len()
        )
    };
    writeln!(
        diagnostics,
        "hawk: {} finding(s) for {} and workspace non-production targets on {}",
        diagnostic_count, production_summary, compilation_target
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

pub fn write_error(raw_args: &[String], error: &anyhow::Error) -> Result<()> {
    let mut output = String::new();
    writeln!(
        output,
        "{}: {}",
        styled("error", ERROR),
        styled(format_args!("{error:#}"), EMPHASIS)
    )
    .expect("formatting an error into a string cannot fail");
    anstream::AutoStream::new(std::io::stderr(), terminal_color(raw_args).into())
        .write_all(output.as_bytes())
        .context("write error output")
}

fn terminal_color(raw_args: &[String]) -> TerminalColor {
    let mut raw_args = raw_args.to_owned();
    if raw_args.get(1).is_some_and(|argument| argument == "hawk") {
        raw_args.remove(1);
    }
    Args::try_parse_from(raw_args).map_or_else(|_| TerminalColor::default(), |args| args.color)
}

struct InstrumentedCargo<'a> {
    args: &'a Args,
    workspace_root: &'a Path,
    manifest_path: &'a Path,
    target_dir: &'a Path,
    driver: &'a Path,
    toolchain: &'a RustToolchain,
}

#[derive(Clone, Copy)]
struct ProductionSelection<'a> {
    package: &'a str,
    binary: &'a str,
}

#[derive(Clone, Copy)]
struct FixRequest<'a> {
    plan: &'a Path,
    packages: &'a [String],
    allow_dirty: bool,
}

#[derive(Clone, Copy)]
enum CargoSelection<'a> {
    Production(ProductionSelection<'a>),
    NonProduction,
    Doctests,
    FixProduction(FixRequest<'a>),
    FixNonProduction(FixRequest<'a>),
}

impl InstrumentedCargo<'_> {
    fn run(
        &self,
        subcommand: &str,
        run_id: &str,
        graph_dir: &Path,
        selection: CargoSelection<'_>,
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
        self.toolchain.configure_command(&mut command)?;
        match selection {
            CargoSelection::Production(product) => {
                command
                    .arg("--package")
                    .arg(product.package)
                    .arg("--bin")
                    .arg(product.binary);
            }
            CargoSelection::NonProduction => {
                command.arg("--workspace").arg("--all-targets");
            }
            CargoSelection::Doctests => {
                command.arg("--workspace").arg("--doc");
            }
            CargoSelection::FixProduction(fix_request) => {
                for package in fix_request.packages {
                    command.arg("--package").arg(package);
                }
                command.arg("--lib");
            }
            CargoSelection::FixNonProduction(fix_request) => {
                for package in fix_request.packages {
                    command.arg("--package").arg(package);
                }
                command.arg("--all-targets");
            }
        }
        if let Some(target) = &self.args.target {
            command.arg("--target").arg(target);
        }
        if matches!(
            selection,
            CargoSelection::FixProduction(_) | CargoSelection::FixNonProduction(_)
        ) {
            let allow_dirty = match selection {
                CargoSelection::FixProduction(request)
                | CargoSelection::FixNonProduction(request) => request.allow_dirty,
                _ => false,
            };
            if self.args.allow_dirty || allow_dirty {
                command.arg("--allow-dirty");
            }
            if self.args.allow_staged {
                command.arg("--allow-staged");
            }
            if self.args.allow_no_vcs {
                command.arg("--allow-no-vcs");
            }
        }
        let consumer_mode = match selection {
            CargoSelection::NonProduction
            | CargoSelection::Doctests
            | CargoSelection::FixNonProduction(_) => "non-production",
            CargoSelection::Production(_) | CargoSelection::FixProduction(_) => "production",
        };
        let root_crate = match selection {
            CargoSelection::Production(product) => product.binary.replace('-', "_"),
            CargoSelection::NonProduction
            | CargoSelection::Doctests
            | CargoSelection::FixProduction(_)
            | CargoSelection::FixNonProduction(_) => String::new(),
        };
        command
            .env("RUSTC_WORKSPACE_WRAPPER", self.driver)
            .env("HAWK_OUTPUT_DIR", graph_dir)
            .env("HAWK_ROOT_CRATE", root_crate)
            .env("HAWK_CONSUMER_MODE", consumer_mode)
            .env("HAWK_RUN_ID", run_id);
        if matches!(selection, CargoSelection::Doctests) {
            command
                .env("RUSTC_BOOTSTRAP", "1")
                .env(
                    "CARGO_ENCODED_RUSTDOCFLAGS",
                    doctest_rustdoc_flags(self.driver),
                )
                .env_remove("RUSTDOCFLAGS");
        }
        if let CargoSelection::FixProduction(fix_request)
        | CargoSelection::FixNonProduction(fix_request) = selection
        {
            command.env("HAWK_FIX_PLAN", fix_request.plan);
        }
        let status = if matches!(selection, CargoSelection::Doctests) {
            let output = command
                .output()
                .with_context(|| format!("run instrumented Cargo {subcommand}"))?;
            if !output.status.success() {
                std::io::stdout()
                    .write_all(&output.stdout)
                    .context("write failing doctest compilation stdout")?;
                std::io::stderr()
                    .write_all(&output.stderr)
                    .context("write failing doctest compilation stderr")?;
            }
            output.status
        } else {
            command
                .status()
                .with_context(|| format!("run instrumented Cargo {subcommand}"))?
        };
        if !status.success() {
            bail!("instrumented Cargo {subcommand} failed with {status}");
        }
        Ok(())
    }
}

fn doctest_rustdoc_flags(executable: &Path) -> OsString {
    let mut flags = if let Some(flags) = env::var_os("CARGO_ENCODED_RUSTDOCFLAGS") {
        flags
    } else {
        let mut encoded = OsString::new();
        if let Some(flags) = env::var_os("RUSTDOCFLAGS") {
            for flag in flags.to_string_lossy().split_whitespace() {
                push_encoded_rustdoc_flag(&mut encoded, OsStr::new(flag));
            }
        }
        encoded
    };
    // Hawk is pinned to compiler internals; rustdoc's builder wrapper is the
    // corresponding unstable hook needed to observe compiled doctest crates.
    for flag in ["-Zunstable-options", "--no-run", "--test-builder-wrapper"] {
        push_encoded_rustdoc_flag(&mut flags, OsStr::new(flag));
    }
    push_encoded_rustdoc_flag(&mut flags, executable.as_os_str());
    flags
}

fn push_encoded_rustdoc_flag(flags: &mut OsString, flag: &OsStr) {
    if !flags.is_empty() {
        flags.push("\u{1f}");
    }
    flags.push(flag);
}

fn write_fix_plan(path: &Path, fix_plan: &FixPlan) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer(file, fix_plan).with_context(|| format!("serialize {}", path.display()))
}

fn fix_plan_for<'a>(
    findings: impl Iterator<Item = &'a Finding<'a>>,
    fragments: &[Fragment],
) -> FixPlan {
    FixPlan {
        targets: findings
            .flat_map(|finding| {
                fragments
                    .iter()
                    .flat_map(|fragment| &fragment.definitions)
                    .filter(move |definition| same_declaration(finding.definition, definition))
                    .map(move |definition| FixTarget {
                        id: definition.id.clone(),
                        crate_name: definition.crate_name.clone(),
                        name: definition.name.clone(),
                        definition_kind: definition.kind,
                        span: definition.span.clone(),
                        kind: finding.kind,
                        replacement: finding
                            .replacement
                            .expect("fixable visibility finding has a replacement"),
                    })
            })
            .collect(),
    }
}

fn same_declaration(left: &Definition, right: &Definition) -> bool {
    left.crate_name == right.crate_name
        && left.name == right.name
        && left.kind == right.kind
        && match (&left.span, &right.span) {
            (Some(left), Some(right)) => {
                left.file == right.file && left.line == right.line && left.column == right.column
            }
            (None, None) => true,
            _ => false,
        }
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

fn workspace_library_crates(metadata: &cargo_metadata::Metadata) -> HashSet<String> {
    metadata
        .workspace_packages()
        .iter()
        .flat_map(|package| &package.targets)
        .filter(|target| target.kind.contains(&TargetKind::Lib))
        .map(|target| target.name.replace('-', "_"))
        .collect()
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
    production_description: &str,
    workspace_root: &Path,
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
        (FindingKind::UnnecessaryRestrictedVisibility, definition_kind, _) => {
            let replacement = finding
                .replacement
                .expect("restricted visibility finding has a replacement");
            assert_eq!(replacement, crate::graph::VisibilityReduction::Private);
            (
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
            )
        }
        (FindingKind::UnnecessaryCrateVisibility, definition_kind, _) => {
            let replacement = finding
                .replacement
                .expect("crate visibility finding has a replacement");
            assert_eq!(replacement, crate::graph::VisibilityReduction::Super);
            (
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
            )
        }
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
        ConfigDiagnosticKind::AmbiguousItem => "hawk::ambiguous_item",
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

fn default_target_dir(workspace_root: &Path) -> PathBuf {
    let workspace = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    env::temp_dir().join("cargo-hawk-target").join(workspace)
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

fn clear_fragments(graph_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(graph_dir)
        .with_context(|| format!("read graph directory {}", graph_dir.display()))?
    {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            fs::remove_file(&path)
                .with_context(|| format!("remove fragment {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use clap::CommandFactory;

    use crate::graph::{Definition, DefinitionKind, Finding, FindingKind, Span};

    use super::{Args, LintLevel, LintLevels, default_target_dir, write_diagnostic};

    #[test]
    fn default_target_dir_uses_platform_temp_directory() {
        let workspace_root = Path::new("/path/to/example-workspace");

        assert_eq!(
            default_target_dir(workspace_root),
            std::env::temp_dir()
                .join("cargo-hawk-target")
                .join("example-workspace")
        );
    }

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
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec![],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let finding = Finding {
            kind: FindingKind::UnnecessaryPublic,
            definition: &definition,
            replacement: Some(crate::graph::VisibilityReduction::Crate),
            test_only: false,
            test_compiled_only: false,
        };
        let mut output = String::new();

        write_diagnostic(
            &mut output,
            &finding,
            "binary `app`",
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
    fn crate_visibility_diagnostic_names_the_required_scope() {
        let definition = Definition {
            id: "scoped::run".into(),
            crate_name: "library".into(),
            name: "scoped::run".into(),
            kind: DefinitionKind::Function,
            span: Some(Span {
                file: "tests/fixtures/crate_visibility_fixes/library/src/lib.rs".into(),
                line: 7,
                column: 5,
            }),
            public_api: false,
            restricted_visible_api: true,
            crate_visible_api: true,
            visible_reexport_api: false,
            module_scope: vec!["scoped".into()],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let finding = Finding {
            kind: FindingKind::UnnecessaryCrateVisibility,
            definition: &definition,
            replacement: Some(crate::graph::VisibilityReduction::Super),
            test_only: false,
            test_compiled_only: false,
        };
        let mut output = String::new();

        write_diagnostic(
            &mut output,
            &finding,
            "binary `app`",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            LintLevel::Warn,
        )
        .expect("render diagnostic");

        let output = anstream::adapter::strip_str(&output);
        insta::assert_snapshot!(output, @r###"
        warning[hawk::unnecessary_crate_visibility]: `scoped::run` is visible throughout the crate but all compiled uses fit within the parent module; it can be `pub(super)`
          --> tests/fixtures/crate_visibility_fixes/library/src/lib.rs:7:5
          |
        7 |     pub(crate) fn run() {
          |     ^^^ crate-visible declaration
          = help: change this declaration to `pub(super)`

        "###);
    }

    #[test]
    fn restricted_visibility_diagnostic_removes_the_modifier() {
        let definition = Definition {
            id: "scoped::private_parent_visible_helper".into(),
            crate_name: "library".into(),
            name: "scoped::private_parent_visible_helper".into(),
            kind: DefinitionKind::Function,
            span: Some(Span {
                file: "tests/fixtures/crate_visibility_fixes/library/src/lib.rs".into(),
                line: 16,
                column: 5,
            }),
            public_api: false,
            restricted_visible_api: true,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec!["scoped".into()],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let finding = Finding {
            kind: FindingKind::UnnecessaryRestrictedVisibility,
            definition: &definition,
            replacement: Some(crate::graph::VisibilityReduction::Private),
            test_only: false,
            test_compiled_only: false,
        };
        let mut output = String::new();

        write_diagnostic(
            &mut output,
            &finding,
            "binary `app`",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            LintLevel::Warn,
        )
        .expect("render diagnostic");

        let output = anstream::adapter::strip_str(&output);
        insta::assert_snapshot!(output, @r###"
        warning[hawk::unnecessary_restricted_visibility]: `scoped::private_parent_visible_helper` has explicit restricted visibility but all compiled uses fit within the defining module; it can be private
          --> tests/fixtures/crate_visibility_fixes/library/src/lib.rs:16:5
           |
        16 |     pub(super) fn private_parent_visible_helper() {}
           |     ^^^ restricted-visibility declaration
           = help: remove this declaration's visibility modifier

        "###);
    }

    #[test]
    fn dead_enum_variant_diagnostic_accounts_for_unreachable_uses() {
        let definition = Definition {
            id: "InternalState::Active".into(),
            crate_name: "library".into(),
            name: "InternalState::Active".into(),
            kind: DefinitionKind::EnumVariant,
            span: None,
            public_api: true,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec![],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let finding = Finding {
            kind: FindingKind::DeadPublic,
            definition: &definition,
            replacement: None,
            test_only: false,
            test_compiled_only: false,
        };
        let mut output = String::new();

        write_diagnostic(
            &mut output,
            &finding,
            "binary `app`",
            Path::new(env!("CARGO_MANIFEST_DIR")),
            LintLevel::Warn,
        )
        .expect("render diagnostic");

        let output = anstream::adapter::strip_str(&output).to_string();
        insta::assert_snapshot!(output, @r###"
        warning[hawk::dead_public]: `InternalState::Active` is a public enum variant but is not reachable from binary `app`
          = note: declaration in crate `library`
          = help: consider removing this variant and its remaining uses

        "###);
        assert!(!output.contains("pub(crate)"));
    }

    #[test]
    fn later_lint_levels_override_the_warnings_group() {
        let matches = Args::command()
            .try_get_matches_from([
                "cargo-hawk",
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
        assert_eq!(
            levels.level("hawk::unnecessary_restricted_visibility"),
            LintLevel::Deny
        );
        assert_eq!(
            levels.level("hawk::unnecessary_crate_visibility"),
            LintLevel::Allow
        );
        assert_eq!(levels.level("hawk::unknown_item"), LintLevel::Allow);
        assert_eq!(
            levels.level("hawk::unfulfilled_expectation"),
            LintLevel::Deny
        );
    }

    #[test]
    fn enabled_opt_in_lint_is_affected_by_later_warnings_group() {
        let matches = Args::command()
            .try_get_matches_from([
                "cargo-hawk",
                "-W",
                "hawk::unnecessary_crate_visibility",
                "-Dwarnings",
            ])
            .expect("parse lint-level arguments");
        let levels = LintLevels::from_matches(&matches).expect("valid lint selectors");

        assert_eq!(
            levels.level("hawk::unnecessary_crate_visibility"),
            LintLevel::Deny
        );
    }
}
