use std::fs;
use std::path::Path;
use std::process::Command;

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
fn prints_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .args(["hawk", "--version"])
        .output()
        .expect("run cargo-hawk --version");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("version output is UTF-8"),
        concat!("cargo hawk ", env!("CARGO_PKG_VERSION"), "\n")
    );
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn honors_cargo_configured_compiler() {
    let source_workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());

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
    let configured_compiler = workspace.path().join("custom-compiler");
    symlink(rustc, &configured_compiler).expect("create renamed compiler symlink");

    let cargo_config = workspace.path().join(".cargo");
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
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("-A")
        .arg("warnings")
        .env("PATH", path)
        .env_remove("RUSTC")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .output()
        .expect("run cargo-hawk");

    assert!(
        output.status.success(),
        "cargo-hawk failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let graph_dir = tempfile::tempdir().expect("temporary graph directory");
    let unrelated_json = graph_dir.path().join("unrelated.json");
    std::fs::write(&unrelated_json, "{}").expect("write unrelated JSON file");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target")
        .arg(host_target)
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--graph-dir")
        .arg(graph_dir.path())
        .output()
        .expect("run cargo-hawk");

    assert!(
        output.status.success(),
        "cargo-hawk failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(unrelated_json.exists());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = anstream::adapter::strip_str(&stdout).to_string();
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
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/production_consumers/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk");

    assert!(
        output.status.success(),
        "cargo-hawk failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("`secondary_api` is public"));
    assert!(stdout.contains(
        "`unused` is public but is not reachable from the configured production binaries"
    ));
    assert!(
        stdout
            .contains("for 2 configured production binaries and workspace non-production targets")
    );
}

#[test]
fn production_products_reuse_shared_dependency_compilations() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/production_consumers/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("-A")
        .arg("warnings")
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .expect("run cargo-hawk");

    assert!(
        output.status.success(),
        "cargo-hawk failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
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
fn requires_a_configured_production_binary() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/Cargo.toml");
    let configuration = tempfile::NamedTempFile::new().expect("temporary empty configuration");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--config")
        .arg(configuration.path())
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=always")
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
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("-D")
        .arg("warnings")
        .arg("-W")
        .arg("hawk::unnecessary_public")
        .arg("-A")
        .arg("hawk::unknown_item")
        .arg("--target-dir")
        .arg(target_dir.path())
        .output()
        .expect("run cargo-hawk");

    assert!(
        !output.status.success(),
        "denied diagnostic did not fail:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = anstream::adapter::strip_str(&stdout).to_string();
    assert!(stdout.contains("error[hawk::dead_public]"));
    assert!(stdout.contains("warning[hawk::unnecessary_public]"));
    assert!(stdout.contains("error[hawk::unfulfilled_expectation]"));
    assert!(!stdout.contains("hawk::unknown_item"));
    assert!(stdout.contains("hawk: 41 finding(s)"));
}

#[test]
fn applies_visibility_fixes_through_cargo_fix() {
    let source_workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("`dead_entry` is public"));
    assert!(stdout.contains("`ProductEnum::Unused`"));
    assert!(!stdout.contains("`internal_helper`"));

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read fixed source");
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

    let test_support = fs::read_to_string(workspace.path().join("test_support/src/lib.rs"))
        .expect("read fixed test-support source");
    assert!(test_support.contains("pub fn entry() {"));
    assert!(test_support.contains("fn helper() {}"));
    assert!(test_support.contains("pub fn dead_test_surface() {}"));

    let unit_support = fs::read_to_string(workspace.path().join("unit_support/src/lib.rs"))
        .expect("read fixed unit-test source");
    assert!(unit_support.contains("pub fn product_entry() {}"));
    assert!(unit_support.contains("pub fn not_exported() {}"));
    assert!(unit_support.contains("fn test_entry() {"));
    assert!(unit_support.contains("fn test_only_helper() {}"));
}

#[test]
fn applies_multiple_fix_passes_in_a_clean_git_repository() {
    let source_workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    initialize_git_repository(workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dead_public_findings_are_not_fixed_into_dead_code_errors() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dead_public_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("`dead_api` is public"));

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read source");
    assert!(library.contains("pub fn dead_api() {}"));
}

#[test]
fn benchmark_consumers_preserve_required_public_visibility() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/non_production_targets/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk");

    assert!(
        output.status.success(),
        "cargo-hawk failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("`bench_api` is public"));
    assert!(
        !stdout.contains("`BenchMode::OnlyBench`"),
        "benchmark-executed variant was diagnosed:\n{stdout}"
    );
    assert!(stdout.contains("`unused` is public"));
}

#[test]
fn exported_symbols_are_treated_as_external_roots() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/exported_symbols/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk");

    assert!(
        output.status.success(),
        "cargo-hawk failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("warning[hawk::dead_public]: `exported_callback` is public"));
    assert!(!stdout.contains("warning[hawk::dead_public]: `renamed_callback` is public"));
    assert!(stdout.contains("warning[hawk::unnecessary_public]: `exported_callback` is public"));
    assert!(stdout.contains("warning[hawk::unnecessary_public]: `renamed_callback` is public"));
}

#[test]
fn doctest_consumers_preserve_required_public_visibility_during_fixes() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/doctest_consumers");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("`unused` is public"));

    let doctest = Command::new("cargo")
        .arg("test")
        .arg("--doc")
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--package")
        .arg("library")
        .arg("--locked")
        .arg("--target-dir")
        .arg(target_dir.path())
        .output()
        .expect("run doctests after fixes");
    assert!(
        doctest.status.success(),
        "doctests failed after cargo-hawk fixes:\n{}",
        String::from_utf8_lossy(&doctest.stderr)
    );

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read fixed source");
    assert!(library.contains("pub fn doc_api() {}"));
    assert!(library.contains("pub fn unused() {}"));
}

#[test]
fn fixes_grouped_public_reexports_only_when_all_aliases_are_safe() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/grouped_reexport_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("public re-export `Narrow`"));

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read fixed source");
    assert!(library.contains("pub use exported::{Kept, Narrow};"));
    assert!(library.contains("pub(crate) use split_consumers::{ProductionOnly, TestOnly};"));
}

#[test]
fn fixes_only_the_matching_cfg_alternative_declaration() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cfg_alternative_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read fixed source");
    assert!(library.contains("#[cfg(not(test))]\npub fn dual() {}"));
    assert!(library.contains("#[cfg(test)]\nfn dual() {}"));
}

#[test]
fn removes_unnecessary_restricted_visibility_by_default() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crate_visibility_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read fixed source");
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
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/path_module_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(workspace.path().join("Cargo.toml"))
        .arg("--fix")
        .arg("--allow-no-vcs")
        .arg("--target-dir")
        .arg(target_dir.path())
        .arg("--color=never")
        .output()
        .expect("run cargo-hawk with fixes");

    assert!(
        output.status.success(),
        "cargo-hawk fix failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let shared = fs::read_to_string(workspace.path().join("library/src/shared.rs"))
        .expect("read fixed source");
    assert!(shared.contains("pub struct Shared"));
    assert!(shared.contains("    pub(crate) value: u8,"));
}

#[test]
fn narrows_crate_visibility_to_the_required_module_scope_when_enabled() {
    let source_workspace =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crate_visibility_fixes");
    let workspace = tempfile::tempdir().expect("temporary fixture workspace");
    copy_directory(&source_workspace, workspace.path());
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
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

    let library =
        fs::read_to_string(workspace.path().join("library/src/lib.rs")).expect("read fixed source");
    assert!(library.contains("pub(super) fn run() {"));
    assert!(library.contains("    fn private_helper() {}"));
    assert!(library.contains("    fn private_parent_visible_helper() {}"));
    assert!(library.contains("    fn private_formatted_helper() {}"));
    assert!(library.contains("    fn parent_helper() {}"));
    assert!(library.contains("        pub(super) fn call_parent_helper() {"));
    assert!(library.contains("    pub(crate) mod api {"));
}
