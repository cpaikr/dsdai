//! `dsd`: the process boundary. Parses arguments, runs one command, and
//! writes exactly one stdout document: a JSON envelope, or text for help,
//! `--version`, and `--format text` views.

// Failures carry their whole report and occur once per process.
#![allow(clippy::result_large_err)]

mod cli;
mod commands;
mod envelope;
mod help;

use std::io::Write as _;
use std::process::ExitCode;

use clap::Parser;
use clap::error::ErrorKind;

use crate::cli::Cli;
use crate::envelope::{Envelope, Failure};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn write_stdout(text: &str) {
    let mut out = std::io::stdout().lock();
    // A closed pipe leaves nothing useful to report.
    let _ = out.write_all(text.as_bytes()).and_then(|()| out.flush());
}

fn command_name(argv: &[String]) -> String {
    argv.iter().skip(1).find(|a| !a.starts_with('-')).cloned().unwrap_or_default()
}

fn main() -> ExitCode {
    let argv_os: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Arguments that are not valid Unicode cannot name anything this tool
    // matches, so they are an invalid invocation rather than a panic.
    let Some(argv) = argv_os.iter().map(|a| a.to_str().map(str::to_owned)).collect::<Option<Vec<String>>>() else {
        let failure =
            Failure::usage("arguments must be valid Unicode".to_owned(), "run `dsd --help` for commands".to_owned());
        write_stdout(&Envelope::failure("", &failure).render(false));
        return ExitCode::from(2);
    };
    let pretty = argv.iter().any(|a| a == "--pretty");
    // Bare `dsd` is exactly `dsd --help`: the same parser renders both.
    let parse_args = if argv.len() == 1 { vec![argv[0].clone(), "--help".to_owned()] } else { argv.clone() };
    let cli = match Cli::try_parse_from(&parse_args) {
        Ok(cli) => cli,
        Err(e) if matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) => {
            write_stdout(&e.to_string());
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            let rendered = e.to_string();
            let message =
                rendered.lines().next().unwrap_or("invalid arguments").trim_start_matches("error: ").to_owned();
            let command = command_name(&argv);
            let hint = if command.is_empty() || e.kind() == ErrorKind::InvalidSubcommand {
                "run `dsd --help` for commands".to_owned()
            } else {
                format!("run `dsd {command} --help` for options and examples")
            };
            let failure = Failure::usage(message, hint);
            write_stdout(&Envelope::failure(&command, &failure).render(pretty));
            return ExitCode::from(2);
        }
    };
    let name = cli.command.name();
    match commands::run(cli.command) {
        Ok(commands::Output::Text(text)) => {
            write_stdout(&text);
            ExitCode::SUCCESS
        }
        Ok(commands::Output::Json { result, warnings }) => {
            write_stdout(&Envelope::success(name, result, warnings).render(cli.pretty));
            ExitCode::SUCCESS
        }
        Err(failure) => {
            write_stdout(&Envelope::failure(name, &failure).render(cli.pretty));
            ExitCode::from(failure.exit_code())
        }
    }
}
