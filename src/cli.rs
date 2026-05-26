use std::collections::HashSet;
use std::env;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use clap::Parser;

use crate::graph::{FindingKind, Fragment, analyze};

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
    let findings = analyze(&fragments, &excluded);
    for finding in &findings {
        let location = finding
            .definition
            .span
            .as_ref()
            .map(|span| format!("{}:{}:{}", span.file, span.line, span.column))
            .unwrap_or_else(|| finding.definition.crate_name.clone());
        match finding.kind {
            FindingKind::DeadPublic => println!(
                "{location}: hawk::dead_public: `{}` is public but is not reachable from binary `{}`",
                finding.definition.name, args.bin
            ),
            FindingKind::UnnecessaryPublic => println!(
                "{location}: hawk::unnecessary_public: `{}` is public but all reachable uses are within `{}`; it can be `pub(crate)`",
                finding.definition.name, finding.definition.crate_name
            ),
        }
    }
    println!(
        "hawk: {} finding(s) for `{} --bin {} --all-features` on the host target",
        findings.len(),
        args.package,
        args.bin
    );
    Ok(ExitCode::SUCCESS)
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
