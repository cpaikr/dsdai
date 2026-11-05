//! `dsd`: the process boundary. Parses arguments, runs one command, and
//! writes exactly one stdout document: a JSON envelope, or text for help,
//! `--version`, and `--format text` views. stderr stays empty except for one
//! release notice after a text view and `upgrade` stages in a terminal.

// Failures carry their whole report and occur once per process.
#![allow(clippy::result_large_err)]

mod cli;
mod commands;
mod envelope;
mod help;
mod release_check;
mod upgrade;

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

/// The human form of an advisory, after `--format text` output: one stderr line.
fn write_notice(advisory: &serde_json::Value) {
    let message = advisory["message"].as_str().unwrap_or_default();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "dsd notice: {message}").and_then(|()| err.flush());
}

fn command_name(argv: &[String]) -> String {
    argv.iter().skip(1).find(|a| !a.starts_with('-')).cloned().unwrap_or_default()
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
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
    // Only a successful command is followed by a release check; it can add a
    // notice but never changes the result or exit status.
    let checks_release = cli.command.checks_release();
    let advisory = || checks_release.then(release_check::incidental_advisory).flatten();
    match commands::run(cli.command) {
        Ok(commands::Output::Text(text)) => {
            write_stdout(&text);
            if let Some(advisory) = advisory() {
                write_notice(&advisory);
            }
            ExitCode::SUCCESS
        }
        Ok(commands::Output::Json { result, warnings }) => {
            let envelope = Envelope::success(name, result, warnings).with_advisories(advisory().into_iter().collect());
            write_stdout(&envelope.render(cli.pretty));
            ExitCode::SUCCESS
        }
        Err(failure) => {
            write_stdout(&Envelope::failure(name, &failure).render(cli.pretty));
            ExitCode::from(failure.exit_code())
        }
    }
}
