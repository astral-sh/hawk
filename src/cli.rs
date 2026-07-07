use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::DefaultHasher};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::{Display, Formatter, Write as _};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anstyle::{AnsiColor, Style};
use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};

use crate::config::{
    AnalysisTarget, Config, ConfigDiagnostic, ConfigDiagnosticKind, FeatureProfile,
};
use crate::protocol;
use cargo_hawk_internal::graph::{
    CollectionOptions, Definition, DefinitionIdentity, DefinitionKind, Finding, FindingKind,
    FixPlan, FixTarget, Fragment, Span, analyze_with_options,
};

const RUSTC_PROBE_MARKER: &[u8] = b"cargo-hawk-rustc-probe-v1";

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
    overrides: Vec<(LintSelector, LintLevel)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LintSelector {
    Warnings,
    Diagnostic(DiagnosticKind),
}

impl LintSelector {
    fn parse(selector: &str) -> Result<Self> {
        if selector == "warnings" {
            return Ok(Self::Warnings);
        }
        DiagnosticKind::from_code(selector)
            .map(Self::Diagnostic)
            .with_context(|| {
                format!(
                    "unknown lint selector `{selector}`; expected `warnings` or a `hawk::...` diagnostic name"
                )
            })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiagnosticKind {
    Finding(FindingKind),
    Config(ConfigDiagnosticKind),
}

impl DiagnosticKind {
    fn from_code(code: &str) -> Option<Self> {
        FindingKind::from_code(code)
            .map(Self::Finding)
            .or_else(|| ConfigDiagnosticKind::from_code(code).map(Self::Config))
    }

    const fn default_level(self) -> LintLevel {
        if matches!(self, Self::Finding(FindingKind::UnnecessaryCrateVisibility)) {
            LintLevel::Allow
        } else {
            LintLevel::Warn
        }
    }
}

impl From<FindingKind> for DiagnosticKind {
    fn from(kind: FindingKind) -> Self {
        Self::Finding(kind)
    }
}

impl From<ConfigDiagnosticKind> for DiagnosticKind {
    fn from(kind: ConfigDiagnosticKind) -> Self {
        Self::Config(kind)
    }
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
                indexed_overrides.push((index, LintSelector::parse(selector)?, level));
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

    fn level(&self, diagnostic: impl Into<DiagnosticKind>) -> LintLevel {
        let diagnostic = diagnostic.into();
        self.overrides.iter().fold(
            diagnostic.default_level(),
            |level, (selector, override_level)| {
                if *selector == LintSelector::Diagnostic(diagnostic)
                    || (*selector == LintSelector::Warnings && level.is_emitted())
                {
                    *override_level
                } else {
                    level
                }
            },
        )
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

impl TerminalColor {
    fn cargo_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

struct RustToolchain {
    rustc: OsString,
    sysroot: PathBuf,
    host: String,
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

        Ok(Self {
            rustc,
            sysroot,
            host: host.to_owned(),
        })
    }

    fn rustc(&self) -> &OsStr {
        &self.rustc
    }

    fn host(&self) -> &str {
        &self.host
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
    // The probe wrapper exits unsuccessfully after reporting rustc. Keep Cargo's cached
    // result for that failed query out of the workspace target directory.
    let target_dir = probe_dir.path().join("target");
    // A random marker name inside the private probe directory acts as a
    // capability. Inherited environment variables cannot identify a live
    // probe merely by pointing at an existing directory.
    let mut marker = tempfile::Builder::new()
        .prefix("request-")
        .tempfile_in(probe_dir.path())
        .context("create Cargo rustc probe marker")?;
    marker
        .write_all(RUSTC_PROBE_MARKER)
        .context("write Cargo rustc probe marker")?;
    marker.flush().context("flush Cargo rustc probe marker")?;
    let (marker_file, marker_path) = marker.keep().context("preserve Cargo rustc probe marker")?;
    drop(marker_file);
    let probe_token = marker_path
        .file_name()
        .context("Cargo rustc probe marker has no file name")?;
    // The compiler driver cannot perform this probe because finding its dynamic
    // rustc libraries requires the selected compiler's sysroot.
    let executable = env::current_exe().context("locate Hawk executable for Cargo rustc probe")?;
    let mut command = Command::new("cargo");
    clear_protocol_environment(&mut command);
    command
        .current_dir(workspace_root)
        .arg("check")
        .arg("--manifest-path")
        .arg(manifest_path)
        .arg("--workspace")
        .arg("--all-targets")
        .arg("--all-features")
        .arg("--target-dir")
        .arg(target_dir)
        .arg("--locked")
        .arg("--quiet")
        .env("RUSTC_WORKSPACE_WRAPPER", executable)
        .env(protocol::RUSTC_PROBE_ENV, &output_path)
        .env(protocol::RUSTC_PROBE_TOKEN_ENV, probe_token);
    let output = command
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
    let output_path = PathBuf::from(env::var_os(protocol::RUSTC_PROBE_ENV)?);
    let token = PathBuf::from(env::var_os(protocol::RUSTC_PROBE_TOKEN_ENV)?);
    let probe_dir = output_path.parent()?;
    // Do not treat stale or forged inherited state as an internal invocation.
    // The token is a random marker file name, never an arbitrary path.
    if !output_path.is_absolute()
        || output_path.file_name() != Some(OsStr::new("rustc"))
        || token.file_name() != Some(token.as_os_str())
        || token.components().count() != 1
    {
        return None;
    }
    let marker_path = probe_dir.join(&token);
    let Ok(marker) = fs::read(&marker_path) else {
        return None;
    };
    if marker != RUSTC_PROBE_MARKER {
        return None;
    }
    let rustc = rustc_probe_compiler(args)?;
    let mut output = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)
    {
        Ok(output) => output,
        // Never truncate an existing path, even if every other probe signal
        // appears valid. Fall through to normal CLI parsing instead.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return None,
        Err(error) => {
            eprintln!(
                "hawk: could not create Cargo rustc probe result {}: {error}",
                output_path.display()
            );
            return Some(ExitCode::FAILURE);
        }
    };
    if let Err(error) = fs::remove_file(&marker_path) {
        drop(output);
        let _ = fs::remove_file(&output_path);
        eprintln!(
            "hawk: could not consume Cargo rustc probe marker {}: {error}",
            marker_path.display()
        );
        return Some(ExitCode::FAILURE);
    }
    let write_result = output
        .write_all(rustc.as_bytes())
        .and_then(|()| output.flush())
        .with_context(|| {
            format!(
                "write Cargo rustc probe result to {}",
                output_path.display()
            )
        });
    if let Err(error) = write_result {
        drop(output);
        let _ = fs::remove_file(&output_path);
        eprintln!("hawk: {error:#}");
    }
    Some(ExitCode::FAILURE)
}

fn rustc_probe_compiler(args: &[String]) -> Option<&str> {
    let rustc = args
        .get(1)
        .filter(|rustc| !rustc.is_empty() && !rustc.starts_with('-'))?;
    if !command_exists(rustc) {
        return None;
    }
    let rustc_arguments = args.get(2..)?;
    let version_query = rustc_arguments == ["-vV"];
    let crate_compilation = rustc_arguments
        .windows(2)
        .any(|arguments| arguments[0] == "--crate-name" && !arguments[1].starts_with('-'));
    (version_query || crate_compilation).then_some(rustc.as_str())
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_file() {
        return true;
    }
    if path.components().count() != 1 {
        return false;
    }
    env::var_os("PATH").is_some_and(|search_path| {
        env::split_paths(&search_path).any(|directory| {
            directory.join(command).is_file()
                || (!env::consts::EXE_SUFFIX.is_empty()
                    && directory
                        .join(format!("{command}{}", env::consts::EXE_SUFFIX))
                        .is_file())
        })
    })
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

fn validate_driver_protocol(driver: &Path, toolchain: &RustToolchain) -> Result<()> {
    let mut command = Command::new(driver);
    clear_protocol_environment(&mut command);
    command.arg(protocol::VERSION_ARGUMENT);
    toolchain.configure_command(&mut command)?;
    let output = command.output().with_context(|| {
        format!(
            "query Hawk compiler driver protocol version from {}",
            driver.display()
        )
    })?;
    if !output.status.success() {
        bail!(
            "query Hawk compiler driver protocol version from {} failed with {}: {}; install `cargo-hawk` and `cargo-hawk-driver` from the same release",
            driver.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let version =
        String::from_utf8(output.stdout).context("decode Hawk compiler driver protocol version")?;
    let version = version
        .trim()
        .parse::<u32>()
        .context("parse Hawk compiler driver protocol version")?;
    if version != protocol::VERSION {
        bail!(
            "Hawk frontend uses compiler driver protocol {}, but {} uses protocol {version}; install `cargo-hawk` and `cargo-hawk-driver` from the same release",
            protocol::VERSION,
            driver.display()
        );
    }
    Ok(())
}

fn clear_protocol_environment(command: &mut Command) {
    for variable in protocol::ENVIRONMENT_VARIABLES {
        command.env_remove(variable);
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
    let candidate_crates = workspace_library_crates(&metadata)?;

    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    let manifest_path = args
        .manifest_path
        .canonicalize()
        .with_context(|| format!("resolve manifest path for {}", args.manifest_path.display()))?;
    let config = Config::load(&workspace_root, args.config.as_deref())?;
    if args.fix && config.feature_profiles().len() > 1 {
        bail!(
            "--fix does not support multiple feature profiles; run analysis without --fix or configure a single `[[feature-profile]]`"
        );
    }
    let toolchain = RustToolchain::discover(&workspace_root, &manifest_path)?;
    let analysis_target = AnalysisTarget::from_rustc(
        args.target.as_deref(),
        toolchain.host(),
        toolchain.rustc(),
        &workspace_root,
    )?;
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
    let mut profile_graphs = Vec::new();
    for (index, feature_profile) in config.feature_profiles().iter().enumerate() {
        let profile_graph_dir = graph_dir
            .join("feature-profiles")
            .join(format!("{index}-{}", feature_profile.name()));
        let production_dir = profile_graph_dir.join("production");
        let non_production_dir = profile_graph_dir.join("non-production");
        fs::create_dir_all(&production_dir).with_context(|| {
            format!(
                "create production graph directory {}",
                production_dir.display()
            )
        })?;
        fs::create_dir_all(&non_production_dir).with_context(|| {
            format!(
                "create non-production graph directory {}",
                non_production_dir.display()
            )
        })?;
        profile_graphs.push(FeatureProfileGraph {
            feature_profile,
            run_id: format!("{run_id}-feature-profile-{index}"),
            production_dir,
            non_production_dir,
        });
    }

    let driver = driver_executable()?;
    validate_driver_protocol(&driver, &toolchain)?;
    let cargo = InstrumentedCargo {
        args: &args,
        workspace_root: &workspace_root,
        manifest_path: &manifest_path,
        target_dir: &target_dir,
        driver: &driver,
        toolchain: &toolchain,
        collection_options: CollectionOptions::new(config.preserve_uniform_field_visibility()),
    };
    let mut production_fragments = Vec::new();
    let mut test_fragments = Vec::new();
    for profile_graph in &profile_graphs {
        let (profile_production, profile_tests) =
            collect_profile_fragments(&cargo, profile_graph, &production_products, "initial")?;
        production_fragments.extend(profile_production);
        test_fragments.extend(profile_tests);
    }
    let excluded: HashSet<String> = args.excluded_crates.iter().cloned().collect();
    if args.fix {
        let profile_graph = profile_graphs
            .first()
            .expect("every feature profile has a graph directory");
        let mut fix_iteration = 0;
        let mut applied_fix_plans = HashSet::new();
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
                .filter(|finding| lint_levels.level(finding.kind).is_emitted())
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
            let production_definitions = definition_index(&production_fragments);
            let test_definitions = definition_index(&test_fragments);
            let production_fix_plan = fix_plan_for(
                fixable_findings
                    .iter()
                    .copied()
                    .filter(|finding| !finding.test_only && !finding.test_compiled_only),
                &production_definitions,
            );
            let test_fix_plan = fix_plan_for(
                fixable_findings
                    .iter()
                    .copied()
                    .filter(|finding| finding.test_only || finding.test_compiled_only),
                &test_definitions,
            );
            // A grouped `pub use` has one visibility span even when its aliases
            // are approved by different consumer modes. Project every approved
            // finding through each graph so fixes never name declarations
            // absent from that compilation mode.
            let production_emission_plan =
                fix_plan_for(fixable_findings.iter().copied(), &production_definitions);
            let test_emission_plan =
                fix_plan_for(fixable_findings.iter().copied(), &test_definitions);
            if production_fix_plan.targets.is_empty() && test_fix_plan.targets.is_empty() {
                break;
            }
            let fix_signature = fix_plan_signature(&production_fix_plan, &test_fix_plan)?;
            if !applied_fix_plans.insert(fix_signature) {
                bail!(
                    "visibility fixes made no progress after {fix_iteration} iteration(s); the same fix plan was produced after re-analysis"
                );
            }
            let mut applied_fixes = false;
            if !test_fix_plan.targets.is_empty() {
                let fix_packages = fix_packages(&metadata, &test_fix_plan)?;
                let fix_plan_path = graph_dir.join(format!("test-fix-plan-{fix_iteration}"));
                write_fix_plan(&fix_plan_path, &test_emission_plan)?;
                cargo.run(
                    &format!("{run_id}-test-fix-{fix_iteration}"),
                    &profile_graph.non_production_dir,
                    CargoInvocation::FixNonProduction {
                        plan: &fix_plan_path,
                        packages: &fix_packages,
                        allow_dirty: fix_iteration > 0,
                    },
                    profile_graph.feature_profile,
                )?;
                applied_fixes = true;
            }
            if !production_fix_plan.targets.is_empty() {
                let fix_packages = fix_packages(&metadata, &production_fix_plan)?;
                let fix_plan_path = graph_dir.join(format!("production-fix-plan-{fix_iteration}"));
                write_fix_plan(&fix_plan_path, &production_emission_plan)?;
                cargo.run(
                    &format!("{run_id}-production-fix-{fix_iteration}"),
                    &profile_graph.production_dir,
                    CargoInvocation::FixProduction {
                        plan: &fix_plan_path,
                        packages: &fix_packages,
                        allow_dirty: fix_iteration > 0 || applied_fixes,
                    },
                    profile_graph.feature_profile,
                )?;
                applied_fixes = true;
            }
            debug_assert!(
                applied_fixes,
                "a non-empty fix plan applies at least one mode"
            );
            fix_iteration += 1;
            clear_fragments(&profile_graph.production_dir)?;
            clear_fragments(&profile_graph.non_production_dir)?;
            (production_fragments, test_fragments) = collect_profile_fragments(
                &cargo,
                profile_graph,
                &production_products,
                &format!("post-fix-{fix_iteration}"),
            )?;
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
    let mut renderer = DiagnosticRenderer::new(&workspace_root);
    let mut diagnostic_count = 0;
    let mut has_denied_diagnostic = false;
    let production_description = if production_products.len() == 1 {
        format!("binary `{}`", production_products[0].binary)
    } else {
        "the configured production binaries".to_owned()
    };
    for finding in &findings.findings {
        let level = lint_levels.level(finding.kind);
        if level.is_emitted() {
            diagnostic_count += 1;
            has_denied_diagnostic |= level == LintLevel::Deny;
            renderer
                .write_diagnostic(finding, &production_description, level)
                .expect("formatting diagnostics into a string cannot fail");
        }
    }
    for diagnostic in &findings.config_diagnostics {
        let level = lint_levels.level(diagnostic.kind);
        if level.is_emitted() {
            diagnostic_count += 1;
            has_denied_diagnostic |= level == LintLevel::Deny;
            renderer
                .write_config_diagnostic(diagnostic, &config, level)
                .expect("formatting diagnostics into a string cannot fail");
        }
    }
    let compilation_target = args.target.as_deref().map_or_else(
        || "the host target".to_owned(),
        |target| format!("target `{target}`"),
    );
    let production_summary = production_summary(&production_products, config.feature_profiles());
    renderer
        .write_summary(diagnostic_count, &production_summary, &compilation_target)
        .expect("formatting diagnostics into a string cannot fail");
    let diagnostics = renderer.into_output();
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
    collection_options: CollectionOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProductionSelection<'a> {
    package: &'a str,
    binary: &'a str,
}

struct FeatureProfileGraph<'a> {
    feature_profile: &'a FeatureProfile,
    run_id: String,
    production_dir: PathBuf,
    non_production_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConsumerMode {
    Production,
    NonProduction,
}

impl ConsumerMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::NonProduction => "non-production",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum CargoInvocation<'a> {
    CheckProduction(ProductionSelection<'a>),
    CheckNonProduction,
    CheckDoctests,
    FixProduction {
        plan: &'a Path,
        packages: &'a [String],
        allow_dirty: bool,
    },
    FixNonProduction {
        plan: &'a Path,
        packages: &'a [String],
        allow_dirty: bool,
    },
}

struct CargoInvocationSpec<'a> {
    subcommand: &'static str,
    selection_arguments: Vec<OsString>,
    consumer_mode: ConsumerMode,
    root_crate: String,
    fix: Option<FixOptions<'a>>,
    doctests: bool,
}

#[derive(Clone, Copy)]
struct FixOptions<'a> {
    plan: &'a Path,
    allow_dirty: bool,
}

struct ConfiguredCargoCommand {
    command: Command,
    subcommand: &'static str,
    capture_output: bool,
}

struct CollectedFragments {
    production: Vec<Fragment>,
    non_production: Vec<Fragment>,
}

impl<'a> CargoInvocation<'a> {
    fn specification(self) -> CargoInvocationSpec<'a> {
        match self {
            Self::CheckProduction(product) => CargoInvocationSpec {
                subcommand: "check",
                selection_arguments: vec![
                    "--package".into(),
                    product.package.into(),
                    "--bin".into(),
                    product.binary.into(),
                ],
                consumer_mode: ConsumerMode::Production,
                root_crate: product.binary.replace('-', "_"),
                fix: None,
                doctests: false,
            },
            Self::CheckNonProduction => CargoInvocationSpec {
                subcommand: "check",
                selection_arguments: vec!["--workspace".into(), "--all-targets".into()],
                consumer_mode: ConsumerMode::NonProduction,
                root_crate: String::new(),
                fix: None,
                doctests: false,
            },
            Self::CheckDoctests => CargoInvocationSpec {
                subcommand: "test",
                selection_arguments: vec!["--workspace".into(), "--doc".into()],
                consumer_mode: ConsumerMode::NonProduction,
                root_crate: String::new(),
                fix: None,
                doctests: true,
            },
            Self::FixProduction {
                plan,
                packages,
                allow_dirty,
            } => CargoInvocationSpec {
                subcommand: "fix",
                selection_arguments: package_arguments(packages, "--lib"),
                consumer_mode: ConsumerMode::Production,
                root_crate: String::new(),
                fix: Some(FixOptions { plan, allow_dirty }),
                doctests: false,
            },
            Self::FixNonProduction {
                plan,
                packages,
                allow_dirty,
            } => CargoInvocationSpec {
                subcommand: "fix",
                selection_arguments: package_arguments(packages, "--all-targets"),
                consumer_mode: ConsumerMode::NonProduction,
                root_crate: String::new(),
                fix: Some(FixOptions { plan, allow_dirty }),
                doctests: false,
            },
        }
    }
}

fn package_arguments(packages: &[String], target: &str) -> Vec<OsString> {
    let mut arguments = Vec::with_capacity(packages.len() * 2 + 1);
    for package in packages {
        arguments.push("--package".into());
        arguments.push(package.as_str().into());
    }
    arguments.push(target.into());
    arguments
}

impl InstrumentedCargo<'_> {
    fn command(
        &self,
        run_id: &str,
        graph_dir: &Path,
        invocation: CargoInvocation<'_>,
        feature_profile: &FeatureProfile,
    ) -> Result<ConfiguredCargoCommand> {
        let CargoInvocationSpec {
            subcommand,
            selection_arguments,
            consumer_mode,
            root_crate,
            fix,
            doctests,
        } = invocation.specification();
        let mut command = Command::new("cargo");
        clear_protocol_environment(&mut command);
        command
            .current_dir(self.workspace_root)
            .arg(subcommand)
            .arg("--manifest-path")
            .arg(self.manifest_path)
            .arg("--locked")
            .arg("--target-dir")
            .arg(self.target_dir)
            .args(selection_arguments)
            .arg("--color")
            .arg(self.args.color.cargo_value());
        feature_profile.configure_cargo(&mut command);
        self.toolchain.configure_command(&mut command)?;
        if let Some(target) = &self.args.target {
            command.arg("--target").arg(target);
        }
        if let Some(fix) = fix {
            if self.args.allow_dirty || fix.allow_dirty {
                command.arg("--allow-dirty");
            }
            if self.args.allow_staged {
                command.arg("--allow-staged");
            }
            if self.args.allow_no_vcs {
                command.arg("--allow-no-vcs");
            }
            command.env(protocol::FIX_PLAN_ENV, fix.plan);
        }
        command
            .env("RUSTC_WORKSPACE_WRAPPER", self.driver)
            .env(protocol::VERSION_ENV, protocol::VERSION.to_string())
            .env(protocol::OUTPUT_DIR_ENV, graph_dir)
            .env(protocol::ROOT_CRATE_ENV, root_crate)
            .env(protocol::CONSUMER_MODE_ENV, consumer_mode.as_str())
            .env(protocol::RUN_ID_ENV, run_id)
            .env(
                protocol::COLLECTION_OPTIONS_ENV,
                self.collection_options.as_env_value(),
            );
        if doctests {
            command
                .arg("--quiet")
                .stdout(Stdio::null())
                .env("RUSTC_BOOTSTRAP", "1")
                .env(
                    "CARGO_ENCODED_RUSTDOCFLAGS",
                    doctest_rustdoc_flags(self.driver),
                )
                .env_remove("RUSTDOCFLAGS");
        }
        Ok(ConfiguredCargoCommand {
            command,
            subcommand,
            capture_output: doctests,
        })
    }

    fn run(
        &self,
        run_id: &str,
        graph_dir: &Path,
        invocation: CargoInvocation<'_>,
        feature_profile: &FeatureProfile,
    ) -> Result<()> {
        let ConfiguredCargoCommand {
            mut command,
            subcommand,
            capture_output,
        } = self.command(run_id, graph_dir, invocation, feature_profile)?;
        let status = if capture_output {
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

    fn collect_fragments(
        &self,
        run_id: &str,
        production_products: &[ProductionSelection<'_>],
        production_graph_dir: &Path,
        non_production_graph_dir: &Path,
        feature_profile: &FeatureProfile,
    ) -> Result<CollectedFragments> {
        // Every production product uses the same compiler mode and feature set. Reuse one
        // dependency fingerprint across the product builds so Cargo can retain fragments from
        // shared dependencies instead of compiling them once per configured binary.
        let production_run_id = format!("{run_id}-production");
        for product in production_products.iter().copied() {
            self.run(
                &production_run_id,
                production_graph_dir,
                CargoInvocation::CheckProduction(product),
                feature_profile,
            )?;
        }
        self.run(
            &format!("{run_id}-non-production"),
            non_production_graph_dir,
            CargoInvocation::CheckNonProduction,
            feature_profile,
        )?;
        self.run(
            &format!("{run_id}-doctests"),
            non_production_graph_dir,
            CargoInvocation::CheckDoctests,
            feature_profile,
        )?;

        Ok(CollectedFragments {
            production: read_fragments(production_graph_dir)?,
            non_production: read_fragments(non_production_graph_dir)?,
        })
    }
}

fn collect_profile_fragments(
    cargo: &InstrumentedCargo<'_>,
    profile_graph: &FeatureProfileGraph<'_>,
    production_products: &[ProductionSelection<'_>],
    phase: &str,
) -> Result<(Vec<Fragment>, Vec<Fragment>)> {
    let CollectedFragments {
        production: production_fragments,
        non_production: test_fragments,
    } = cargo.collect_fragments(
        &format!("{}-{phase}", profile_graph.run_id),
        production_products,
        &profile_graph.production_dir,
        &profile_graph.non_production_dir,
        profile_graph.feature_profile,
    )?;
    if !production_fragments
        .iter()
        .any(|fragment| fragment.is_product_root)
    {
        bail!(
            "no instrumented fragment was emitted for a configured production binary under feature profile `{}`; rerun with a fresh --target-dir",
            profile_graph.feature_profile.name()
        );
    }
    Ok((production_fragments, test_fragments))
}

fn production_summary(
    production_products: &[ProductionSelection<'_>],
    feature_profiles: &[FeatureProfile],
) -> String {
    if production_products.len() == 1 {
        let product = production_products[0];
        if feature_profiles.len() == 1 {
            let cargo_arguments = feature_profiles[0].cargo_arguments_description();
            let separator = if cargo_arguments.is_empty() { "" } else { " " };
            return format!(
                "`{} --bin {}{separator}{cargo_arguments}`",
                product.package, product.binary
            );
        }
        return format!(
            "`{} --bin {}` across {} feature profiles",
            product.package,
            product.binary,
            feature_profiles.len()
        );
    }

    let summary = format!(
        "{} configured production binaries",
        production_products.len()
    );
    if feature_profiles.len() == 1 {
        summary
    } else {
        format!(
            "{summary} across {} feature profiles",
            feature_profiles.len()
        )
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

type DefinitionIndex<'a> = HashMap<DefinitionIdentity<'a>, Vec<&'a Definition>>;

fn definition_index(fragments: &[Fragment]) -> DefinitionIndex<'_> {
    let mut definitions: DefinitionIndex<'_> = HashMap::new();
    for definition in fragments.iter().flat_map(|fragment| &fragment.definitions) {
        definitions
            .entry(DefinitionIdentity::new(
                &definition.crate_name,
                &definition.name,
                definition.kind,
                definition.span.as_ref(),
            ))
            .or_default()
            .push(definition);
    }
    definitions
}

fn fix_plan_signature(production: &FixPlan, non_production: &FixPlan) -> Result<Vec<Vec<u8>>> {
    let mut signature = Vec::with_capacity(production.targets.len() + non_production.targets.len());
    for (mode, plan) in [(b'p', production), (b'n', non_production)] {
        for target in &plan.targets {
            let encoded = serde_json::to_vec(&(
                mode,
                &target.crate_name,
                &target.name,
                target.definition_kind,
                &target.span,
                target.kind,
                target.replacement,
            ))
            .context("serialize fix plan signature")?;
            signature.push(encoded);
        }
    }
    signature.sort_unstable();
    Ok(signature)
}

fn fix_plan_for<'a>(
    findings: impl Iterator<Item = &'a Finding<'a>>,
    definitions: &DefinitionIndex<'_>,
) -> FixPlan {
    FixPlan {
        protocol_version: crate::protocol::ProtocolVersion,
        targets: findings
            .filter_map(|finding| {
                finding
                    .kind
                    .visibility_reduction()
                    .map(|replacement| (finding, replacement))
            })
            .flat_map(|(finding, replacement)| {
                definitions
                    .get(&DefinitionIdentity::new(
                        &finding.definition.crate_name,
                        &finding.definition.name,
                        finding.definition.kind,
                        finding.definition.span.as_ref(),
                    ))
                    .into_iter()
                    .flatten()
                    .map(move |definition| FixTarget {
                        id: definition.id.clone(),
                        crate_name: definition.crate_name.clone(),
                        name: definition.name.clone(),
                        definition_kind: definition.kind,
                        span: definition.span.clone(),
                        kind: finding.kind,
                        replacement,
                    })
            })
            .collect(),
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

fn workspace_library_crates(metadata: &cargo_metadata::Metadata) -> Result<HashSet<String>> {
    let mut packages_by_crate: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in metadata.workspace_packages() {
        for target in &package.targets {
            if target.kind.contains(&TargetKind::Lib) {
                packages_by_crate
                    .entry(target.name.replace('-', "_"))
                    .or_default()
                    .insert(package.name.to_string());
            }
        }
    }

    let conflicts = packages_by_crate
        .iter()
        .filter(|(_, packages)| packages.len() > 1)
        .map(|(crate_name, packages)| {
            format!(
                "`{crate_name}` ({})",
                packages
                    .iter()
                    .map(|package| format!("`{package}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>();
    if !conflicts.is_empty() {
        bail!(
            "workspace library crate names must be unique; conflicting names: {}. Hawk identifies graph definitions and fix targets by crate name; give each `[lib]` target a unique `name`",
            conflicts.join("; ")
        );
    }

    Ok(packages_by_crate.into_keys().collect())
}

const WARNING: Style = AnsiColor::Yellow.on_default().bold();
const ERROR: Style = AnsiColor::Red.on_default().bold();
const LOCATION: Style = AnsiColor::BrightBlue.on_default().bold();
const SEPARATOR: Style = AnsiColor::Cyan.on_default();
const HELP: Style = AnsiColor::BrightCyan.on_default().bold();
const EMPHASIS: Style = Style::new().bold();

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

struct DiagnosticRenderer<'a, L = SourceLoader> {
    workspace_root: &'a Path,
    sources: HashMap<PathBuf, Option<CachedSource>>,
    load_source: L,
    output: String,
}

impl<'a> DiagnosticRenderer<'a> {
    fn new(workspace_root: &'a Path) -> Self {
        Self::with_source_loader(workspace_root, load_source)
    }
}

impl<'a, L> DiagnosticRenderer<'a, L>
where
    L: FnMut(&Path) -> std::io::Result<String>,
{
    fn with_source_loader(workspace_root: &'a Path, load_source: L) -> Self {
        Self {
            workspace_root,
            sources: HashMap::new(),
            load_source,
            output: String::new(),
        }
    }

    fn write_diagnostic(
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

    fn write_config_diagnostic(
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

    fn write_summary(
        &mut self,
        diagnostic_count: usize,
        production_summary: &str,
        compilation_target: &str,
    ) -> std::fmt::Result {
        writeln!(
            self.output,
            "hawk: {diagnostic_count} finding(s) for {production_summary} and workspace non-production targets on {compilation_target}"
        )
    }

    fn source_line(&mut self, span: &Span) -> Option<&str> {
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

    fn into_output(self) -> String {
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

// Stay below the 255-byte/code-unit component limits of supported filesystems
// while leaving the ordinary workspace name readable.
const DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES: usize = 240;

fn default_target_dir(workspace_root: &Path) -> PathBuf {
    let workspace = workspace_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("workspace");
    let mut hasher = DefaultHasher::new();
    workspace_root.hash(&mut hasher);
    let suffix = format!("-{:016x}", hasher.finish());
    let max_workspace_bytes = DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES - suffix.len();
    let mut workspace_end = workspace.len().min(max_workspace_bytes);
    while !workspace.is_char_boundary(workspace_end) {
        workspace_end -= 1;
    }
    let workspace = format!("{}{suffix}", &workspace[..workspace_end]);
    env::temp_dir().join("cargo-hawk-target").join(workspace)
}

fn read_fragments(graph_dir: &Path) -> Result<Vec<Fragment>> {
    let mut paths = fs::read_dir(graph_dir)
        .with_context(|| format!("read graph directory {}", graph_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort_unstable();
    let mut fragments = Vec::new();
    for path in paths {
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
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    use clap::CommandFactory;

    use crate::config::ConfigDiagnosticKind;
    use cargo_hawk_internal::graph::{
        Definition, DefinitionKind, Finding, FindingKind, FixPlan, FixTarget, Span,
        VisibilityReduction,
    };

    use super::{
        Args, CargoInvocation, ConsumerMode, DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES,
        DiagnosticRenderer, LintLevel, LintLevels, ProductionSelection, default_target_dir,
        fix_plan_signature,
    };

    fn render_diagnostic(finding: &Finding<'_>) -> String {
        let mut renderer = DiagnosticRenderer::new(Path::new(env!("CARGO_MANIFEST_DIR")));
        renderer
            .write_diagnostic(finding, "binary `app`", LintLevel::Warn)
            .expect("render diagnostic");
        renderer.into_output()
    }

    fn assert_cargo_invocation(
        invocation: CargoInvocation<'_>,
        subcommand: &str,
        arguments: &[&str],
        consumer_mode: ConsumerMode,
        root_crate: &str,
        fix: Option<(&Path, bool)>,
        doctests: bool,
    ) {
        let specification = invocation.specification();
        assert_eq!(specification.subcommand, subcommand);
        assert_eq!(
            specification.selection_arguments,
            arguments
                .iter()
                .map(|argument| OsString::from(*argument))
                .collect::<Vec<_>>()
        );
        assert_eq!(specification.consumer_mode, consumer_mode);
        assert_eq!(specification.root_crate, root_crate);
        assert_eq!(
            specification.fix.map(|fix| (fix.plan, fix.allow_dirty)),
            fix
        );
        assert_eq!(specification.doctests, doctests);
    }

    #[test]
    fn default_target_dir_uses_platform_temp_directory() {
        let workspace_root = Path::new("/path/to/example-workspace");
        let target_dir = default_target_dir(workspace_root);

        assert_eq!(
            target_dir.parent(),
            Some(std::env::temp_dir().join("cargo-hawk-target").as_path())
        );
        assert!(
            target_dir
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("example-workspace-"))
        );
        assert_ne!(
            target_dir,
            default_target_dir(Path::new("/another/path/to/example-workspace"))
        );
    }

    #[test]
    fn default_target_dir_truncates_long_workspace_names() {
        let workspace = "a".repeat(245);
        let workspace_root = PathBuf::from("/path/to").join(&workspace);
        let other_workspace_root = PathBuf::from("/another/path/to").join(&workspace);
        let target_dir = default_target_dir(&workspace_root);
        let other_target_dir = default_target_dir(&other_workspace_root);
        let component = target_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 target directory component");
        let other_component = other_target_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 target directory component");

        assert_eq!(component.len(), DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES);
        let (workspace, suffix) = component
            .rsplit_once('-')
            .expect("target directory has a hash suffix");
        assert_eq!(
            workspace,
            "a".repeat(DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES - suffix.len() - 1)
        );
        assert_eq!(suffix.len(), 16);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(component, other_component);
    }

    #[test]
    fn default_target_dir_truncates_at_a_utf8_boundary() {
        let workspace_root = PathBuf::from("/path/to").join("é".repeat(123));
        let target_dir = default_target_dir(&workspace_root);
        let component = target_dir
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 target directory component");
        let (workspace, suffix) = component
            .rsplit_once('-')
            .expect("target directory has a hash suffix");

        assert!(component.len() <= DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES);
        assert!(!workspace.is_empty());
        assert!(workspace.chars().all(|character| character == 'é'));
        assert_eq!(suffix.len(), 16);
    }

    #[test]
    fn fix_plan_signatures_are_independent_of_target_order() {
        let target = |id: &str, name: &str| FixTarget {
            id: id.into(),
            crate_name: "library".into(),
            name: name.into(),
            definition_kind: DefinitionKind::Function,
            span: None,
            kind: FindingKind::UnnecessaryPublic,
            replacement: VisibilityReduction::Crate,
        };
        let forward = FixPlan {
            protocol_version: crate::protocol::ProtocolVersion,
            targets: vec![
                target("before-first", "first"),
                target("before-second", "second"),
            ],
        };
        let reverse = FixPlan {
            protocol_version: crate::protocol::ProtocolVersion,
            targets: vec![
                target("after-second", "second"),
                target("after-first", "first"),
            ],
        };
        let empty = FixPlan {
            protocol_version: crate::protocol::ProtocolVersion,
            targets: vec![],
        };

        assert_eq!(
            fix_plan_signature(&forward, &empty).expect("serialize forward fix plan"),
            fix_plan_signature(&reverse, &empty).expect("serialize reverse fix plan")
        );
        assert_ne!(
            fix_plan_signature(&forward, &empty).expect("serialize production fix plan"),
            fix_plan_signature(&empty, &forward).expect("serialize non-production fix plan")
        );
    }

    #[test]
    fn diagnostic_renderer_loads_each_source_once() {
        let load_count = Cell::new(0);
        let mut renderer =
            DiagnosticRenderer::with_source_loader(Path::new("/workspace"), |path| {
                assert_eq!(path, Path::new("/workspace/src/lib.rs"));
                load_count.set(load_count.get() + 1);
                Ok("first\r\nsecond\n".to_owned())
            });
        let mut span = Span {
            file: "src/lib.rs".into(),
            line: 1,
            column: 1,
        };

        assert_eq!(renderer.source_line(&span), Some("first"));
        span.line = 2;
        assert_eq!(renderer.source_line(&span), Some("second"));
        span.line = 3;
        assert_eq!(renderer.source_line(&span), None);
        assert_eq!(load_count.get(), 1);
    }

    #[test]
    fn cargo_invocations_encode_valid_subcommands_and_modes() {
        let packages = vec!["library".to_owned(), "support".to_owned()];
        let fix_plan = Path::new("fix-plan.json");

        assert_cargo_invocation(
            CargoInvocation::CheckProduction(ProductionSelection {
                package: "app-package",
                binary: "app-cli",
            }),
            "check",
            &["--package", "app-package", "--bin", "app-cli"],
            ConsumerMode::Production,
            "app_cli",
            None,
            false,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckNonProduction,
            "check",
            &["--workspace", "--all-targets"],
            ConsumerMode::NonProduction,
            "",
            None,
            false,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckDoctests,
            "test",
            &["--workspace", "--doc"],
            ConsumerMode::NonProduction,
            "",
            None,
            true,
        );
        assert_cargo_invocation(
            CargoInvocation::FixProduction {
                plan: fix_plan,
                packages: &packages,
                allow_dirty: false,
            },
            "fix",
            &["--package", "library", "--package", "support", "--lib"],
            ConsumerMode::Production,
            "",
            Some((fix_plan, false)),
            false,
        );
        assert_cargo_invocation(
            CargoInvocation::FixNonProduction {
                plan: fix_plan,
                packages: &packages,
                allow_dirty: true,
            },
            "fix",
            &[
                "--package",
                "library",
                "--package",
                "support",
                "--all-targets",
            ],
            ConsumerMode::NonProduction,
            "",
            Some((fix_plan, true)),
            false,
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
            test_only: false,
            test_compiled_only: false,
        };
        let output = render_diagnostic(&finding);
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
            test_only: false,
            test_compiled_only: false,
        };
        let output = render_diagnostic(&finding);
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
            test_only: false,
            test_compiled_only: false,
        };
        let output = render_diagnostic(&finding);
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
            test_only: false,
            test_compiled_only: false,
        };
        let output = render_diagnostic(&finding);
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

        assert_eq!(levels.level(FindingKind::DeadPublic), LintLevel::Deny);
        assert_eq!(
            levels.level(FindingKind::UnnecessaryPublic),
            LintLevel::Warn
        );
        assert_eq!(
            levels.level(FindingKind::UnnecessaryRestrictedVisibility),
            LintLevel::Deny
        );
        assert_eq!(
            levels.level(FindingKind::UnnecessaryCrateVisibility),
            LintLevel::Allow
        );
        assert_eq!(
            levels.level(ConfigDiagnosticKind::UnknownItem),
            LintLevel::Allow
        );
        assert_eq!(
            levels.level(ConfigDiagnosticKind::UnfulfilledExpectation),
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
            levels.level(FindingKind::UnnecessaryCrateVisibility),
            LintLevel::Deny
        );
    }
}
