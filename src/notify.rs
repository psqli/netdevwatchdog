//! Notifications (already localized) and the log.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Local};

use crate::checks::FailureCode;
use crate::config::CommandNotification;
use crate::i18n::header_value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Problem,
    Recovery,
    Test,
}

impl Kind {
    pub fn id(self) -> &'static str {
        match self {
            Kind::Problem => "problem",
            Kind::Recovery => "recovery",
            Kind::Test => "test",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub kind: Kind,
    pub device: String,
    pub host: String,
    pub failure: Option<FailureCode>,
    pub title: String,
    pub body: String,
    pub time: SystemTime,
}

/// Delivers notifications: alert window, desktop notification, phone push, command...
#[async_trait]
pub trait Notifier: Send + Sync {
    fn name(&self) -> &str;

    async fn notify(&self, n: &Notification) -> Result<(), String>;
}

/// Receives log lines.
pub trait LogSink: Send + Sync {
    fn write(&self, message: &str);
}

pub fn timestamp(t: SystemTime) -> String {
    DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M:%S").to_string()
}

pub struct ConsoleLog;

impl LogSink for ConsoleLog {
    fn write(&self, message: &str) {
        println!("{}  {message}", timestamp(SystemTime::now()));
    }
}

/// One UTF-8 file per month in a directory. Errors are ignored: logging must
/// never stop the watchdog.
pub struct FileLog {
    pub directory: PathBuf,
    lock: Mutex<()>,
}

impl FileLog {
    pub fn new(directory: PathBuf) -> Self {
        FileLog { directory, lock: Mutex::new(()) }
    }
}

impl LogSink for FileLog {
    fn write(&self, message: &str) {
        let now = Local::now();
        let file = self.directory.join(format!("watchdog-{}.log", now.format("%Y-%m")));
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let _ = std::fs::create_dir_all(&self.directory);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
            let _ = writeln!(f, "{}  {message}", now.format("%Y-%m-%d %H:%M:%S"));
        }
    }
}

/// Writes to several sinks.
pub struct CompositeLog(pub Vec<Box<dyn LogSink>>);

impl LogSink for CompositeLog {
    fn write(&self, message: &str) {
        for sink in &self.0 {
            sink.write(message);
        }
    }
}

/// Phone push via <https://ntfy.sh>.
pub struct NtfyNotifier {
    client: reqwest::Client,
    url: reqwest::Url,
    token: Option<String>,
}

impl NtfyNotifier {
    pub fn new(url: reqwest::Url, token: Option<String>) -> Self {
        NtfyNotifier { client: crate::checks::http_client(Duration::from_secs(15), false), url, token: token.filter(|t| !t.is_empty()) }
    }
}

#[async_trait]
impl Notifier for NtfyNotifier {
    fn name(&self) -> &str {
        "ntfy"
    }

    async fn notify(&self, n: &Notification) -> Result<(), String> {
        let problem = n.kind != Kind::Recovery;
        let mut request = self
            .client
            .post(self.url.clone())
            .header("Title", header_value(&n.title))
            .header("Priority", if problem { "high" } else { "default" })
            .header("Tags", if problem { "warning" } else { "white_check_mark" })
            .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(n.body.clone());
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        if response.status().is_success() { Ok(()) } else { Err(format!("HTTP {}", response.status())) }
    }
}

/// Runs an external program per notification (e-mail, SMS gateway, MQTT...).
pub struct CommandNotifier(pub CommandNotification);

impl CommandNotifier {
    fn values(n: &Notification) -> Vec<(&'static str, String)> {
        vec![
            ("kind", n.kind.id().to_owned()),
            ("device", n.device.clone()),
            ("host", n.host.clone()),
            ("failure", n.failure.map(|f| f.id().to_owned()).unwrap_or_default()),
            ("title", n.title.clone()),
            ("body", n.body.clone()),
            ("time", DateTime::<Local>::from(n.time).to_rfc3339()),
        ]
    }
}

#[async_trait]
impl Notifier for CommandNotifier {
    fn name(&self) -> &str {
        "command"
    }

    async fn notify(&self, n: &Notification) -> Result<(), String> {
        let values = Self::values(n);
        let mut cmd = tokio::process::Command::new(&self.0.path);
        for arg in &self.0.arguments {
            let mut a = arg.clone();
            for (k, v) in &values {
                a = a.replace(&format!("{{{k}}}"), v);
            }
            cmd.arg(a);
        }
        for (k, v) in &values {
            cmd.env(format!("NDW_{}", k.to_ascii_uppercase()), v);
        }
        cmd.stdin(std::process::Stdio::null()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", self.0.path))?;
        match tokio::time::timeout(Duration::from_secs(self.0.timeout_seconds), child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(Ok(status)) => Err(format!("{} exited with {status}", self.0.path)),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => {
                let _ = child.kill().await;
                Err(format!("{} did not finish within {} s", self.0.path, self.0.timeout_seconds))
            }
        }
    }
}
