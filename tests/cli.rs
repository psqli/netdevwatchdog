//! The executable end to end, in a private home folder.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::{FakePrinter, PrinterMode, closed_port};

struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(name: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("ndw-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Home { dir }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_netdevwatchdog"));
        c.args(args)
            .env("HOME", &self.dir)
            .env("USERPROFILE", &self.dir)
            .env("XDG_CONFIG_HOME", self.dir.join(".config"))
            .env("LOCALAPPDATA", self.dir.join("AppData"))
            .env("LANG", "en_US.UTF-8")
            // Run headless even when the tests run in a desktop session.
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn user_config(&self) -> PathBuf {
        if cfg!(windows) {
            self.dir.join("AppData").join("NetworkDeviceWatchdog").join("watchdog.json")
        } else {
            self.dir.join(".config").join("network-device-watchdog").join("watchdog.json")
        }
    }

    fn write_config(&self, devices: &str) -> PathBuf {
        let path = self.dir.join("cfg").join("watchdog.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = format!(
            r#"{{
  "language": "en",
  "intervalSeconds": 5,
  "notifications": {{ "desktop": false, "alertWindow": false }},
  "timeouts": {{ "connectMs": 5000, "readMs": 500 }},
  "devices": [{devices}],
}}"#
        );
        std::fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn device(name: &str, port: u16) -> String {
    format!(r#"{{ "name": "{name}", "type": "generic", "host": "127.0.0.1", "port": {port}, "ping": false }}"#)
}

/// Waits until a log file in `dir` contains `text`.
fn wait_for_log(dir: &Path, text: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let log: String =
            std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(|e| std::fs::read_to_string(e.path()).ok()).collect();
        if log.contains(text) {
            return log;
        }
        assert!(Instant::now() < deadline, "log never contained {text:?}:\n{log}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_exit(child: &mut Child, within: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + within;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the watchdog did not exit");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn help_version_and_usage_errors() {
    let home = Home::new("help");
    let help = home.run(&["--help"]);
    assert!(help.status.success());
    assert!(stdout(&help).contains("--test-alert"), "{}", stdout(&help));
    assert!(stdout(&home.run(&["--version"])).contains(env!("CARGO_PKG_VERSION")));
    let bad = home.run(&["--once", "--validate"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("cannot be used with"), "{}", stderr(&bad));
    assert_eq!(home.run(&["--frobnicate"]).status.code(), Some(2));
}

#[test]
fn init_then_validate_in_the_user_folder() {
    let home = Home::new("init");
    let missing = home.run(&["--validate"]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stdout(&missing).contains("Configuration file not found"), "{}", stdout(&missing));

    let init = home.run(&["--init"]);
    assert!(init.status.success(), "{}", stderr(&init));
    assert!(home.user_config().is_file());
    assert_eq!(home.run(&["--init"]).status.code(), Some(2), "does not overwrite");
    assert!(home.run(&["--init", "--force"]).status.success());

    let valid = home.run(&["--validate"]);
    assert!(valid.status.success());
    assert_eq!(stdout(&valid).trim(), "Configuration OK: 4 device(s).");
    let pt = home.run(&["--validate", "--language", "pt"]);
    assert_eq!(stdout(&pt).trim(), "Configuração OK: 4 dispositivo(s).");
}

#[test]
fn invalid_configuration_lists_every_error() {
    let home = Home::new("invalid");
    let path = home.write_config(r#"{ "name": "", "type": "toaster", "host": "bad host" }"#);
    let out = home.run(&["--validate", "--config", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("has errors and was not loaded"), "{text}");
    assert!(text.lines().filter(|l| l.starts_with("  - ")).count() >= 3, "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn once_prints_a_table_and_fails_when_a_device_has_a_problem() {
    let home = Home::new("once");
    let printer = FakePrinter::start(PrinterMode::Ready).await;
    let path = home.write_config(&format!("{}, {}", device("Bar", printer.port), device("Kitchen", closed_port())));
    let cmd = home.command(&["--once", "--config", path.to_str().unwrap()]);
    let out = tokio::task::spawn_blocking(move || { cmd }.output().unwrap()).await.unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].starts_with("Bar      OK"), "{text}");
    assert!(lines[1].starts_with("Kitchen  Port blocked"), "{text}");

    let path = home.write_config(&device("Bar", printer.port));
    let cmd = home.command(&["--once", "--config", path.to_str().unwrap()]);
    let out = tokio::task::spawn_blocking(move || { cmd }.output().unwrap()).await.unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
}

#[cfg(unix)]
#[test]
fn install_writes_a_systemd_user_unit_and_uninstall_removes_it() {
    let home = Home::new("install");
    let path = home.write_config(&device("Bar", 9100));
    let out = home.run(&["--install", "--config", path.to_str().unwrap()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let unit = home.dir.join(".config/systemd/user/network-device-watchdog.service");
    let text = std::fs::read_to_string(&unit).unwrap();
    assert!(text.contains(&format!("--config \"{}\"", path.display())), "{text}");
    assert!(home.run(&["--uninstall"]).status.success());
    assert!(!unit.exists());
}

#[test]
fn runs_until_uninstall_reloads_on_save_and_allows_one_instance() {
    let home = Home::new("run");
    let path = home.write_config(&device("Bar", closed_port()));
    let logs = path.parent().unwrap().join("logs");
    let mut child = home.command(&["--config", path.to_str().unwrap()]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    wait_for_log(&logs, "Watchdog started: 1 device(s)");

    let second = home.run(&["--config", path.to_str().unwrap()]);
    assert_eq!(second.status.code(), Some(1));
    assert!(stderr(&second).contains("already running"), "{}", stderr(&second));

    // A broken save keeps the previous configuration; a good one is loaded.
    std::fs::write(&path, "{ \"devices\": [ { \"name\": \"x\" } ] }").unwrap();
    wait_for_log(&logs, "has errors and was not loaded");
    home.write_config(&format!("{}, {}", device("Bar", closed_port()), device("Kitchen", closed_port())));
    wait_for_log(&logs, "Configuration reloaded");
    wait_for_log(&logs, "Watchdog started: 2 device(s)");

    assert!(home.run(&["--uninstall"]).status.success());
    let status = wait_exit(&mut child, Duration::from_secs(15));
    assert!(status.success(), "{status}");
    wait_for_log(&logs, "Watchdog stopped.");
}
