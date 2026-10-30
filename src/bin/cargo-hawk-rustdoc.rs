use std::env;
use std::path::Path;
use std::process::{Command, ExitCode};

use cargo_hawk_internal::protocol;

fn main() -> ExitCode {
    let Some(rustdoc) = env::var_os(protocol::RUSTDOC_ENV) else {
        eprintln!("hawk: cargo-hawk-rustdoc is an internal rustdoc wrapper");
        return ExitCode::FAILURE;
    };
    let mut command = Command::new(&rustdoc);
    command
        .args(env::args_os().skip(1))
        .env("RUSTC_BOOTSTRAP", "1")
        .env("RUSTDOC", &rustdoc)
        .env_remove(protocol::RUSTDOC_ENV)
        // Rustdoc generates fresh doctest crates outside Cargo's unit graph.
        .env_remove(protocol::FRAGMENT_CACHE_ENV);
    #[cfg(unix)]
    let error = std::os::unix::process::CommandExt::exec(&mut command);
    #[cfg(not(unix))]
    let error = match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => error,
    };
    eprintln!(
        "hawk: could not run rustdoc {}: {error}",
        Path::new(&rustdoc).display()
    );
    ExitCode::FAILURE
}
