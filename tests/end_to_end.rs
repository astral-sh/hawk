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
    insta::assert_snapshot!(stdout.as_ref());
}
