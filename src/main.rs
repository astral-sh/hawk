use cargo_hawk_internal::protocol;

mod cli;
mod config;
mod diagnostics;
mod toolchain;

fn main() -> std::process::ExitCode {
    let Ok(args): Result<Vec<String>, _> = std::env::args_os()
        .map(std::ffi::OsString::into_string)
        .collect()
    else {
        eprintln!("hawk: command-line arguments must be valid UTF-8");
        return std::process::ExitCode::FAILURE;
    };
    if let Some(exit_code) = toolchain::run_rustc_probe(&args) {
        return exit_code;
    }
    match cli::run(args.clone()) {
        Ok(exit_code) => exit_code,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe) =>
        {
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            if let Err(output_error) = cli::write_error(&args, &error) {
                eprintln!("hawk: {error:#}: {output_error:#}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}
