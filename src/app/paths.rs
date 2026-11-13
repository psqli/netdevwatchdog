//! Where the configuration, logs and locale overrides live.

use std::path::{Path, PathBuf};

use netdevwatchdog::config::Config;

pub const CONFIG_FILE: &str = "watchdog.json";
pub const SAMPLE_CONFIG: &str = include_str!("../../watchdog.json");

fn env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Per-user folder: `%LOCALAPPDATA%\NetworkDeviceWatchdog` or
/// `$XDG_CONFIG_HOME/network-device-watchdog` (default `~/.config/...`).
pub fn user_dir() -> PathBuf {
    if cfg!(windows) {
        let base =
            env("LOCALAPPDATA").or_else(|| env("USERPROFILE").map(|h| h.join("AppData").join("Local"))).unwrap_or_else(|| ".".into());
        base.join("NetworkDeviceWatchdog")
    } else {
        let base = env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config"))).unwrap_or_else(|| ".".into());
        base.join("network-device-watchdog")
    }
}

pub fn executable() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("netdevwatchdog"))
}

/// The explicit path, else `watchdog.json` next to the program (portable
/// install), else the user folder.
pub fn resolve_config(explicit: Option<&Path>) -> PathBuf {
    if let Some(p) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    }
    let beside = executable().parent().map(|d| d.join(CONFIG_FILE));
    match beside {
        Some(b) if b.is_file() => b,
        _ => user_dir().join(CONFIG_FILE),
    }
}

pub fn log_dir(config: &Config, config_path: &Path) -> PathBuf {
    relative_to_config(config.log_directory.as_deref(), config_path, "logs")
}

pub fn locales_dir(config: &Config, config_path: &Path) -> PathBuf {
    relative_to_config(config.locales_directory.as_deref(), config_path, "locales")
}

fn relative_to_config(value: Option<&str>, config_path: &Path, default: &str) -> PathBuf {
    let dir = config_path.parent().unwrap_or(Path::new("."));
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => dir.join(default),
        Some(v) => {
            let p = PathBuf::from(expand_env(v));
            if p.is_absolute() { p } else { dir.join(p) }
        }
    }
}

/// Expands `%NAME%` (any platform) and a leading `~/` (Unix).
pub fn expand_env(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else { break };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    if cfg!(unix)
        && let Some(tail) = out.strip_prefix("~/")
        && let Some(home) = env("HOME")
    {
        return home.join(tail).to_string_lossy().into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_variables_and_keeps_unknown_ones() {
        // SAFETY: tests in this module do not read this variable concurrently.
        unsafe { std::env::set_var("NDW_TEST_DIR", "/srv/ndw") };
        assert_eq!(expand_env("%NDW_TEST_DIR%/logs"), "/srv/ndw/logs");
        assert_eq!(expand_env("%NDW_NOT_SET%/x"), "%NDW_NOT_SET%/x");
        assert_eq!(expand_env("100%"), "100%");
    }

    #[test]
    fn directories_are_relative_to_the_config_file() {
        let cfg = Config { log_directory: Some("my-logs".into()), ..Config::default() };
        let path = Path::new("/etc/ndw/watchdog.json");
        assert_eq!(log_dir(&cfg, path), Path::new("/etc/ndw/my-logs"));
        assert_eq!(locales_dir(&cfg, path), Path::new("/etc/ndw/locales"));
        let cfg = Config { log_directory: Some("/var/log/ndw".into()), ..Config::default() };
        assert_eq!(log_dir(&cfg, path), Path::new("/var/log/ndw"));
    }
}
