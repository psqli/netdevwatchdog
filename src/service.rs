//! Checks every device each interval, logs what happens and sends notifications.
//! Devices are checked concurrently; a slow or dead device does not delay the others.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use futures::future::join_all;
use tokio_util::sync::CancellationToken;

use crate::checks::FailureCode;
use crate::config::Config;
use crate::i18n::{Localizer, Msg};
use crate::monitor::{DeviceMonitor, Event, Snapshot, Thresholds};
use crate::msg;
use crate::notify::{Kind, LogSink, Notification, Notifier};
use crate::platform::Platform;
use crate::probe::DeviceProbe;

type StatusListener = Box<dyn Fn(Vec<Snapshot>) + Send + Sync>;

pub struct Service {
    pub config: Config,
    pub localizer: Localizer,
    monitors: Vec<tokio::sync::Mutex<DeviceMonitor>>,
    snapshots: Mutex<Vec<Snapshot>>,
    notifiers: Vec<Arc<dyn Notifier>>,
    log: Arc<dyn LogSink>,
    listeners: Mutex<Vec<StatusListener>>,
    round: tokio::sync::Mutex<()>,
}

impl Service {
    pub fn new(
        config: Config,
        localizer: Localizer,
        monitors: Vec<DeviceMonitor>,
        notifiers: Vec<Arc<dyn Notifier>>,
        log: Arc<dyn LogSink>,
    ) -> Self {
        let snapshots = monitors.iter().map(|m| m.snapshot().clone()).collect();
        Service {
            config,
            localizer,
            monitors: monitors.into_iter().map(tokio::sync::Mutex::new).collect(),
            snapshots: Mutex::new(snapshots),
            notifiers,
            log,
            listeners: Mutex::new(Vec::new()),
            round: tokio::sync::Mutex::new(()),
        }
    }

    /// Builds monitors for every enabled device.
    pub fn from_config(
        config: Config,
        localizer: Localizer,
        platform: Arc<dyn Platform>,
        notifiers: Vec<Arc<dyn Notifier>>,
        log: Arc<dyn LogSink>,
    ) -> Self {
        let thresholds = Thresholds { fail: config.fail_threshold, busy: config.busy_threshold };
        let monitors = config
            .devices
            .iter()
            .filter(|d| d.enabled)
            .map(|d| {
                let probe = DeviceProbe::for_device(d, &config.timeouts, platform.clone());
                DeviceMonitor::new(d.name.trim(), d.host.trim(), probe, thresholds)
            })
            .collect();
        Service::new(config, localizer, monitors, notifiers, log)
    }

    /// Called after every round with the state of all devices.
    pub fn on_status(&self, listener: impl Fn(Vec<Snapshot>) + Send + Sync + 'static) {
        self.listeners.lock().unwrap().push(Box::new(listener));
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        self.snapshots.lock().unwrap().clone()
    }

    fn log(&self, message: &str) {
        self.log.write(message);
    }

    fn text(&self, m: Msg) -> String {
        self.localizer.format(&m)
    }

    /// Checks until cancelled.
    pub async fn run(&self, stop: CancellationToken) {
        self.log(&self.text(msg!(
            "app.started",
            count = self.monitors.len(),
            interval = self.config.interval_seconds,
            threshold = self.config.fail_threshold
        )));
        let mut interval = tokio::time::interval(Duration::from_secs(self.config.interval_seconds));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                _ = interval.tick() => {
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        _ = self.check_all() => {}
                    }
                }
            }
        }
        self.log(self.localizer.get("app.stopped"));
    }

    /// One round of checks (also "Check now" and `--once`).
    pub async fn check_all(&self) -> Vec<Snapshot> {
        let _round = self.round.lock().await;
        let now = SystemTime::now();
        let results = join_all(self.monitors.iter().map(|m| async move {
            let mut m = m.lock().await;
            let events = m.check(now).await;
            (m.snapshot().clone(), events)
        }))
        .await;

        let snapshots: Vec<Snapshot> = results.iter().map(|(s, _)| s.clone()).collect();
        *self.snapshots.lock().unwrap() = snapshots.clone();
        for (snapshot, events) in results {
            for event in events {
                self.handle(&snapshot, event, now).await;
            }
        }
        for listener in self.listeners.lock().unwrap().iter() {
            listener(snapshots.clone());
        }
        snapshots
    }

    /// Sends a test notification through every channel.
    pub async fn send_test_alert(&self) {
        let device = self.localizer.get("alert.device.test").to_owned();
        let failure = FailureCode::PortBlocked;
        let n = Notification {
            kind: Kind::Test,
            title: self.text(msg!("alert.title.test", device = &device, failure = failure.title())),
            body: self.localizer.get("alert.body.test").to_owned(),
            device,
            host: String::new(),
            failure: Some(failure),
            time: SystemTime::now(),
        };
        self.notify(&n).await;
    }

    async fn handle(&self, s: &Snapshot, event: Event, now: SystemTime) {
        let (device, host) = (&s.device, &s.host);
        match event {
            Event::FailureObserved { failure, message } => {
                self.log(&self.text(msg!("log.problem", device = device, failure = failure.title(), message = message)));
            }
            Event::TransientCleared { failure, checks } => {
                self.log(&self.text(msg!("log.transientCleared", device = device, failure = failure.title(), count = checks)));
            }
            Event::AlertRaised { failure, message } => {
                let n = Notification {
                    kind: Kind::Problem,
                    device: device.clone(),
                    host: host.clone(),
                    failure: Some(failure),
                    title: self.text(msg!("alert.title.problem", device = device, failure = failure.title())),
                    body: self.text(message),
                    time: now,
                };
                self.notify(&n).await;
            }
            Event::Recovered { previous } => {
                let body = self.text(msg!("alert.body.recovered", host = host, failure = previous.title()));
                if self.config.notifications.notify_recovery {
                    let n = Notification {
                        kind: Kind::Recovery,
                        device: device.clone(),
                        host: host.clone(),
                        failure: Some(previous),
                        title: self.text(msg!("alert.title.recovered", device = device)),
                        body,
                        time: now,
                    };
                    self.notify(&n).await;
                } else {
                    self.log(&body);
                }
            }
        }
    }

    /// Channels run concurrently and independently: one failing channel (e.g.
    /// no internet for ntfy) must not keep the alert window from showing.
    async fn notify(&self, n: &Notification) {
        self.log(&self.text(msg!("log.alert", title = &n.title, body = &n.body)));
        let results =
            join_all(self.notifiers.iter().map(|notifier| async move { (notifier.name().to_owned(), notifier.notify(n).await) })).await;
        for (name, result) in results {
            if let Err(error) = result {
                self.log(&self.text(msg!("log.notifierFailed", notifier = name, error = error)));
            }
        }
    }
}
