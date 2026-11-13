//! ICMP echo. Windows uses IcmpSendEcho (no privileges needed); elsewhere an
//! ICMP socket via surge-ping, falling back to the system `ping` program when
//! the process may not open ICMP sockets.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::net;
use super::{CheckResult, DeviceCheck, FailureCode};
use crate::msg;
use crate::platform::Platform;

pub struct PingCheck {
    pub host: String,
    pub timeout: Duration,
    pub platform: Arc<dyn Platform>,
}

#[async_trait]
impl DeviceCheck for PingCheck {
    fn name(&self) -> String {
        "ping".into()
    }

    async fn run(&self) -> CheckResult {
        let reachable = match net::resolve(&self.host).await.first() {
            None => false,
            Some(&ip) => ping(ip, self.timeout, self.platform.clone()).await,
        };
        if reachable { CheckResult::ok() } else { CheckResult::fail(FailureCode::Unreachable, msg!("msg.unreachable", host = &self.host)) }
    }
}

async fn ping(ip: IpAddr, timeout: Duration, platform: Arc<dyn Platform>) -> bool {
    let native = tokio::task::spawn_blocking(move || platform.ping(ip, timeout)).await.ok().flatten();
    if let Some(result) = native {
        return result;
    }
    portable_ping(ip, timeout).await
}

#[cfg(unix)]
async fn portable_ping(ip: IpAddr, timeout: Duration) -> bool {
    use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence};

    let kind = if ip.is_ipv4() { ICMP::V4 } else { ICMP::V6 };
    match Client::new(&Config::builder().kind(kind).build()) {
        Ok(client) => {
            let mut pinger = client.pinger(ip, PingIdentifier(std::process::id() as u16)).await;
            pinger.timeout(timeout);
            pinger.ping(PingSequence(0), b"netdevwatchdog").await.is_ok()
        }
        // No permission for ICMP sockets: the system ping is setuid or has CAP_NET_RAW.
        Err(_) => system_ping(ip, timeout).await,
    }
}

#[cfg(not(unix))]
async fn portable_ping(ip: IpAddr, timeout: Duration) -> bool {
    system_ping(ip, timeout).await
}

async fn system_ping(ip: IpAddr, timeout: Duration) -> bool {
    let secs = timeout.as_secs().max(1).to_string();
    let mut cmd = tokio::process::Command::new("ping");
    if cfg!(target_os = "macos") {
        cmd.args(["-c", "1", "-t", &secs]);
    } else {
        cmd.args(["-c", "1", "-W", &secs]);
    }
    cmd.arg(ip.to_string()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    matches!(tokio::time::timeout(timeout + Duration::from_secs(2), cmd.status()).await, Ok(Ok(s)) if s.success())
}
