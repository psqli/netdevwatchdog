use std::path::PathBuf;

use clap::{ArgGroup, Parser};

const AFTER_HELP: &str = "\
Without a command the watchdog runs until stopped (Windows: in the notification area).

On Windows, pipe the output to see it in PowerShell, e.g.:
  .\\netdevwatchdog.exe --once | Out-String";

/// Network Device Watchdog - monitors network printers and devices and alerts on failures.
#[derive(Debug, Default, Parser)]
#[command(version, after_help = AFTER_HELP, group(ArgGroup::new("command").multiple(false)))]
pub struct Cli {
    /// Check every device once, print the result and exit (exit code 1 if any device has a problem).
    #[arg(long, group = "command")]
    pub once: bool,

    /// Send a test alert through every notification channel.
    #[arg(long, group = "command")]
    pub test_alert: bool,

    /// Check the configuration file and exit.
    #[arg(long, group = "command")]
    pub validate: bool,

    /// Write a sample configuration file.
    #[arg(long, group = "command")]
    pub init: bool,

    /// Start the watchdog automatically at logon (Windows: current user; Linux: systemd user service).
    #[arg(long, group = "command")]
    pub install: bool,

    /// Undo --install and stop the running watchdog.
    #[arg(long, group = "command")]
    pub uninstall: bool,

    /// Configuration file. Default: watchdog.json next to the program if present,
    /// otherwise the user configuration folder.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Override the configured language (en, pt, es, hi, zh, ar...).
    #[arg(long, visible_alias = "lang", value_name = "TAG")]
    pub language: Option<String>,

    /// Allow --init to overwrite an existing file.
    #[arg(long, requires = "init")]
    pub force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Run,
    Once,
    TestAlert,
    Validate,
    Init,
    Install,
    Uninstall,
}

impl Cli {
    pub fn command(&self) -> Command {
        match () {
            _ if self.once => Command::Once,
            _ if self.test_alert => Command::TestAlert,
            _ if self.validate => Command::Validate,
            _ if self.init => Command::Init,
            _ if self.install => Command::Install,
            _ if self.uninstall => Command::Uninstall,
            _ => Command::Run,
        }
    }

    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("netdevwatchdog").chain(args.iter().copied()))
    }

    #[test]
    fn commands_and_options() {
        assert_eq!(parse(&[]).unwrap().command(), Command::Run);
        let cli = parse(&["--once", "--config", "a.json", "--lang", "pt"]).unwrap();
        assert_eq!(cli.command(), Command::Once);
        assert_eq!(cli.config, Some(PathBuf::from("a.json")));
        assert_eq!(cli.language(), Some("pt"));
        assert!(parse(&["--init", "--force"]).unwrap().force);
    }

    #[test]
    fn rejects_conflicts_and_unknown_arguments() {
        assert!(parse(&["--once", "--validate"]).is_err());
        assert!(parse(&["--force"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
        assert!(parse(&["--config"]).is_err());
    }
}
