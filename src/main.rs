mod cli;
mod config;
// The frontend and compiler driver use different halves of the shared graph model.
#[allow(dead_code)]
mod graph;

fn main() -> std::process::ExitCode {
    let Ok(args): Result<Vec<String>, _> = std::env::args_os()
        .map(std::ffi::OsString::into_string)
        .collect()
    else {
        eprintln!("hawk: command-line arguments must be valid UTF-8");
        return std::process::ExitCode::FAILURE;
    };
    if let Some(exit_code) = cli::run_rustc_probe(&args) {
        return exit_code;
    }
    match cli::run(args.clone()) {
        Ok(exit_code) => exit_code,
        Err(error) => {
            if let Err(output_error) = cli::write_error(&args, &error) {
                eprintln!("hawk: {error:#}: {output_error:#}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}
