//! Start at logon, single instance, and stopping the running instance.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use super::paths;

#[cfg(not(windows))]
const UNIT_NAME: &str = "network-device-watchdog.service";

/// Held while this process is the running watchdog; the OS releases the lock
/// when the process exits, even if it crashes.
pub struct InstanceLock {
    _file: File,
}

/// Returns `None` when another instance is already running.
pub fn claim_instance() -> io::Result<Option<InstanceLock>> {
    let dir = paths::user_dir();
    std::fs::create_dir_all(&dir)?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("instance.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// `--uninstall` creates this file; the running instance sees it and exits.
pub fn stop_request_path() -> PathBuf {
    paths::user_dir().join("stop-request")
}

pub fn stop_requested() -> bool {
    stop_request_path().exists()
}

pub fn clear_stop_request() {
    let _ = std::fs::remove_file(stop_request_path());
}

fn request_stop() {
    let _ = std::fs::create_dir_all(paths::user_dir());
    let _ = std::fs::write(stop_request_path(), b"");
}

fn quote(p: &Path) -> String {
    format!("\"{}\"", p.display())
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "NetworkDeviceWatchdog";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn install(config_path: &Path) -> io::Result<String> {
        let exe = paths::executable();
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?;
        key.set_value(RUN_VALUE, &format!("{} --config {}", quote(&exe), quote(config_path)))?;
        // Start it now as well, so there is no need to log off.
        std::process::Command::new(&exe).arg("--config").arg(config_path).creation_flags(CREATE_NO_WINDOW).spawn()?;
        Ok(format!(r"HKEY_CURRENT_USER\{RUN_KEY}\{RUN_VALUE}"))
    }

    pub fn uninstall() -> io::Result<()> {
        if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, winreg::enums::KEY_SET_VALUE) {
            match key.delete_value(RUN_VALUE) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    fn unit_path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| ".".into());
        base.join("systemd").join("user").join(UNIT_NAME)
    }

    pub fn unit_file(exe: &Path, config_path: &Path) -> String {
        // on-failure: a clean exit (e.g. after --uninstall) is not restarted.
        format!(
            "[Unit]\nDescription=Network Device Watchdog\nAfter=network-online.target\n\n\
             [Service]\nExecStart={} --config {}\nRestart=on-failure\nRestartSec=10\n\n\
             [Install]\nWantedBy=default.target\n",
            quote(exe),
            quote(config_path)
        )
    }

    pub fn install(config_path: &Path) -> io::Result<String> {
        let path = unit_path();
        std::fs::create_dir_all(path.parent().expect("unit folder"))?;
        std::fs::write(&path, unit_file(&paths::executable(), config_path))?;
        Ok(path.display().to_string())
    }

    pub fn uninstall() -> io::Result<()> {
        let path = unit_path();
        if path.exists() {
            // Best effort: systemd may not be running (containers, WSL).
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "disable", UNIT_NAME])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

/// Registers the watchdog to start at logon. Returns the file or key written.
pub fn install(config_path: &Path) -> io::Result<String> {
    platform::install(config_path)
}

/// Removes the registration and asks a running instance to exit.
pub fn uninstall() -> io::Result<()> {
    platform::uninstall()?;
    request_stop();
    Ok(())
}

#[cfg(all(test, not(windows)))]
mod tests {
    #[test]
    fn unit_file_quotes_paths() {
        let unit = super::platform::unit_file("/opt/ndw/netdevwatchdog".as_ref(), "/home/a b/watchdog.json".as_ref());
        assert!(unit.contains("ExecStart=\"/opt/ndw/netdevwatchdog\" --config \"/home/a b/watchdog.json\"\n"), "{unit}");
        assert!(unit.contains("Restart=on-failure"));
    }
}
