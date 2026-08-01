use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use cargo_hawk_internal::graph::Fragment;

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

fn add_library_support_package(
    context: &HawkTestContext,
    support_dependencies: &str,
    support_source: &str,
) {
    let workspace_manifest_path = context.workspace().join("Cargo.toml");
    let workspace_manifest = fs::read_to_string(&workspace_manifest_path)
        .expect("read workspace manifest")
        .replace(
            "members = [\"api\", \"consumer\"]",
            "members = [\"api\", \"consumer\", \"support\"]",
        );
    fs::write(workspace_manifest_path, workspace_manifest).expect("add support package");

    let support_source_path = context.workspace().join("support/src");
    fs::create_dir_all(&support_source_path).expect("create support package");
    fs::write(
        context.workspace().join("support/Cargo.toml"),
        format!(
            "[package]\nname = \"support\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\n{support_dependencies}"
        ),
    )
    .expect("write support manifest");
    fs::write(support_source_path.join("lib.rs"), support_source).expect("write support source");

    let consumer_manifest_path = context.workspace().join("consumer/Cargo.toml");
    let mut consumer_manifest =
        fs::read_to_string(&consumer_manifest_path).expect("read consumer manifest");
    consumer_manifest.push_str("\n[dev-dependencies]\nsupport = { path = \"../support\" }\n");
    fs::write(consumer_manifest_path, consumer_manifest).expect("add support dev dependency");

    let consumer_source_path = context.workspace().join("consumer/src/lib.rs");
    let mut consumer_source =
        fs::read_to_string(&consumer_source_path).expect("read consumer source");
    consumer_source.push_str(
        "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn uses_dev_support() {\n        support::fixture();\n    }\n}\n",
    );
    fs::write(consumer_source_path, consumer_source).expect("add support test");

    regenerate_fixture_lockfile(context);
}

fn regenerate_fixture_lockfile(context: &HawkTestContext) {
    let output = context
        .cargo()
        .args(["generate-lockfile", "--offline"])
        .output()
        .expect("regenerate fixture lockfile");
    assert!(
        output.status.success(),
        "could not regenerate fixture lockfile:\n{}",
        context.normalized_stderr(&output)
    );
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
        (
            Some("production"),
            Some("run"),
            "Hawk frontend did not provide HAWK_WORKSPACE_ROOT",
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
            .env_remove("HAWK_RUN_ID")
            .env_remove("HAWK_WORKSPACE_ROOT");
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
fn resolves_workspace_cargo_configuration_from_an_external_launch_directory() {
    let context = HawkTestContext::new("basic");
    let patched_dependency = tempfile::tempdir().expect("temporary patched dependency");
    fs::create_dir(patched_dependency.path().join("src")).expect("create patched dependency");
    fs::write(
        patched_dependency.path().join("Cargo.toml"),
        "[package]\nname = \"hawk-metadata-config-support\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("write patched dependency manifest");
    fs::write(
        patched_dependency.path().join("src/lib.rs"),
        "pub fn fixture() {}\n",
    )
    .expect("write patched dependency source");

    let app_manifest_path = context.workspace().join("app/Cargo.toml");
    let app_manifest = fs::read_to_string(&app_manifest_path)
        .expect("read app manifest")
        .replace(
            "[dependencies]\n",
            "[dependencies]\nhawk-metadata-config-support = \"0.1.0\"\n",
        );
    fs::write(app_manifest_path, app_manifest).expect("add patched dependency");

    let cargo_config = context.workspace().join(".cargo");
    fs::create_dir(&cargo_config).expect("create Cargo configuration directory");
    fs::write(
        cargo_config.join("config.toml"),
        format!(
            "[patch.crates-io]\nhawk-metadata-config-support = {{ path = \"{}\" }}\n",
            patched_dependency
                .path()
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        ),
    )
    .expect("write Cargo dependency patch configuration");
    regenerate_fixture_lockfile(&context);

    let launch_directory = tempfile::tempdir().expect("temporary launch directory");
    let output = context
        .command()
        .current_dir(launch_directory.path())
        .arg("-A")
        .arg("warnings")
        .output()
        .expect("run cargo-hawk outside its configured workspace");

    context.assert_success(&output);
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
        "hawk: 42 finding(s) for `app --bin app --all-features` and workspace non-production targets on target `{host_target}`\n  hawk::dead_public: 15 (library: 14, test_support: 1)\n  hawk::unfulfilled_expectation: 1 (configuration: 1)\n  hawk::unknown_item: 1 (configuration: 1)\n  hawk::unnecessary_public: 25 (library: 22, test_support: 1, unit_support: 2)\n"
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
fn discovers_workspace_binaries_without_configuration() {
    let context = HawkTestContext::new("production_consumers");
    fs::remove_file(context.workspace().join("hawk.toml"))
        .expect("remove production configuration");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(
        report["summary"]["production"],
        serde_json::json!([
            {"package": "app", "binary": "app"},
            {"package": "secondary", "binary": "library"},
        ])
    );
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["identity"]["item"] == "unused")
    );
    assert!(diagnostics.iter().all(|diagnostic| !matches!(
        diagnostic["identity"]["item"].as_str(),
        Some("primary_api" | "secondary_api")
    )));
}

#[test]
fn fragment_file_names_support_long_package_names() {
    let context = HawkTestContext::new("long_package_name");
    let output = context.run(&[]);

    context.assert_success(&output);
}

#[test]
fn configuration_keeps_binary_selection_explicit() {
    let context = HawkTestContext::new("production_consumers");
    fs::write(
        context.workspace().join("hawk.toml"),
        "[[production]]\npackage = \"app\"\nbin = \"app\"\nreason = \"only the primary binary is shipped\"\n",
    )
    .expect("replace production configuration");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(
        report["summary"]["production"],
        serde_json::json!([{"package": "app", "binary": "app"}])
    );
}

#[test]
fn library_only_workspaces_without_configuration_require_explicit_targets() {
    let context = HawkTestContext::new("library_products");
    fs::remove_file(context.workspace().join("hawk.toml"))
        .expect("remove production configuration");

    let output = context.run(&[]);

    assert!(!output.status.success());
    assert!(context.normalized_stderr(&output).contains(
        "no binary targets found in this workspace; add a `hawk.toml` with a `[[production]]` library target"
    ));
}

#[test]
fn library_production_targets_audit_actual_workspace_uses() {
    let context = HawkTestContext::new("library_products");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(
        stdout.contains("warning[hawk::dead_public]: `unused` is public"),
        "unused library export was not diagnosed:\n{stdout}"
    );
    assert!(
        stdout.contains("warning[hawk::unnecessary_public]: `used_only_within_crate` is public"),
        "crate-local library export was not diagnosed:\n{stdout}"
    );
    assert!(
        !stdout.contains("`used_across_workspace`"),
        "cross-crate workspace use was not preserved:\n{stdout}"
    );
    assert!(
        !stdout.contains("`consume`"),
        "unselected workspace consumer became a diagnostic target:\n{stdout}"
    );
    assert!(stdout.contains("`internal-api --lib --all-features`"));
}

#[test]
fn library_production_targets_are_described_in_json_reports() {
    let context = HawkTestContext::new("library_products");
    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["schema_version"], 4);
    assert_eq!(
        report["summary"]["production"],
        serde_json::json!([{"package": "internal-api", "library": "internal_api"}])
    );
    assert_eq!(report["summary"]["diagnostic_count"], 2);
}

fn add_binary_production_product(context: &HawkTestContext) {
    let consumer_manifest_path = context.workspace().join("consumer/Cargo.toml");
    let mut consumer_manifest =
        fs::read_to_string(&consumer_manifest_path).expect("read consumer manifest");
    consumer_manifest.push_str("\n[[bin]]\nname = \"runner\"\npath = \"src/main.rs\"\n");
    fs::write(consumer_manifest_path, consumer_manifest).expect("add configured binary target");
    fs::write(
        context.workspace().join("consumer/src/main.rs"),
        "fn main() { internal_api::used_across_workspace(); }\n",
    )
    .expect("write configured binary target");

    let configuration_path = context.workspace().join("hawk.toml");
    let mut configuration =
        fs::read_to_string(&configuration_path).expect("read production configuration");
    configuration.push_str(
        "\n[[production]]\npackage = \"consumer\"\nbin = \"runner\"\nreason = \"mixed binary and library product\"\n",
    );
    fs::write(configuration_path, configuration).expect("add binary production product");
}

fn assert_library_audit_ignores_same_named_integration_test(include_binary_product: bool) {
    let context = HawkTestContext::new("library_products");
    if include_binary_product {
        add_binary_production_product(&context);
    }
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\n#[cfg(test)]\npub fn library_test_helper() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn uses_library_test_helper() {\n        super::library_test_helper();\n    }\n}\n",
    );
    fs::write(&library_path, library).expect("add test-only library declaration");

    let integration_path = context.workspace().join("api/tests/internal_api.rs");
    fs::create_dir_all(
        integration_path
            .parent()
            .expect("integration test directory"),
    )
    .expect("create integration test directory");
    let integration_source = "pub fn integration_test_helper() {\n    internal_api::used_across_workspace();\n}\n\npub fn unused_integration_test_helper() {}\n\n#[test]\nfn exercises_public_api() {\n    integration_test_helper();\n}\n";
    fs::write(&integration_path, integration_source).expect("add same-named integration test");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| { diagnostic["location"]["file"] != "api/tests/internal_api.rs" }),
        "same-named integration test expanded the library diagnostic surface: {report}"
    );
    let test_helper = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "library_test_helper")
        .expect("library test-harness declaration remains a diagnostic candidate");
    assert_eq!(test_helper["code"], "hawk::unnecessary_public");
    assert_eq!(test_helper["test_only"], true);
    assert_eq!(test_helper["test_compiled_only"], true);

    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    assert_eq!(
        fs::read_to_string(&integration_path).expect("read integration test after fixing"),
        integration_source,
        "library-only fixing modified the same-named integration-test target"
    );
    let library = fs::read_to_string(library_path).expect("read fixed library source");
    assert!(library.contains("fn library_test_helper()"));
    assert!(!library.contains("pub fn library_test_helper()"));
}

#[test]
fn library_production_targets_ignore_same_named_integration_tests() {
    assert_library_audit_ignores_same_named_integration_test(false);
}

#[test]
fn mixed_production_targets_ignore_same_named_integration_tests() {
    assert_library_audit_ignores_same_named_integration_test(true);
}

#[test]
fn mixed_products_audit_libraries_sharing_their_source_with_binary_products() {
    let context = HawkTestContext::new("library_products");
    let manifest_path = context.workspace().join("api/Cargo.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("read library manifest");
    manifest.push_str("\n[[bin]]\nname = \"internal_api\"\npath = \"src/lib.rs\"\n");
    fs::write(manifest_path, manifest).expect("add same-source binary target");

    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read shared library source");
    library.push_str(
        "\nfn main() {\n    used_across_workspace();\n    crate_restricted_helper();\n}\n\npub(crate) fn crate_restricted_helper() {}\n",
    );
    fs::write(&library_path, library).expect("add shared binary entry point");

    let configuration_path = context.workspace().join("hawk.toml");
    let mut configuration =
        fs::read_to_string(&configuration_path).expect("read production configuration");
    configuration.push_str(
        "\n[[production]]\npackage = \"internal-api\"\nbin = \"internal_api\"\nreason = \"binary and library products share one source file\"\n",
    );
    fs::write(configuration_path, configuration).expect("select same-source binary product");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic["identity"]["item"] == "unused" && diagnostic["code"] == "hawk::dead_public"
        }),
        "a same-source binary product suppressed the selected library's dead public export: {report}"
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic["identity"]["item"] == "crate_restricted_helper"
                && diagnostic["code"] == "hawk::unnecessary_restricted_visibility"
        }),
        "a same-source binary product suppressed the library's restricted export: {report}"
    );

    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let library = fs::read_to_string(&library_path).expect("read fixed shared source");
    assert!(library.contains("pub fn unused() {}"));
    assert!(library.contains("fn used_only_within_crate() {}"));
    assert!(!library.contains("pub fn used_only_within_crate() {}"));
    assert!(library.contains("fn crate_restricted_helper() {}"));
    assert!(!library.contains("pub(crate) fn crate_restricted_helper() {}"));
}

fn assert_library_audit_ignores_same_named_integration_test_overrides(
    include_binary_product: bool,
) {
    let context = HawkTestContext::new("library_products");
    if include_binary_product {
        add_binary_production_product(&context);
    }
    let integration_path = context.workspace().join("api/tests/internal_api.rs");
    fs::create_dir_all(
        integration_path
            .parent()
            .expect("integration test directory"),
    )
    .expect("create integration test directory");
    fs::write(
        integration_path,
        "pub fn integration_only() {}\n\n#[test]\nfn integration_test() {}\n",
    )
    .expect("add same-named integration test");

    let configuration_path = context.workspace().join("hawk.toml");
    let mut configuration =
        fs::read_to_string(&configuration_path).expect("read production configuration");
    configuration.push_str(
        "\n[[override]]\nlint = \"hawk::dead_public\"\ncrate = \"internal_api\"\nitem = \"integration_only\"\nlevel = \"expect\"\nreason = \"integration targets are outside the library audit\"\n",
    );
    fs::write(configuration_path, configuration).expect("add integration-target override");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "hawk::unknown_item"),
        "an integration-test declaration was accepted as an audited library item: {report}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "hawk::unfulfilled_expectation"),
        "an integration-test override became an unfulfilled library expectation: {report}"
    );
}

#[test]
fn library_production_targets_ignore_same_named_integration_test_overrides() {
    assert_library_audit_ignores_same_named_integration_test_overrides(false);
}

#[test]
fn mixed_production_targets_ignore_same_named_integration_test_overrides() {
    assert_library_audit_ignores_same_named_integration_test_overrides(true);
}

#[test]
fn library_production_targets_preserve_test_only_caller_classification() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_tests() {\n    used_only_within_tests();\n}\n\npub fn used_only_within_tests() {}\n",
    );
    fs::write(library_path, library).expect("add test-only library exports");
    let consumer_path = context.workspace().join("consumer/src/lib.rs");
    let mut consumer = fs::read_to_string(&consumer_path).expect("read consumer source");
    consumer.push_str(
        "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn uses_library() {\n        internal_api::used_only_by_tests();\n    }\n}\n",
    );
    fs::write(consumer_path, consumer).expect("add test-only library caller");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "used_only_within_tests")
        .expect("test-only helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
}

#[test]
fn library_production_targets_report_public_callees_of_unreachable_private_code() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\nfn unreachable_private_caller() {\n    unreachable_public_callee();\n}\n\npub fn unreachable_public_callee() {}\n",
    );
    fs::write(library_path, library).expect("add unreachable library declarations");

    let output = context.run(&["--only", "dead-public", "--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "unreachable_public_callee")
        .expect("dead public callee diagnostic");
    assert_eq!(diagnostic["code"], "hawk::dead_public");
}

#[test]
fn library_production_targets_classify_example_callers_as_non_production() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_example() {\n    example_helper();\n}\n\npub fn example_helper() {}\n",
    );
    fs::write(library_path, library).expect("add example-only library exports");
    let examples = context.workspace().join("consumer/examples");
    fs::create_dir_all(&examples).expect("create consumer example directory");
    fs::write(
        examples.join("uses_api.rs"),
        "fn main() { internal_api::used_only_by_example(); }\n",
    )
    .expect("write non-production library consumer");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "example_helper")
        .expect("example-only helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
}

#[test]
fn library_production_targets_classify_library_format_examples_as_non_production() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_library_example() {\n    library_example_helper();\n}\n\npub fn library_example_helper() {}\n",
    );
    fs::write(library_path, library).expect("add library-format example exports");

    let manifest_path = context.workspace().join("consumer/Cargo.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("read consumer manifest");
    manifest.push_str(
        "\n[[example]]\nname = \"library_example\"\npath = \"examples/library_example.rs\"\ncrate-type = [\"rlib\"]\n",
    );
    fs::write(manifest_path, manifest).expect("add library-format example target");
    let examples = context.workspace().join("consumer/examples");
    fs::create_dir_all(&examples).expect("create consumer example directory");
    fs::write(
        examples.join("library_example.rs"),
        "pub fn example_entry() { internal_api::used_only_by_library_example(); }\n",
    )
    .expect("write library-format example consumer");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "library_example_helper")
        .expect("library-format example helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
}

#[test]
fn library_production_targets_do_not_root_examples_sharing_workspace_library_sources() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let library = fs::read_to_string(&library_path).expect("read library source");
    fs::write(
        &library_path,
        format!("#![deny(dead_code)]\n\n{library}\npub fn shared_source_dead_api() {{}}\n"),
    )
    .expect("add dead-code-denying shared library source");

    let manifest_path = context.workspace().join("consumer/Cargo.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("read consumer manifest");
    manifest.push_str(
        "\n[[example]]\nname = \"mirrored_api\"\npath = \"../api/src/../src/lib.rs\"\ncrate-type = [\"rlib\"]\n",
    );
    fs::write(manifest_path, manifest).expect("add cross-package shared-source example");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "shared_source_dead_api")
        .expect("shared-source dead public export diagnostic");
    assert_eq!(diagnostic["code"], "hawk::dead_public");
    assert_eq!(diagnostic["test_only"], false);

    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let fixed_library = fs::read_to_string(library_path).expect("read fixed library source");
    assert!(
        fixed_library.contains("pub fn shared_source_dead_api()"),
        "a shared-source example made --fix narrow a dead library export: {fixed_library}"
    );
}

#[test]
fn library_production_targets_classify_doctest_callers_as_non_production() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_doctest() {\n    doctest_helper();\n}\n\npub fn doctest_helper() {}\n",
    );
    fs::write(library_path, library).expect("add doctest-only library exports");
    let consumer_path = context.workspace().join("consumer/src/lib.rs");
    let mut consumer = fs::read_to_string(&consumer_path).expect("read consumer source");
    consumer.push_str(
        "\n/// ```\n/// internal_api::used_only_by_doctest();\n/// ```\npub fn documented() {}\n",
    );
    fs::write(consumer_path, consumer).expect("add doctest-only library caller");

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "doctest_helper")
        .expect("doctest-only helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
}

#[test]
fn library_production_targets_classify_transitive_dev_dependencies_as_non_production() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_dev_support() {\n    dev_support_helper();\n}\n\npub fn dev_support_helper() {}\n",
    );
    fs::write(library_path, library).expect("add dev-only library exports");

    add_library_support_package(
        &context,
        "internal-api = { path = \"../api\" }\n",
        "pub fn fixture() { internal_api::used_only_by_dev_support(); }\n",
    );

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "dev_support_helper")
        .expect("dev-support-only helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
}

#[test]
fn library_production_targets_preserve_production_consumers_in_dev_dependency_cycles() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_dev_support() {\n    dev_support_helper();\n}\n\npub fn dev_support_helper() {}\n",
    );
    fs::write(library_path, library).expect("add dev-only library exports");
    add_library_support_package(
        &context,
        "consumer = { path = \"../consumer\" }\ninternal-api = { path = \"../api\" }\n",
        "pub fn fixture() { consumer::consume(); internal_api::used_only_by_dev_support(); }\n",
    );

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic["identity"]["item"] == "used_across_workspace"),
        "a production consumer in a legal dev-dependency cycle was ignored: {report}"
    );
    let helper = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "used_only_within_crate")
        .expect("production helper diagnostic");
    assert_eq!(helper["code"], "hawk::unnecessary_public");
    assert_eq!(helper["test_only"], false);
    let dev_helper = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "dev_support_helper")
        .expect("dev-support-only helper diagnostic");
    assert_eq!(dev_helper["code"], "hawk::unnecessary_public");
    assert_eq!(dev_helper["test_only"], true);
}

fn assert_production_consumer_in_reversed_dev_dependency_cycle(support_uses_api: bool) {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn cycle_api() {\n    cycle_helper();\n}\n\npub fn cycle_helper() {}\n\npub fn support_api() {}\n",
    );
    fs::write(library_path, library).expect("add library exports for dependency cycle");

    let (support_dependencies, support_source) = if support_uses_api {
        (
            "internal-api = { path = \"../api\" }\n",
            "pub fn fixture() { internal_api::support_api(); }\n",
        )
    } else {
        ("", "pub fn fixture() {}\n")
    };
    add_library_support_package(&context, support_dependencies, support_source);

    let consumer_manifest_path = context.workspace().join("consumer/Cargo.toml");
    let consumer_manifest = fs::read_to_string(&consumer_manifest_path)
        .expect("read consumer manifest")
        .replace(
            "[dependencies]\n",
            "[dependencies]\nsupport = { path = \"../support\" }\n",
        )
        .replace(
            "\n[dev-dependencies]\nsupport = { path = \"../support\" }\n",
            "\n",
        );
    fs::write(consumer_manifest_path, consumer_manifest)
        .expect("move support to production dependencies");

    let support_manifest_path = context.workspace().join("support/Cargo.toml");
    let mut support_manifest =
        fs::read_to_string(&support_manifest_path).expect("read support manifest");
    support_manifest.push_str("\n[dev-dependencies]\nconsumer = { path = \"../consumer\" }\n");
    fs::write(support_manifest_path, support_manifest)
        .expect("add development dependency back to the consumer");

    let consumer_source_path = context.workspace().join("consumer/src/lib.rs");
    let consumer_source = fs::read_to_string(&consumer_source_path)
        .expect("read consumer source")
        .replace(
            "internal_api::used_across_workspace();",
            "internal_api::cycle_api();\n    support::fixture();",
        );
    fs::write(consumer_source_path, consumer_source)
        .expect("add production use from the cyclic consumer");
    regenerate_fixture_lockfile(&context);

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic["identity"]["item"] == "cycle_api"),
        "a production consumer in the reversed development-dependency cycle was ignored: {report}"
    );
    let helper = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "cycle_helper")
        .expect("production helper diagnostic");
    assert_eq!(helper["code"], "hawk::unnecessary_public");
    assert_eq!(helper["test_only"], false);
}

#[test]
fn library_production_targets_preserve_consumers_in_reversed_dev_dependency_cycles() {
    assert_production_consumer_in_reversed_dev_dependency_cycle(false);
}

#[test]
fn library_production_targets_preserve_consumers_when_cyclic_support_also_uses_the_product() {
    assert_production_consumer_in_reversed_dev_dependency_cycle(true);
}

#[test]
fn library_production_targets_distinguish_external_packages_with_workspace_names() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_dev_support() {\n    dev_support_helper();\n}\n\npub fn dev_support_helper() {}\n",
    );
    fs::write(library_path, library).expect("add dev-only library exports");
    add_library_support_package(
        &context,
        "internal-api = { path = \"../api\" }\n",
        "pub fn fixture() { internal_api::used_only_by_dev_support(); }\n",
    );

    let outside_support = tempfile::tempdir().expect("external support package directory");
    fs::create_dir_all(outside_support.path().join("src"))
        .expect("create external support package");
    fs::write(
        outside_support.path().join("Cargo.toml"),
        "[package]\nname = \"support\"\nversion = \"0.2.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .expect("write external support manifest");
    fs::write(
        outside_support.path().join("src/lib.rs"),
        "pub fn outside() {}\n",
    )
    .expect("write external support source");
    let consumer_manifest_path = context.workspace().join("consumer/Cargo.toml");
    let consumer_manifest = fs::read_to_string(&consumer_manifest_path)
        .expect("read consumer manifest")
        .replace(
            "[dependencies]\n",
            &format!(
                "[dependencies]\noutside-support = {{ package = \"support\", path = \"{}\" }}\n",
                outside_support.path().display()
            ),
        );
    fs::write(consumer_manifest_path, consumer_manifest).expect("add external support dependency");
    regenerate_fixture_lockfile(&context);

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "dev_support_helper")
        .expect("dev-support-only helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
}

#[test]
fn library_production_targets_ignore_disabled_optional_dependencies() {
    let context = HawkTestContext::new("library_products");
    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\npub fn used_only_by_dev_support() {\n    dev_support_helper();\n}\n\npub fn dev_support_helper() {}\n",
    );
    fs::write(library_path, library).expect("add dev-only library exports");
    add_library_support_package(
        &context,
        "internal-api = { path = \"../api\" }\n",
        "pub fn fixture() { internal_api::used_only_by_dev_support(); }\n",
    );

    let consumer_manifest_path = context.workspace().join("consumer/Cargo.toml");
    let consumer_manifest = fs::read_to_string(&consumer_manifest_path)
        .expect("read consumer manifest")
        .replace(
            "[dependencies]\n",
            "[dependencies]\nsupport = { path = \"../support\", optional = true }\n",
        );
    fs::write(consumer_manifest_path, consumer_manifest).expect("add optional support dependency");
    let configuration_path = context.workspace().join("hawk.toml");
    let mut configuration =
        fs::read_to_string(&configuration_path).expect("read production configuration");
    configuration.push_str(
        "\n[[feature-profile]]\nname = \"without-support\"\nno-default-features = true\n",
    );
    fs::write(configuration_path, configuration).expect("add feature profile");
    regenerate_fixture_lockfile(&context);

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostic = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array")
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "dev_support_helper")
        .expect("dev-support-only helper diagnostic");
    assert_eq!(diagnostic["code"], "hawk::unnecessary_public");
    assert_eq!(diagnostic["test_only"], true);
    assert_eq!(
        report["summary"]["feature_profiles"],
        serde_json::json!(["without-support"])
    );
}

#[test]
fn library_production_targets_preserve_every_selected_feature_variant() {
    let context = HawkTestContext::new("library_products");
    let workspace_manifest_path = context.workspace().join("Cargo.toml");
    let workspace_manifest = fs::read_to_string(&workspace_manifest_path)
        .expect("read workspace manifest")
        .replace(
            "members = [\"api\", \"consumer\"]",
            "members = [\"api\", \"consumer\", \"feature-consumer\"]",
        );
    fs::write(workspace_manifest_path, workspace_manifest).expect("add feature-enabled consumer");

    let library_manifest_path = context.workspace().join("api/Cargo.toml");
    let mut library_manifest =
        fs::read_to_string(&library_manifest_path).expect("read library manifest");
    library_manifest.push_str("\n[features]\ndefault = []\nextra = []\n");
    fs::write(library_manifest_path, library_manifest).expect("add optional library feature");

    let library_path = context.workspace().join("api/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        "\n#[cfg(feature = \"extra\")]\npub fn feature_api() {\n    feature_helper();\n}\n\n#[cfg(feature = \"extra\")]\npub fn feature_helper() {}\n",
    );
    fs::write(library_path, library).expect("add feature-enabled library declarations");

    let consumer_manifest_path = context.workspace().join("consumer/Cargo.toml");
    let consumer_manifest = fs::read_to_string(&consumer_manifest_path)
        .expect("read consumer manifest")
        .replace(
            "internal-api = { path = \"../api\" }",
            "internal-api = { path = \"../api\", default-features = false }",
        );
    fs::write(consumer_manifest_path, consumer_manifest)
        .expect("disable optional API features in the first selected product");

    let feature_consumer = context.workspace().join("feature-consumer");
    fs::create_dir_all(feature_consumer.join("src"))
        .expect("create feature-enabled consumer package");
    fs::write(
        feature_consumer.join("Cargo.toml"),
        "[package]\nname = \"feature-consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\ninternal-api = { path = \"../api\", features = [\"extra\"] }\n",
    )
    .expect("write feature-enabled consumer manifest");
    fs::write(
        feature_consumer.join("src/lib.rs"),
        "pub fn consume_extra() {\n    internal_api::feature_api();\n}\n",
    )
    .expect("write feature-enabled consumer source");

    fs::write(
        context.workspace().join("hawk.toml"),
        "[[production]]\npackage = \"consumer\"\nlib = \"consumer\"\nreason = \"product with optional API features disabled\"\n\n[[production]]\npackage = \"internal-api\"\nlib = \"internal_api\"\nreason = \"product with every optional API feature enabled\"\n",
    )
    .expect("select both library feature variants");
    regenerate_fixture_lockfile(&context);

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic["identity"]["item"] == "feature_api"),
        "a feature-enabled API used across the workspace was diagnosed: {report}"
    );
    let helper = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "feature_helper")
        .expect("feature-enabled helper diagnostic");
    assert_eq!(helper["code"], "hawk::unnecessary_public");
    assert_eq!(helper["test_only"], false);
}

#[test]
fn library_production_targets_support_explicit_rlib_crate_types() {
    let context = HawkTestContext::new("library_products");
    let library_manifest_path = context.workspace().join("api/Cargo.toml");
    let mut library_manifest =
        fs::read_to_string(&library_manifest_path).expect("read library manifest");
    library_manifest.push_str("crate-type = [\"rlib\"]\n");
    fs::write(library_manifest_path, library_manifest).expect("set explicit rlib crate type");

    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let library = fs::read_to_string(context.workspace().join("api/src/lib.rs"))
        .expect("read fixed rlib source");
    assert!(library.contains("fn used_only_within_crate()"));
    assert!(!library.contains("pub fn used_only_within_crate()"));
}

#[test]
fn library_production_targets_allow_unrelated_duplicate_workspace_crate_names() {
    let context = HawkTestContext::new("library_products");
    let workspace_manifest_path = context.workspace().join("Cargo.toml");
    let workspace_manifest = fs::read_to_string(&workspace_manifest_path)
        .expect("read workspace manifest")
        .replace(
            "members = [\"api\", \"consumer\"]",
            "members = [\"api\", \"consumer\", \"duplicate-a\", \"duplicate-b\"]",
        );
    fs::write(workspace_manifest_path, workspace_manifest)
        .expect("add unrelated duplicate workspace libraries");

    for (package, function) in [
        ("duplicate-a", "first_unrelated_export"),
        ("duplicate-b", "second_unrelated_export"),
    ] {
        let package_path = context.workspace().join(package);
        fs::create_dir_all(package_path.join("src")).expect("create unrelated library package");
        fs::write(
            package_path.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\nname = \"shared\"\n"
            ),
        )
        .expect("write unrelated library manifest");
        fs::write(
            package_path.join("src/lib.rs"),
            format!("pub fn {function}() {{}}\n"),
        )
        .expect("write unrelated library source");
    }
    regenerate_fixture_lockfile(&context);

    let output = context.run(&["--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert!(
        report["diagnostics"]
            .as_array()
            .expect("diagnostics is an array")
            .iter()
            .all(|diagnostic| diagnostic["identity"]["crate"] == "internal_api"),
        "unrelated duplicate libraries became diagnostic targets: {report}"
    );
}

#[test]
fn library_production_targets_ignore_unselected_workspace_expectations() {
    let context = HawkTestContext::new("library_products");
    let consumer_path = context.workspace().join("consumer/src/lib.rs");
    let mut consumer = fs::read_to_string(&consumer_path).expect("read consumer source");
    consumer.push_str("\npub fn unused_consumer() {}\n");
    fs::write(consumer_path, consumer).expect("add unselected consumer export");
    let configuration_path = context.workspace().join("hawk.toml");
    let mut configuration =
        fs::read_to_string(&configuration_path).expect("read production configuration");
    configuration.push_str(
        "\n[[override]]\nlint = \"hawk::dead_public\"\ncrate = \"consumer\"\nitem = \"unused_consumer\"\nlevel = \"expect\"\nreason = \"separate consumer package expectation\"\n",
    );
    fs::write(configuration_path, configuration).expect("add out-of-scope expectation");

    let output = context.run(&["-A", "warnings", "-D", "hawk::unfulfilled_expectation"]);

    context.assert_success(&output);
}

#[test]
fn library_production_targets_select_the_requested_compilation_target() {
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
    let Some(target) = installed_targets.lines().find(|target| {
        target
            .split_once('-')
            .is_some_and(|(arch, _)| arch != host_arch)
    }) else {
        return;
    };
    let target_arch = target
        .split_once('-')
        .expect("target has an architecture")
        .0;
    let context = HawkTestContext::new("library_products");
    fs::write(
        context.workspace().join("api/src/lib.rs"),
        format!(
            "#[cfg(target_arch = \"{host_arch}\")]\npub fn host_api() {{ host_helper(); }}\n#[cfg(target_arch = \"{host_arch}\")]\npub fn host_helper() {{}}\n#[cfg(target_arch = \"{host_arch}\")]\npub fn host_only_unused() {{}}\n\n#[cfg(target_arch = \"{target_arch}\")]\npub fn target_api() {{ target_helper(); }}\n#[cfg(target_arch = \"{target_arch}\")]\npub fn target_helper() {{}}\n\npub fn unused() {{}}\n"
        ),
    )
    .expect("write target-specific library source");
    fs::write(
        context.workspace().join("consumer/src/lib.rs"),
        "pub fn consume() { internal_api::target_api(); }\n",
    )
    .expect("write target-specific consumer source");
    fs::write(
        context.workspace().join("consumer/build.rs"),
        "fn main() { internal_api::host_api(); }\n",
    )
    .expect("write host-only build script");
    let manifest_path = context.workspace().join("consumer/Cargo.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("read consumer manifest");
    manifest.push_str("\n[build-dependencies]\ninternal-api = { path = \"../api\" }\n");
    fs::write(manifest_path, manifest).expect("add host-only library dependency");
    let configuration_path = context.workspace().join("hawk.toml");
    let mut configuration =
        fs::read_to_string(&configuration_path).expect("read production configuration");
    configuration.push_str(
        "\n[[production]]\npackage = \"consumer\"\nlib = \"consumer\"\nreason = \"cross-target workspace library consumer\"\n\n[[override]]\nlint = \"hawk::dead_public\"\ncrate = \"internal_api\"\nitem = \"host_only_unused\"\nlevel = \"expect\"\nreason = \"host-only export is not part of the selected target\"\n",
    );
    fs::write(configuration_path, configuration).expect("add consumer library product");

    let output = context.run(&["--target", target, "--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    let helper = diagnostics
        .iter()
        .find(|diagnostic| diagnostic["identity"]["item"] == "target_helper")
        .expect("target helper diagnostic");
    assert_eq!(helper["code"], "hawk::unnecessary_public");
    assert_eq!(helper["test_only"], false);
    assert!(
        !diagnostics.iter().any(|diagnostic| {
            diagnostic["category"] == "finding"
                && diagnostic["identity"]["item"] == "host_only_unused"
        }),
        "host-only declarations leaked into the target diagnostic surface: {report}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "hawk::unknown_item"),
        "a host-only override was not diagnosed as unknown for the selected target: {report}"
    );
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "hawk::unfulfilled_expectation"),
        "a host-only override became an unfulfilled target expectation: {report}"
    );
}

#[test]
fn library_production_targets_apply_safe_visibility_fixes() {
    let context = HawkTestContext::new("library_products");
    let output = context.run(&["--fix", "--allow-no-vcs"]);

    context.assert_success(&output);
    let library = fs::read_to_string(context.workspace().join("api/src/lib.rs"))
        .expect("read fixed library source");
    assert!(library.contains("pub fn used_across_workspace()"));
    assert!(
        !library.contains("pub fn used_only_within_crate()")
            && library.contains("fn used_only_within_crate()"),
        "crate-local visibility was not reduced:\n{library}"
    );
    assert!(library.contains("pub fn unused()"));
}

#[test]
fn mixed_binary_and_library_products_reuse_dependency_fragments() {
    let context = HawkTestContext::new("production_consumers");
    let configuration = context.workspace().join("hawk.toml");
    let mut source = fs::read_to_string(&configuration).expect("read production configuration");
    source.push_str(
        "\n[[production]]\npackage = \"library\"\nlib = \"library\"\nreason = \"audit internal library exports\"\n",
    );
    fs::write(configuration, source).expect("add library production target");

    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("warning[hawk::dead_public]: `unused` is public"));
    assert!(stdout.contains("3 configured production targets"));
}

#[test]
fn rejects_unknown_library_production_targets() {
    let context = HawkTestContext::new("library_products");
    let configuration = context.workspace().join("hawk.toml");
    fs::write(
        configuration,
        "[[production]]\npackage = \"internal-api\"\nlib = \"missing\"\nreason = \"invalid library\"\n",
    )
    .expect("write invalid library configuration");

    let output = context.run(&[]);

    assert!(!output.status.success());
    assert!(
        context
            .normalized_stderr(&output)
            .contains("package `internal-api` has no library target `missing`")
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
        3,
        "the shared library should compile once per production, non-production, and documentation mode:\n{stderr}"
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
fn rejects_duplicate_audited_library_crate_names() {
    let context = HawkTestContext::new("duplicate_library_names");
    fs::write(
        context.workspace().join("hawk.toml"),
        "[[production]]\npackage = \"library-a\"\nlib = \"shared\"\nreason = \"library product under analysis\"\n",
    )
    .expect("select an ambiguous library product");

    let output = context.run(&[]);

    assert!(!output.status.success());
    let stderr = context.normalized_stderr(&output);
    assert!(stderr.contains("conflicting names: `shared` (`library-a`, `library-b`)"));
}

#[test]
fn rejects_unknown_excluded_crates_before_compilation() {
    let context = HawkTestContext::new("basic");

    let output = context.run(&[
        "--exclude-crate",
        "library",
        "--exclude-crate",
        "libary",
        "--exclude-crate",
        "unit_suport",
    ]);

    assert!(!output.status.success());

    let stderr = context.normalized_stderr(&output);

    assert!(stderr.contains("unknown --exclude-crate value(s): `libary`, `unit_suport`"));

    assert!(stderr.contains(
        "valid workspace library crate names: `library`, `test_support`, `unit_support`"
    ));

    assert!(
        fs::read_dir(context.target_dir())
            .expect("read target directory")
            .next()
            .is_none(),
        "unknown excluded crate started compilation:\n{stderr}"
    );
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
    let run_dir = fs::read_dir(graph_dir.path())
        .expect("read graph directory")
        .map(|entry| entry.expect("read graph entry"))
        .find(|entry| entry.file_type().expect("read graph entry type").is_dir())
        .expect("retained graph run directory")
        .path();

    // Check the serialized source identities first: corrupt identities produce
    // arbitrary reachability, so this reports the cause rather than a symptom.
    assert_workspace_source_paths_are_stable(&run_dir);

    let stdout = context.normalized_stdout(&output);
    assert!(
        !stdout.contains("`fallback_api` is public"),
        "API used by the default-disabled profile was diagnosed:\n{stdout}"
    );
    assert!(stdout.contains("`unused_api` is public"));
    assert!(stdout.contains("`app --bin app` across 2 feature profiles"));

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

/// Hawk merges definitions from separate compilations by source span, so one
/// workspace file must have exactly one spelling within a run. Cargo compiles
/// the same file from the workspace root in one target and from the package
/// root in another, so a span that kept the compiler's own wording would split
/// a declaration into two identities and silently corrupt reachability.
///
/// Only workspace-backed sources are covered. Generated rustdoc bundles live
/// in a temporary directory outside the workspace and keep absolute paths;
/// inventing a shared identity for those could merge unrelated snippets.
fn assert_workspace_source_paths_are_stable(run_dir: &Path) {
    let expected_roots = [
        ("app", "app/src/main.rs"),
        ("library", "library/src/lib.rs"),
    ];
    let mut observed = BTreeSet::new();
    let mut checked_spans = 0;

    let mut stack = vec![run_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read graph directory") {
            let path = entry.expect("read graph entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let fragment: Fragment =
                serde_json::from_str(&fs::read_to_string(&path).expect("read fragment"))
                    .expect("parse fragment");
            let Some((crate_name, expected_root)) = expected_roots
                .iter()
                .find(|(name, _)| *name == fragment.crate_name)
            else {
                continue;
            };
            observed.insert(*crate_name);

            assert_eq!(
                fragment.crate_root.as_deref(),
                Some(*expected_root),
                "crate `{}` in {} reported an unexpected source root",
                fragment.crate_name,
                path.display()
            );
            for definition in &fragment.definitions {
                let Some(span) = definition.span.as_ref() else {
                    continue;
                };
                checked_spans += 1;
                assert!(
                    !span.file.contains('\\'),
                    "span path `{}` in {} keeps a native separator",
                    span.file,
                    path.display()
                );
                assert!(
                    Path::new(&span.file).is_relative(),
                    "workspace span path `{}` in {} is not workspace relative",
                    span.file,
                    path.display()
                );
                // The JSON declaration span is a second path-producing surface
                // for the same file. It has to resolve to the same identity as
                // the ordinary span, or JSON `location.file` and stable ids
                // would diverge from the reachability the ordinary span drives.
                if let Some(declaration_span) = definition.declaration_span.as_ref() {
                    assert_eq!(
                        declaration_span.file,
                        span.file,
                        "declaration span path `{}` disagrees with span path `{}` in {}",
                        declaration_span.file,
                        span.file,
                        path.display()
                    );
                }
            }
        }
    }

    // Guard against the checks silently covering nothing.
    assert_eq!(
        observed,
        expected_roots.iter().map(|(name, _)| *name).collect(),
        "not every workspace crate appeared in the retained fragments"
    );
    assert!(checked_spans > 0, "no workspace spans were checked");
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
fn requires_a_configured_production_target() {
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
    assert!(stderr.contains("error: no applicable production targets configured"));
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
fn later_warnings_group_reenables_default_warnings() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&["-A", "warnings", "-D", "warnings"]);

    assert!(
        !output.status.success(),
        "denied diagnostic did not fail:\n{}",
        context.normalized_stdout(&output)
    );
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("error[hawk::dead_public]"));
    assert!(!stdout.contains("hawk::unnecessary_crate_visibility"));
}

#[test]
fn later_warnings_group_reenables_explicitly_enabled_opt_in_lints() {
    let context = HawkTestContext::new("crate_visibility_fixes");
    let output = context.run(&[
        "-W",
        "hawk::unnecessary_crate_visibility",
        "-A",
        "warnings",
        "-D",
        "warnings",
    ]);

    assert!(
        !output.status.success(),
        "denied opt-in diagnostic did not fail:\n{}",
        context.normalized_stdout(&output)
    );
    assert!(
        context
            .normalized_stdout(&output)
            .contains("error[hawk::unnecessary_crate_visibility]")
    );
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
    assert_eq!(report["schema_version"], 4);
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
    assert_eq!(report["schema_version"], 4);
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
    assert_eq!(report["schema_version"], 4);
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
    assert_eq!(report["schema_version"], 4);
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
            .contains("error: no applicable production targets configured")
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
fn codegen_roots_preserve_reachable_items() {
    let context = HawkTestContext::new("exported_symbols");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    for live in [
        "exported_callback",
        "renamed_callback",
        "RETAINED_REGISTRATION",
        "retained_callback",
        "retained_helper",
        "macro_registered_callback",
    ] {
        assert!(
            !stdout.contains(&format!("warning[hawk::dead_public]: `{live}` is public")),
            "declaration `{live}` reachable from a codegen root was diagnosed as dead:\n{stdout}"
        );
        assert!(
            stdout.contains(&format!(
                "warning[hawk::unnecessary_public]: `{live}` is public"
            )),
            "declaration `{live}` reachable from a codegen root was not diagnosed as unnecessarily public:\n{stdout}"
        );
    }
    for unretained in [
        "UNRETAINED_REGISTRATION",
        "unretained_callback",
        "unretained_helper",
    ] {
        assert!(
            stdout.contains(&format!(
                "warning[hawk::dead_public]: `{unretained}` is public"
            )),
            "unretained declaration `{unretained}` was not diagnosed as dead:\n{stdout}"
        );
        assert!(
            !stdout.contains(&format!(
                "warning[hawk::unnecessary_public]: `{unretained}` is public"
            )),
            "unretained declaration `{unretained}` was unexpectedly diagnosed as live:\n{stdout}"
        );
    }
}

#[test]
fn doctest_consumers_preserve_apis_from_multiple_packages() {
    let context = HawkTestContext::new("doctest_consumers");
    fs::write(
        context.workspace().join("skipped/src/lib.rs"),
        "pub fn other_doc_api() {}\n\n/// ```\n/// skipped::other_doc_api();\n/// ```\npub fn documented() {}\n",
    )
    .expect("replace unselected doctest with a valid consumer");
    let config_path = context.workspace().join("hawk.toml");
    let mut config = fs::read_to_string(&config_path).expect("read fixture configuration");
    config.push_str("\n[[doctest]]\npackage = \"skipped\"\n");
    fs::write(config_path, config).expect("select both doctest packages");

    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    for api in [
        "doc_api",
        "standalone_first_api",
        "standalone_second_api",
        "other_doc_api",
    ] {
        assert!(
            !stdout.contains(&format!("`{api}` is public")),
            "API required by a selected doctest was diagnosed:\n{stdout}"
        );
    }
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
    assert!(library.contains("pub fn standalone_first_api() {}"));
    assert!(library.contains("pub fn standalone_second_api() {}"));
    assert!(library.contains("pub fn unused() {}"));
}

#[test]
fn rustdoc_links_preserve_public_visibility() {
    let context = HawkTestContext::new("rustdoc_links");
    let output = context.run(&[]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    for linked in [
        "LINKED_CONSTANT",
        "DocumentedType::linked_method",
        "DocumentedType::linked_field",
        "AliasTarget::alias_linked_method",
        "INLINE_REEXPORT_LINKED_CONSTANT",
        "GLOB_INLINE_REEXPORT_LINKED_CONSTANT",
        "TRAIT_IMPL_LINKED_CONSTANT",
        "INLINE_TRAIT_IMPL_LINKED_CONSTANT",
        "INLINE_DIRECT_TRAIT_IMPL_LINKED_CONSTANT",
        "NESTED_REEXPORT_LINKED_CONSTANT",
        "USED_REFERENCE_DEFINITION",
        "CFG_DOC_LINKED_CONSTANT",
        "CFG_ATTR_DOC_LINKED_CONSTANT",
        "CROSS_CRATE_CFG_DOC_LINKED_CONSTANT",
        "PRIVATE_BINARY_DOC_LINKED_CONSTANT",
        "PROC_MACRO_CFG_DOC_LINKED_CONSTANT",
        "DocumentedEnum",
    ] {
        assert!(
            !stdout.contains(&format!("`{linked}` is public")),
            "linked declaration was diagnosed:\n{stdout}"
        );
    }
    for unlinked in [
        "UNLINKED_CONSTANT",
        "PRIVATE_LINKED_CONSTANT",
        "HIDDEN_LINKED_CONSTANT",
        "DocumentedType::unlinked_method",
        "DocumentedType::unlinked_field",
        "AliasTarget::alias_unlinked_method",
        "NO_INLINE_REEXPORT_LINKED_CONSTANT",
        "PRIVATE_TRAIT_IMPL_LINKED_CONSTANT",
        "HIDDEN_TRAIT_IMPL_LINKED_CONSTANT",
        "UNUSED_REFERENCE_DEFINITION",
        "HIDDEN_VARIANT_FIELD_LINKED_CONSTANT",
        "HIDDEN_NESTED_REEXPORT_LINKED_CONSTANT",
        "PRIVATE_INLINE_TRAIT_IMPL_LINKED_CONSTANT",
        "PRIVATE_REFERENCE_INLINE_TRAIT_IMPL_LINKED_CONSTANT",
        "PRIVATE_DYNAMIC_INLINE_TRAIT_IMPL_LINKED_CONSTANT",
    ] {
        assert!(
            stdout.contains(&format!("`{unlinked}` is public")),
            "unlinked declaration was not diagnosed:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("`DocumentedEnum::Linked` is a public enum variant"),
        "linked enum variant was diagnosed:\n{stdout}"
    );
    assert!(
        stdout.contains("`DocumentedEnum::Unlinked` is a public enum variant"),
        "unlinked enum variant was not diagnosed:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("`NamespaceCollision` is public").count(),
        1,
        "the type namespace should be preserved without preserving the value namespace:\n{stdout}"
    );
    assert!(
        !stdout.contains("cfg_doc_documented"),
        "documentation-only definitions must not become candidates:\n{stdout}"
    );
    for reexport in ["DirectDocumentedReexport", "AssociatedDocumentedReexport"] {
        assert!(
            !stdout.contains(&format!("public re-export `{reexport}`")),
            "re-export named by documentation was diagnosed:\n{stdout}"
        );
    }

    let rustdoc = context
        .cargo()
        .arg("doc")
        .arg("--manifest-path")
        .arg(context.workspace().join("Cargo.toml"))
        .arg("--workspace")
        .arg("--no-deps")
        .arg("--locked")
        .arg("--target-dir")
        .arg(context.target_dir())
        .env(
            "RUSTDOCFLAGS",
            "-D rustdoc::broken_intra_doc_links -D rustdoc::private_intra_doc_links",
        )
        .output()
        .expect("render fixture documentation");
    assert!(
        rustdoc.status.success(),
        "rustdoc failed:\n{}",
        context.normalized_stderr(&rustdoc)
    );
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
    assert!(stdout.contains("  hawk::dead_public: 1 (right-package: 1)"));
    assert!(!stdout.contains("right_shared: 1"));
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
fn reduces_uniform_restricted_field_visibility_together() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

mod uniform_restricted_fields {
    #[derive(Debug)]
    pub(crate) struct Fields {
        /// Used by the parent module.
        pub(crate) used_across_modules: u8,
        /// Used only within the defining module.
        pub(crate) used_inside_module: u8,
    }

    pub(crate) fn fields() -> Fields {
        let fields = Fields {
            used_across_modules: 1,
            used_inside_module: 2,
        };
        let _ = fields.used_inside_module;
        fields
    }
}

pub fn exercise_uniform_restricted_fields() {
    let _ = uniform_restricted_fields::fields().used_across_modules;
}
",
    );
    fs::write(&library_path, library).expect("add uniformly visible restricted fields");

    let app_path = context.workspace().join("app/src/main.rs");
    let app = fs::read_to_string(&app_path)
        .expect("read application source")
        .replacen(
            "fn main() {\n",
            "fn main() {\n    library::exercise_uniform_restricted_fields();\n",
            1,
        );
    fs::write(app_path, app).expect("exercise uniformly visible restricted fields");

    let output = context.run(&[
        "--fix",
        "--allow-no-vcs",
        "-W",
        "hawk::unnecessary_crate_visibility",
    ]);

    context.assert_success(&output);
    let library = fs::read_to_string(library_path).expect("read fixed library source");
    assert!(library.contains("        pub(super) used_across_modules: u8,"));
    assert!(library.contains("        pub(super) used_inside_module: u8,"));
}

#[test]
fn preserves_uniform_visibility_when_a_field_is_cfg_disabled() {
    let context = HawkTestContext::new("basic");
    let library_path = context.workspace().join("library/src/lib.rs");
    let mut library = fs::read_to_string(&library_path).expect("read library source");
    library.push_str(
        r"

mod cfg_uniform_fields {
    #[derive(Debug)]
    pub(crate) struct Fields {
        /// Used by the parent module.
        pub(crate) used_across_modules: u8,
        #[cfg(any())]
        pub(crate) cfg_disabled: u8,
        /// Used only within the defining module.
        pub(crate) used_inside_module: u8,
    }

    pub(crate) fn fields() -> Fields {
        let fields = Fields {
            used_across_modules: 1,
            used_inside_module: 2,
        };
        let _ = fields.used_inside_module;
        fields
    }
}

pub fn exercise_cfg_uniform_fields() {
    let _ = cfg_uniform_fields::fields().used_across_modules;
}
",
    );
    fs::write(&library_path, library).expect("add cfg-dependent uniformly visible fields");

    let app_path = context.workspace().join("app/src/main.rs");
    let app = fs::read_to_string(&app_path)
        .expect("read application source")
        .replacen(
            "fn main() {\n",
            "fn main() {\n    library::exercise_cfg_uniform_fields();\n",
            1,
        );
    fs::write(app_path, app).expect("exercise cfg-dependent uniformly visible fields");

    let output = context.run(&[
        "--fix",
        "--allow-no-vcs",
        "-W",
        "hawk::unnecessary_crate_visibility",
    ]);

    context.assert_success(&output);
    let library = fs::read_to_string(library_path).expect("read fixed library source");
    assert!(library.contains("        pub(crate) used_across_modules: u8,"));
    assert!(library.contains("        pub(crate) cfg_disabled: u8,"));
    assert!(library.contains("        pub(crate) used_inside_module: u8,"));
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

#[test]
fn reports_only_dead_public_findings() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&["--only", "dead-public"]);

    context.assert_success(&output);
    let stdout = context.normalized_stdout(&output);
    assert!(stdout.contains("warning[hawk::dead_public]"));
    assert!(!stdout.contains("warning[hawk::unnecessary_public]"));
    assert!(stdout.contains("warning[hawk::unknown_item]"));
    assert!(stdout.contains("warning[hawk::unfulfilled_expectation]"));
    assert!(stdout.contains("hawk: 17 finding(s)"));
    assert!(stdout.contains("  hawk::dead_public: 15 (library: 14, test_support: 1)"));
    assert!(stdout.contains("  hawk::unknown_item: 1 (configuration: 1)"));
    assert!(!stdout.contains("  hawk::unnecessary_public:"));
}

#[test]
fn reports_only_dead_public_findings_as_json() {
    let context = HawkTestContext::new("basic");
    let output = context.run(&["--only", "dead-public", "--output-format=json"]);

    context.assert_success(&output);
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout contains one JSON report");
    assert_eq!(report["schema_version"], 4);
    assert_eq!(report["summary"]["diagnostic_count"], 17);
    let diagnostics = report["diagnostics"]
        .as_array()
        .expect("diagnostics is an array");
    assert_eq!(diagnostics.len(), 17);
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic["category"] == "configuration"
                || diagnostic["code"] == "hawk::dead_public")
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["category"] == "configuration")
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic["code"] != "hawk::unnecessary_public")
    );
}
