//! Fake devices on 127.0.0.1 for integration tests.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrinterMode {
    Ready,
    PaperLow,
    CoverOpen,
    PaperOut,
    CutterError,
    Silent,
    Garbage,
    AutoStatusOnConnect,
}

/// A TCP server that answers DLE EOT like an ESC/POS printer.
pub struct FakePrinter {
    pub port: u16,
    pub connections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakePrinter {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn status(mode: PrinterMode, n: u8) -> u8 {
    const BASE: u8 = 0x12;
    use PrinterMode::*;
    match (mode, n) {
        (PaperLow, 4) => BASE | 0x0C,
        (CoverOpen, 1) | (PaperOut, 1) | (CutterError, 1) => BASE | 0x04 | 0x08,
        (CoverOpen, 2) => BASE | 0x04,
        (PaperOut, 2) => BASE | 0x20,
        (PaperOut, 4) => BASE | 0x60,
        (CutterError, 2) => BASE | 0x40,
        (CutterError, 3) => BASE | 0x08,
        (_, 1) => BASE | 0x04,
        _ => BASE,
    }
}

impl FakePrinter {
    pub async fn start(mode: PrinterMode) -> FakePrinter {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    if mode == PrinterMode::AutoStatusOnConnect {
                        let _ = s.write_all(&[0x10, 0x00, 0x00, 0x0F]).await; // ASB packet
                    }
                    let mut req = [0u8; 3];
                    while s.read_exact(&mut req).await.is_ok() {
                        let reply = match mode {
                            PrinterMode::Silent => continue,
                            PrinterMode::Garbage => 0xFF,
                            m => status(m, req[2]),
                        };
                        if s.write_all(&[reply]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        FakePrinter { port, connections, task }
    }
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A minimal HTTP server: `respond(method, path, body) -> (status, content type, body)`.
pub struct FakeHttp {
    pub port: u16,
    pub requests: Arc<std::sync::Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Responder = Arc<dyn Fn(&str, &str, &[u8]) -> (u16, &'static str, Vec<u8>) + Send + Sync>;

impl FakeHttp {
    pub async fn start(respond: impl Fn(&str, &str, &[u8]) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static) -> FakeHttp {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = requests.clone();
        let respond: Responder = Arc::new(respond);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { return };
                let respond = respond.clone();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let header_end = loop {
                        let Ok(n) = s.read(&mut chunk).await else { return };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap_or_default().split_whitespace();
                    let (method, path) = (first.next().unwrap_or_default().to_owned(), first.next().unwrap_or_default().to_owned());
                    let headers: Vec<(String, String)> =
                        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).collect();
                    let length: usize = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    while buf.len() < header_end + length {
                        let Ok(n) = s.read(&mut chunk).await else { return };
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body = buf[header_end..].to_vec();
                    let (status, content_type, reply) = respond(&method, &path, &body);
                    log.lock().unwrap().push(Request { method, path, headers, body });
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.len()
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&reply).await;
                });
            }
        });
        FakeHttp { port, requests, task }
    }
}

/// A loopback port that nothing listens on.
pub fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// IPP response with printer-state and printer-state-reasons.
pub fn ipp_response(status: u16, state: i32, reasons: &[&str]) -> Vec<u8> {
    fn attr(out: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
        out.push(tag);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value);
    }
    let mut out = vec![2, 0];
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 1, 0x01]);
    attr(&mut out, 0x47, "attributes-charset", b"utf-8");
    out.push(0x04);
    attr(&mut out, 0x23, "printer-state", &state.to_be_bytes());
    for (i, r) in reasons.iter().enumerate() {
        attr(&mut out, 0x44, if i == 0 { "printer-state-reasons" } else { "" }, r.as_bytes());
    }
    out.push(0x03);
    out
}
