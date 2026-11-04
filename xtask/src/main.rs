//! Repository automation for coblet, run through `cargo xtask <command>`.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "xtask is a terminal tool and reports results on stdout and stderr"
)]

use std::process::ExitCode;

mod module_roots;
mod workspace;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check-module-roots   reject definitions inside lib.rs and mod.rs files
  help                 print this message";

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    let outcome = match command.as_deref() {
        Some("check-module-roots") => module_roots::run(&workspace::root()),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
        None => Err(USAGE.to_owned()),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
