use std::fmt::Write as _;
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
    let mut child = context
        .command()
        .arg("-A")
        .arg("warnings")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cargo-hawk");
    drop(child.stdout.take());

    assert!(child.wait().expect("wait for cargo-hawk").success());
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
        "hawk: 38 finding(s) for `app --bin app --all-features` and workspace non-production targets on target `{host_target}`\n"
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
fn spanless_expansions_protect_their_compiled_dependencies() {
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
        assert!(stdout.contains("hawk: 0 finding(s)"), "{stdout}");
        assert!(!stdout.contains("hawk::dead_public"), "{stdout}");
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
    assert!(stdout.contains("hawk: 37 finding(s)"));
}

#[test]
fn dead_public_descendants_respect_configured_overrides() {
    let context = HawkTestContext::new("basic");
    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "test binary product"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "DeadFields"
level = "allow"
reason = "parent is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "DeadUnion::value"
level = "expect"
reason = "field is intentionally retained"
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
    assert!(!stdout.contains("`DeadFields` is public"));
    assert!(stdout.contains("`DeadFields::unused` is public"));
    assert!(!stdout.contains("`DeadUnion` is public"));
    assert!(!stdout.contains("`DeadUnion::value` is public"));
    assert!(!stdout.contains("hawk::unfulfilled_expectation"));
}

#[test]
fn reports_dead_inherent_members_separately_from_their_type() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

pub struct DeadInherent;

impl DeadInherent {
    pub const VALUE: u8 = 1;

    pub fn method() {}
}
",
    );
    fs::write(library_path, library).expect("write library source");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`DeadInherent` is public"));
    assert!(stdout.contains("`DeadInherent::VALUE` is public"));
    assert!(stdout.contains("`DeadInherent::method` is public"));
}

#[test]
fn collapses_restricted_visibility_descendants_beneath_dead_modules() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

pub mod dead_with_restricted_helper {
    pub(crate) fn helper() {}
}
",
    );
    fs::write(library_path, library).expect("write library source");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`dead_with_restricted_helper`"));
    assert!(!stdout.contains("`dead_with_restricted_helper::helper`"));
}

#[test]
fn configured_restricted_descendant_protects_its_dead_module() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

pub mod retained_with_restricted_helper {
    pub(crate) fn helper() {}
}
",
    );
    fs::write(library_path, library).expect("write library source");
    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "test binary product"

[[override]]
lint = "hawk::unnecessary_restricted_visibility"
crate = "library"
item = "retained_with_restricted_helper::helper"
level = "expect"
reason = "helper is intentionally retained"
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
    assert!(!stdout.contains("`retained_with_restricted_helper`"));
    assert!(!stdout.contains("`retained_with_restricted_helper::helper`"));
    assert!(!stdout.contains("hawk::unfulfilled_expectation"));
}

#[test]
fn allowed_dead_public_keeps_and_fixes_restricted_descendant_findings() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

pub mod dead_with_restricted_helper {
    pub(crate) fn helper() {}
}
",
    );
    fs::write(&library_path, library).expect("write library source");

    let output = context.run(&["-A", "hawk::dead_public"]);
    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(!stdout.contains("public module `dead_with_restricted_helper`"));
    assert!(
        stdout.contains("`dead_with_restricted_helper::helper` has explicit restricted visibility"),
        "{stdout}"
    );

    let output = context.run(&["-A", "hawk::dead_public", "--fix", "--allow-no-vcs"]);
    context.assert_success(&output);
    let library = fs::read_to_string(library_path).expect("read fixed library source");
    assert!(library.contains("pub mod dead_with_restricted_helper {\n    fn helper() {}"));
}

#[test]
fn retained_inherent_members_protect_their_receiver_and_enclosing_module() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

pub struct RetainedTop;

impl RetainedTop {
    pub fn retained_method() {}

    pub const ACTIONABLE: u8 = 1;
}

pub mod retained_inherent_module {
    pub struct Nested;

    impl Nested {
        pub fn actionable_method() {}

        pub const RETAINED: u8 = 1;
    }
}
",
    );
    fs::write(library_path, library).expect("write library source");
    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "test binary product"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "RetainedTop::retained_method"
kind = "inherent_method"
level = "allow"
reason = "method is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "retained_inherent_module::Nested::RETAINED"
kind = "inherent_associated_constant"
level = "allow"
reason = "constant is intentionally retained"
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
    assert!(!stdout.contains("`RetainedTop` is public"));
    assert!(!stdout.contains("`RetainedTop::retained_method`"));
    assert!(stdout.contains("`RetainedTop::ACTIONABLE` is public"));
    assert!(!stdout.contains("public module `retained_inherent_module`"));
    assert!(!stdout.contains("`retained_inherent_module::Nested` is public"));
    assert!(!stdout.contains("`retained_inherent_module::Nested::RETAINED`"));
    assert!(stdout.contains("`retained_inherent_module::Nested::actionable_method` is public"));
}

fn write_kind_changing_cfg_alternatives(context: &HawkTestContext) {
    fs::write(
        context.workspace().join("library/src/lib.rs"),
        r"#![deny(dead_code)]

pub fn dead_api() {}

#[cfg(not(test))]
pub struct StructAlternative {
    pub production_dead: u8,
}

#[cfg(test)]
pub union StructAlternative {
    pub test_live: u8,
}

#[cfg(not(test))]
#[allow(non_snake_case)]
pub mod ModuleAlternative {
    pub fn production_dead() {}
}

#[cfg(test)]
pub struct ModuleAlternative {
    pub test_live: u8,
}

#[cfg(test)]
mod tests {
    #[test]
    fn reaches_kind_changing_alternatives() {
        let value = crate::StructAlternative { test_live: 1 };
        let _ = unsafe { value.test_live };
        let value = crate::ModuleAlternative { test_live: 1 };
        let _ = value.test_live;
    }
}
",
    )
    .expect("write kind-changing cfg alternatives");
}

#[test]
fn kind_changing_cfg_alternatives_keep_their_child_diagnostics() {
    let context = HawkTestContext::new("dead_public_fixes");
    write_kind_changing_cfg_alternatives(&context);
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`StructAlternative::production_dead` is public"));
    assert!(
        stdout.contains("`StructAlternative::test_live` is public but is needed only by tests")
    );
    assert!(stdout.contains("`ModuleAlternative::production_dead` is public"));
    assert!(
        stdout.contains("`ModuleAlternative::test_live` is public but is needed only by tests")
    );
}

#[test]
fn kind_changing_cfg_alternatives_fix_their_live_fields() {
    let context = HawkTestContext::new("dead_public_fixes");
    write_kind_changing_cfg_alternatives(&context);
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let library = fs::read_to_string(context.workspace().join("library/src/lib.rs"))
        .expect("read fixed cfg alternatives");
    assert!(library.contains("union StructAlternative {\n    test_live: u8,"));
    assert!(library.contains("struct ModuleAlternative {\n    test_live: u8,"));
    assert!(library.contains("pub struct StructAlternative {\n    pub production_dead: u8,"));
    assert!(library.contains("pub mod ModuleAlternative {\n    pub fn production_dead() {}"));
}

#[test]
fn retained_cfg_alternative_fields_do_not_protect_dead_production_parents() {
    let context = HawkTestContext::new("dead_public_fixes");
    write_kind_changing_cfg_alternatives(&context);
    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "test binary product"

[[override]]
lint = "hawk::unnecessary_public"
crate = "library"
item = "StructAlternative::test_live"
kind = "field"
level = "allow"
reason = "union field is intentionally retained"

[[override]]
lint = "hawk::unnecessary_public"
crate = "library"
item = "ModuleAlternative::test_live"
kind = "field"
level = "allow"
reason = "struct field is intentionally retained"
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
    assert!(stdout.contains("`StructAlternative` is public but is not reachable"));
    assert!(stdout.contains("`StructAlternative::production_dead` is public"));
    assert!(stdout.contains("public module `ModuleAlternative` has no declaration reachable"));
    assert!(stdout.contains("`ModuleAlternative::production_dead` is public"));
    assert!(!stdout.contains("`StructAlternative::test_live`"));
    assert!(!stdout.contains("`ModuleAlternative::test_live`"));
}

#[test]
fn retained_definitions_keep_their_dependency_closure_compilable() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let source = r"#![allow(non_local_definitions)]

extern crate self as local;

pub struct BodyDependency;

pub fn retained_body() {
    let _ = BodyDependency;
}

pub struct InterfaceDependency;
pub type InterfaceAlias = InterfaceDependency;

pub struct Retained {
    pub field: InterfaceAlias,
}

pub struct ExportDependency;
pub use ExportDependency as RetainedExport;

pub struct TypeReceiver;
pub type ReceiverAlias = TypeReceiver;

impl ReceiverAlias {
    pub fn retained_method() {}
}

pub struct UseReceiver;
pub mod receiver_alias {
    pub use super::{UseReceiver as First};
    pub use self::{First as Alias};
}

impl receiver_alias::Alias {
    pub const RETAINED: u8 = 1;
}

pub struct AliasReceiver;
pub type InnerReceiver = AliasReceiver;
pub mod type_receiver_alias {
    pub use super::InnerReceiver as Alias;
}

impl type_receiver_alias::Alias {
    pub const TYPE_ALIAS: u8 = 1;
}

pub struct SelfReceiver;
pub mod self_receiver_alias {
    pub use super::SelfReceiver as Alias;
}

impl local::self_receiver_alias::Alias {
    pub const SELF_ALIAS: u8 = 1;
}

pub struct SuperReceiver;
pub mod super_receiver {
    pub mod receiver_alias {
        pub use super::super::SuperReceiver as Alias;
    }
    pub mod implementation {
        impl super::receiver_alias::Alias {
            pub const SUPER_ALIAS: u8 = 1;
        }
    }
}

pub mod module_receiver {
    pub struct ModuleReceiver;
}
pub use module_receiver as receiver_module_alias;

impl receiver_module_alias::ModuleReceiver {
    pub const MODULE_ALIAS: u8 = 1;
}

pub mod export_module {
    pub struct ModuleExportDependency;
}
pub use export_module as export_module_alias;
pub use export_module_alias::ModuleExportDependency as RetainedModuleExport;

pub struct GenericReceiver<T>(T);
pub type GenericInner<T> = GenericReceiver<T>;
pub mod generic_receiver_alias {
    pub use super::GenericInner as Alias;
}

impl<T> generic_receiver_alias::Alias<T> {
    pub const GENERIC: u8 = 1;
}

pub struct GlobReceiver;
pub mod glob_source {
    pub use super::GlobReceiver as Alias;
}
pub mod glob_receiver_alias {
    pub use super::glob_source::*;
}

impl glob_receiver_alias::Alias {
    pub const GLOB: u8 = 1;
}

pub struct PrivateReceiver;
pub mod private_receiver_alias {
    pub use super::PrivateReceiver as First;
    use self::First as Alias;

    impl Alias {
        pub const PRIVATE: u8 = 1;
    }
}

pub struct BlockReceiver;
pub type BlockInner = BlockReceiver;
pub mod block_receiver_alias {
    pub use super::BlockInner as Alias;
}

pub fn block_defined() {
    use block_receiver_alias::Alias as BlockAlias;

    impl BlockAlias {
        pub const BLOCK: u8 = 1;
    }
}

pub struct BlockChainReceiver;
pub mod block_chain_receiver {
    pub use super::BlockChainReceiver as Alias;
}

pub fn block_chain() {
    use block_chain_receiver::Alias as First;
    use First as Second;

    impl Second {
        pub const BLOCK_CHAIN: u8 = 1;
    }
}

pub struct BlockGlobReceiver;
pub mod block_glob_receiver {
    pub use super::BlockGlobReceiver as Alias;
}

pub fn block_glob() {
    use block_glob_receiver::*;

    impl Alias {
        pub const BLOCK_GLOB: u8 = 1;
    }
}

pub mod unrelated_receiver_alias {
    pub use super::UseReceiver as Alias;
}

pub struct Removable;
pub struct SignatureTarget;
pub mod signature_receiver {
    pub use super::SignatureTarget as Alias;
}

pub fn kept_signature(_: signature_receiver::Alias) {}

pub struct BodyTarget;
pub mod body_receiver {
    pub use super::BodyTarget as Alias;
}

pub fn kept_body() {
    let _ = body_receiver::Alias;
}

pub struct TransitiveTarget;
pub mod transitive_receiver {
    pub use super::TransitiveTarget as Alias;
}

pub fn kept_transitive() {
    private_callee();
}

fn private_callee() {
    let _ = transitive_receiver::Alias;
}

pub struct ClosureTarget;
pub mod closure_receiver {
    pub use super::ClosureTarget as Alias;
}

pub struct AsyncTarget;
pub mod async_receiver {
    pub use super::AsyncTarget as Alias;
}

pub struct ConstTarget;
pub mod const_receiver {
    pub use super::ConstTarget as Alias;
}

pub fn kept_nested_bodies() {
    (|| {
        let _ = closure_receiver::Alias;
    })();
    let _future = async {
        let _ = async_receiver::Alias;
    };
    let _ = const { const_receiver::Alias };
}
";
    fs::write(&library_path, source).expect("write retained dependency fixture");
    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "test binary product"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "retained_body"
kind = "function"
level = "allow"
reason = "function body is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "Retained::field"
kind = "field"
level = "allow"
reason = "field API is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "RetainedExport"
kind = "reexport"
level = "allow"
reason = "re-export API is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "TypeReceiver::retained_method"
kind = "inherent_method"
level = "allow"
reason = "type-alias receiver method is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "UseReceiver::RETAINED"
kind = "inherent_associated_constant"
level = "allow"
reason = "re-export receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "AliasReceiver::TYPE_ALIAS"
kind = "inherent_associated_constant"
level = "allow"
reason = "re-exported type-alias receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "SelfReceiver::SELF_ALIAS"
kind = "inherent_associated_constant"
level = "allow"
reason = "self-crate alias receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "super_receiver::implementation::<impl SuperReceiver>::SUPER_ALIAS"
kind = "inherent_associated_constant"
level = "allow"
reason = "super-path receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "<impl module_receiver::ModuleReceiver>::MODULE_ALIAS"
kind = "inherent_associated_constant"
level = "allow"
reason = "module-alias receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "RetainedModuleExport"
kind = "reexport"
level = "allow"
reason = "module-alias re-export is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "GenericReceiver::<T>::GENERIC"
kind = "inherent_associated_constant"
level = "allow"
reason = "generic receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "GlobReceiver::GLOB"
kind = "inherent_associated_constant"
level = "allow"
reason = "glob re-export receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "private_receiver_alias::<impl PrivateReceiver>::PRIVATE"
kind = "inherent_associated_constant"
level = "allow"
reason = "private intermediate receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "block_defined::<impl BlockReceiver>::BLOCK"
kind = "inherent_associated_constant"
level = "allow"
reason = "block-local receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "block_chain::<impl BlockChainReceiver>::BLOCK_CHAIN"
kind = "inherent_associated_constant"
level = "allow"
reason = "block-local alias chain receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "block_glob::<impl BlockGlobReceiver>::BLOCK_GLOB"
kind = "inherent_associated_constant"
level = "allow"
reason = "block-local glob receiver constant is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "kept_signature"
kind = "function"
level = "allow"
reason = "source signature is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "kept_body"
kind = "function"
level = "allow"
reason = "source body is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "kept_transitive"
kind = "function"
level = "allow"
reason = "transitive source body is intentionally retained"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "kept_nested_bodies"
kind = "function"
level = "allow"
reason = "nested source bodies are intentionally retained"
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
    assert!(stdout.contains("`Removable` is public but is not reachable"));
    assert!(stdout.contains("public re-export `unrelated_receiver_alias::Alias`"));
    assert_eq!(
        stdout.matches("warning[hawk::dead_public]").count(),
        2,
        "{stdout}"
    );
    assert!(!stdout.contains("hawk::unknown_item"), "{stdout}");
    assert!(
        !stdout.contains("hawk::unfulfilled_expectation"),
        "{stdout}"
    );
    for item in [
        "BodyDependency",
        "InterfaceDependency",
        "InterfaceAlias",
        "Retained",
        "ExportDependency",
        "RetainedExport",
        "TypeReceiver",
        "ReceiverAlias",
        "UseReceiver",
        "receiver_alias",
        "AliasReceiver",
        "InnerReceiver",
        "type_receiver_alias",
        "SelfReceiver",
        "self_receiver_alias",
        "SuperReceiver",
        "super_receiver",
        "module_receiver",
        "receiver_module_alias",
        "export_module",
        "export_module_alias",
        "RetainedModuleExport",
        "GenericReceiver",
        "GenericInner",
        "generic_receiver_alias",
        "GlobReceiver",
        "glob_source",
        "glob_receiver_alias",
        "PrivateReceiver",
        "private_receiver_alias",
        "BlockReceiver",
        "BlockInner",
        "block_receiver_alias",
        "block_defined",
        "BlockChainReceiver",
        "block_chain_receiver",
        "block_chain",
        "BlockGlobReceiver",
        "block_glob_receiver",
        "block_glob",
        "SignatureTarget",
        "signature_receiver",
        "BodyTarget",
        "body_receiver",
        "TransitiveTarget",
        "transitive_receiver",
        "ClosureTarget",
        "closure_receiver",
        "AsyncTarget",
        "async_receiver",
        "ConstTarget",
        "const_receiver",
    ] {
        assert!(!stdout.contains(&format!("`{item}`")), "{stdout}");
    }

    let remaining = source
        .replace(
            "pub mod unrelated_receiver_alias {\n    pub use super::UseReceiver as Alias;\n}",
            "pub mod unrelated_receiver_alias {}",
        )
        .replace("\npub struct Removable;\n", "\n");
    fs::write(&library_path, remaining).expect("remove reported dead declaration");
    let output = context
        .cargo()
        .args(["check", "--workspace", "--locked"])
        .arg("--target-dir")
        .arg(context.target_dir())
        .output()
        .expect("compile workspace after removing reported declaration");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn expanded_declarations_protect_only_their_actual_reexport_dependencies() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let source = r"pub struct BodyTarget;
pub mod body_used {
    pub use super::BodyTarget as Alias;
}
pub mod body_unused {
    pub use super::BodyTarget as Alias;
}

macro_rules! define_body {
    () => {
        pub fn generated_body() {
            let _ = $crate::body_used::Alias;
        }
    };
}
define_body!();

pub struct InterfaceTarget;
pub mod interface_used {
    pub use super::InterfaceTarget as Alias;
}
pub mod interface_unused {
    pub use super::InterfaceTarget as Alias;
}

macro_rules! define_interface {
    () => {
        pub fn generated_interface(_: $crate::interface_used::Alias) {}
    };
}
define_interface!();

pub struct ImplTarget;
pub mod impl_used {
    pub use super::ImplTarget as Alias;
}
pub mod impl_unused {
    pub use super::ImplTarget as Alias;
}

macro_rules! define_impl {
    () => {
        impl $crate::impl_used::Alias {
            pub const GENERATED: u8 = 1;
        }
    };
}
define_impl!();

pub struct Removable;
";
    fs::write(&library_path, source).expect("write expanded dependency fixture");

    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    for item in [
        "body_unused::Alias",
        "interface_unused::Alias",
        "impl_unused::Alias",
    ] {
        assert!(
            stdout.contains(&format!("public re-export `{item}`")),
            "{stdout}"
        );
    }
    assert!(stdout.contains("`Removable` is public but is not reachable"));
    assert_eq!(
        stdout.matches("warning[hawk::dead_public]").count(),
        4,
        "{stdout}"
    );
    for item in [
        "BodyTarget",
        "body_used::Alias",
        "InterfaceTarget",
        "interface_used::Alias",
        "ImplTarget",
        "impl_used::Alias",
    ] {
        assert!(!stdout.contains(&format!("`{item}`")), "{stdout}");
    }

    let remaining = source
        .replace(
            "pub mod body_unused {\n    pub use super::BodyTarget as Alias;\n}",
            "pub mod body_unused {}",
        )
        .replace(
            "pub mod interface_unused {\n    pub use super::InterfaceTarget as Alias;\n}",
            "pub mod interface_unused {}",
        )
        .replace(
            "pub mod impl_unused {\n    pub use super::ImplTarget as Alias;\n}",
            "pub mod impl_unused {}",
        )
        .replace("\npub struct Removable;\n", "\n");
    fs::write(&library_path, remaining).expect("remove reported expanded dependencies");
    let output = context
        .cargo()
        .args(["check", "--workspace", "--locked"])
        .arg("--target-dir")
        .arg(context.target_dir())
        .output()
        .expect("compile workspace after removing reported declarations");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn collects_many_same_target_imports_without_redundant_findings() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut source = String::from("pub struct Target;\n");
    for index in 0..1_000 {
        writeln!(
            source,
            "mod import_{index} {{ use super::Target as Alias; pub fn keep(_: Alias) {{}} }}"
        )
        .expect("append same-target import");
    }
    source.push_str("pub fn root() {\n");
    for index in 0..1_000 {
        writeln!(source, "    import_{index}::keep(Target);")
            .expect("append same-target import use");
    }
    source.push_str("}\n");
    fs::write(&library_path, source).expect("write same-target imports fixture");
    fs::write(
        context.workspace().join("app/src/main.rs"),
        "fn main() { library::root(); }\n",
    )
    .expect("write same-target imports consumer");

    let output = context.run(&["-A", "warnings"]);

    context.assert_success(&output);
    assert!(
        context
            .normalized_stdout(&output)
            .contains("hawk: 0 finding(s)"),
        "{}",
        context.normalized_stdout(&output)
    );
}

#[test]
fn collects_many_block_globs_without_redundant_findings() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut source = String::from("#![allow(non_local_definitions)]\n");
    for index in 0..200 {
        writeln!(source, "pub struct Target{index};").expect("append block-glob target");
    }
    source.push_str("pub mod exports {\n");
    for index in 0..200 {
        writeln!(source, "    pub use super::Target{index} as Alias{index};")
            .expect("append block-glob re-export");
    }
    source.push_str("}\n");
    for index in 0..200 {
        writeln!(
            source,
            "pub fn block{index}() {{ use exports::*; impl Alias{index} {{ pub const KEEP{index}: u8 = 1; }} }}"
        )
        .expect("append block-glob receiver");
    }
    source.push_str("pub struct Removable;\n");
    fs::write(&library_path, source).expect("write block-glob fixture");

    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    let mut contents = String::from(
        "[[production]]\npackage = \"app\"\nbin = \"app\"\nreason = \"test binary product\"\n",
    );
    for index in 0..200 {
        writeln!(
            contents,
            "\n[[override]]\nlint = \"hawk::dead_public\"\ncrate = \"library\"\nitem = \"block{index}::<impl Target{index}>::KEEP{index}\"\nkind = \"inherent_associated_constant\"\nlevel = \"allow\"\nreason = \"block-glob receiver is intentionally retained\""
        )
        .expect("append block-glob override");
    }
    fs::write(configuration.path(), contents).expect("write block-glob configuration");

    let output = context
        .command()
        .arg("--config")
        .arg(configuration.path())
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`Removable` is public but is not reachable"));
    assert_eq!(
        stdout.matches("warning[hawk::dead_public]").count(),
        1,
        "{stdout}"
    );
    assert!(!stdout.contains("hawk::unknown_item"), "{stdout}");
}

#[test]
fn collects_many_distinct_globs_in_one_block_without_redundant_findings() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut source = String::from("#![allow(non_local_definitions)]\n");
    for index in 0..200 {
        writeln!(
            source,
            "pub struct Target{index}; pub mod exports{index} {{ pub use super::Target{index} as Alias{index}; }}"
        )
        .expect("append distinct block-glob export");
    }
    source.push_str("pub fn block() {\n");
    for index in 0..200 {
        writeln!(source, "    use exports{index}::*;").expect("append distinct block-glob import");
    }
    for index in 0..200 {
        writeln!(
            source,
            "    impl Alias{index} {{ pub const KEEP{index}: u8 = 1; }}"
        )
        .expect("append distinct block-glob receiver");
    }
    source.push_str("}\npub struct Removable;\n");
    fs::write(&library_path, source).expect("write distinct block-glob fixture");

    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    let mut contents = String::from(
        "[[production]]\npackage = \"app\"\nbin = \"app\"\nreason = \"test binary product\"\n",
    );
    for index in 0..200 {
        writeln!(
            contents,
            "\n[[override]]\nlint = \"hawk::dead_public\"\ncrate = \"library\"\nitem = \"block::<impl Target{index}>::KEEP{index}\"\nkind = \"inherent_associated_constant\"\nlevel = \"allow\"\nreason = \"distinct block-glob receiver is intentionally retained\""
        )
        .expect("append distinct block-glob override");
    }
    fs::write(configuration.path(), contents).expect("write distinct block-glob configuration");

    let output = context
        .command()
        .arg("--config")
        .arg(configuration.path())
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("`Removable` is public but is not reachable"));
    assert_eq!(
        stdout.matches("warning[hawk::dead_public]").count(),
        1,
        "{stdout}"
    );
    assert!(!stdout.contains("hawk::unknown_item"), "{stdout}");
}

#[test]
fn collects_repeated_block_paths_with_many_matching_module_children() {
    let context = HawkTestContext::new("dead_public_fixes");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut source = String::from("#![allow(unused_imports)]\npub struct Target;\n");
    for index in 0..200 {
        writeln!(
            source,
            "pub mod exports{index} {{ pub use super::Target as Alias; }}"
        )
        .expect("append matching module child");
    }
    source.push_str("pub fn retained_body() {\n    use exports0::*;\n    use exports0::*;\n");
    for _ in 0..200 {
        source.push_str("    let _ = Alias;\n");
    }
    source.push_str("}\npub struct Removable;\n");
    fs::write(&library_path, source).expect("write matching module-child fixture");

    let configuration = tempfile::NamedTempFile::new().expect("temporary configuration");
    fs::write(
        configuration.path(),
        r#"
[[production]]
package = "app"
bin = "app"
reason = "test binary product"

[[override]]
lint = "hawk::dead_public"
crate = "library"
item = "retained_body"
kind = "function"
level = "allow"
reason = "repeated block paths are intentionally retained"
"#,
    )
    .expect("write matching module-child configuration");

    let output = context
        .command()
        .arg("--config")
        .arg(configuration.path())
        .output()
        .expect("run cargo-hawk");

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(
        !stdout.contains("public re-export `exports0::Alias`"),
        "{stdout}"
    );
    assert!(
        stdout.contains("public re-export `exports1::Alias`"),
        "{stdout}"
    );
    assert!(
        stdout.contains("public re-export `exports199::Alias`"),
        "{stdout}"
    );
    assert!(stdout.contains("`Removable` is public but is not reachable"));
    assert_eq!(
        stdout.matches("warning[hawk::dead_public]").count(),
        200,
        "{stdout}"
    );
    assert!(!stdout.contains("hawk::unknown_item"), "{stdout}");
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
