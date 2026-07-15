use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use crate::protocol;

const RUSTC_PROBE_MARKER: &[u8] = b"cargo-hawk-rustc-probe-v1";

pub(crate) struct RustToolchain {
    rustc: OsString,
    sysroot: PathBuf,
    host: String,
}

impl RustToolchain {
    pub(crate) fn discover(workspace_root: &Path, manifest_path: &Path) -> Result<Self> {
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

    pub(crate) fn rustc(&self) -> &OsStr {
        &self.rustc
    }

    pub(crate) fn host(&self) -> &str {
        &self.host
    }

    pub(crate) fn configure_command(&self, command: &mut Command) -> Result<()> {
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
    // A random marker name inside the private probe directory acts as a capability.
    // Inherited environment variables cannot identify a live probe merely by pointing at
    // an existing directory.
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
    // The compiler driver cannot perform this probe because finding its dynamic rustc
    // libraries requires the selected compiler's sysroot.
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

pub(crate) fn run_rustc_probe(args: &[String]) -> Option<ExitCode> {
    let output_path = PathBuf::from(env::var_os(protocol::RUSTC_PROBE_ENV)?);
    let token = PathBuf::from(env::var_os(protocol::RUSTC_PROBE_TOKEN_ENV)?);
    let probe_dir = output_path.parent()?;
    // Do not treat stale or forged inherited state as an internal invocation. The token is
    // a random marker file name, never an arbitrary path.
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
        // Never truncate an existing path, even if every other probe signal appears valid.
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

pub(crate) fn driver_executable() -> Result<PathBuf> {
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

pub(crate) fn validate_driver_protocol(driver: &Path, toolchain: &RustToolchain) -> Result<()> {
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

pub(crate) fn clear_protocol_environment(command: &mut Command) {
    for variable in protocol::ENVIRONMENT_VARIABLES {
        command.env_remove(variable);
    }
}
