use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create fixture copy directory");
    for entry in fs::read_dir(source).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let destination = destination.join(entry.file_name());
        if entry.file_type().expect("read fixture entry type").is_dir() {
            copy_directory(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).expect("copy fixture file");
        }
    }
}

fn initialize_git_repository(path: &Path) {
    let status = Command::new("git")
        .arg("init")
        .arg("--quiet")
        .current_dir(path)
        .status()
        .expect("initialize Git repository");
    assert!(status.success());

    let status = Command::new("git")
        .arg("add")
        .arg(".")
        .current_dir(path)
        .status()
        .expect("stage fixture workspace");
    assert!(status.success());

    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Hawk Tests",
            "-c",
            "user.email=hawk-tests@example.com",
            "commit",
            "--quiet",
            "-m",
            "Initial fixture",
        ])
        .current_dir(path)
        .status()
        .expect("commit fixture workspace");
    assert!(status.success());
}

struct HawkTestContext {
    workspace: tempfile::TempDir,
    target_dir: tempfile::TempDir,
}

impl HawkTestContext {
    fn new(fixture: &str) -> Self {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let workspace = tempfile::tempdir().expect("temporary fixture workspace");
        copy_directory(&source, workspace.path());
        Self {
            workspace,
            target_dir: tempfile::tempdir().expect("temporary target directory"),
        }
    }

    fn workspace(&self) -> &Path {
        self.workspace.path()
    }

    fn target_dir(&self) -> &Path {
        self.target_dir.path()
    }

    fn command(&self) -> Command {
        self.command_with_color("never")
    }

    fn command_with_color(&self, color: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"));
        command
            .current_dir(self.workspace())
            .arg("check")
            .arg("--manifest-path")
            .arg(self.workspace().join("Cargo.toml"))
            .arg("--target-dir")
            .arg(self.target_dir())
            .arg(format!("--color={color}"));
        command
    }

    fn cargo(&self) -> Command {
        let mut command = Command::new("cargo");
        command.current_dir(self.workspace());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run cargo-hawk")
    }

    fn initialize_git(&self) {
        initialize_git_repository(self.workspace());
    }

    fn assert_success(&self, output: &Output) {
        assert!(
            output.status.success(),
            "cargo-hawk failed:\n{}",
            self.normalized_stderr(output)
        );
    }

    fn normalized_stdout(&self, output: &Output) -> String {
        self.normalize(&output.stdout)
    }

    fn normalized_stderr(&self, output: &Output) -> String {
        self.normalize(&output.stderr)
    }

    fn git_diff(&self) -> String {
        let output = Command::new("git")
            .args(["diff", "--no-ext-diff", "--no-color"])
            .current_dir(self.workspace())
            .output()
            .expect("read fixture diff");
        assert!(output.status.success());
        self.normalize(&output.stdout)
            .lines()
            .filter(|line| !line.starts_with("index "))
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    fn normalize(&self, output: &[u8]) -> String {
        let output = String::from_utf8_lossy(output);
        let mut output = anstream::adapter::strip_str(&output)
            .to_string()
            .replace("\r\n", "\n");
        for (path, replacement) in [
            (self.workspace(), "[WORKSPACE]"),
            (self.target_dir(), "[TARGET_DIR]"),
        ] {
            if let Ok(path) = path.canonicalize() {
                output = output.replace(&path.display().to_string(), replacement);
            }
            output = output.replace(&path.display().to_string(), replacement);
        }
        output
    }
}

#[test]
fn test_context_normalizes_canonical_paths() {
    let context = HawkTestContext::new("basic");
    let workspace = context
        .workspace()
        .canonicalize()
        .expect("canonical workspace path");
    let target_dir = context
        .target_dir()
        .canonicalize()
        .expect("canonical target path");
    let output = format!("{}\n{}\n", workspace.display(), target_dir.display());

    assert_eq!(
        context.normalize(output.as_bytes()),
        "[WORKSPACE]\n[TARGET_DIR]\n"
    );
}

#[cfg(unix)]
#[test]
fn rejects_non_utf8_arguments_without_panicking() {
    for executable in [
        env!("CARGO_BIN_EXE_cargo-hawk"),
        env!("CARGO_BIN_EXE_cargo-hawk-driver"),
    ] {
        let output = Command::new(executable)
            .arg(std::ffi::OsString::from_vec(vec![0xff]))
            .output()
            .expect("run Hawk with a non-UTF-8 argument");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("hawk: command-line arguments must be valid UTF-8"));
        assert!(!stderr.contains("panicked"));
    }
}

#[test]
fn rejects_incomplete_driver_protocol_environment() {
    let output_dir = tempfile::tempdir().expect("temporary graph directory");
    for (consumer_mode, run_id, expected) in [
        (
            None,
            Some("run"),
            "Hawk frontend did not provide HAWK_CONSUMER_MODE",
        ),
        (
            Some("invalid"),
            Some("run"),
            "unsupported HAWK_CONSUMER_MODE value `invalid`",
        ),
        (
            Some("production"),
            None,
            "Hawk frontend did not provide HAWK_RUN_ID",
        ),
        (
            Some("production"),
            Some(""),
            "HAWK_RUN_ID must not be empty",
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-hawk-driver"));
        command
            .arg("rustc")
            .env(
                "HAWK_PROTOCOL_VERSION",
                cargo_hawk_internal::protocol::VERSION.to_string(),
            )
            .env("HAWK_OUTPUT_DIR", output_dir.path())
            .env("HAWK_ROOT_CRATE", "app")
            .env_remove("HAWK_CONSUMER_MODE")
            .env_remove("HAWK_RUN_ID");
        if let Some(consumer_mode) = consumer_mode {
            command.env("HAWK_CONSUMER_MODE", consumer_mode);
        }
        if let Some(run_id) = run_id {
            command.env("HAWK_RUN_ID", run_id);
        }
        let output = command.output().expect("run Hawk compiler driver");

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
    }
}

#[cfg(unix)]
#[test]
fn exits_successfully_when_diagnostic_output_is_closed() {
    let context = HawkTestContext::new("basic");
    for output_format in ["text", "json"] {
        let mut child = context
            .command()
            .arg(format!("--output-format={output_format}"))
            .arg("-A")
            .arg("warnings")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cargo-hawk");
        drop(child.stdout.take());

        let output = child.wait_with_output().expect("wait for cargo-hawk");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{output_format}: {stderr}");
        assert!(!stderr.contains("Broken pipe"), "{output_format}: {stderr}");
    }
}

#[test]
fn prints_usage_without_a_subcommand() {
    for args in [&[][..], &["hawk"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
            .args(args)
            .output()
            .expect("run cargo-hawk without a subcommand");

        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).expect("usage output is UTF-8");
        assert!(stderr.contains("Usage: cargo hawk <COMMAND>"));
        assert!(stderr.contains("check  Check a Cargo workspace for unnecessary public surface"));
    }
}

#[test]
fn prints_version_without_overwriting_an_inherited_rustc_probe_path() {
    let probe_dir = tempfile::tempdir().expect("temporary rustc probe directory");
    let victim = probe_dir.path().join("rustc");
    fs::write(&victim, "do not overwrite").expect("write probe victim");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .args(["hawk", "--version"])
        .env("HAWK_RUSTC_PROBE", &victim)
        .env("HAWK_RUSTC_PROBE_TOKEN", probe_dir.path())
        .output()
        .expect("run cargo-hawk --version");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("version output is UTF-8"),
        concat!("cargo hawk ", env!("CARGO_PKG_VERSION"), "\n")
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        fs::read_to_string(&victim).expect("read probe victim"),
        "do not overwrite"
    );
}

#[test]
fn repeated_runs_do_not_reuse_a_failed_rustc_probe() {
    let context = HawkTestContext::new("basic");

    for run in 1..=2 {
        let output = context.run(&["-A", "warnings"]);

        assert!(
            output.status.success(),
            "cargo-hawk run {run} failed:\n{}",
            context.normalized_stderr(&output)
        );
        assert!(
            !context.workspace().join("target/.rustc_info.json").exists(),
            "cargo-hawk run {run} persisted rustc probe state in the workspace target directory"
        );
    }
}

#[test]
fn resolves_relative_target_directory_from_the_launch_directory() {
    let context = HawkTestContext::new("basic");
    let launch_directory = tempfile::tempdir().expect("temporary launch directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .current_dir(launch_directory.path())
        .arg("check")
        .arg("--manifest-path")
        .arg(context.workspace().join("Cargo.toml"))
        .arg("--target-dir")
        .arg("target")
        .arg("-A")
        .arg("warnings")
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk from a separate directory");

    context.assert_success(&output);
    assert!(launch_directory.path().join("target/debug").is_dir());
    assert!(!context.workspace().join("target").exists());
}

#[test]
fn ignores_stale_fix_plan_during_analysis() {
    let context = HawkTestContext::new("basic");
    let output = context
        .command()
        .arg("-A")
        .arg("warnings")
        .env(
            "HAWK_FIX_PLAN",
            context.target_dir().join("stale-fix-plan.json"),
        )
        .output()
        .expect("run cargo-hawk with a stale fix plan");

    context.assert_success(&output);
}

#[cfg(unix)]
#[test]
fn honors_cargo_configured_compiler() {
    let context = HawkTestContext::new("basic");

    let rustc_sysroot = Command::new("rustc")
        .arg("--print=sysroot")
        .output()
        .expect("read Rust compiler sysroot");
    assert!(rustc_sysroot.status.success());
    let rustc = Path::new(
        std::str::from_utf8(&rustc_sysroot.stdout)
            .expect("Rust compiler sysroot")
            .trim(),
    )
    .join("bin")
    .join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    let configured_compiler = context.workspace().join("custom-compiler");
    symlink(rustc, &configured_compiler).expect("create renamed compiler symlink");

    let cargo_config = context.workspace().join(".cargo");
    fs::create_dir(&cargo_config).expect("create Cargo config directory");
    fs::write(
        cargo_config.join("config.toml"),
        format!(
            "[build]\nrustc = \"{}\"\n",
            configured_compiler
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        ),
    )
    .expect("write Cargo config");

    let fake_bin = tempfile::tempdir().expect("temporary fake binary directory");
    let fake_rustc = fake_bin.path().join("rustc");
    fs::write(
        &fake_rustc,
        "#!/bin/sh\n\
         echo 'rustc 0.0.0 (fake)'\n\
         echo 'release: 0.0.0'\n\
         echo 'commit-hash: fake'\n\
         echo 'host: fake'\n",
    )
    .expect("write fake rustc");
    fs::set_permissions(&fake_rustc, fs::Permissions::from_mode(0o755))
        .expect("make fake rustc executable");

    let path = std::env::join_paths(std::iter::once(fake_bin.path().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set")),
    ))
    .expect("construct PATH");
    let output = context
        .command()
        .arg("-A")
        .arg("warnings")
        .env("PATH", path)
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
}

#[test]
fn diagnoses_public_surface_of_a_binary_product() {
    let rustc_version = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("read Rust compiler version");
    assert!(rustc_version.status.success());
    let rustc_version = String::from_utf8(rustc_version.stdout).expect("Rust compiler version");
    let host_target = rustc_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("Rust compiler host target");
    let context = HawkTestContext::new("basic");
    let graph_dir = tempfile::tempdir().expect("temporary graph directory");
    let unrelated_json = graph_dir.path().join("unrelated.json");
    std::fs::write(&unrelated_json, "{}").expect("write unrelated JSON file");
    let output = context
        .command()
        .arg("--target")
        .arg(host_target)
        .arg("--graph-dir")
        .arg(graph_dir.path())
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    assert!(unrelated_json.exists());
    let stdout = context.normalized_stdout(&output);
    let summary = format!(
        "hawk: 42 finding(s) for `app --bin app --all-features` and workspace non-production targets on target `{host_target}`\n"
    );
    let diagnostics = stdout
        .strip_suffix(&summary)
        .expect("target-specific findings summary");
    insta::assert_snapshot!(diagnostics, @r###"
    warning[hawk::unnecessary_public]: `internal_helper` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:5:1
      |
    5 | pub fn internal_helper() {}
      | ^^^ public declaration
      = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `ContextOptionsAlias` is public but is not reachable from binary `app`
      --> library/src/lib.rs:21:1
       |
    21 | pub type ContextOptionsAlias = ContextOptions;
       | ^^^ public declaration
       = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `PrivateContextOptions` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:57:1
       |
    57 | pub struct PrivateContextOptions;
       | ^^^ public declaration
       = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: public re-export `ReexportedValue` is not required by any compiled cross-crate use; it can be `pub(crate)`
      --> library/src/lib.rs:71:9
       |
    71 | pub use exported::ReexportedValue;
       |         ^^^ public re-export
       = help: change this re-export to `pub(crate) use`

    warning[hawk::unnecessary_public]: `InternalRenderer` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:91:1
       |
    91 | pub trait InternalRenderer {
       | ^^^ public declaration
       = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalRenderResult` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:97:1
       |
    97 | pub struct InternalRenderResult;
       | ^^^ public declaration
       = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalNamespace` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:107:1
        |
    107 | pub struct InternalNamespace;
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalNamespace::LIVE_VALUE` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:110:5
        |
    110 |     pub const LIVE_VALUE: u8 = 1;
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `InternalNamespace::DEAD_VALUE` is public but is not reachable from binary `app`
      --> library/src/lib.rs:112:5
        |
    112 |     pub const DEAD_VALUE: u8 = 2;
        |     ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `InternalNamespace::live_inside_crate` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:114:5
        |
    114 |     pub fn live_inside_crate() {}
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `InternalNamespace::dead_method` is public but is not reachable from binary `app`
      --> library/src/lib.rs:116:5
        |
    116 |     pub fn dead_method() {}
        |     ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `InternalFields` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:124:1
        |
    124 | pub struct InternalFields {
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalFields::constructed` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:125:5
        |
    125 |     pub constructed: u8,
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalFields::projected` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:126:5
        |
    126 |     pub projected: u8,
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalTupleFields` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:129:1
        |
    129 | pub struct InternalTupleFields(pub u8);
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `InternalTupleFields::0` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:129:32
        |
    129 | pub struct InternalTupleFields(pub u8);
        |                                ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `DeadFields` is public but is not reachable from binary `app`
      --> library/src/lib.rs:131:1
        |
    131 | pub struct DeadFields {
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `DeadFields::unused` is public but is not reachable from binary `app`
      --> library/src/lib.rs:132:5
        |
    132 |     pub unused: u8,
        |     ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `ConstructedTuple` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:155:1
        |
    155 | pub struct ConstructedTuple(u8);
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `ConstructedEnum` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:157:1
        |
    157 | pub enum ConstructedEnum {
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `ConstructedEnum::Dead` is a public enum variant but is not reachable from binary `app`
      --> library/src/lib.rs:159:5
        |
    159 |     Dead,
        |     ^^^ public enum variant
        = help: consider removing this variant and its remaining uses

    warning[hawk::dead_public]: `DeadUnion` is public but is not reachable from binary `app`
      --> library/src/lib.rs:162:1
        |
    162 | pub union DeadUnion {
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `DeadUnion::value` is public but is not reachable from binary `app`
      --> library/src/lib.rs:163:5
        |
    163 |     pub value: u8,
        |     ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `ProductEnum::Unused` is a public enum variant but is not reachable from binary `app`
      --> library/src/lib.rs:176:5
        |
    176 |     Unused,
        |     ^^^ public enum variant
        = help: consider removing this variant and its remaining uses

    warning[hawk::dead_public]: `dead_entry` is public but is not reachable from binary `app`
      --> library/src/lib.rs:190:1
        |
    190 | pub fn dead_entry() {
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `dead_helper` is public but is not reachable from binary `app`
      --> library/src/lib.rs:194:1
        |
    194 | pub fn dead_helper() {}
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `dead_code_allowed_helper` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:201:1
        |
    201 | pub fn dead_code_allowed_helper() {}
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: public re-export `dead_export_path` has no target reachable from binary `app`
      --> library/src/lib.rs:236:9
        |
    236 | pub use dead_export_target::dead_export_path;
        |         ^^^ public re-export
        = help: consider restricting this re-export's visibility or removing it

    warning[hawk::unnecessary_public]: public module `internal_outer` is used only within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:244:1
        |
    244 | pub mod internal_outer {
        | ^^^ public module
        = help: change this module to `pub(crate) mod`

    warning[hawk::unnecessary_public]: public module `internal_outer::internal_nested` is used only within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:245:5
        |
    245 |     pub mod internal_nested {
        |     ^^^ public module
        = help: change this module to `pub(crate) mod`

    warning[hawk::dead_public]: public module `dead_outer` has no declaration reachable from binary `app`
      --> library/src/lib.rs:260:1
        |
    260 | pub mod dead_outer {
        | ^^^ public module
        = help: consider restricting this module's visibility or removing it

    warning[hawk::dead_public]: public module `dead_outer::dead_nested` has no declaration reachable from binary `app`
      --> library/src/lib.rs:261:5
        |
    261 |     pub mod dead_nested {}
        |     ^^^ public module
        = help: consider restricting this module's visibility or removing it

    warning[hawk::unnecessary_public]: `test_only_helper` is public but is needed only by tests; it can be `pub(crate)`
      --> library/src/lib.rs:289:1
        |
    289 | pub fn test_only_helper() {}
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `CfgMixedProductFields::used_inside_crate` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:310:5
        |
    310 |     pub used_inside_crate: u8,
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `CfgAlternativeFields` is public but is needed only by tests; it can be `pub(crate)`
      --> library/src/lib.rs:330:1
        |
    330 | pub struct CfgAlternativeFields {
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `CfgAlternativeFields::used_inside_crate` is public but is needed only by tests; it can be `pub(crate)`
      --> library/src/lib.rs:331:5
        |
    331 |     pub used_inside_crate: u8,
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `helper` is public but is needed only by tests; it can be `pub(crate)`
      --> test_support/src/lib.rs:5:1
      |
    5 | pub fn helper() {}
      | ^^^ public declaration
      = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `dead_test_surface` is public but is not reachable from any workspace test
      --> test_support/src/lib.rs:7:1
      |
    7 | pub fn dead_test_surface() {}
      | ^^^ public declaration
      = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `test_entry` is public but is needed only by tests; it can be `pub(crate)`
      --> unit_support/src/lib.rs:9:1
      |
    9 | pub fn test_entry() {
      | ^^^ public declaration
      = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `test_only_helper` is public but is needed only by tests; it can be `pub(crate)`
      --> unit_support/src/lib.rs:14:1
       |
    14 | pub fn test_only_helper() {}
       | ^^^ public declaration
       = help: change this declaration to `pub(crate)`

    warning[hawk::unknown_item]: override for `hawk::dead_public` references unknown item `library::removed_api`
      --> hawk.toml:15:1
       |
    15 | [[override]]
       | ^^^ no matching item was found
      = note: reason: covered by stale selector diagnostic
      = help: remove this override or update its `crate` and `item` selectors

    warning[hawk::unfulfilled_expectation]: expected `hawk::dead_public` for `library::PrivateContextOptions`, but no finding was produced
      --> hawk.toml:22:1
       |
    22 | [[override]]
       | ^^^ unfulfilled expectation
      = note: reason: covered by unfulfilled expectation diagnostic
      = help: remove this expectation or update its `lint` selector

    "###);
}

#[test]
fn production_binary_named_like_a_library_does_not_suppress_its_findings() {
    let context = HawkTestContext::new("production_consumers");
    let output = context.run(&[]);

    context.assert_success(&output);
    insta::assert_snapshot!(
        "multiple_production_consumers",
        context.normalized_stdout(&output)
    );
}

#[test]
fn distinct_spanless_expansions_do_not_keep_a_library_item_live() {
    for binary_name in [None, Some("same")] {
        let context = HawkTestContext::new("spanless_target_collision");
        let mut command = context.command();
        if let Some(binary_name) = binary_name {
            command.env("CARGO_BIN_NAME", binary_name);
        } else {
            command.env_remove("CARGO_BIN_NAME");
        }
        let output = command.output().expect("run cargo-hawk");

        context.assert_success(&output);
        let stdout = context.normalized_stdout(&output);
        assert!(stdout.contains("warning[hawk::dead_public]: `dead_api`"));
        assert!(!stdout.contains("hawk::unnecessary_public"));
    }
}

#[test]
fn production_products_reuse_shared_dependency_compilations() {
    let context = HawkTestContext::new("production_consumers");
    let output = context
        .command()
        .arg("-A")
        .arg("warnings")
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stderr = context.normalized_stderr(&output);
    assert_eq!(
        stderr
            .lines()
            .filter(|line| line.trim_start().starts_with("Checking library "))
            .count(),
        2,
        "the shared library should compile once for production and once for non-production:\n{stderr}"
    );
}

#[test]
fn rejects_duplicate_workspace_library_crate_names() {
    let context = HawkTestContext::new("duplicate_library_names");
    let output = context.run(&[]);

    assert!(!output.status.success());
    let stderr = context.normalized_stderr(&output);
    assert!(stderr.contains(
        "conflicting names: `shared` (`library-a`, `library-b`). Hawk identifies graph definitions and fix targets by crate name"
    ));
    assert!(stderr.contains("give each `[lib]` target a unique `name`"));
}

#[test]
fn feature_profiles_union_reachability_across_configurations() {
    let context = HawkTestContext::new("feature_profiles");
    let graph_dir = tempfile::tempdir().expect("temporary graph directory");
    let output = context
        .command()
        .arg("--graph-dir")
        .arg(graph_dir.path())
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(
        !stdout.contains("`fallback_api` is public"),
        "API used by the default-disabled profile was diagnosed:\n{stdout}"
    );
    assert!(stdout.contains("`unused_api` is public"));
    assert!(stdout.contains("`app --bin app` across 2 feature profiles"));

    let run_dir = fs::read_dir(graph_dir.path())
        .expect("read graph directory")
        .map(|entry| entry.expect("read graph entry"))
        .find(|entry| entry.file_type().expect("read graph entry type").is_dir())
        .expect("retained graph run directory")
        .path();
    for profile in ["0-all", "1-fallback"] {
        let production_dir = run_dir
            .join("feature-profiles")
            .join(profile)
            .join("production");
        assert!(
            fs::read_dir(&production_dir)
                .expect("read feature-profile graph directory")
                .map(|entry| entry.expect("read graph entry").path())
                .any(|path| path
                    .extension()
                    .is_some_and(|extension| extension == "json")),
            "no fragments retained in {}",
            production_dir.display()
        );
    }
}

#[test]
fn rejects_fixes_with_multiple_feature_profiles() {
    let context = HawkTestContext::new("feature_profiles");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    assert!(!output.status.success());
    assert!(
        context
            .normalized_stderr(&output)
            .contains("--fix does not support multiple feature profiles")
    );
}

#[test]
fn requires_a_configured_production_binary() {
    let context = HawkTestContext::new("basic");
    let configuration = tempfile::NamedTempFile::new().expect("temporary empty configuration");
    let output = context
        .command_with_color("always")
        .arg("--config")
        .arg(configuration.path())
        .output()
        .expect("run cargo-hawk");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains('\u{1b}'));
    let stderr = anstream::adapter::strip_str(&stderr).to_string();
    assert!(stderr.contains("error: no applicable production binaries configured"));
}

#[test]
fn ordered_lint_levels_control_severity_and_exit_status() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&[
        "-D",
        "warnings",
        "-W",
        "hawk::unnecessary_public",
        "-A",
        "hawk::unknown_item",
    ]);

    assert!(
        !output.status.success(),
        "denied diagnostic did not fail:\n{}",
        context.normalized_stdout(&output)
    );
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("error[hawk::dead_public]"));
    assert!(stdout.contains("warning[hawk::unnecessary_public]"));
    assert!(stdout.contains("error[hawk::unfulfilled_expectation]"));
    assert!(!stdout.contains("hawk::unknown_item"));
    assert!(stdout.contains("hawk: 41 finding(s)"));
}

#[test]
fn emits_versioned_json_diagnostics_and_keeps_cargo_output_on_stderr() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&[
        "--output-format=json",
        "-D",
        "warnings",
        "-W",
        "hawk::unnecessary_public",
        "-A",
        "hawk::unknown_item",
    ]);

    assert!(
        !output.status.success(),
        "denied JSON diagnostics did not fail"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["summary"]["diagnostic_count"], 41);
    assert_eq!(
        report["summary"]["production"],
        serde_json::json!([{"package": "app", "binary": "app"}])
    );
    assert_eq!(
        report["summary"]["feature_profiles"],
        serde_json::json!(["all-features"])
    );
    assert_eq!(report["summary"]["includes_non_production_targets"], true);
    assert!(
        report["summary"]["target"]
            .as_str()
            .is_some_and(|target| !target.is_empty())
    );

    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert_eq!(diagnostics.len(), 41);

    let dead_entry = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "dead_entry")
        .expect("dead_entry diagnostic");
    assert_eq!(dead_entry["category"], "finding");
    assert_eq!(dead_entry["code"], "hawk::dead_public");
    assert_eq!(dead_entry["severity"], "error");
    assert_eq!(dead_entry["kind"], "dead_public");
    assert_eq!(dead_entry["identity"]["package"], "library");
    assert_eq!(dead_entry["identity"]["crate"], "library");
    assert_eq!(dead_entry["identity"]["kind"], "function");
    assert_eq!(dead_entry["identity"]["parent"], serde_json::Value::Null);
    assert_eq!(
        dead_entry["identity"]["module_scope"],
        serde_json::json!([])
    );
    assert_eq!(
        dead_entry["identity"]["id"],
        "v1|7:library|7:library|10:dead_entry|8:function|6:source|18:library/src/lib.rs|190|1"
    );
    assert!(
        dead_entry["identity"]["compiler_id"]
            .as_str()
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    );
    assert_eq!(
        dead_entry["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 3353,
            "byte_end": 3395,
            "line": 190,
            "column": 1,
            "end_line": 192,
            "end_column": 2,
        })
    );
    assert_eq!(dead_entry["expansion"], serde_json::Value::Null);
    assert_eq!(dead_entry["test_only"], false);
    assert_eq!(dead_entry["test_compiled_only"], false);

    let dead_field = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "DeadFields::unused")
        .expect("dead field diagnostic");
    assert_eq!(dead_field["identity"]["kind"], "field");
    assert_eq!(dead_field["identity"]["parent"], "DeadFields");
    assert_eq!(
        dead_field["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 2327,
            "byte_end": 2342,
            "line": 132,
            "column": 5,
            "end_line": 132,
            "end_column": 20,
        })
    );

    let dead_variant = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "ProductEnum::Unused")
        .expect("dead enum-variant diagnostic");
    assert_eq!(
        dead_variant["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 3127,
            "byte_end": 3134,
            "line": 176,
            "column": 5,
            "end_line": 176,
            "end_column": 12,
        })
    );

    let test_only = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "test_only_helper")
        .expect("test-only diagnostic");
    assert_eq!(test_only["severity"], "warning");
    assert_eq!(test_only["test_only"], true);

    let config = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["code"] == "hawk::unfulfilled_expectation")
        .expect("configuration diagnostic");
    assert_eq!(config["category"], "configuration");
    assert_eq!(config["severity"], "error");
    assert_eq!(config["lint"], "hawk::dead_public");
    assert_eq!(config["identity"]["crate"], "library");
    assert_eq!(config["identity"]["item"], "PrivateContextOptions");
    assert_eq!(
        config["location"],
        serde_json::json!({"file": "hawk.toml", "line": 22, "column": 1})
    );
    assert_eq!(
        config["reason"],
        "covered by unfulfilled expectation diagnostic"
    );

    let stderr = context.normalized_stderr(&output);
    assert!(stderr.contains("Finished `dev` profile"));
}

#[test]
fn emits_an_empty_json_report_when_all_warnings_are_allowed() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&["--output-format=json", "-A", "warnings"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["schema_version"], 3);
    assert_eq!(report["summary"]["diagnostic_count"], 0);
    assert_eq!(report["diagnostics"], serde_json::json!([]));
}

#[test]
fn json_stable_diagnostic_ids_ignore_target_compilation_metadata() {
    let context = HawkTestContext::new("dead_public_fixes");
    let reports = ["hawk-target-a", "hawk-target-b"].map(|metadata| {
        let output = context
            .command()
            .arg("--output-format=json")
            .env("CARGO_ENCODED_RUSTFLAGS", format!("-Cmetadata={metadata}"))
            .env_remove("RUSTFLAGS")
            .output()
            .expect("run cargo-hawk with target-specific compilation metadata");
        context.assert_success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .expect("stdout contains one JSON report")
    });
    let identities = reports.each_ref().map(|report| {
        report["diagnostics"]
            .as_array()
            .expect("diagnostics is an array")
            .iter()
            .find(|diagnostic| diagnostic["identity"]["item"] == "dead_api")
            .expect("dead_api diagnostic")["identity"]
            .clone()
    });

    assert_eq!(identities[0]["id"], identities[1]["id"]);
    assert_ne!(identities[0]["compiler_id"], identities[1]["compiler_id"]);
}

#[test]
fn json_uses_the_host_target_when_cargo_configures_another_target() {
    let rustc_version = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("read Rust compiler version");
    assert!(rustc_version.status.success());
    let rustc_version = String::from_utf8(rustc_version.stdout).expect("Rust compiler version");
    let host_target = rustc_version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("Rust compiler host target");
    let host_arch = host_target
        .split_once('-')
        .expect("host target has an architecture")
        .0;
    let installed_targets = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .expect("list installed Rust targets");
    assert!(installed_targets.status.success());
    let installed_targets =
        String::from_utf8(installed_targets.stdout).expect("installed Rust targets");
    let configured_target = installed_targets
        .lines()
        .find(|target| *target != host_target)
        .unwrap_or(host_target);
    let context = HawkTestContext::new("dead_public_fixes");
    let cargo_config = context.workspace().join(".cargo");
    fs::create_dir(&cargo_config).expect("create Cargo configuration directory");
    fs::write(
        cargo_config.join("config.toml"),
        format!("[build]\ntarget = \"{configured_target}\"\n"),
    )
    .expect("write Cargo target configuration");
    fs::write(
        context.workspace().join("library/src/lib.rs"),
        format!("#[cfg(target_arch = \"{host_arch}\")]\npub fn host_only() {{}}\n"),
    )
    .expect("write target-specific library source");
    fs::write(
        context.workspace().join("hawk.toml"),
        format!(
            "[[production]]\npackage = \"app\"\nbin = \"app\"\nreason = \"binary product under analysis\"\n\n[[override]]\nlint = \"hawk::dead_public\"\ncrate = \"library\"\nitem = \"host_only\"\nlevel = \"allow\"\nreason = \"host-only declaration is intentionally retained\"\ntarget = 'cfg(target_arch = \"{host_arch}\")'\n"
        ),
    )
    .expect("write Hawk configuration");

    let output = context
        .command()
        .arg("--output-format=json")
        .env("CARGO_BUILD_TARGET", configured_target)
        .output()
        .expect("run cargo-hawk with configured Cargo target");

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["summary"]["target"], host_target);
    assert_eq!(report["summary"]["diagnostic_count"], 0);
    assert_eq!(report["diagnostics"], serde_json::json!([]));
}

#[test]
fn json_locations_include_complete_documented_declarations() {
    let context = HawkTestContext::new("dead_public_fixes");
    fs::write(
        context.workspace().join("library/src/lib.rs"),
        "#![deny(dead_code)]\n\n/// A retained source-spanned doc comment.\n#[deprecated(note = \"exercise a source-spanned attribute\")]\n#[inline]\npub fn dead_api() {}\n\n#[must_use]\npub fn must_use_api() -> bool { true }\n\n#[doc(hidden)]\npub struct DeadDocHidden;\n\n#[cold]\npub fn cold_api() {}\n",
    )
    .expect("write documented declaration");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "dead_api")
        .expect("dead_api diagnostic");
    assert_eq!(
        diagnostic["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 21,
            "byte_end": 154,
            "line": 3,
            "column": 1,
            "end_line": 6,
            "end_column": 21,
        })
    );
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    let must_use = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "must_use_api")
        .expect("must_use_api diagnostic");
    assert_eq!(
        must_use["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 156,
            "byte_end": 206,
            "line": 8,
            "column": 1,
            "end_line": 9,
            "end_column": 39,
        })
    );
    let doc_hidden = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "DeadDocHidden")
        .expect("DeadDocHidden diagnostic");
    assert_eq!(
        doc_hidden["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 208,
            "byte_end": 248,
            "line": 11,
            "column": 1,
            "end_line": 12,
            "end_column": 26,
        })
    );
    let cold = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "cold_api")
        .expect("cold_api diagnostic");
    assert_eq!(
        cold["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "line": 15,
            "column": 1,
        })
    );
}

#[test]
fn json_locations_include_grouped_reexport_separators() {
    let context = HawkTestContext::new("grouped_reexport_fixes");
    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "ProductionOnly")
        .expect("ProductionOnly re-export diagnostic");
    assert_eq!(
        diagnostic["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 270,
            "byte_end": 285,
            "line": 17,
            "column": 27,
            "end_line": 17,
            "end_column": 42,
        })
    );
}

#[test]
fn json_locations_include_separators_after_trivia_and_can_be_deleted() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut source = "pub struct DeadFields {\n    pub unused: u8 // field separator\n    ,\n    pub remaining: u8,\n}\n\npub enum DeadEnum {\n    Unused /* variant separator */ ,\n    Remaining,\n}\n\nmod exports {\n    pub struct Unused;\n    pub struct Remaining;\n}\n\npub use exports::{Unused /* re-export separator */ , Remaining};\n".to_string();
    fs::write(&library_path, &source).expect("write declarations with separated commas");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    let ranges = [
        ("DeadFields::unused", "field", 28, 67, 2, 5, 3, 6),
        ("DeadEnum::Unused", "enum_variant", 118, 150, 8, 5, 8, 37),
        ("Unused", "reexport", 253, 287, 17, 19, 17, 53),
    ]
    .map(
        |(item, kind, byte_start, byte_end, line, column, end_line, end_column)| {
            let diagnostic = diagnostics
                .iter()
                .find(|diagnostic| {
                    diagnostic["identity"]["item"] == item && diagnostic["identity"]["kind"] == kind
                })
                .unwrap_or_else(|| panic!("{item} {kind} diagnostic"));
            assert_eq!(
                diagnostic["location"],
                serde_json::json!({
                    "file": "library/src/lib.rs",
                    "byte_start": byte_start,
                    "byte_end": byte_end,
                    "line": line,
                    "column": column,
                    "end_line": end_line,
                    "end_column": end_column,
                })
            );
            byte_start..byte_end
        },
    );
    let mut ranges = ranges;
    ranges.sort_by_key(|range| range.start);
    for range in ranges.into_iter().rev() {
        source.replace_range(range, "");
    }
    fs::write(&library_path, source).expect("delete diagnostic ranges");

    let output = context
        .cargo()
        .args(["check", "--workspace", "--locked"])
        .arg("--target-dir")
        .arg(context.target_dir())
        .output()
        .expect("compile declarations after deleting diagnostic ranges");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn json_byte_offsets_delete_unicode_declarations() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut source =
        "\u{feff}pub struct DeadFields {\r\n    /* 😀é */ pub unused: u8 /* 😀é */ ,\r\n    pub remaining: u8,\r\n}\r\n"
            .to_string();
    fs::write(&library_path, &source).expect("write Unicode declaration");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["schema_version"], 3);
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "DeadFields::unused")
        .expect("dead field diagnostic");
    assert_eq!(
        diagnostic["location"],
        serde_json::json!({
            "file": "library/src/lib.rs",
            "byte_start": 45,
            "byte_end": 74,
            "line": 2,
            "column": 14,
            "end_line": 2,
            "end_column": 39,
        })
    );
    let location = &diagnostic["location"];
    let byte_start = usize::try_from(location["byte_start"].as_u64().expect("byte_start"))
        .expect("byte_start fits in usize");
    let byte_end = usize::try_from(location["byte_end"].as_u64().expect("byte_end"))
        .expect("byte_end fits in usize");
    assert_eq!(
        source.get(byte_start..byte_end),
        Some("pub unused: u8 /* 😀é */ ,")
    );
    source.replace_range(byte_start..byte_end, "");
    fs::write(&library_path, source).expect("delete Unicode declaration range");

    let output = context
        .cargo()
        .args(["check", "--workspace", "--locked"])
        .arg("--target-dir")
        .arg(context.target_dir())
        .output()
        .expect("compile declarations after deleting Unicode range");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn json_still_emits_a_report_when_stderr_is_closed() {
    let context = HawkTestContext::new("dead_public_fixes");
    let (reader, writer) = std::io::pipe().expect("create stderr pipe");
    drop(reader);
    let output = context
        .command()
        .arg("--output-format=json")
        .stderr(writer)
        .output()
        .expect("run cargo-hawk with closed stderr");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["schema_version"], 3);
}

#[cfg(unix)]
#[test]
fn json_returns_a_normal_cargo_failure_when_stderr_is_closed() {
    let context = HawkTestContext::new("dead_public_fixes");
    fs::write(
        context.workspace().join("library/src/lib.rs"),
        "compile_error!(\"EXPECTED-JSON-CARGO-FAILURE\");\n",
    )
    .expect("write failing library source");
    let (reader, writer) = std::io::pipe().expect("create stderr pipe");
    drop(reader);
    let output = context
        .command()
        .arg("--output-format=json")
        .stderr(writer)
        .output()
        .expect("run cargo-hawk with closed stderr");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn json_closes_inherited_cargo_output_after_analysis() {
    use std::time::{Duration, Instant};

    let context = HawkTestContext::new("dead_public_fixes");
    let shim_directory = tempfile::tempdir().expect("temporary Cargo shim directory");
    let shim = shim_directory.path().join("cargo");
    let shim_source = shim_directory.path().join("cargo.rs");
    let helper_done = shim_directory.path().join("helper-done");
    fs::write(
        &shim_source,
        format!(
            "use std::env;\nuse std::io::Write as _;\nuse std::process::{{Command, Stdio}};\nuse std::time::{{Duration, Instant}};\nfn main() {{\n    let mut args = env::args_os().skip(1);\n    if args.next().as_deref() == Some(std::ffi::OsStr::new(\"--hawk-test-helper\")) {{\n        let deadline = Instant::now() + Duration::from_secs(10);\n        let mut stderr = std::io::stderr().lock();\n        while Instant::now() < deadline {{\n            match stderr.write_all(b\"BACKGROUND-CARGO-HELPER-WRITE-0123456789abcdefghijklmnopqrstuvwxyz\\n\") {{\n                Ok(()) => {{}},\n                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {{\n                    std::fs::write({:?}, \"closed\").unwrap();\n                    return;\n                }}\n                Err(error) => panic!(\"unexpected helper output error: {{error}}\"),\n            }}\n        }}\n        std::fs::write({:?}, \"timed-out\").unwrap();\n        return;\n    }}\n    let args = env::args_os().skip(1).collect::<Vec<_>>();\n    let status = Command::new({:?}).args(&args).status().unwrap();\n    if env::var_os(\"HAWK_OUTPUT_DIR\").is_some() && args.iter().any(|argument| argument == \"--bin\") {{\n        eprintln!(\"EXPECTED-CARGO-RELAY-OUTPUT: {{}}\", \"x\".repeat(20_000));\n        Command::new(env::current_exe().unwrap()).arg(\"--hawk-test-helper\").stdin(Stdio::null()).spawn().unwrap();\n    }}\n    std::process::exit(status.code().unwrap_or(1));\n}}\n",
            helper_done,
            helper_done,
            env!("CARGO")
        ),
    )
    .expect("write Cargo shim source");
    let compiler = Command::new("rustc")
        .arg(&shim_source)
        .arg("--edition=2024")
        .arg("-o")
        .arg(&shim)
        .output()
        .expect("compile Cargo shim");
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );

    let mut paths = vec![shim_directory.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is set"),
    ));
    let output = context
        .command()
        .arg("--output-format=json")
        .env("PATH", std::env::join_paths(paths).expect("construct PATH"))
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("EXPECTED-CARGO-RELAY-OUTPUT:"), "{stderr}");
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("stdout contains one JSON report");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !helper_done.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        fs::read_to_string(&helper_done).expect("background Cargo helper finished"),
        "closed"
    );
}

#[cfg(unix)]
#[test]
fn json_does_not_wait_for_background_cargo_helpers_holding_output_pipes() {
    use std::time::{Duration, Instant};

    let context = HawkTestContext::new("dead_public_fixes");
    let shim_directory = tempfile::tempdir().expect("temporary Cargo shim directory");
    let shim = shim_directory.path().join("cargo");
    let shim_source = shim_directory.path().join("cargo.rs");
    let keep_alive = shim_directory.path().join("keep-alive");
    fs::write(&keep_alive, "").expect("write helper marker");
    fs::write(
        &shim_source,
        format!(
            "use std::env;\nuse std::process::{{Command, Stdio}};\nuse std::time::Duration;\nfn main() {{\n    let mut args = env::args_os().skip(1);\n    if args.next().as_deref() == Some(std::ffi::OsStr::new(\"--hawk-test-helper\")) {{\n        while std::path::Path::new({:?}).exists() {{ std::thread::sleep(Duration::from_millis(25)); }}\n        return;\n    }}\n    let status = Command::new({:?}).args(env::args_os().skip(1)).status().unwrap();\n    if env::var_os(\"HAWK_OUTPUT_DIR\").is_some() {{\n        Command::new(env::current_exe().unwrap()).arg(\"--hawk-test-helper\").stdin(Stdio::null()).spawn().unwrap();\n    }}\n    std::process::exit(status.code().unwrap_or(1));\n}}\n",
            keep_alive,
            env!("CARGO")
        ),
    )
    .expect("write Cargo shim source");
    let compiler = Command::new("rustc")
        .arg(&shim_source)
        .arg("--edition=2024")
        .arg("-o")
        .arg(&shim)
        .output()
        .expect("compile Cargo shim");
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );

    let mut paths = vec![shim_directory.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is set"),
    ));
    let mut child = context
        .command()
        .arg("--output-format=json")
        .env("PATH", std::env::join_paths(paths).expect("construct PATH"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cargo-hawk");
    let deadline = Instant::now() + Duration::from_mins(1);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll cargo-hawk") {
            break status;
        }
        if Instant::now() >= deadline {
            fs::remove_file(&keep_alive).expect("release background helper");
            let _ = child.kill();
            panic!("cargo-hawk waited for a background Cargo helper holding output pipes");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (release_sender, release_receiver) = std::sync::mpsc::sync_channel(1);
    let release_path = keep_alive.clone();
    let release = std::thread::spawn(move || {
        if release_receiver
            .recv_timeout(Duration::from_secs(5))
            .is_err()
        {
            fs::remove_file(release_path).expect("release blocked background helper");
            return false;
        }
        true
    });
    let output = child.wait_with_output().expect("read cargo-hawk output");
    let _ = release_sender.send(());
    let completed_before_release = release.join().expect("join helper-release watchdog");
    if completed_before_release {
        fs::remove_file(&keep_alive).expect("release background helper");
    }

    assert!(
        completed_before_release,
        "cargo-hawk left background Cargo helpers holding captured output pipes"
    );

    assert!(
        status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout)
        .expect("stdout contains one JSON report");
}

#[cfg(unix)]
#[test]
fn json_replays_failing_cargo_output_while_background_helpers_write() {
    let context = HawkTestContext::new("dead_public_fixes");
    fs::write(
        context.workspace().join("library/src/lib.rs"),
        "compile_error!(\"EXPECTED-RUSTC-FAILURE\");\n",
    )
    .expect("write failing library source");
    let shim_directory = tempfile::tempdir().expect("temporary Cargo shim directory");
    let shim = shim_directory.path().join("cargo");
    let shim_source = shim_directory.path().join("cargo.rs");
    let keep_alive = shim_directory.path().join("keep-alive");
    fs::write(&keep_alive, "").expect("write helper marker");
    fs::write(
        &shim_source,
        format!(
            "use std::env;\nuse std::io::Write as _;\nuse std::process::{{Command, Stdio}};\nuse std::time::Duration;\nfn main() {{\n    let mut args = env::args_os().skip(1);\n    if args.next().as_deref() == Some(std::ffi::OsStr::new(\"--hawk-test-helper\")) {{\n        let mut stderr = std::io::stderr().lock();\n        while std::path::Path::new({:?}).exists() {{\n            stderr.write_all(b\"BACKGROUND-CARGO-HELPER-WRITE-0123456789abcdefghijklmnopqrstuvwxyz\\n\").unwrap();\n        }}\n        return;\n    }}\n    let status = Command::new({:?}).args(env::args_os().skip(1)).status().unwrap();\n    if env::var_os(\"HAWK_OUTPUT_DIR\").is_some() && !status.success() {{\n        println!(\"EXPECTED-CARGO-STDOUT-FAILURE\");\n        eprintln!(\"EXPECTED-CARGO-STDERR-FAILURE\");\n        Command::new(env::current_exe().unwrap()).arg(\"--hawk-test-helper\").stdin(Stdio::null()).spawn().unwrap();\n        std::thread::sleep(Duration::from_millis(50));\n    }}\n    std::process::exit(status.code().unwrap_or(1));\n}}\n",
            keep_alive,
            env!("CARGO")
        ),
    )
    .expect("write Cargo shim source");
    let compiler = Command::new("rustc")
        .arg(&shim_source)
        .arg("--edition=2024")
        .arg("-o")
        .arg(&shim)
        .output()
        .expect("compile Cargo shim");
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );

    let mut paths = vec![shim_directory.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is set"),
    ));
    let output = context
        .command()
        .arg("--output-format=json")
        .env("PATH", std::env::join_paths(paths).expect("construct PATH"))
        .output()
        .expect("run cargo-hawk");
    fs::remove_file(&keep_alive).expect("release background helper");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("EXPECTED-RUSTC-FAILURE"), "{stderr}");
    assert!(stderr.contains("EXPECTED-CARGO-STDOUT-FAILURE"), "{stderr}");
    assert!(stderr.contains("EXPECTED-CARGO-STDERR-FAILURE"), "{stderr}");
    assert!(
        stderr.contains("instrumented Cargo check failed with exit status: 101"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn json_replays_failing_doctest_stdout_and_stderr() {
    let context = HawkTestContext::new("dead_public_fixes");
    let shim_directory = tempfile::tempdir().expect("temporary Cargo shim directory");
    let shim = shim_directory.path().join("cargo");
    let shim_source = shim_directory.path().join("cargo.rs");
    fs::write(
        &shim_source,
        format!(
            "use std::env;\nuse std::process::Command;\nfn main() {{\n    if env::var_os(\"HAWK_OUTPUT_DIR\").is_some() && env::args_os().any(|argument| argument == \"--doc\") {{\n        println!(\"EXPECTED-DOCTEST-STDOUT-FAILURE\");\n        eprintln!(\"EXPECTED-DOCTEST-STDERR-FAILURE\");\n        std::process::exit(72);\n    }}\n    let status = Command::new({:?}).args(env::args_os().skip(1)).status().unwrap();\n    std::process::exit(status.code().unwrap_or(1));\n}}\n",
            env!("CARGO")
        ),
    )
    .expect("write Cargo shim source");
    let compiler = Command::new("rustc")
        .arg(&shim_source)
        .arg("--edition=2024")
        .arg("-o")
        .arg(&shim)
        .output()
        .expect("compile Cargo shim");
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );

    let mut paths = vec![shim_directory.path().to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").expect("PATH is set"),
    ));
    let output = context
        .command()
        .arg("--output-format=json")
        .env("PATH", std::env::join_paths(paths).expect("construct PATH"))
        .output()
        .expect("run cargo-hawk");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("EXPECTED-DOCTEST-STDOUT-FAILURE"),
        "{stderr}"
    );
    assert!(
        stderr.contains("EXPECTED-DOCTEST-STDERR-FAILURE"),
        "{stderr}"
    );
    assert!(
        stderr.contains("instrumented Cargo test failed with exit status: 72"),
        "{stderr}"
    );
}

#[test]
fn reports_operational_json_errors_on_stderr() {
    let context = HawkTestContext::new("basic");
    let configuration = tempfile::NamedTempFile::new().expect("temporary empty configuration");
    let output = context
        .command()
        .arg("--output-format=json")
        .arg("--config")
        .arg(configuration.path())
        .output()
        .expect("run cargo-hawk");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        context
            .normalized_stderr(&output)
            .contains("error: no applicable production binaries configured")
    );
}

#[test]
fn applies_visibility_fixes_through_cargo_fix() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`dead_entry` is public"));
    assert!(stdout.contains("`ProductEnum::Unused`"));
    assert!(!stdout.contains("`internal_helper`"));

    let library = fs::read_to_string(context.workspace().join("library/src/lib.rs"))
        .expect("read fixed source");
    assert!(library.contains("fn internal_helper() {}"));
    assert!(library.contains("pub(crate) use exported::ReexportedValue;"));
    assert!(library.contains("pub const DEAD_VALUE: u8 = 2;"));
    assert!(library.contains("constructed: u8,"));
    assert!(library.contains("pub mod dead_outer {"));
    assert!(library.contains("pub fn dead_code_allowed_entry() {"));
    assert!(library.contains("fn dead_code_allowed_helper() {}"));
    assert!(library.contains("pub enum ProductEnum {"));
    assert!(library.contains("pub fn integration_test_support() {"));
    assert!(library.contains("fn test_only_helper() {}"));
    assert!(library.contains("use std::fmt::Debug;"));

    let test_support = fs::read_to_string(context.workspace().join("test_support/src/lib.rs"))
        .expect("read fixed test-support source");
    assert!(test_support.contains("pub fn entry() {"));
    assert!(test_support.contains("fn helper() {}"));
    assert!(test_support.contains("pub fn dead_test_surface() {}"));

    let unit_support = fs::read_to_string(context.workspace().join("unit_support/src/lib.rs"))
        .expect("read fixed unit-test source");
    assert!(unit_support.contains("pub fn product_entry() {}"));
    assert!(unit_support.contains("pub fn not_exported() {}"));
    assert!(unit_support.contains("fn test_entry() {"));
    assert!(unit_support.contains("fn test_only_helper() {}"));
}

#[test]
fn applies_multiple_fix_passes_in_a_clean_git_repository() {
    let context = HawkTestContext::new("basic");
    context.initialize_git();
    let output = context.run(&["--fix"]);

    context.assert_success(&output);
}

#[test]
fn dead_public_findings_are_not_fixed_into_dead_code_errors() {
    let context = HawkTestContext::new("dead_public_fixes");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`dead_api` is public"));

    let library =
        fs::read_to_string(context.workspace().join("library/src/lib.rs")).expect("read source");
    assert!(library.contains("pub fn dead_api() {}"));
}

#[test]
fn benchmark_consumers_preserve_required_public_visibility() {
    let context = HawkTestContext::new("non_production_targets");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(!stdout.contains("`bench_api` is public"));
    assert!(
        !stdout.contains("`BenchMode::OnlyBench`"),
        "benchmark-executed variant was diagnosed:\n{stdout}"
    );
    assert!(stdout.contains("`unused` is public"));
}

#[test]
fn exported_symbols_are_treated_as_external_roots() {
    let context = HawkTestContext::new("exported_symbols");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(!stdout.contains("warning[hawk::dead_public]: `exported_callback` is public"));
    assert!(!stdout.contains("warning[hawk::dead_public]: `renamed_callback` is public"));
    assert!(stdout.contains("warning[hawk::unnecessary_public]: `exported_callback` is public"));
    assert!(stdout.contains("warning[hawk::unnecessary_public]: `renamed_callback` is public"));
}

#[test]
fn doctest_consumers_preserve_required_public_visibility_during_fixes() {
    let context = HawkTestContext::new("doctest_consumers");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    assert!(
        context
            .normalized_stdout(&output)
            .contains("`unused` is public")
    );

    let doctest = context
        .cargo()
        .arg("test")
        .arg("--doc")
        .arg("--manifest-path")
        .arg(context.workspace().join("Cargo.toml"))
        .arg("--package")
        .arg("library")
        .arg("--locked")
        .arg("--target-dir")
        .arg(context.target_dir())
        .output()
        .expect("run doctests after fixes");
    assert!(
        doctest.status.success(),
        "doctests failed after cargo-hawk fixes:\n{}",
        String::from_utf8_lossy(&doctest.stderr)
    );

    let library = fs::read_to_string(context.workspace().join("library/src/lib.rs"))
        .expect("read fixed source");
    assert!(library.contains("pub fn doc_api() {}"));
    assert!(library.contains("pub fn unused() {}"));
}

#[test]
fn fixes_grouped_public_reexports_only_when_all_aliases_are_safe() {
    let context = HawkTestContext::new("grouped_reexport_fixes");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("public re-export `Narrow`"));

    let library = fs::read_to_string(context.workspace().join("library/src/lib.rs"))
        .expect("read fixed source");
    assert!(library.contains("pub use exported::{Kept, Narrow};"));
    assert!(library.contains("pub(crate) use split_consumers::{ProductionOnly, TestOnly};"));
}

#[test]
fn fixes_only_the_matching_cfg_alternative_declaration() {
    let context = HawkTestContext::new("cfg_alternative_fixes");
    context.initialize_git();
    let output = context.run(&["--fix"]);

    context.assert_success(&output);
    insta::assert_snapshot!(
        "cfg_alternative_fix_output",
        context.normalized_stdout(&output)
    );
    insta::assert_snapshot!("cfg_alternative_fix_diff", context.git_diff());
}

#[test]
fn expectation_matches_cfg_alternatives_as_one_logical_item() {
    let context = HawkTestContext::new("cfg_alternative_fixes");
    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "binary product under analysis"

[[override]]
lint = "hawk::unnecessary_public"
crate = "library"
item = "dual"
level = "expect"
reason = "test-only alternative remains intentionally public"
"#,
    )
    .expect("write temporary configuration");
    let output = context
        .command()
        .arg("--config")
        .arg(configuration.path())
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(!stdout.contains("hawk::ambiguous_item"));
    assert!(!stdout.contains("hawk::unfulfilled_expectation"));
    assert!(!stdout.contains("hawk::unnecessary_public"));
    assert!(stdout.contains("hawk: 0 finding(s)"));
}

#[test]
fn override_does_not_suppress_a_same_named_item_in_another_crate() {
    let context = HawkTestContext::new("ambiguous_packages");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert_eq!(
        stdout
            .matches("warning[hawk::dead_public]: `duplicate`")
            .count(),
        1,
        "only the unselected package declaration should remain:\n{stdout}"
    );
    assert!(!stdout.contains("left/src/lib.rs:3:1"));
    assert!(stdout.contains("right/src/lib.rs:3:1"));
    assert!(!stdout.contains("hawk::ambiguous_item"));
    assert!(!stdout.contains("hawk::unfulfilled_expectation"));
    assert!(stdout.contains("hawk: 1 finding(s)"));
}

#[test]
fn removes_unnecessary_restricted_visibility_by_default() {
    let context = HawkTestContext::new("crate_visibility_fixes");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);

    let library = fs::read_to_string(context.workspace().join("library/src/lib.rs"))
        .expect("read fixed source");
    assert!(library.contains("pub(crate) fn run() {"));
    assert!(library.contains("    fn private_helper() {}"));
    assert!(library.contains("    fn private_parent_visible_helper() {}"));
    assert!(library.contains("    fn private_formatted_helper() {}"));
    assert!(library.contains("    fn parent_helper() {}"));
    assert!(library.contains("        pub(crate) fn call_parent_helper() {"));
    assert!(library.contains("    pub(crate) mod api {"));
    assert!(library.contains("    pub(crate) struct ApprovalKey;"));
}

#[test]
fn path_modules_preserve_visibility_required_by_other_targets() {
    let context = HawkTestContext::new("path_module_fixes");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);

    let shared = fs::read_to_string(context.workspace().join("library/src/shared.rs"))
        .expect("read fixed source");
    assert!(shared.contains("pub struct Shared"));
    assert!(shared.contains("    pub(crate) value: u8,"));
}

#[test]
fn repeated_path_modules_only_apply_shared_safe_visibility_fixes() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/path_module_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    fs::create_dir_all(workspace.path().join("library/src/first_parent"))
        .expect("create first module directory");
    fs::create_dir_all(workspace.path().join("library/src/second_parent"))
        .expect("create second module directory");
    fs::write(
        workspace.path().join("hawk.toml"),
        r#"preserve-uniform-field-visibility = true

[[production]]
package = "app"
bin = "app"
reason = "shipped application binary"
"#,
    )
    .expect("write Hawk configuration");
    fs::write(
        workspace.path().join("library/src/lib.rs"),
        r#"mod first_parent {
    #[path = "../shared.rs"]
    pub(crate) mod first;

    pub(crate) fn call_second() {
        crate::second_parent::second::cross_helper();
        let value: crate::second_parent::second::Shared = unsafe { std::mem::zeroed() };
        let _ = value.value;
    }
}
mod second_parent {
    #[path = "../shared.rs"]
    pub(crate) mod second;
}

pub fn entry() {
    first_parent::first::exercise();
    second_parent::second::exercise();
    first_parent::call_second();
}
"#,
    )
    .expect("write library source");
    fs::write(
        workspace.path().join("library/src/shared.rs"),
        r"pub struct Shared {
    pub(crate) value: u8,
    pub(crate) spare: u8,
}

pub(crate) fn exercise() {
    local_helper();
}

pub(crate) fn local_helper() {}

pub(crate) fn cross_helper() {}
",
    )
    .expect("write shared source");
    fs::write(workspace.path().join("library/tests/shared.rs"), "")
        .expect("clear unrelated integration test");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("check")
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .arg("-W")
        .arg("hawk::unnecessary_crate_visibility")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let shared = fs::read_to_string(workspace.path().join("library/src/shared.rs"))
        .expect("read fixed source");
    assert!(shared.contains("    pub(crate) value: u8,"));
    assert!(shared.contains("    pub(crate) spare: u8,"));
    assert!(shared.contains("fn local_helper() {}"));
    assert!(shared.contains("pub(crate) fn cross_helper() {}"));
}

#[test]
fn narrows_crate_visibility_to_the_required_module_scope_when_enabled() {
    let context = HawkTestContext::new("crate_visibility_fixes");
    let output = context.run(&[
        "--fix",
        "--allow-no-vcs",
        "-W",
        "hawk::unnecessary_crate_visibility",
    ]);

    context.assert_success(&output);

    let library = fs::read_to_string(context.workspace().join("library/src/lib.rs"))
        .expect("read fixed source");
    assert!(library.contains("pub(super) fn run() {"));
    assert!(library.contains("    fn private_helper() {}"));
    assert!(library.contains("    fn private_parent_visible_helper() {}"));
    assert!(library.contains("    fn private_formatted_helper() {}"));
    assert!(library.contains("    fn parent_helper() {}"));
    assert!(library.contains("        pub(super) fn call_parent_helper() {"));
    assert!(library.contains("    pub(crate) mod api {"));
}
