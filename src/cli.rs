use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, hash_map::DefaultHasher};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::io::{BufReader, PipeReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::time::Duration;

use anstyle::Style;
use anyhow::{Context, Result, bail};
use cargo_metadata::{DependencyKind, MetadataCommand, Target, TargetKind};
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use tempfile::NamedTempFile;

use crate::config::{
    AnalysisTarget, Config, ConfigDiagnosticKind, FeatureProfile, ProductionProduct,
};
use crate::diagnostics::{DiagnosticRenderer, EMPHASIS, ERROR, WARNING, styled};
use crate::protocol;
use crate::toolchain::{
    RustToolchain, clear_protocol_environment, driver_executable, validate_driver_protocol,
};
use cargo_hawk_internal::graph::{
    CollectionOptions, Definition, DefinitionId, DefinitionIdentity, DefinitionKind, EdgeKind,
    Finding, FindingKind, FixPlan, FixTarget, Fragment, analyze_with_options,
};

#[derive(Debug, Parser)]
#[command(
    name = "cargo hawk",
    bin_name = "cargo hawk",
    about = "Find unnecessary public surface in a Cargo workspace product",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Check a Cargo workspace for unnecessary public surface.
    Check(CheckArgs),
}

#[derive(Debug, clap::Args)]
struct CheckArgs {
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

    /// Report only findings from the selected category.
    #[arg(long, value_enum, value_name = "KIND")]
    only: Option<OnlyFinding>,

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

    /// Select the diagnostic output format.
    #[arg(long, value_enum, default_value_t, value_name = "FORMAT")]
    output_format: OutputFormat,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum LintLevel {
    /// Do not emit a diagnostic.
    Allow,

    /// Report a diagnostic without failing.
    #[default]
    Warn,

    /// Report a diagnostic as an error and fail.
    Deny,
}

impl LintLevel {
    pub(crate) fn severity(self) -> &'static str {
        match self {
            Self::Allow => unreachable!("allowed diagnostics are not rendered"),
            Self::Warn => "warning",
            Self::Deny => "error",
        }
    }

    pub(crate) fn style(self) -> Style {
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
        let default_level = diagnostic.default_level();
        let mut level = default_level;
        let mut in_warnings_group = default_level.is_emitted();

        for (selector, override_level) in &self.overrides {
            if *selector == LintSelector::Diagnostic(diagnostic) {
                level = *override_level;
                in_warnings_group = default_level.is_emitted() || level.is_emitted();
            } else if *selector == LintSelector::Warnings && in_warnings_group {
                level = *override_level;
            }
        }

        level
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
    /// Emit human-readable diagnostics.
    #[default]
    Text,

    /// Emit a versioned JSON diagnostic report.
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OnlyFinding {
    DeadPublic,
}

impl OnlyFinding {
    const fn includes(self, kind: FindingKind) -> bool {
        matches!((self, kind), (Self::DeadPublic, FindingKind::DeadPublic))
    }
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

pub(crate) fn run(mut raw_args: Vec<String>) -> Result<ExitCode> {
    if raw_args.get(1).is_some_and(|argument| argument == "hawk") {
        raw_args.remove(1);
    }
    let matches = match Args::command().try_get_matches_from(&raw_args) {
        Ok(matches) => matches,
        Err(error) => {
            let exit_code = error.exit_code();
            error.print().context("print command-line help")?;
            return Ok(ExitCode::from(u8::try_from(exit_code).unwrap_or(1)));
        }
    };
    let check_matches = matches
        .subcommand_matches("check")
        .expect("required check subcommand has matches");
    let lint_levels = LintLevels::from_matches(check_matches)?;
    let Commands::Check(args) = Args::from_arg_matches(&matches)
        .context("read command-line arguments")?
        .command;
    debug_assert_eq!(
        lint_levels.overrides.len(),
        args.allow.len() + args.warn.len() + args.deny.len()
    );
    let metadata = MetadataCommand::new()
        .manifest_path(&args.manifest_path)
        .no_deps()
        .exec()
        .with_context(|| format!("read Cargo metadata from {}", args.manifest_path.display()))?;

    let workspace_root = metadata.workspace_root.clone().into_std_path_buf();
    #[cfg(unix)]
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("resolve workspace root {}", workspace_root.display()))?;
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
    let mut inferred_production_products = if config.path().is_none() {
        metadata
            .workspace_packages()
            .into_iter()
            .flat_map(|package| {
                package
                    .targets
                    .iter()
                    .filter(|target| target.kind.contains(&TargetKind::Bin))
                    .map(|target| {
                        (
                            package.name.as_str(),
                            ProductionProduct::Binary(target.name.clone()),
                        )
                    })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    inferred_production_products.sort_unstable_by(
        |(left_package, left_product), (right_package, right_product)| {
            left_package
                .cmp(right_package)
                .then_with(|| left_product.name().cmp(right_product.name()))
        },
    );
    let mut production_products: Vec<ProductionSelection<'_>> = Vec::new();
    for consumer in config.production_consumers(&analysis_target) {
        let config_path = config
            .path()
            .expect("configured production consumer has a configuration path");
        validate_product(&metadata, &consumer.package, &consumer.product).with_context(|| {
            format!(
                "validate production consumer in {}:{}:{}: {}",
                config_path.display(),
                consumer.span.line,
                consumer.span.column,
                consumer.reason
            )
        })?;
        if !production_products.iter().any(|product| {
            product.package == consumer.package && product.product == &consumer.product
        }) {
            production_products.push(ProductionSelection {
                package: &consumer.package,
                product: &consumer.product,
            });
        }
    }
    production_products.extend(
        inferred_production_products
            .iter()
            .map(|(package, product)| ProductionSelection { package, product }),
    );
    if production_products.is_empty() {
        if config.path().is_none() {
            bail!(
                "no binary targets found in this workspace; add a `hawk.toml` with a `[[production]]` library target"
            );
        }
        let config_path = config
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.join("hawk.toml"));
        bail!(
            "no applicable production targets configured in {}; add a `[[production]]` entry",
            config_path.display()
        );
    }
    let audited_library_crates = production_products
        .iter()
        .all(|product| matches!(product.product, ProductionProduct::Library(_)))
        .then(|| {
            production_products
                .iter()
                .map(|product| product.product.name().replace('-', "_"))
                .collect::<HashSet<_>>()
        });
    let workspace_crates = workspace_library_crates(&metadata, audited_library_crates.as_ref())?;
    validate_excluded_crates(&args.excluded_crates, &workspace_crates)?;
    let candidate_crates = audited_library_crates.unwrap_or(workspace_crates);
    let documentation_targets = documentation_targets(&metadata);
    let doctest_packages = config
        .doctest_packages()
        .map(|packages| {
            packages
                .iter()
                .map(|package| {
                    validate_package(&metadata, &package.package).with_context(|| {
                        let config_path = config
                            .path()
                            .expect("configured doctest package has a configuration path");
                        format!(
                            "validate doctest package in {}:{}:{}",
                            config_path.display(),
                            package.span.line,
                            package.span.column
                        )
                    })?;
                    Ok(package.package.clone())
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let target_dir = args.target_dir.as_ref().map_or_else(
        || Ok(default_target_dir(&workspace_root)),
        |target_dir| {
            std::path::absolute(target_dir)
                .with_context(|| format!("resolve target directory {}", target_dir.display()))
        },
    )?;
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
        let mut metadata_command = MetadataCommand::new();
        metadata_command
            .current_dir(&workspace_root)
            .manifest_path(&manifest_path)
            .other_options(vec![
                "--locked".to_owned(),
                "--filter-platform".to_owned(),
                args.target
                    .as_deref()
                    .unwrap_or(toolchain.host())
                    .to_owned(),
            ]);
        feature_profile.configure_metadata(&mut metadata_command);
        let resolved_metadata = metadata_command.exec().with_context(|| {
            format!(
                "resolve Cargo dependencies for feature profile `{}`",
                feature_profile.name()
            )
        })?;
        let profile_graph_dir = graph_dir
            .join("feature-profiles")
            .join(format!("{index}-{}", feature_profile.name()));
        let production_dir = profile_graph_dir.join("production");
        let non_production_dir = profile_graph_dir.join("non-production");
        let documentation_dir = profile_graph_dir.join("documentation");
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
        fs::create_dir_all(&documentation_dir).with_context(|| {
            format!(
                "create documentation graph directory {}",
                documentation_dir.display()
            )
        })?;
        profile_graphs.push(FeatureProfileGraph {
            feature_profile,
            run_id: format!("{run_id}-feature-profile-{index}"),
            production_dir,
            non_production_dir,
            documentation_dir,
            production_consumer_packages: production_workspace_packages(
                &resolved_metadata,
                &production_products,
                &analysis_target,
            )?,
        });
    }

    let driver = driver_executable()?;
    validate_driver_protocol(&driver, &toolchain)?;
    let workspace_library_sources = workspace_library_sources(&metadata);
    let workspace_library_source_paths = workspace_library_sources
        .values()
        .map(|source| source.path.clone())
        .collect();
    let cargo = InstrumentedCargo {
        args: &args,
        workspace_root: &workspace_root,
        manifest_path: &manifest_path,
        target_dir: &target_dir,
        driver: &driver,
        toolchain: &toolchain,
        collection_options: if args.output_format == OutputFormat::Json {
            CollectionOptions::new(config.preserve_uniform_field_visibility())
                .with_declaration_spans()
        } else {
            CollectionOptions::new(config.preserve_uniform_field_visibility())
        },
        doctest_packages: doctest_packages.as_deref(),
        documentation_targets: &documentation_targets,
        workspace_library_sources,
        workspace_library_source_paths,
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
                &candidate_crates,
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
                .filter(|finding| args.only.is_none_or(|only| only.includes(finding.kind)))
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
            clear_fragments(&profile_graph.documentation_dir)?;
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
        &candidate_crates,
        analyze_with_options(
            &production_fragments,
            &test_fragments,
            &candidate_crates,
            &excluded,
            config.preserve_uniform_field_visibility(),
        ),
    );
    let mut renderer = DiagnosticRenderer::new(&workspace_root);
    let mut json_diagnostics = Vec::new();
    let mut diagnostic_count = 0;
    let mut diagnostic_counts = BTreeMap::<&str, BTreeMap<&str, usize>>::new();
    let emitted_finding_ids: HashSet<_> = findings
        .findings
        .iter()
        .filter(|finding| args.only.is_none_or(|only| only.includes(finding.kind)))
        .filter(|finding| lint_levels.level(finding.kind).is_emitted())
        .map(|finding| finding.definition.id)
        .collect();
    let definition_packages =
        definition_packages(&production_fragments, &test_fragments, &emitted_finding_ids);
    let mut has_denied_diagnostic = false;
    let production_description = if production_products.len() == 1 {
        let product = production_products[0].product;
        format!("{} `{}`", product.kind().as_str(), product.name())
    } else if production_products
        .iter()
        .all(|product| matches!(product.product, ProductionProduct::Binary(_)))
    {
        "the configured production binaries".to_owned()
    } else {
        "the configured production targets".to_owned()
    };
    for finding in &findings.findings {
        if args.only.is_some_and(|only| !only.includes(finding.kind)) {
            continue;
        }
        let level = lint_levels.level(finding.kind);
        if level.is_emitted() {
            diagnostic_count += 1;
            let package = definition_packages.get(&finding.definition.id).copied();
            if args.output_format == OutputFormat::Text {
                *diagnostic_counts
                    .entry(finding.kind.code())
                    .or_default()
                    .entry(package.unwrap_or(&finding.definition.crate_name))
                    .or_default() += 1;
            }
            has_denied_diagnostic |= level == LintLevel::Deny;
            match args.output_format {
                OutputFormat::Text => renderer
                    .write_diagnostic(finding, &production_description, level)
                    .expect("formatting diagnostics into a string cannot fail"),
                OutputFormat::Json => json_diagnostics.push(json_finding(finding, level, package)),
            }
        }
    }
    for diagnostic in &findings.config_diagnostics {
        let level = lint_levels.level(diagnostic.kind);
        if level.is_emitted() {
            diagnostic_count += 1;
            if args.output_format == OutputFormat::Text {
                *diagnostic_counts
                    .entry(diagnostic.kind.code())
                    .or_default()
                    .entry("configuration")
                    .or_default() += 1;
            }
            has_denied_diagnostic |= level == LintLevel::Deny;
            match args.output_format {
                OutputFormat::Text => renderer
                    .write_config_diagnostic(diagnostic, &config, level)
                    .expect("formatting diagnostics into a string cannot fail"),
                OutputFormat::Json => json_diagnostics.push(json_config_diagnostic(
                    diagnostic,
                    &config,
                    &workspace_root,
                    level,
                )),
            }
        }
    }
    let compilation_target = args.target.as_deref().map_or_else(
        || "the host target".to_owned(),
        |target| format!("target `{target}`"),
    );
    let production_summary = production_summary(&production_products, config.feature_profiles());
    match args.output_format {
        OutputFormat::Text => {
            renderer
                .write_summary(
                    diagnostic_count,
                    &diagnostic_counts,
                    &production_summary,
                    &compilation_target,
                )
                .expect("formatting diagnostics into a string cannot fail");
            let diagnostics = renderer.into_output();
            anstream::AutoStream::new(std::io::stdout(), args.color.into())
                .write_all(diagnostics.as_bytes())
                .context("write diagnostic output")?;
        }
        OutputFormat::Json => {
            let output = serde_json::json!({
                "schema_version": 4,
                "summary": {
                    "diagnostic_count": diagnostic_count,
                    "target": args.target.as_deref().unwrap_or(toolchain.host()),
                    "production": production_products
                        .iter()
                        .map(|product| match product.product {
                            ProductionProduct::Binary(binary) => serde_json::json!({
                                "package": product.package,
                                "binary": binary,
                            }),
                            ProductionProduct::Library(library) => serde_json::json!({
                                "package": product.package,
                                "library": library,
                            }),
                        })
                        .collect::<Vec<_>>(),
                    "feature_profiles": config
                        .feature_profiles()
                        .iter()
                        .map(FeatureProfile::name)
                        .collect::<Vec<_>>(),
                    "includes_non_production_targets": true,
                },
                "diagnostics": json_diagnostics,
            });
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            serde_json::to_writer_pretty(&mut stdout, &output)
                .context("serialize JSON diagnostic output")?;
            writeln!(stdout).context("write JSON diagnostic output")?;
        }
    }
    Ok(if has_denied_diagnostic {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn json_finding(
    finding: &Finding<'_>,
    level: LintLevel,
    package: Option<&str>,
) -> serde_json::Value {
    let definition = finding.definition;
    serde_json::json!({
        "category": "finding",
        "code": finding.kind.code(),
        "severity": level.severity(),
        "kind": json_finding_kind(finding.kind),
        "identity": {
            "id": stable_finding_id(definition, package),
            "compiler_id": definition.id.to_string(),
            "package": package,
            "crate": definition.crate_name,
            "item": definition.name,
            "kind": json_definition_kind(definition.kind),
            "parent": definition.name.rsplit_once("::").map(|(parent, _)| parent),
            "module_scope": definition.module_scope,
        },
        "location": definition.declaration_span.as_ref().map_or_else(
            || definition.span.as_ref().map(|span| serde_json::json!({
                "file": span.file,
                "line": span.line,
                "column": span.column,
            })),
            |span| Some(serde_json::json!({
                "file": span.file,
                "byte_start": span.byte_start,
                "byte_end": span.byte_end,
                "line": span.start_line,
                "column": span.start_column,
                "end_line": span.end_line,
                "end_column": span.end_column,
            })),
        ),
        "expansion": definition.expansion_span,
        "test_only": finding.test_only,
        "test_compiled_only": finding.test_compiled_only,
    })
}

/// Builds a target-independent finding identity from length-prefixed semantic and source components.
fn stable_finding_id(definition: &Definition, package: Option<&str>) -> String {
    let source = definition
        .span
        .as_ref()
        .map(|span| ("source", span.file.as_str(), span.line, span.column))
        .or_else(|| {
            definition.declaration_span.as_ref().map(|span| {
                (
                    "declaration",
                    span.file.as_str(),
                    span.start_line,
                    span.start_column,
                )
            })
        })
        .or_else(|| {
            definition.expansion_span.as_ref().map(|span| {
                (
                    "expansion-callsite",
                    span.callsite.file.as_str(),
                    span.callsite.line,
                    span.callsite.column,
                )
            })
        })
        .unwrap_or(("none", "", 0, 0));
    let mut id = String::from("v1");
    for component in [
        package.unwrap_or(""),
        definition.crate_name.as_str(),
        definition.name.as_str(),
        json_definition_kind(definition.kind),
        source.0,
        source.1,
    ] {
        write!(id, "|{}:{component}", component.len())
            .expect("formatting a stable diagnostic ID cannot fail");
    }
    write!(id, "|{}|{}", source.2, source.3)
        .expect("formatting a stable diagnostic ID cannot fail");
    id
}

fn json_config_diagnostic(
    diagnostic: &crate::config::ConfigDiagnostic<'_>,
    config: &Config,
    workspace_root: &Path,
    level: LintLevel,
) -> serde_json::Value {
    let entry = diagnostic.entry;
    let path = config.path().expect("diagnostic requires a loaded config");
    let path = path.strip_prefix(workspace_root).unwrap_or(path);
    serde_json::json!({
        "category": "configuration",
        "code": diagnostic.kind.code(),
        "severity": level.severity(),
        "lint": entry.lint.code(),
        "identity": {
            "crate": entry.crate_name,
            "item": entry.item,
            "kind": entry.definition_kind.map(json_definition_kind),
        },
        "location": {
            "file": path,
            "line": entry.span.line,
            "column": entry.span.column,
        },
        "reason": entry.reason,
    })
}

const fn json_finding_kind(kind: FindingKind) -> &'static str {
    match kind {
        FindingKind::DeadPublic => "dead_public",
        FindingKind::UnnecessaryPublic => "unnecessary_public",
        FindingKind::UnnecessaryRestrictedVisibility => "unnecessary_restricted_visibility",
        FindingKind::UnnecessaryCrateVisibility => "unnecessary_crate_visibility",
    }
}

const fn json_definition_kind(kind: DefinitionKind) -> &'static str {
    match kind {
        DefinitionKind::Function => "function",
        DefinitionKind::InherentMethod => "inherent_method",
        DefinitionKind::InherentAssociatedConstant => "inherent_associated_constant",
        DefinitionKind::Trait => "trait",
        DefinitionKind::Struct => "struct",
        DefinitionKind::Enum => "enum",
        DefinitionKind::Union => "union",
        DefinitionKind::TypeAlias => "type_alias",
        DefinitionKind::Constant => "constant",
        DefinitionKind::Static => "static",
        DefinitionKind::Field => "field",
        DefinitionKind::EnumVariant => "enum_variant",
        DefinitionKind::Reexport => "reexport",
        DefinitionKind::Module => "module",
        DefinitionKind::Other => "other",
    }
}

pub(crate) fn write_error(raw_args: &[String], error: &anyhow::Error) -> Result<()> {
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
    Args::try_parse_from(raw_args).map_or_else(
        |_| TerminalColor::default(),
        |args| match args.command {
            Commands::Check(args) => args.color,
        },
    )
}

struct InstrumentedCargo<'a> {
    args: &'a CheckArgs,
    workspace_root: &'a Path,
    manifest_path: &'a Path,
    target_dir: &'a Path,
    driver: &'a Path,
    toolchain: &'a RustToolchain,
    collection_options: CollectionOptions,
    doctest_packages: Option<&'a [String]>,
    documentation_targets: &'a [DocumentationTarget],
    workspace_library_sources: HashMap<String, WorkspaceLibrarySource>,
    workspace_library_source_paths: HashSet<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkspaceLibrarySource {
    crate_name: String,
    path: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProductionSelection<'a> {
    package: &'a str,
    product: &'a ProductionProduct,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentationTargetKind {
    Library,
    Binary,
    ProcMacro,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocumentationTarget {
    package_name: String,
    crate_name: String,
    kind: DocumentationTargetKind,
}

struct FeatureProfileGraph<'a> {
    feature_profile: &'a FeatureProfile,
    run_id: String,
    production_dir: PathBuf,
    non_production_dir: PathBuf,
    documentation_dir: PathBuf,
    production_consumer_packages: HashSet<String>,
}

#[derive(Clone, Copy, Debug)]
enum CargoInvocation<'a> {
    CheckProduction(ProductionSelection<'a>),
    CheckNonProduction,
    CheckDoctests {
        packages: Option<&'a [String]>,
    },
    CheckDocumentation,
    CheckProcMacroDocumentation(&'a DocumentationTarget),
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
    consumer_mode: protocol::ConsumerMode,
    root_crate: String,
    fix: Option<FixOptions<'a>>,
    doctests: bool,
    documentation: DocumentationMode<'a>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentationMode<'a> {
    None,
    Targets,
    ProcMacro(&'a DocumentationTarget),
}

impl DocumentationMode<'_> {
    fn env_value(self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Targets => Some("targets".to_owned()),
            Self::ProcMacro(target) => Some(format!(
                "proc-macro:{}:{}",
                target.package_name, target.crate_name
            )),
        }
    }
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
    cargo_output: Option<CargoOutputCapture>,
}

/// Captures Cargo's combined output without allowing inherited writers to keep analysis alive.
struct CargoOutputCapture {
    output: NamedTempFile,
    reader: PipeReader,
}

impl CargoOutputCapture {
    fn new(command: &mut Command) -> Result<Self> {
        let output = NamedTempFile::new().context("create temporary Cargo output file")?;
        let (reader, writer) = std::io::pipe().context("create Cargo output pipe")?;
        command.stdout(
            writer
                .try_clone()
                .context("duplicate Cargo output pipe for stdout")?,
        );
        command.stderr(writer);
        Ok(Self { output, reader })
    }

    /// Drains output while Cargo runs, then closes the reader before returning the captured bytes.
    fn run(
        mut self,
        mut command: Command,
        subcommand: &str,
    ) -> Result<(ExitStatus, NamedTempFile)> {
        let mut child = command
            .spawn()
            .with_context(|| format!("run instrumented Cargo {subcommand}"))?;
        drop(command);

        let mut buffer = [0_u8; 16 * 1024];
        let status = loop {
            let status = child
                .try_wait()
                .with_context(|| format!("poll instrumented Cargo {subcommand}"))?;
            let mut pending =
                cargo_output_pending(&self.reader).context("inspect pending Cargo output")?;
            while pending != 0 {
                let requested = pending.min(buffer.len());
                let read = self
                    .reader
                    .read(&mut buffer[..requested])
                    .with_context(|| format!("read captured Cargo {subcommand} output"))?;
                if read == 0 {
                    bail!("Cargo output pipe closed while draining pending output");
                }
                self.output
                    .as_file_mut()
                    .write_all(&buffer[..read])
                    .context("write temporary Cargo output file")?;
                pending -= read;
            }
            if let Some(status) = status {
                break status;
            }
            std::thread::sleep(Duration::from_millis(1));
        };

        drop(self.reader);
        self.output
            .as_file_mut()
            .flush()
            .context("flush temporary Cargo output file")?;
        Ok((status, self.output))
    }
}

/// Returns the bytes immediately readable from Cargo's pipe without waiting for inherited writers.
#[cfg(unix)]
fn cargo_output_pending(reader: &PipeReader) -> std::io::Result<usize> {
    usize::try_from(rustix::io::ioctl_fionread(reader)?)
        .map_err(|_| std::io::Error::other("pending Cargo output exceeds usize"))
}

/// Returns the bytes immediately readable from Cargo's pipe without waiting for inherited writers.
#[cfg(windows)]
#[expect(unsafe_code, reason = "Windows pipe inspection requires PeekNamedPipe")]
fn cargo_output_pending(reader: &PipeReader) -> std::io::Result<usize> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let mut pending = 0_u32;
    // SAFETY: the pipe handle is valid for the duration of this call, and all
    // output pointers are either null or point to initialized local storage.
    let result = unsafe {
        PeekNamedPipe(
            reader.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut pending,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        let code = error
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok());
        if matches!(code, Some(ERROR_BROKEN_PIPE | ERROR_NO_DATA)) {
            return Ok(0);
        }
        return Err(error);
    }
    usize::try_from(pending)
        .map_err(|_| std::io::Error::other("pending Cargo output exceeds usize"))
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
                selection_arguments: [
                    "--package".into(),
                    product.package.into(),
                    product.product.cargo_flag().into(),
                ]
                .into_iter()
                .chain(
                    matches!(product.product, ProductionProduct::Binary(_))
                        .then(|| product.product.name().into()),
                )
                .collect(),
                consumer_mode: protocol::ConsumerMode::Production,
                root_crate: product.product.name().replace('-', "_"),
                fix: None,
                doctests: false,
                documentation: DocumentationMode::None,
            },
            Self::CheckNonProduction => CargoInvocationSpec {
                subcommand: "check",
                selection_arguments: vec!["--workspace".into(), "--all-targets".into()],
                consumer_mode: protocol::ConsumerMode::NonProduction,
                root_crate: String::new(),
                fix: None,
                doctests: false,
                documentation: DocumentationMode::None,
            },
            Self::CheckDoctests { packages } => CargoInvocationSpec {
                subcommand: "test",
                selection_arguments: packages.map_or_else(
                    || vec!["--workspace".into(), "--doc".into()],
                    |packages| package_arguments(packages, "--doc"),
                ),
                consumer_mode: protocol::ConsumerMode::NonProduction,
                root_crate: String::new(),
                fix: None,
                doctests: true,
                documentation: DocumentationMode::None,
            },
            Self::CheckDocumentation => CargoInvocationSpec {
                subcommand: "check",
                selection_arguments: vec!["--workspace".into(), "--lib".into(), "--bins".into()],
                consumer_mode: protocol::ConsumerMode::Production,
                root_crate: String::new(),
                fix: None,
                doctests: false,
                documentation: DocumentationMode::Targets,
            },
            Self::CheckProcMacroDocumentation(target) => CargoInvocationSpec {
                subcommand: "check",
                selection_arguments: vec![
                    "--package".into(),
                    target.package_name.as_str().into(),
                    "--lib".into(),
                ],
                consumer_mode: protocol::ConsumerMode::Production,
                root_crate: target.crate_name.clone(),
                fix: None,
                doctests: false,
                documentation: DocumentationMode::ProcMacro(target),
            },
            Self::FixProduction {
                plan,
                packages,
                allow_dirty,
            } => CargoInvocationSpec {
                subcommand: "fix",
                selection_arguments: package_arguments(packages, "--lib"),
                consumer_mode: protocol::ConsumerMode::Production,
                root_crate: String::new(),
                fix: Some(FixOptions { plan, allow_dirty }),
                doctests: false,
                documentation: DocumentationMode::None,
            },
            Self::FixNonProduction {
                plan,
                packages,
                allow_dirty,
            } => CargoInvocationSpec {
                subcommand: "fix",
                selection_arguments: package_arguments(packages, "--all-targets"),
                consumer_mode: protocol::ConsumerMode::NonProduction,
                root_crate: String::new(),
                fix: Some(FixOptions { plan, allow_dirty }),
                doctests: false,
                documentation: DocumentationMode::None,
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
            documentation,
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
        command
            .arg("--target")
            .arg(self.args.target.as_deref().unwrap_or(self.toolchain.host()));
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
            .env(protocol::WORKSPACE_ROOT_ENV, self.workspace_root)
            .env(protocol::CONSUMER_MODE_ENV, consumer_mode.as_str())
            .env(protocol::RUN_ID_ENV, run_id)
            .env(
                protocol::COLLECTION_OPTIONS_ENV,
                self.collection_options.as_env_value(),
            );
        if let Some(documentation) = documentation.env_value() {
            command.env(protocol::DOCUMENTATION_ENV, documentation);
        }
        if doctests {
            command
                .arg("--quiet")
                .env("RUSTC_BOOTSTRAP", "1")
                .env(
                    "CARGO_ENCODED_RUSTDOCFLAGS",
                    doctest_rustdoc_flags(self.driver),
                )
                .env_remove("RUSTDOCFLAGS");
            if self.args.output_format == OutputFormat::Text {
                command.stdout(Stdio::null());
            }
        }
        let cargo_output = if self.args.output_format == OutputFormat::Json {
            Some(CargoOutputCapture::new(&mut command)?)
        } else {
            None
        };
        Ok(ConfiguredCargoCommand {
            command,
            subcommand,
            capture_output: doctests && self.args.output_format == OutputFormat::Text,
            cargo_output,
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
            cargo_output,
        } = self.command(run_id, graph_dir, invocation, feature_profile)?;
        let status = if let Some(cargo_output) = cargo_output {
            let (status, cargo_output) = cargo_output.run(command, subcommand)?;
            let mut reader = cargo_output
                .reopen()
                .context("open temporary Cargo output file for reading")?;
            match std::io::copy(&mut reader, &mut std::io::stderr()) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(error) => {
                    return Err(error).context("write captured Cargo output to stderr");
                }
            }
            status
        } else if capture_output {
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
        documentation_graph_dir: &Path,
        feature_profile: &FeatureProfile,
        production_consumer_packages: &HashSet<String>,
    ) -> Result<CollectedFragments> {
        // Every production product uses the same compiler mode and feature set. Reuse one
        // dependency fingerprint across the product builds so Cargo can retain fragments from
        // shared dependencies instead of compiling them once per configured target.
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
            CargoInvocation::CheckDoctests {
                packages: self.doctest_packages,
            },
            feature_profile,
        )?;
        self.run(
            &format!("{run_id}-documentation"),
            documentation_graph_dir,
            CargoInvocation::CheckDocumentation,
            feature_profile,
        )?;
        for (index, target) in self
            .documentation_targets
            .iter()
            .filter(|target| target.kind == DocumentationTargetKind::ProcMacro)
            .enumerate()
        {
            self.run(
                &format!("{run_id}-documentation-proc-macro-{index}"),
                documentation_graph_dir,
                CargoInvocation::CheckProcMacroDocumentation(target),
                feature_profile,
            )?;
        }

        let mut production = read_fragments(production_graph_dir)?;
        let mut non_production = read_fragments(non_production_graph_dir)?;
        let documentation = read_fragments(documentation_graph_dir)?;
        apply_documentation_roots(
            &mut production,
            &mut non_production,
            &documentation,
            self.documentation_targets,
        );
        for fragment in &mut non_production {
            classify_non_production_target(
                fragment,
                &self.workspace_library_sources,
                &self.workspace_library_source_paths,
                self.workspace_root,
            );
        }
        for fragment in production.iter_mut().chain(&mut non_production) {
            if !production_consumer_packages.contains(&fragment.package_name) {
                fragment.non_production_consumer = true;
            }
        }
        for product in production_products {
            let ProductionProduct::Library(library) = product.product else {
                continue;
            };
            let crate_name = library.replace('-', "_");
            let mut found = false;
            for fragment in production.iter_mut().filter(|fragment| {
                fragment.package_name == product.package
                    && fragment.crate_name == crate_name
                    && fragment.compilation_target
                        == self.args.target.as_deref().unwrap_or(self.toolchain.host())
                    && fragment.product_root_kind != Some(protocol::ProductionTargetKind::Binary)
            }) {
                // Other products can compile this library with different feature sets before
                // Cargo checks the selected product itself. Retain every matching variant.
                fragment.is_product_root = true;
                fragment.product_root_kind = Some(protocol::ProductionTargetKind::Library);
                found = true;
            }
            if !found {
                bail!(
                    "no instrumented fragment was emitted for configured library `{}` in package `{}`",
                    library,
                    product.package
                );
            }
        }

        Ok(CollectedFragments {
            production,
            non_production,
        })
    }
}

fn apply_documentation_roots(
    production: &mut [Fragment],
    non_production: &mut [Fragment],
    documentation: &[Fragment],
    documentation_targets: &[DocumentationTarget],
) {
    let rendered_documentation = documentation.iter().filter(|fragment| {
        documentation_targets.iter().any(|target| {
            target.package_name == fragment.package_name && target.crate_name == fragment.crate_name
        })
    });
    // A link can resolve to a definition in another crate's fragment.
    let required_root_ids = rendered_documentation
        .clone()
        .flat_map(|fragment| fragment.documentation_roots.iter().copied())
        .collect::<HashSet<_>>();
    let conservative_source_crates = rendered_documentation
        .flat_map(|fragment| fragment.conservative_documentation_crates.iter().copied())
        .collect::<HashSet<_>>();
    let conservative_root_ids = documentation
        .iter()
        .filter(|fragment| conservative_source_crates.contains(&fragment.crate_id))
        .flat_map(|fragment| fragment.conservative_documentation_roots.iter().copied())
        .collect::<HashSet<_>>();
    let rooted_definitions = documentation
        .iter()
        .flat_map(|fragment| fragment.definitions.iter())
        .filter(|definition| {
            required_root_ids.contains(&definition.id)
                || conservative_root_ids.contains(&definition.id)
        })
        .map(|definition| {
            DefinitionIdentity::new(
                &definition.crate_name,
                &definition.name,
                definition.kind,
                definition.span.as_ref(),
            )
        })
        .collect::<HashSet<_>>();
    for fragment in production.iter_mut().chain(non_production) {
        let mut fragment_roots = fragment
            .definitions
            .iter()
            .filter(|definition| {
                rooted_definitions.contains(&DefinitionIdentity::new(
                    &definition.crate_name,
                    &definition.name,
                    definition.kind,
                    definition.span.as_ref(),
                ))
            })
            .map(|definition| definition.id)
            .collect::<HashSet<_>>();
        let public_reexports = fragment
            .definitions
            .iter()
            .filter(|definition| {
                definition.kind == DefinitionKind::Reexport && definition.public_api
            })
            .map(|definition| definition.id)
            .collect::<HashSet<_>>();
        loop {
            let previous_len = fragment_roots.len();
            let reexports = fragment
                .edges
                .iter()
                .filter(|edge| {
                    edge.kind == EdgeKind::Reexport
                        && public_reexports.contains(&edge.from)
                        && fragment_roots.contains(&edge.to)
                })
                .map(|edge| edge.from)
                .collect::<Vec<_>>();
            fragment_roots.extend(reexports);
            if fragment_roots.len() == previous_len {
                break;
            }
        }
        fragment.required_public_roots.extend(fragment_roots);
        fragment.required_public_roots.sort_unstable();
        fragment.required_public_roots.dedup();
    }
}

fn classify_non_production_target(
    fragment: &mut Fragment,
    workspace_library_sources: &HashMap<String, WorkspaceLibrarySource>,
    workspace_library_source_paths: &HashSet<PathBuf>,
    workspace_root: &Path,
) {
    if fragment.is_product_root || fragment.test_surface {
        return;
    }

    let Some(crate_root) = fragment.crate_root.as_deref().map(Path::new) else {
        return;
    };
    let crate_root = if crate_root.is_absolute() {
        crate_root.to_path_buf()
    } else {
        workspace_root.join(crate_root)
    };
    let crate_root = normalize_workspace_source_path(&crate_root);
    let library_source = workspace_library_sources.get(&fragment.package_name);
    if library_source
        .is_some_and(|source| source.crate_name == fragment.crate_name && source.path == crate_root)
    {
        return;
    }

    // Rustdoc bundles and library-format examples are library-shaped compiler
    // invocations, but they are not the owning package's production library.
    fragment.is_product_root = true;
    fragment.non_production_consumer = true;
    if !workspace_library_source_paths.contains(&crate_root) {
        // An example can intentionally share its source file with the package
        // library. Rooting its exports would also root their equivalent library
        // declarations, making genuinely dead APIs appear test-live.
        fragment.roots.extend(
            fragment
                .definitions
                .iter()
                .filter(|definition| definition.public_api)
                .map(|definition| definition.id),
        );
        fragment.roots.sort();
        fragment.roots.dedup();
    }
}

/// Normalizes Cargo metadata paths before comparing source-backed targets.
///
/// Canonicalization aligns workspace aliases when the source exists; lexical
/// normalization remains available for target-generated paths.
fn normalize_workspace_source_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized.canonicalize().unwrap_or(normalized)
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
        &profile_graph.documentation_dir,
        profile_graph.feature_profile,
        &profile_graph.production_consumer_packages,
    )?;
    if !production_fragments
        .iter()
        .any(|fragment| fragment.is_product_root)
    {
        bail!(
            "no instrumented fragment was emitted for a configured production target under feature profile `{}`; rerun with a fresh --target-dir",
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
                "`{} {}{}{separator}{cargo_arguments}`",
                product.package,
                product.product.cargo_flag(),
                match product.product {
                    ProductionProduct::Binary(binary) => format!(" {binary}"),
                    ProductionProduct::Library(_) => String::new(),
                }
            );
        }
        return format!(
            "`{} {}{}` across {} feature profiles",
            product.package,
            product.product.cargo_flag(),
            match product.product {
                ProductionProduct::Binary(binary) => format!(" {binary}"),
                ProductionProduct::Library(_) => String::new(),
            },
            feature_profiles.len()
        );
    }

    let product_kind = if production_products
        .iter()
        .all(|product| matches!(product.product, ProductionProduct::Binary(_)))
    {
        "binaries"
    } else if production_products
        .iter()
        .all(|product| matches!(product.product, ProductionProduct::Library(_)))
    {
        "libraries"
    } else {
        "targets"
    };
    let summary = format!(
        "{} configured production {product_kind}",
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

/// Maps emitted finding definitions to Cargo packages without indexing unrelated definitions.
fn definition_packages<'a>(
    production_fragments: &'a [Fragment],
    test_fragments: &'a [Fragment],
    emitted_finding_ids: &HashSet<DefinitionId>,
) -> HashMap<DefinitionId, &'a str> {
    if emitted_finding_ids.is_empty() {
        return HashMap::new();
    }

    production_fragments
        .iter()
        .chain(test_fragments)
        .flat_map(|fragment| {
            fragment.definitions.iter().filter_map(|definition| {
                emitted_finding_ids
                    .contains(&definition.id)
                    .then_some((definition.id, fragment.package_name.as_str()))
            })
        })
        .collect()
}

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
                        id: definition.id,
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
            if is_library_target(target) && remaining.remove(&target.name.replace('-', "_")) {
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

fn is_library_target(target: &Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind,
            TargetKind::Lib
                | TargetKind::RLib
                | TargetKind::DyLib
                | TargetKind::CDyLib
                | TargetKind::StaticLib
        )
    })
}

fn documentation_targets(metadata: &cargo_metadata::Metadata) -> Vec<DocumentationTarget> {
    let mut targets = Vec::new();
    for package in metadata.workspace_packages() {
        let library_name = package.targets.iter().find_map(|target| {
            (is_library_target(target) || target.kind.contains(&TargetKind::ProcMacro))
                .then_some(target.name.as_str())
        });
        targets.extend(package.targets.iter().filter_map(|target| {
            if !target.doc {
                return None;
            }
            let kind = if target.kind.contains(&TargetKind::ProcMacro) {
                DocumentationTargetKind::ProcMacro
            } else if is_library_target(target) {
                DocumentationTargetKind::Library
            } else if target.kind.contains(&TargetKind::Bin)
                && Some(target.name.as_str()) != library_name
            {
                DocumentationTargetKind::Binary
            } else {
                return None;
            };
            Some(DocumentationTarget {
                package_name: package.name.to_string(),
                crate_name: target.name.replace('-', "_"),
                kind,
            })
        }));
    }
    targets.sort_unstable_by(|left, right| {
        left.package_name
            .cmp(&right.package_name)
            .then_with(|| left.crate_name.cmp(&right.crate_name))
    });
    targets
}

fn workspace_library_sources(
    metadata: &cargo_metadata::Metadata,
) -> HashMap<String, WorkspaceLibrarySource> {
    metadata
        .workspace_packages()
        .into_iter()
        .filter_map(|package| {
            let target = package.targets.iter().find(|target| {
                is_library_target(target) || target.kind.contains(&TargetKind::ProcMacro)
            })?;
            Some((
                package.name.to_string(),
                WorkspaceLibrarySource {
                    crate_name: target.name.replace('-', "_"),
                    path: normalize_workspace_source_path(target.src_path.as_std_path()),
                },
            ))
        })
        .collect()
}

fn workspace_library_crates(
    metadata: &cargo_metadata::Metadata,
    audited_crates: Option<&HashSet<String>>,
) -> Result<HashSet<String>> {
    let mut packages_by_crate: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in metadata.workspace_packages() {
        for target in &package.targets {
            if is_library_target(target) {
                packages_by_crate
                    .entry(target.name.replace('-', "_"))
                    .or_default()
                    .insert(package.name.to_string());
            }
        }
    }

    let conflicts = packages_by_crate
        .iter()
        .filter(|(crate_name, packages)| {
            packages.len() > 1
                && audited_crates.is_none_or(|audited_crates| audited_crates.contains(*crate_name))
        })
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

fn production_workspace_packages(
    metadata: &cargo_metadata::Metadata,
    production_products: &[ProductionSelection<'_>],
    analysis_target: &AnalysisTarget,
) -> Result<HashSet<String>> {
    let workspace_packages = metadata.workspace_packages();
    let packages: HashMap<_, _> = workspace_packages
        .iter()
        .map(|package| (&package.id, package.name.as_str()))
        .collect();
    let resolve = metadata
        .resolve
        .as_ref()
        .context("Cargo metadata did not contain a resolved dependency graph")?;
    let mut dependencies: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut non_production_incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut normal_dependencies: HashMap<&str, Vec<&str>> = HashMap::new();
    for node in &resolve.nodes {
        let Some(&package) = packages.get(&node.id) else {
            continue;
        };
        for dependency in &node.deps {
            let Some(&dependency_package) = packages.get(&dependency.pkg) else {
                continue;
            };
            let mut applicable = dependency.dep_kinds.iter().filter(|kind| {
                kind.target
                    .as_ref()
                    .is_none_or(|platform| analysis_target.matches_platform(platform))
            });
            let Some(first_kind) = applicable.next() else {
                continue;
            };
            dependencies
                .entry(package)
                .or_default()
                .push(dependency_package);
            incoming
                .entry(dependency_package)
                .or_default()
                .push(package);
            if first_kind.kind == DependencyKind::Normal
                || applicable.any(|kind| kind.kind == DependencyKind::Normal)
            {
                normal_dependencies
                    .entry(package)
                    .or_default()
                    .push(dependency_package);
            } else {
                non_production_incoming
                    .entry(dependency_package)
                    .or_default()
                    .push(package);
            }
        }
    }

    let mut visited = HashSet::new();
    let mut ordered = Vec::with_capacity(packages.len());
    for package in packages.values().copied() {
        let mut pending = vec![(package, false)];
        while let Some((package, expanded)) = pending.pop() {
            if expanded {
                ordered.push(package);
            } else if visited.insert(package) {
                pending.push((package, true));
                if let Some(dependencies) = dependencies.get(package) {
                    pending.extend(dependencies.iter().map(|dependency| (*dependency, false)));
                }
            }
        }
    }

    let mut components: Vec<Vec<&str>> = Vec::new();
    let mut component_by_package = HashMap::new();
    while let Some(package) = ordered.pop() {
        if component_by_package.contains_key(package) {
            continue;
        }
        let component_index = components.len();
        let mut component = Vec::new();
        let mut pending = vec![package];
        while let Some(package) = pending.pop() {
            if component_by_package.contains_key(package) {
                continue;
            }
            component_by_package.insert(package, component_index);
            component.push(package);
            if let Some(dependents) = incoming.get(package) {
                pending.extend(dependents);
            }
        }
        components.push(component);
    }

    let mut root_components = vec![true; components.len()];
    for (&package, dependents) in &incoming {
        let component = component_by_package[package];
        if dependents
            .iter()
            .any(|dependent| component_by_package[dependent] != component)
        {
            root_components[component] = false;
        }
    }

    let mut pending: Vec<&str> = components
        .into_iter()
        .enumerate()
        .filter(|(index, _)| root_components[*index])
        .flat_map(|(index, component)| {
            // A dev-dependency cycle has no package with zero incoming edges.
            // Start outside its dev/build-only edges so fixtures remain non-production.
            let roots: Vec<_> = component
                .iter()
                .copied()
                .filter(|package| {
                    non_production_incoming
                        .get(package)
                        .is_none_or(|dependents| {
                            dependents
                                .iter()
                                .all(|dependent| component_by_package[dependent] != index)
                        })
                })
                .collect();
            if roots.is_empty() { component } else { roots }
        })
        .chain(production_products.iter().map(|product| product.package))
        .collect();
    let mut production_packages = HashSet::new();
    while let Some(package) = pending.pop() {
        if production_packages.insert(package.to_owned())
            && let Some(dependencies) = normal_dependencies.get(package)
        {
            pending.extend(dependencies);
        }
    }
    Ok(production_packages)
}

fn validate_excluded_crates(
    excluded_crates: &[String],
    candidate_crates: &HashSet<String>,
) -> Result<()> {
    let unknown_crates = excluded_crates
        .iter()
        .filter(|crate_name| !candidate_crates.contains(*crate_name))
        .collect::<BTreeSet<_>>();

    if unknown_crates.is_empty() {
        return Ok(());
    }

    let unknown_crates = unknown_crates
        .iter()
        .map(|crate_name| format!("`{crate_name}`"))
        .collect::<Vec<_>>()
        .join(", ");

    let valid_crates = candidate_crates.iter().collect::<BTreeSet<_>>();

    if valid_crates.is_empty() {
        bail!(
            "unknown --exclude-crate value(s): {unknown_crates}; this workspace has no library crates"
        );
    }

    let valid_crates = valid_crates
        .iter()
        .map(|crate_name| format!("`{crate_name}`"))
        .collect::<Vec<_>>()
        .join(", ");

    bail!(
        "unknown --exclude-crate value(s): {unknown_crates}; valid workspace library crate names: {valid_crates}"
    );
}

fn validate_product(
    metadata: &cargo_metadata::Metadata,
    package: &str,
    product: &ProductionProduct,
) -> Result<()> {
    let package = workspace_package(metadata, package)?;
    if !package.targets.iter().any(|target| {
        target.name == product.name()
            && match product {
                ProductionProduct::Binary(_) => target.kind.contains(&TargetKind::Bin),
                ProductionProduct::Library(_) => is_library_target(target),
            }
    }) {
        bail!(
            "package `{}` has no {} target `{}`",
            package.name,
            product.kind().as_str(),
            product.name()
        );
    }
    Ok(())
}

fn validate_package(metadata: &cargo_metadata::Metadata, package: &str) -> Result<()> {
    workspace_package(metadata, package).map(|_| ())
}

fn workspace_package<'a>(
    metadata: &'a cargo_metadata::Metadata,
    package: &str,
) -> Result<&'a cargo_metadata::Package> {
    let Some(package) = metadata.packages.iter().find(|candidate| {
        candidate.name.as_str() == package && metadata.workspace_members.contains(&candidate.id)
    }) else {
        bail!("package `{package}` is not in the selected workspace");
    };
    Ok(package)
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
    use std::collections::{HashMap, HashSet};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use clap::CommandFactory;

    use crate::config::ConfigDiagnosticKind;
    use crate::protocol::ConsumerMode;
    use cargo_hawk_internal::graph::{
        Definition, DefinitionId, DefinitionKind, Edge, EdgeKind, Finding, FindingKind, FixPlan,
        FixTarget, Fragment, Span, VisibilityReduction,
    };

    fn test_id(value: &str) -> DefinitionId {
        let hash = value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        DefinitionId::new(0, hash)
    }

    #[cfg(unix)]
    use super::normalize_workspace_source_path;
    use super::{
        Args, CargoInvocation, DEFAULT_TARGET_DIR_COMPONENT_MAX_BYTES, DiagnosticRenderer,
        DocumentationMode, DocumentationTarget, DocumentationTargetKind, LintLevel, LintLevels,
        ProductionProduct, ProductionSelection, WorkspaceLibrarySource, apply_documentation_roots,
        classify_non_production_target, default_target_dir, definition_packages,
        fix_plan_signature, json_definition_kind, json_finding_kind, validate_excluded_crates,
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
        documentation: DocumentationMode<'_>,
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
        assert_eq!(specification.documentation, documentation);
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
    fn rejects_unknown_excluded_crates_without_workspace_libraries() {
        let error = validate_excluded_crates(&["foo".to_owned()], &HashSet::new())
            .expect_err("unknown excluded crate is rejected");

        assert_eq!(
            error.to_string(),
            "unknown --exclude-crate value(s): `foo`; this workspace has no library crates"
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
            id: test_id(id),
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
    fn foreign_inline_roots_only_documentation_targets_from_the_source_crate() {
        let definition = |id: &str| Definition {
            id: test_id(id),
            crate_name: "support".into(),
            name: id.into(),
            kind: DefinitionKind::Constant,
            span: None,
            declaration_span: None,
            expansion_span: None,
            public_api: true,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec![],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let fragment = |crate_name: &str, crate_id: DefinitionId| Fragment {
            protocol_version: crate::protocol::ProtocolVersion,
            package_name: crate_name.into(),
            crate_name: crate_name.into(),
            compilation_target: "aarch64-apple-darwin".into(),
            crate_id,
            crate_root: None,
            is_product_root: false,
            product_root_kind: None,
            test_surface: false,
            non_production_consumer: false,
            definitions: vec![],
            edges: vec![],
            roots: vec![],
            conservative_roots: vec![],
            required_public_roots: vec![],
            documentation_roots: vec![],
            conservative_documentation_roots: vec![],
            conservative_documentation_crates: vec![],
        };
        let support_crate = test_id("support-crate");
        let linked = definition("linked");
        let unlinked = definition("unlinked");
        let mut linked_alias = definition("linked-alias");
        linked_alias.kind = DefinitionKind::Reexport;
        linked_alias.visible_reexport_api = true;
        let mut production = fragment("support", support_crate);
        production.definitions = vec![linked.clone(), unlinked, linked_alias.clone()];
        production.edges.push(Edge {
            from: linked_alias.id,
            to: linked.id,
            kind: EdgeKind::Reexport,
        });
        let mut support_docs = production.clone();
        support_docs.conservative_documentation_roots = vec![linked.id];
        let mut facade_docs = fragment("facade", test_id("facade-crate"));
        facade_docs.conservative_documentation_crates = vec![support_crate];
        let documentation_targets = [DocumentationTarget {
            package_name: "facade".into(),
            crate_name: "facade".into(),
            kind: DocumentationTargetKind::Library,
        }];

        apply_documentation_roots(
            std::slice::from_mut(&mut production),
            &mut [],
            &[support_docs, facade_docs],
            &documentation_targets,
        );

        let mut expected = vec![linked_alias.id, linked.id];
        expected.sort_unstable();
        assert_eq!(production.required_public_roots, expected);
    }

    #[test]
    fn definition_packages_only_indexes_emitted_findings() {
        let definition = |id: &str, name: &str| Definition {
            id: test_id(id),
            crate_name: "renamed_library".into(),
            name: name.into(),
            kind: DefinitionKind::Function,
            span: None,
            declaration_span: None,
            expansion_span: None,
            public_api: true,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec![],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let fragment = |package_name: &str, definitions: Vec<Definition>| Fragment {
            protocol_version: crate::protocol::ProtocolVersion,
            package_name: package_name.into(),
            crate_name: "renamed_library".into(),
            compilation_target: "aarch64-apple-darwin".into(),
            crate_id: test_id(package_name),
            crate_root: None,
            is_product_root: false,
            product_root_kind: None,
            test_surface: false,
            non_production_consumer: false,
            definitions,
            edges: vec![],
            roots: vec![],
            conservative_roots: vec![],
            required_public_roots: vec![],
            documentation_roots: vec![],
            conservative_documentation_roots: vec![],
            conservative_documentation_crates: vec![],
        };
        let production = vec![fragment(
            "library-package",
            vec![
                definition("production-emitted", "production_emitted"),
                definition("production-suppressed", "production_suppressed"),
            ],
        )];
        let tests = vec![fragment(
            "test-package",
            vec![
                definition("test-emitted", "test_emitted"),
                definition("test-suppressed", "test_suppressed"),
            ],
        )];
        let emitted_finding_ids =
            HashSet::from([test_id("production-emitted"), test_id("test-emitted")]);

        let packages = definition_packages(&production, &tests, &emitted_finding_ids);

        assert_eq!(packages.len(), 2);
        assert_eq!(
            packages.get(&test_id("production-emitted")),
            Some(&"library-package")
        );
        assert_eq!(
            packages.get(&test_id("test-emitted")),
            Some(&"test-package")
        );
        assert!(!packages.contains_key(&test_id("production-suppressed")));
        assert!(!packages.contains_key(&test_id("test-suppressed")));
        assert!(definition_packages(&production, &tests, &HashSet::new()).is_empty());
    }

    #[test]
    fn non_production_library_targets_are_classified_by_their_source_paths() {
        let sources = HashMap::from([
            (
                "consumer".to_owned(),
                WorkspaceLibrarySource {
                    crate_name: "consumer".to_owned(),
                    path: PathBuf::from("/workspace/consumer/src/lib.rs"),
                },
            ),
            (
                "api".to_owned(),
                WorkspaceLibrarySource {
                    crate_name: "api".to_owned(),
                    path: PathBuf::from("/workspace/api/src/lib.rs"),
                },
            ),
        ]);
        let library_paths = sources.values().map(|source| source.path.clone()).collect();
        let definition = Definition {
            id: test_id("non-production-entry"),
            crate_name: "consumer".into(),
            name: "entry".into(),
            kind: DefinitionKind::Function,
            span: None,
            declaration_span: None,
            expansion_span: None,
            public_api: true,
            restricted_visible_api: false,
            crate_visible_api: false,
            visible_reexport_api: false,
            module_scope: vec![],
            uniform_field_group: None,
            dead_code_allowed: false,
        };
        let fragment = |crate_root: &str| Fragment {
            protocol_version: crate::protocol::ProtocolVersion,
            package_name: "consumer".into(),
            crate_name: "consumer".into(),
            compilation_target: "aarch64-apple-darwin".into(),
            crate_id: test_id("consumer"),
            crate_root: Some(crate_root.into()),
            is_product_root: false,
            product_root_kind: None,
            test_surface: false,
            non_production_consumer: false,
            definitions: vec![definition.clone()],
            edges: vec![],
            roots: vec![],
            conservative_roots: vec![],
            required_public_roots: vec![],
            documentation_roots: vec![],
            conservative_documentation_roots: vec![],
            conservative_documentation_crates: vec![],
        };

        let mut library = fragment("consumer/src/lib.rs");
        classify_non_production_target(
            &mut library,
            &sources,
            &library_paths,
            Path::new("/workspace"),
        );
        assert!(!library.is_product_root);
        assert!(!library.non_production_consumer);
        assert!(library.roots.is_empty());

        let mut normalized_library = fragment("consumer/src/../src/./lib.rs");
        classify_non_production_target(
            &mut normalized_library,
            &sources,
            &library_paths,
            Path::new("/workspace"),
        );
        assert!(!normalized_library.is_product_root);
        assert!(!normalized_library.non_production_consumer);

        let mut same_source_example = fragment("consumer/src/lib.rs");
        same_source_example.crate_name = "example_library".into();
        classify_non_production_target(
            &mut same_source_example,
            &sources,
            &library_paths,
            Path::new("/workspace"),
        );
        assert!(same_source_example.is_product_root);
        assert!(same_source_example.non_production_consumer);
        assert!(same_source_example.roots.is_empty());

        let mut other_library_example = fragment("api/src/lib.rs");
        other_library_example.crate_name = "api_example".into();
        classify_non_production_target(
            &mut other_library_example,
            &sources,
            &library_paths,
            Path::new("/workspace"),
        );
        assert!(other_library_example.is_product_root);
        assert!(other_library_example.non_production_consumer);
        assert!(other_library_example.roots.is_empty());

        for crate_root in [
            "consumer/examples/library.rs",
            "/tmp/rustdoctest/doctest_bundle_2024.rs",
        ] {
            let mut non_production = fragment(crate_root);
            classify_non_production_target(
                &mut non_production,
                &sources,
                &library_paths,
                Path::new("/workspace"),
            );
            assert!(non_production.is_product_root);
            assert!(non_production.non_production_consumer);
            assert_eq!(non_production.roots, vec![test_id("non-production-entry")]);
        }

        #[cfg(unix)]
        {
            // Cargo metadata can preserve a symlink spelling while rustc
            // reports the canonical path. Both must still identify the
            // package's actual library target.
            let directory = tempfile::tempdir().expect("temporary workspace directory");
            let workspace = directory.path().join("workspace");
            let source = workspace.join("consumer/src/lib.rs");
            fs::create_dir_all(source.parent().expect("source has a parent"))
                .expect("create workspace source directory");
            fs::write(&source, "").expect("write workspace source");
            let workspace_alias = directory.path().join("workspace-alias");
            symlink(&workspace, &workspace_alias).expect("create workspace alias");

            let sources = HashMap::from([(
                "consumer".to_owned(),
                WorkspaceLibrarySource {
                    crate_name: "consumer".to_owned(),
                    path: normalize_workspace_source_path(
                        &workspace_alias.join("consumer/src/lib.rs"),
                    ),
                },
            )]);
            let library_paths = sources.values().map(|source| source.path.clone()).collect();
            let mut aliased_library = fragment("consumer/src/lib.rs");
            classify_non_production_target(
                &mut aliased_library,
                &sources,
                &library_paths,
                &workspace.canonicalize().expect("canonical workspace"),
            );

            assert!(!aliased_library.is_product_root);
            assert!(!aliased_library.non_production_consumer);
            assert!(aliased_library.roots.is_empty());
        }
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
        let binary = ProductionProduct::Binary("app-cli".to_owned());
        let library = ProductionProduct::Library("public-api".to_owned());
        let proc_macro = DocumentationTarget {
            package_name: "macro-package".into(),
            crate_name: "macro_package".into(),
            kind: DocumentationTargetKind::ProcMacro,
        };

        assert_cargo_invocation(
            CargoInvocation::CheckProduction(ProductionSelection {
                package: "app-package",
                product: &binary,
            }),
            "check",
            &["--package", "app-package", "--bin", "app-cli"],
            ConsumerMode::Production,
            "app_cli",
            None,
            false,
            DocumentationMode::None,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckProduction(ProductionSelection {
                package: "api-package",
                product: &library,
            }),
            "check",
            &["--package", "api-package", "--lib"],
            ConsumerMode::Production,
            "public_api",
            None,
            false,
            DocumentationMode::None,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckNonProduction,
            "check",
            &["--workspace", "--all-targets"],
            ConsumerMode::NonProduction,
            "",
            None,
            false,
            DocumentationMode::None,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckDoctests { packages: None },
            "test",
            &["--workspace", "--doc"],
            ConsumerMode::NonProduction,
            "",
            None,
            true,
            DocumentationMode::None,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckDoctests {
                packages: Some(&packages),
            },
            "test",
            &["--package", "library", "--package", "support", "--doc"],
            ConsumerMode::NonProduction,
            "",
            None,
            true,
            DocumentationMode::None,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckDocumentation,
            "check",
            &["--workspace", "--lib", "--bins"],
            ConsumerMode::Production,
            "",
            None,
            false,
            DocumentationMode::Targets,
        );
        assert_cargo_invocation(
            CargoInvocation::CheckProcMacroDocumentation(&proc_macro),
            "check",
            &["--package", "macro-package", "--lib"],
            ConsumerMode::Production,
            "macro_package",
            None,
            false,
            DocumentationMode::ProcMacro(&proc_macro),
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
            DocumentationMode::None,
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
            DocumentationMode::None,
        );
    }

    #[test]
    fn json_schema_uses_stable_kind_names() {
        assert_eq!(json_finding_kind(FindingKind::DeadPublic), "dead_public");
        assert_eq!(
            json_finding_kind(FindingKind::UnnecessaryPublic),
            "unnecessary_public"
        );
        assert_eq!(
            json_finding_kind(FindingKind::UnnecessaryRestrictedVisibility),
            "unnecessary_restricted_visibility"
        );
        assert_eq!(
            json_finding_kind(FindingKind::UnnecessaryCrateVisibility),
            "unnecessary_crate_visibility"
        );

        for (kind, expected) in [
            (DefinitionKind::Function, "function"),
            (DefinitionKind::InherentMethod, "inherent_method"),
            (
                DefinitionKind::InherentAssociatedConstant,
                "inherent_associated_constant",
            ),
            (DefinitionKind::Trait, "trait"),
            (DefinitionKind::Struct, "struct"),
            (DefinitionKind::Enum, "enum"),
            (DefinitionKind::Union, "union"),
            (DefinitionKind::TypeAlias, "type_alias"),
            (DefinitionKind::Constant, "constant"),
            (DefinitionKind::Static, "static"),
            (DefinitionKind::Field, "field"),
            (DefinitionKind::EnumVariant, "enum_variant"),
            (DefinitionKind::Reexport, "reexport"),
            (DefinitionKind::Module, "module"),
            (DefinitionKind::Other, "other"),
        ] {
            assert_eq!(json_definition_kind(kind), expected);
        }
    }

    #[test]
    fn diagnostic_rendering_includes_terminal_styles() {
        let definition = Definition {
            id: test_id("internal_helper"),
            crate_name: "library".into(),
            name: "internal_helper".into(),
            kind: DefinitionKind::Function,
            span: Some(Span {
                file: "tests/fixtures/basic/library/src/lib.rs".into(),
                line: 5,
                column: 1,
            }),
            declaration_span: None,
            expansion_span: None,
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
            id: test_id("scoped::run"),
            crate_name: "library".into(),
            name: "scoped::run".into(),
            kind: DefinitionKind::Function,
            span: Some(Span {
                file: "tests/fixtures/crate_visibility_fixes/library/src/lib.rs".into(),
                line: 7,
                column: 5,
            }),
            declaration_span: None,
            expansion_span: None,
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
            id: test_id("scoped::private_parent_visible_helper"),
            crate_name: "library".into(),
            name: "scoped::private_parent_visible_helper".into(),
            kind: DefinitionKind::Function,
            span: Some(Span {
                file: "tests/fixtures/crate_visibility_fixes/library/src/lib.rs".into(),
                line: 16,
                column: 5,
            }),
            declaration_span: None,
            expansion_span: None,
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
            id: test_id("InternalState::Active"),
            crate_name: "library".into(),
            name: "InternalState::Active".into(),
            kind: DefinitionKind::EnumVariant,
            span: None,
            declaration_span: None,
            expansion_span: None,
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
                "check",
                "-Dwarnings",
                "--warn",
                "hawk::unnecessary_public",
                "-A",
                "hawk::unknown_item",
            ])
            .expect("parse lint-level arguments");
        let levels = LintLevels::from_matches(
            matches
                .subcommand_matches("check")
                .expect("check subcommand matches"),
        )
        .expect("valid lint selectors");

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
                "check",
                "-W",
                "hawk::unnecessary_crate_visibility",
                "-Awarnings",
                "-Dwarnings",
            ])
            .expect("parse lint-level arguments");
        let levels = LintLevels::from_matches(
            matches
                .subcommand_matches("check")
                .expect("check subcommand matches"),
        )
        .expect("valid lint selectors");

        assert_eq!(
            levels.level(FindingKind::UnnecessaryCrateVisibility),
            LintLevel::Deny
        );
    }

    #[test]
    fn later_warnings_group_reenables_default_warnings() {
        let matches = Args::command()
            .try_get_matches_from(["cargo-hawk", "check", "-Awarnings", "-Dwarnings"])
            .expect("parse lint-level arguments");
        let levels = LintLevels::from_matches(
            matches
                .subcommand_matches("check")
                .expect("check subcommand matches"),
        )
        .expect("valid lint selectors");

        assert_eq!(levels.level(FindingKind::DeadPublic), LintLevel::Deny);
        assert_eq!(
            levels.level(FindingKind::UnnecessaryCrateVisibility),
            LintLevel::Allow
        );
    }
}
