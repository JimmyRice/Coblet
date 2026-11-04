//! Repository automation for coblet, run through `cargo xtask <command>`.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "xtask is a terminal tool and reports results on stdout and stderr"
)]

use std::process::ExitCode;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  help                 print this message";

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("error: unknown command `{other}`\n\n{USAGE}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
