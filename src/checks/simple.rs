//! TCP port, MAC address, local-connection ("busy") and print-queue checks.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::net;
use super::{CheckResult, DeviceCheck, FailureCode};
use crate::config::format_mac;
use crate::i18n::Msg;
use crate::msg;
use crate::platform::Platform;

/// "Port blocked" explanation, plus a hint for raw (port 9100) printers.
pub(crate) fn port_blocked(host: &str, port: u16, detail: Msg, raw_print_hint: bool) -> Msg {
    let main = msg!("msg.portBlocked", host = host, port = port, detail = detail);
    if raw_print_hint { msg!("joined", a = main, b = msg!("msg.portBlocked.rawHint", port = port)) } else { main }
}

/// Every port must accept a TCP connection.
pub struct TcpPortCheck {
    pub host: String,
    pub ports: Vec<u16>,
    pub timeout: Duration,
}

#[async_trait]
impl DeviceCheck for TcpPortCheck {
    fn name(&self) -> String {
        format!("tcp:{}", self.ports.iter().map(u16::to_string).collect::<Vec<_>>().join(","))
    }

    async fn run(&self) -> CheckResult {
        for &port in &self.ports {
            if let Err(detail) = net::connect(&self.host, port, self.timeout).await {
                return CheckResult::fail(FailureCode::PortBlocked, port_blocked(&self.host, port, detail, false));
            }
        }
        CheckResult::ok()
    }
}

/// Compares the MAC address answering for the device's IPv4 address with the
/// expected one. Passes when the MAC cannot be determined (other subnet).
pub struct MacCheck {
    pub host: String,
    pub expected: [u8; 6],
    pub platform: Arc<dyn Platform>,
}

#[async_trait]
impl DeviceCheck for MacCheck {
    fn name(&self) -> String {
        "mac".into()
    }

    async fn run(&self) -> CheckResult {
        let Some(IpAddr::V4(ip)) = net::resolve(&self.host).await.into_iter().find(IpAddr::is_ipv4) else {
            return CheckResult::ok();
        };
        let platform = self.platform.clone();
        let seen = tokio::task::spawn_blocking(move || platform.mac_address(ip)).await.ok().flatten();
        match seen {
            Some(mac) if mac != self.expected && mac != [0; 6] => CheckResult::fail(
                FailureCode::IpConflict,
                msg!("msg.ipConflict", ip = ip.to_string(), seen = format_mac(&mac), expected = format_mac(&self.expected)),
            ),
            _ => CheckResult::ok(),
        }
    }
}

/// Reports `Busy` while this computer (normally its print spooler) has the
/// device's raw print port open, so the probe does not compete with it.
pub struct LocalConnectionBusyCheck {
    pub host: String,
    pub port: u16,
    pub platform: Arc<dyn Platform>,
}

#[async_trait]
impl DeviceCheck for LocalConnectionBusyCheck {
    fn name(&self) -> String {
        format!("busy:{}", self.port)
    }

    async fn run(&self) -> CheckResult {
        let addrs = net::resolve(&self.host).await;
        if addrs.is_empty() {
            return CheckResult::ok();
        }
        let platform = self.platform.clone();
        let port = self.port;
        let busy = tokio::task::spawn_blocking(move || platform.has_established_connection(&addrs, port)).await.unwrap_or(false);
        if busy { CheckResult::Busy } else { CheckResult::ok() }
    }
}

/// Fails when the Windows print queue has a job in an error state.
pub struct PrintQueueCheck {
    pub queue: String,
    pub platform: Arc<dyn Platform>,
}

#[async_trait]
impl DeviceCheck for PrintQueueCheck {
    fn name(&self) -> String {
        "queue".into()
    }

    async fn run(&self) -> CheckResult {
        let platform = self.platform.clone();
        let queue = self.queue.clone();
        match tokio::task::spawn_blocking(move || platform.print_queue_problem(&queue)).await.ok().flatten() {
            None => CheckResult::ok(),
            Some(p) => CheckResult::fail(
                FailureCode::QueueError,
                msg!("msg.queueError", queue = &self.queue, job = p.job_id, document = p.document, status = p.status),
            ),
        }
    }
}
