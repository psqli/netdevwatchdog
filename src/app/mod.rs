//! The executable: command line, configuration lookup, and hosting the
//! service in a terminal, under systemd, or as a desktop application.

mod cli;
mod commands;
mod paths;
mod runtime;
mod startup;
mod system;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use commands::{PROBLEMS, early_localizer};

pub fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            system::attach_console();
            let _ = e.print();
            return ExitCode::from(if e.use_stderr() { commands::USAGE_ERROR } else { commands::SUCCESS });
        }
    };
    let command = cli.command();
    // The Windows desktop build has no console of its own; command-line use
    // writes to the console it was started from.
    if command != Command::Run {
        system::attach_console();
    }
    let path = paths::resolve_config(cli.config.as_deref());

    let result = match command {
        Command::Init => commands::init(&cli, &path).map_err(Failure::Other),
        Command::Validate => Ok(commands::validate(&cli, &path)),
        Command::Install => commands::install(&cli, &path).map_err(Failure::Other),
        Command::Uninstall => commands::uninstall(&cli).map_err(Failure::Other),
        Command::Once => commands::once(&cli, &path).map_err(Failure::Load),
        Command::TestAlert => test_alert(&cli, &path).map_err(Failure::Load),
        Command::Run => run(&cli, &path).map_err(Failure::Load),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            let message = match failure {
                Failure::Load(e) => e.describe(&early_localizer(&cli)),
                Failure::Other(e) => e,
            };
            eprintln!("{message}");
            // Started from the Run key or a double-click there is no console: show a dialog.
            if matches!(command, Command::Run | Command::TestAlert) {
                system::error_dialog(&message);
            }
            ExitCode::from(PROBLEMS)
        }
    }
}

enum Failure {
    Load(runtime::LoadError),
    Other(String),
}

fn run(cli: &Cli, path: &std::path::Path) -> Result<u8, runtime::LoadError> {
    commands::run_console(cli, path)
}

fn test_alert(cli: &Cli, path: &std::path::Path) -> Result<u8, runtime::LoadError> {
    commands::test_alert_console(cli, path)
}
