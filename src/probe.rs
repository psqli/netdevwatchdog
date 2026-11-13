//! A device's ordered checks. The first check that does not pass decides the
//! result, so cheap and basic checks run first and the diagnosis is the most
//! specific one that applies.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;

use crate::checks::{
    CheckResult, DeviceCheck, EscPosStatusCheck, FailureCode, HttpCheck, IppCheck, LocalConnectionBusyCheck, MacCheck, PingCheck,
    PrintQueueCheck, TcpPortCheck,
};
use crate::config::{Device, DeviceKind, Ipp, Timeouts, parse_mac};
use crate::msg;
use crate::platform::Platform;

pub struct DeviceProbe {
    pub checks: Vec<Box<dyn DeviceCheck>>,
}

impl DeviceProbe {
    pub fn new(checks: Vec<Box<dyn DeviceCheck>>) -> Self {
        DeviceProbe { checks }
    }

    pub fn names(&self) -> Vec<String> {
        self.checks.iter().map(|c| c.name()).collect()
    }

    pub async fn run(&self) -> CheckResult {
        let mut notes = Vec::new();
        for check in &self.checks {
            // A panicking check must not take the whole watchdog down.
            let result = match std::panic::AssertUnwindSafe(check.run()).catch_unwind().await {
                Ok(r) => r,
                Err(panic) => {
                    let what = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "panic".into());
                    CheckResult::fail(FailureCode::CheckFailed, msg!("msg.checkFailed", detail = format!("{}: {what}", check.name())))
                }
            };
            match result {
                CheckResult::Ok { notes: n } => notes.extend(n),
                other => return other.with_notes_before(notes),
            }
        }
        CheckResult::Ok { notes }
    }

    /// Builds the pipeline for a configured device (the configuration is already validated).
    pub fn for_device(device: &Device, timeouts: &Timeouts, platform: Arc<dyn Platform>) -> DeviceProbe {
        let kind = DeviceKind::parse(&device.kind).unwrap_or(DeviceKind::Generic);
        let host = device.host.trim().to_owned();
        let ms = Duration::from_millis;
        let (connect, read) = (ms(timeouts.connect_ms), ms(timeouts.read_ms));
        let port = device.port.or(kind.default_port());
        let mut checks: Vec<Box<dyn DeviceCheck>> = Vec::new();

        if let (DeviceKind::EscPosPrinter, Some(port)) = (kind, port) {
            checks.push(Box::new(LocalConnectionBusyCheck { host: host.clone(), port, platform: platform.clone() }));
        }
        if device.ping {
            checks.push(Box::new(PingCheck { host: host.clone(), timeout: ms(timeouts.ping_ms), platform: platform.clone() }));
        }
        if let Some(mac) = device.mac.as_deref().and_then(parse_mac) {
            checks.push(Box::new(MacCheck { host: host.clone(), expected: mac, platform: platform.clone() }));
        }

        let default_ipp = Ipp::default();
        let ipp = device.ipp.as_ref().unwrap_or(&default_ipp);
        match (kind, port) {
            (DeviceKind::EscPosPrinter, Some(port)) => {
                checks.push(Box::new(EscPosStatusCheck { host: host.clone(), port, connect_timeout: connect, read_timeout: read }))
            }
            (DeviceKind::Printer, Some(port)) if ipp.enabled => {
                checks.push(Box::new(IppCheck::new(&host, port, &ipp.path, ipp.tls, ms(timeouts.http_ms))))
            }
            (DeviceKind::Printer, _) => {
                checks.push(Box::new(TcpPortCheck { host: host.clone(), ports: vec![device.port.unwrap_or(9100)], timeout: connect }))
            }
            (DeviceKind::Generic, Some(port)) => {
                checks.push(Box::new(TcpPortCheck { host: host.clone(), ports: vec![port], timeout: connect }))
            }
            _ => {}
        }

        let mut extra: Vec<u16> = Vec::new();
        for p in &device.tcp_ports {
            if Some(*p) != port && !extra.contains(p) {
                extra.push(*p);
            }
        }
        if !extra.is_empty() {
            checks.push(Box::new(TcpPortCheck { host: host.clone(), ports: extra, timeout: connect }));
        }

        if let Some(http) = &device.http
            && let Ok(url) = reqwest::Url::parse(&http.url)
        {
            checks.push(Box::new(HttpCheck::new(
                url,
                http.expect_status,
                http.expect_body_contains.clone(),
                http.ignore_certificate_errors,
                ms(timeouts.http_ms),
            )));
        }

        if let Some(queue) = device.windows_queue.as_deref().filter(|q| !q.trim().is_empty())
            && platform.supports_print_queues()
        {
            checks.push(Box::new(PrintQueueCheck { queue: queue.to_owned(), platform: platform.clone() }));
        }
        DeviceProbe::new(checks)
    }
}
