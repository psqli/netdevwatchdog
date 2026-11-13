//! Socket helpers that turn network errors into localizable details.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::i18n::Msg;
use crate::msg;

/// Resolves a host (IPv4 first). Empty on failure.
pub async fn resolve(host: &str) -> Vec<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return vec![ip];
    }
    match tokio::net::lookup_host((host, 0)).await {
        Ok(addrs) => {
            let mut ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
            ips.sort_by_key(|ip| !ip.is_ipv4());
            ips.dedup();
            ips
        }
        Err(_) => Vec::new(),
    }
}

/// Opens a TCP connection with a timeout, or explains why it failed.
pub async fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, Msg> {
    let addrs = resolve(host).await;
    if addrs.is_empty() {
        return Err(Msg::key("detail.hostNotFound"));
    }
    let attempt = async {
        let mut last = None;
        for ip in addrs {
            match TcpStream::connect(SocketAddr::new(ip, port)).await {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        Err(last.expect("at least one address"))
    };
    match tokio::time::timeout(timeout, attempt).await {
        Ok(Ok(stream)) => {
            let _ = stream.set_nodelay(true);
            Ok(stream)
        }
        Ok(Err(e)) => Err(describe(&e)),
        Err(_) => Err(Msg::key("detail.timeout")),
    }
}

/// Maps an I/O error to a localizable detail.
pub fn describe(e: &std::io::Error) -> Msg {
    match e.kind() {
        ErrorKind::ConnectionRefused => Msg::key("detail.refused"),
        ErrorKind::TimedOut => Msg::key("detail.timeout"),
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe => Msg::key("detail.reset"),
        ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable | ErrorKind::NetworkDown => Msg::key("detail.unreachable"),
        _ => match e.raw_os_error() {
            Some(code) => msg!("detail.socketError", code = code),
            None => msg!("detail.socketError", code = format!("{:?}", e.kind())),
        },
    }
}

/// Reads exactly one byte, or `None` on timeout or closed connection.
pub async fn read_byte(stream: &mut TcpStream, timeout: Duration) -> Option<u8> {
    let mut b = [0u8; 1];
    match tokio::time::timeout(timeout, stream.read(&mut b)).await {
        Ok(Ok(1)) => Some(b[0]),
        _ => None,
    }
}

pub async fn write_all(stream: &mut TcpStream, data: &[u8]) -> bool {
    stream.write_all(data).await.is_ok()
}

/// Discards bytes the device sent on its own (e.g. automatic status packets).
pub async fn drain(stream: &mut TcpStream, settle: Duration) {
    tokio::time::sleep(settle).await;
    let mut buf = [0u8; 256];
    loop {
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(_) => break, // WouldBlock: nothing more buffered
        }
    }
}
