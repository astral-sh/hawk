use std::env;
use std::process::Command;

fn main() {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .expect("run rustc --print sysroot");
    assert!(output.status.success(), "rustc --print sysroot failed");
    let sysroot = String::from_utf8(output.stdout).expect("sysroot is utf-8");
    let sysroot = sysroot.trim();

    println!("cargo:rustc-link-search=native={sysroot}/lib");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{sysroot}/lib");
}
