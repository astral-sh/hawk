use std::env;
use std::process::Command;

fn main() {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(&rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("run rustc --print sysroot");
    assert!(output.status.success(), "rustc --print sysroot failed");
    let sysroot = String::from_utf8(output.stdout).expect("sysroot is utf-8");
    let sysroot = sysroot.trim();

    println!("cargo:rustc-link-search=native={sysroot}/lib");
    if env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("unix") {
        // Unit tests that link rustc_private run without the frontend's loader setup.
        println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{sysroot}/lib");
    }

    let output = Command::new(rustc)
        .arg("-vV")
        .output()
        .expect("run rustc -vV");
    assert!(output.status.success(), "rustc -vV failed");
    let version = String::from_utf8(output.stdout).expect("rustc version is utf-8");
    for (field, environment) in [
        ("release", "HAWK_RUSTC_RELEASE"),
        ("commit-hash", "HAWK_RUSTC_COMMIT_HASH"),
        ("host", "HAWK_RUSTC_HOST"),
    ] {
        let value = version
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{field}: ")))
            .unwrap_or_else(|| panic!("rustc -vV did not report {field}"));
        println!("cargo:rustc-env={environment}={value}");
    }
}
