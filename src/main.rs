#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod cli;
mod config;
mod driver;
mod graph;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if driver::is_wrapper_invocation(&args) {
        return driver::run_wrapper(args);
    }

    match cli::run(args) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("hawk: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
