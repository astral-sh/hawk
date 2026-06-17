#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_lint_defs;
extern crate rustc_middle;
extern crate rustc_parse;
extern crate rustc_session;
extern crate rustc_span;

#[path = "../driver.rs"]
mod driver;
// The frontend and compiler driver use different halves of the shared graph model.
#[allow(dead_code)]
#[path = "../graph.rs"]
mod graph;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if driver::is_wrapper_invocation(&args) {
        driver::run_wrapper(args)
    } else {
        eprintln!("hawk: cargo-hawk-driver is an internal compiler wrapper");
        std::process::ExitCode::FAILURE
    }
}
