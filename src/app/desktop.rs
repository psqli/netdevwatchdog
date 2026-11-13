//! The watchdog as a desktop application: notification-area icon with a status
//! menu, alert windows, desktop notifications, and live reload. Slint runs on
//! the main thread; the checks run on a Tokio runtime in the background.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use netdevwatchdog::config::Config;
use netdevwatchdog::gui::tray::{Action, Tray};
use netdevwatchdog::gui::{AlertStyle, AlertWindowNotifier, DesktopNotifier, open_alerts};
use netdevwatchdog::i18n::Localizer;
use netdevwatchdog::monitor::Snapshot;
use netdevwatchdog::notify::{Kind, Notification, Notifier};
use tokio_util::sync::CancellationToken;

use super::cli::Cli;
use super::commands::{self, PROBLEMS, SUCCESS, early_localizer};
use super::runtime::{Hooks, LoadError, Runtime, supervise};
use super::{startup, system};

/// Whether a desktop session is available (always on Windows). Selects the
/// Slint backend, so call it once, before any other Slint function.
pub fn available() -> bool {
    if !cfg!(windows) && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    slint::BackendSelector::new().select().is_ok()
}

fn notifiers(config: &Config, l: &Localizer) -> Vec<Arc<dyn Notifier>> {
    let n = &config.notifications;
    let mut list: Vec<Arc<dyn Notifier>> = Vec::new();
    if n.alert_window {
        list.push(Arc::new(AlertWindowNotifier::new(AlertStyle::new(l, n.alert_font_size))));
    }
    if n.desktop {
        list.push(Arc::new(DesktopNotifier { app_name: l.get("app.name").to_owned() }));
    }
    list
}

thread_local! {
    static TRAY: std::cell::RefCell<Option<Tray>> = const { std::cell::RefCell::new(None) };
}

fn show_status(l: Localizer, snapshots: Vec<Snapshot>) {
    let _ = slint::invoke_from_event_loop(move || {
        TRAY.with(|t| {
            if let Some(tray) = t.borrow().as_ref() {
                tray.update(&l, &snapshots);
            }
        })
    });
}

/// Runs until Exit in the menu, logoff, Ctrl+C or `--uninstall`.
pub fn run(cli: &Cli, path: &Path) -> Result<u8, LoadError> {
    let Ok(Some(_instance)) = startup::claim_instance() else {
        let text = early_localizer(cli).get("app.alreadyRunning").to_owned();
        eprintln!("{text}");
        system::info_dialog(&text);
        return Ok(PROBLEMS);
    };
    // Windows: no console. Elsewhere the log also goes to the terminal.
    let console = !cfg!(windows);
    let runtime = Arc::new(Runtime::load(path, cli.language(), console, &notifiers)?);

    let tokio = commands::tokio_runtime();
    let stop = CancellationToken::new();
    let current = Arc::new(Mutex::new(runtime.clone()));

    let handle = tokio.handle().clone();
    let (menu_current, menu_stop, config_path) = (current.clone(), stop.clone(), path.to_owned());
    match Tray::new(move |action| match action {
        Action::CheckNow => {
            let rt = menu_current.lock().unwrap().clone();
            handle.spawn(async move { rt.service.check_all().await });
        }
        Action::EditConfig => system::open(&config_path, true),
        Action::OpenLog => {
            let dir = menu_current.lock().unwrap().log_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            system::open(&dir, false);
        }
        Action::Exit => menu_stop.cancel(),
    }) {
        Ok(tray) => {
            tray.update(runtime.localizer(), &runtime.service.snapshots());
            let _ = tray.show();
            TRAY.with(|t| *t.borrow_mut() = Some(tray));
        }
        // E.g. no StatusNotifier host on a Linux desktop: alerts still work.
        Err(e) => runtime.log.write(&format!("Cannot show the notification-area icon: {e}")),
    }

    let hooks = Hooks {
        notifiers: Box::new(notifiers),
        on_start: Box::new(move |rt| {
            *current.lock().unwrap() = rt.clone();
            show_status(rt.localizer().clone(), rt.service.snapshots());
            let l = rt.localizer().clone();
            rt.service.on_status(move |snapshots| show_status(l.clone(), snapshots));
        }),
        on_reload_error: Box::new(|rt, text| {
            let n = Notification {
                kind: Kind::Problem,
                device: String::new(),
                host: String::new(),
                failure: None,
                title: rt.localizer().get("app.name").to_owned(),
                body: text.to_owned(),
                time: SystemTime::now(),
            };
            let desktop = DesktopNotifier { app_name: n.title.clone() };
            tokio::spawn(async move { desktop.notify(&n).await });
        }),
    };

    let _guard = tokio.enter();
    commands::stop_on_signals(stop.clone());
    let (path, language, supervisor_stop) = (path.to_owned(), cli.language.clone(), stop.clone());
    let supervisor = tokio.spawn(async move {
        supervise(runtime, &path, language.as_deref(), console, &hooks, supervisor_stop).await;
        let _ = slint::quit_event_loop();
    });

    let result = slint::run_event_loop_until_quit();
    stop.cancel();
    let _ = tokio.block_on(tokio::time::timeout(Duration::from_secs(10), supervisor));
    TRAY.with(|t| t.borrow_mut().take());
    match result {
        Ok(()) => Ok(SUCCESS),
        Err(e) => {
            eprintln!("{e}");
            Ok(PROBLEMS)
        }
    }
}

/// Sends a test alert, then exits once its window has been closed.
pub fn test_alert(cli: &Cli, path: &Path) -> Result<u8, LoadError> {
    let runtime = Runtime::load(path, cli.language(), true, &notifiers)?;
    let window = runtime.service.config.notifications.alert_window;
    let tokio = commands::tokio_runtime();
    let sent = Arc::new(AtomicBool::new(false));
    let (service, done) = (runtime.service.clone(), sent.clone());
    tokio.spawn(async move {
        service.send_test_alert().await;
        done.store(true, Ordering::SeqCst);
    });
    // With only a desktop notification, stay long enough for it to be delivered.
    let (started, minimum) = (Instant::now(), Duration::from_secs(if window { 1 } else { 5 }));
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(250), move || {
        if sent.load(Ordering::SeqCst) && open_alerts() == 0 && started.elapsed() > minimum {
            let _ = slint::quit_event_loop();
        }
    });
    slint::run_event_loop_until_quit().map(|_| SUCCESS).or_else(|e| {
        eprintln!("{e}");
        Ok(PROBLEMS)
    })
}
