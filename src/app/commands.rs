//! Commands that work the same on every platform.

use std::path::Path;
use std::sync::Arc;

use netdevwatchdog::config::Config;
use netdevwatchdog::i18n::Localizer;
use netdevwatchdog::monitor::{Health, Snapshot};
use netdevwatchdog::msg;
use tokio_util::sync::CancellationToken;

use super::cli::Cli;
use super::runtime::{Hooks, LoadError, Runtime, supervise};
use super::{paths, startup};

pub const SUCCESS: u8 = 0;
pub const PROBLEMS: u8 = 1;
pub const USAGE_ERROR: u8 = 2;

/// Localizer usable before a configuration is loaded.
pub fn early_localizer(cli: &Cli) -> Localizer {
    Localizer::new(cli.language(), None)
}

pub fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

pub fn init(cli: &Cli, path: &Path) -> Result<u8, String> {
    if path.exists() && !cli.force {
        println!("{} already exists. Use --force to overwrite it.", path.display());
        return Ok(USAGE_ERROR);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, paths::SAMPLE_CONFIG).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("{}", early_localizer(cli).format(&msg!("app.configCreated", path = path.display().to_string())));
    Ok(SUCCESS)
}

pub fn validate(cli: &Cli, path: &Path) -> u8 {
    if !path.is_file() {
        println!("{}", LoadError::NotFound(path.to_owned()).describe(&early_localizer(cli)));
        return PROBLEMS;
    }
    match Config::load(path) {
        Ok(config) => {
            let l = Localizer::new(Some(cli.language().unwrap_or(&config.language)), Some(&paths::locales_dir(&config, path)));
            println!("{}", l.format(&msg!("app.configValid", count = config.devices.len())));
            SUCCESS
        }
        Err(e) => {
            println!("{}", LoadError::Invalid(e).describe(&early_localizer(cli)));
            PROBLEMS
        }
    }
}

fn status(l: &Localizer, s: &Snapshot) -> String {
    match s.health {
        Health::Ok => l.get("status.ok").to_owned(),
        Health::Busy => l.get("status.busy").to_owned(),
        Health::Problem => s.failure.map(|f| l.format(&f.title())).unwrap_or_else(|| l.get("status.problem").to_owned()),
        Health::Unknown => l.get("status.unknown").to_owned(),
    }
}

/// One line per device: name, status, explanation and notes.
pub fn format_table(l: &Localizer, snapshots: &[Snapshot]) -> String {
    let rows: Vec<(String, String, String)> = snapshots
        .iter()
        .map(|s| {
            let mut message = s.message.as_ref().map(|m| l.format(m)).unwrap_or_default();
            if !s.notes.is_empty() {
                let notes: Vec<String> = s.notes.iter().map(|n| l.format(n)).collect();
                if !message.is_empty() {
                    message.push(' ');
                }
                message.push_str(&format!("({})", notes.join(l.get("list.separator"))));
            }
            (s.device.clone(), status(l, s), message)
        })
        .collect();
    let width = |f: fn(&(String, String, String)) -> &String| rows.iter().map(|r| f(r).chars().count()).max().unwrap_or(0).max(6);
    let (w1, w2) = (width(|r| &r.0), width(|r| &r.1));
    let pad = |s: &str, w: usize| format!("{s}{}", " ".repeat(w.saturating_sub(s.chars().count())));
    rows.iter().map(|(d, st, m)| format!("{}  {}  {m}", pad(d, w1), pad(st, w2)).trim_end().to_owned() + "\n").collect()
}

/// Checks every device once and prints a table. Exit code 1 if any device has a problem.
pub fn once(cli: &Cli, path: &Path) -> Result<u8, LoadError> {
    let runtime = Runtime::load(path, cli.language(), false, &|_, _| Vec::new())?;
    let snapshots = tokio_runtime().block_on(runtime.service.check_all());
    print!("{}", format_table(runtime.localizer(), &snapshots));
    Ok(if snapshots.iter().any(|s| s.health == Health::Problem) { PROBLEMS } else { SUCCESS })
}

/// Test alert without a user interface (log, ntfy, command).
pub fn test_alert_console(cli: &Cli, path: &Path) -> Result<u8, LoadError> {
    let runtime = Runtime::load(path, cli.language(), true, &|_, _| Vec::new())?;
    tokio_runtime().block_on(runtime.service.send_test_alert());
    Ok(SUCCESS)
}

pub fn install(cli: &Cli, path: &Path) -> Result<u8, String> {
    let l = early_localizer(cli);
    if !path.is_file() {
        println!("{}", LoadError::NotFound(path.to_owned()).describe(&l));
        return Ok(PROBLEMS);
    }
    let target = startup::install(path).map_err(|e| e.to_string())?;
    if cfg!(windows) {
        println!("{}", l.format(&msg!("app.installed", path = path.display().to_string())));
    } else {
        println!("{}", l.format(&msg!("app.installLinux", path = target)));
    }
    Ok(SUCCESS)
}

pub fn uninstall(cli: &Cli) -> Result<u8, String> {
    startup::uninstall().map_err(|e| e.to_string())?;
    println!("{}", early_localizer(cli).get("app.uninstalled"));
    Ok(SUCCESS)
}

/// Cancels `stop` on Ctrl+C or SIGTERM.
pub fn stop_on_signals(stop: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            match signal(SignalKind::terminate()) {
                Ok(mut term) => {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {}
                        _ = term.recv() => {}
                    }
                }
                Err(_) => {
                    let _ = tokio::signal::ctrl_c().await;
                }
            }
        }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        stop.cancel();
    });
}

/// Runs in the foreground until Ctrl+C, SIGTERM or `--uninstall`, reloading
/// the configuration when the file changes (Linux under systemd, or a terminal).
pub fn run_console(cli: &Cli, path: &Path) -> Result<u8, LoadError> {
    let Ok(Some(_instance)) = startup::claim_instance() else {
        eprintln!("{}", early_localizer(cli).get("app.alreadyRunning"));
        return Ok(PROBLEMS);
    };
    let runtime = Arc::new(Runtime::load(path, cli.language(), true, &|_, _| Vec::new())?);
    tokio_runtime().block_on(async {
        let stop = CancellationToken::new();
        stop_on_signals(stop.clone());
        supervise(runtime, path, cli.language(), true, &Hooks::console(), stop).await;
    });
    Ok(SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use netdevwatchdog::checks::FailureCode;
    use netdevwatchdog::i18n::Msg;

    fn snap(device: &str, health: Health) -> Snapshot {
        Snapshot {
            device: device.into(),
            host: "h".into(),
            health,
            failure: None,
            message: None,
            notes: vec![],
            alerted: false,
            last_check: None,
        }
    }

    #[test]
    fn table_aligns_columns() {
        let l = Localizer::new(Some("en"), None);
        let mut problem = snap("Kitchen printer", Health::Problem);
        problem.failure = Some(FailureCode::DeviceError);
        problem.message = Some(netdevwatchdog::msg!("msg.deviceError", reasons = vec![Msg::key("reason.coverOpen")]));
        let mut ok = snap("Bar", Health::Ok);
        ok.notes = vec![Msg::key("reason.paperNearEnd")];
        let table = format_table(&l, &[ok, problem, snap("Office", Health::Unknown)]);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "Bar              OK               (paper running low)");
        assert_eq!(lines[1], "Kitchen printer  Device error     The device reports: cover open.");
        assert_eq!(lines[2], "Office           Not checked yet");
    }
}
