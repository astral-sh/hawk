use std::env;
use std::error::Error;
use std::io;
use std::process::Command;

fn command_output(command: &mut Command, description: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{description} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
        .into());
    }
    Ok(output.stdout)
}

fn main() -> Result<(), Box<dyn Error>> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = command_output(
        Command::new(&rustc).args(["--print", "sysroot"]),
        "rustc --print sysroot",
    )?;
    let sysroot = String::from_utf8(sysroot)?;
    let sysroot = sysroot.trim();

    println!("cargo:rustc-link-search=native={sysroot}/lib");
    if env::var("CARGO_CFG_TARGET_FAMILY").as_deref() == Ok("unix") {
        // Unit tests that link rustc_private run without the frontend's loader setup.
        println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{sysroot}/lib");
    }

    let version = command_output(Command::new(rustc).arg("-vV"), "rustc -vV")?;
    let version = String::from_utf8(version)?;
    for (field, environment) in [
        ("release", "HAWK_RUSTC_RELEASE"),
        ("commit-hash", "HAWK_RUSTC_COMMIT_HASH"),
        ("host", "HAWK_RUSTC_HOST"),
    ] {
        let value = version
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{field}: ")))
            .ok_or_else(|| io::Error::other(format!("rustc -vV did not report {field}")))?;
        println!("cargo:rustc-env={environment}={value}");
    }

    // The driver links unstable rustc_private APIs whose signatures drift between
    // compiler versions; gate the affected call sites on the selected compiler.
    println!("cargo:rustc-check-cfg=cfg(hawk_rustc_1_99)");
    let release = version
        .lines()
        .find_map(|line| line.strip_prefix("release: "))
        .ok_or_else(|| io::Error::other("rustc -vV did not report release"))?;
    let minor = release
        .split('.')
        .nth(1)
        .and_then(|minor| minor.parse::<u32>().ok())
        .ok_or_else(|| io::Error::other(format!("unrecognized rustc release {release}")))?;
    if minor >= 99 {
        println!("cargo:rustc-cfg=hawk_rustc_1_99");
    }
    Ok(())
}
