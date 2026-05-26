use std::process::Command;

#[test]
fn diagnoses_public_surface_of_a_binary_product() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/Cargo.toml");
    let target_dir = tempfile::tempdir().expect("temporary target directory");
    let graph_dir = tempfile::tempdir().expect("temporary graph directory");
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hawk::unnecessary_public: `internal_helper`"));
    assert!(stdout.contains("hawk::dead_public: `dead_entry`"));
    assert!(stdout.contains("hawk::dead_public: `InternalNamespace::dead_method`"));
    assert!(stdout.contains("hawk::unnecessary_public: `retained_helper`"));
    assert!(stdout.contains("hawk::unnecessary_public: `ConstructedTuple`"));
    assert!(stdout.contains("hawk::unnecessary_public: `ConstructedEnum`"));
    assert!(stdout.contains("hawk::dead_public: `DeadUnion`"));
    assert!(stdout.contains("hawk::dead_public: `ProductContext`"));
    assert!(stdout.contains("hawk::dead_public: `ContextOptionsAlias`"));
    assert!(stdout.contains("hawk::unnecessary_public: `PrivateContextOptions`"));
    assert!(stdout.contains("hawk::unnecessary_public: `InternalRenderer`"));
    assert!(stdout.contains("hawk::unnecessary_public: `InternalRenderResult`"));
    assert!(!stdout.contains("`ProductValue`"));
    assert!(!stdout.contains("`ContextOptions`"));
    assert!(!stdout.contains("exported::ReexportedValue"));
    assert!(!stdout.contains("`TypeCheckedAcrossCrates`"));
    assert!(!stdout.contains("`PublicRenderResult`"));
    assert!(!stdout.contains("`passthrough`"));
    assert!(!stdout.contains("{use#"));
    assert!(stdout.contains("hawk: 15 finding(s)"));
}
