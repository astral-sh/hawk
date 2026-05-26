use std::process::Command;

#[test]
fn diagnoses_public_surface_of_a_binary_product() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let graph_dir = tempfile::tempdir().expect("temporary graph directory");
    let unrelated_json = graph_dir.path().join("unrelated.json");
    std::fs::write(&unrelated_json, "{}").expect("write unrelated JSON file");
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-hawk"))
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--package")
        .arg("app")
        .arg("--bin")
        .arg("app")
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
    let stdout = anstream::adapter::strip_str(&stdout);
    insta::assert_snapshot!(stdout, @r###"
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

    warning[hawk::unnecessary_public]: `InternalNamespace::live_inside_crate` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:110:5
        |
    110 |     pub fn live_inside_crate() {}
        |     ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `InternalNamespace::dead_method` is public but is not reachable from binary `app`
      --> library/src/lib.rs:112:5
        |
    112 |     pub fn dead_method() {}
        |     ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::unnecessary_public]: `ConstructedTuple` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:119:1
        |
    119 | pub struct ConstructedTuple(u8);
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::unnecessary_public]: `ConstructedEnum` is public but all reachable uses are within `library`; it can be `pub(crate)`
      --> library/src/lib.rs:121:1
        |
    121 | pub enum ConstructedEnum {
        | ^^^ public declaration
        = help: change this declaration to `pub(crate)`

    warning[hawk::dead_public]: `DeadUnion` is public but is not reachable from binary `app`
      --> library/src/lib.rs:125:1
        |
    125 | pub union DeadUnion {
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `dead_entry` is public but is not reachable from binary `app`
      --> library/src/lib.rs:142:1
        |
    142 | pub fn dead_entry() {
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `dead_helper` is public but is not reachable from binary `app`
      --> library/src/lib.rs:146:1
        |
    146 | pub fn dead_helper() {}
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

    warning[hawk::dead_public]: `dead_code_allowed_helper` is public but is not reachable from binary `app`
      --> library/src/lib.rs:153:1
        |
    153 | pub fn dead_code_allowed_helper() {}
        | ^^^ public declaration
        = help: consider restricting this declaration's visibility or removing it

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

    hawk: 16 finding(s) for `app --bin app --all-features` on the host target
    "###);
}
