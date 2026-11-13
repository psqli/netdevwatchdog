//! Everything built from one configuration file, and live reload.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use netdevwatchdog::config::{Config, ConfigError};
use netdevwatchdog::i18n::Localizer;
use netdevwatchdog::msg;
use netdevwatchdog::notify::{CommandNotifier, CompositeLog, ConsoleLog, FileLog, LogSink, Notifier, NtfyNotifier};
use netdevwatchdog::platform::SystemPlatform;
use netdevwatchdog::service::Service;
use tokio_util::sync::CancellationToken;

use super::{paths, startup};

#[derive(Debug)]
pub enum LoadError {
    NotFound(PathBuf),
    Invalid(ConfigError),
}

impl LoadError {
    pub fn describe(&self, l: &Localizer) -> String {
        match self {
            LoadError::NotFound(p) => l.format(&msg!("app.configNotFound", path = p.display().to_string())),
            LoadError::Invalid(e) => {
                let mut text = l.format(&msg!("app.configInvalid", path = e.source_name.clone()));
                for error in &e.errors {
                    text.push_str("\n  - ");
                    text.push_str(error);
                }
                text
            }
        }
    }
}

/// Builds the platform's own channels (alert window, desktop notification).
pub type NotifierFactory = dyn Fn(&Config, &Localizer) -> Vec<Arc<dyn Notifier>> + Send + Sync;

pub struct Runtime {
    pub log: Arc<dyn LogSink>,
    #[cfg_attr(not(feature = "gui"), allow(dead_code))]
    pub log_dir: PathBuf,
    pub service: Arc<Service>,
}

impl Runtime {
    pub fn load(config_path: &Path, language: Option<&str>, console: bool, platform: &NotifierFactory) -> Result<Runtime, LoadError> {
        if !config_path.is_file() {
            return Err(LoadError::NotFound(config_path.to_owned()));
        }
        let config = Config::load(config_path).map_err(LoadError::Invalid)?;
        let localizer = Localizer::new(Some(language.unwrap_or(&config.language)), Some(&paths::locales_dir(&config, config_path)));
        let log_dir = paths::log_dir(&config, config_path);
        let file: Box<dyn LogSink> = Box::new(FileLog::new(log_dir.clone()));
        let log: Arc<dyn LogSink> = if console { Arc::new(CompositeLog(vec![file, Box::new(ConsoleLog)])) } else { Arc::from(file) };

        let mut notifiers = platform(&config, &localizer);
        let n = &config.notifications;
        if let Some(url) = n.ntfy_url.as_deref().filter(|u| !u.trim().is_empty())
            && let Ok(url) = url.trim().parse()
        {
            notifiers.push(Arc::new(NtfyNotifier::new(url, n.ntfy_token.clone())));
        }
        if let Some(command) = &n.command {
            notifiers.push(Arc::new(CommandNotifier(command.clone())));
        }
        let service = Service::from_config(config, localizer, Arc::new(SystemPlatform), notifiers, log.clone());
        Ok(Runtime { log, log_dir, service: Arc::new(service) })
    }

    pub fn localizer(&self) -> &Localizer {
        &self.service.localizer
    }
}

/// Calls `on_change` (debounced) whenever `path` is created, written or replaced.
/// Watches the parent folder, because editors often save by renaming a temporary file.
pub struct FileWatch {
    _watcher: notify::RecommendedWatcher,
}

impl FileWatch {
    pub fn new(path: &Path, debounce: Duration, on_change: impl Fn() + Send + 'static) -> notify::Result<FileWatch> {
        use notify::{EventKind, RecursiveMode, Watcher};
        let dir = path.parent().unwrap_or(Path::new(".")).to_owned();
        let name = path.file_name().map(|n| n.to_owned());
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(e) = event
                && matches!(e.kind, EventKind::Create(_) | EventKind::Modify(_))
                && e.paths.iter().any(|p| p.file_name().map(|n| n.to_owned()) == name)
            {
                let _ = tx.send(());
            }
        })?;
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;
        // The thread ends when the watcher (and with it the sender) is dropped.
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                loop {
                    match rx.recv_timeout(debounce) {
                        Ok(()) => continue,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                on_change();
            }
        });
        Ok(FileWatch { _watcher: watcher })
    }
}

type StartHook = dyn Fn(&Arc<Runtime>) + Send + Sync;
type ReloadErrorHook = dyn Fn(&Runtime, &str) + Send + Sync;

/// Hooks for the hosting user interface.
pub struct Hooks {
    pub notifiers: Box<NotifierFactory>,
    /// A runtime was started (at startup and after each reload).
    pub on_start: Box<StartHook>,
    /// The configuration file was saved with errors; monitoring continues with the previous one.
    pub on_reload_error: Box<ReloadErrorHook>,
}

impl Hooks {
    pub fn console() -> Hooks {
        Hooks { notifiers: Box::new(|_, _| Vec::new()), on_start: Box::new(|_| {}), on_reload_error: Box::new(|_, _| {}) }
    }
}

/// Runs the service until `stop` is cancelled or `--uninstall` asks this
/// instance to exit, reloading the configuration whenever the file is saved.
pub async fn supervise(
    mut runtime: Arc<Runtime>,
    config_path: &Path,
    language: Option<&str>,
    console: bool,
    hooks: &Hooks,
    stop: CancellationToken,
) {
    let (tx, mut reload) = tokio::sync::mpsc::unbounded_channel();
    let watch = FileWatch::new(config_path, Duration::from_millis(1500), move || {
        let _ = tx.send(());
    });
    if let Err(e) = &watch {
        runtime.log.write(&format!("Cannot watch {} for changes: {e}", config_path.display()));
    }

    startup::clear_stop_request();
    let stop_file = startup::stop_request_path();
    let requested = stop.clone();
    let _stop_watch = FileWatch::new(&stop_file, Duration::from_millis(200), move || {
        if startup::stop_requested() {
            startup::clear_stop_request();
            requested.cancel();
        }
    });

    loop {
        (hooks.on_start)(&runtime);
        let round = stop.child_token();
        let service = runtime.service.clone();
        let token = round.clone();
        let mut task = tokio::spawn(async move { service.run(token).await });
        loop {
            tokio::select! {
                _ = &mut task => return,
                Some(()) = reload.recv() => {
                    match Runtime::load(config_path, language, console, &*hooks.notifiers) {
                        Ok(next) => {
                            round.cancel();
                            let _ = (&mut task).await;
                            runtime = Arc::new(next);
                            runtime.log.write(&runtime.localizer().format(&msg!("app.configReloaded", path = config_path.display().to_string())));
                            break;
                        }
                        Err(e) => {
                            let text = e.describe(runtime.localizer());
                            runtime.log.write(&text);
                            (hooks.on_reload_error)(&runtime, &text);
                        }
                    }
                }
            }
        }
    }
}
