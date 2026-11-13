//! Checks against fake devices on real loopback sockets.

mod common;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use netdevwatchdog::checks::*;
use netdevwatchdog::config::{Config, Device, HttpCheck as HttpConfig, Ipp, Timeouts};
use netdevwatchdog::i18n::{Localizer, Msg};
use netdevwatchdog::platform::{Platform, PrintQueueProblem, SystemPlatform};
use netdevwatchdog::probe::DeviceProbe;

// Windows retries refused loopback connections for ~2 s, so allow generous timeouts.
const SLOW: Duration = Duration::from_secs(10);
const FAST: Duration = Duration::from_millis(500);

fn en() -> Localizer {
    Localizer::new(Some("en"), None)
}

fn text(r: &CheckResult) -> String {
    en().format(r.message().expect("a failure message"))
}

fn escpos(port: u16, timeout: Duration) -> EscPosStatusCheck {
    EscPosStatusCheck { host: "127.0.0.1".into(), port, connect_timeout: timeout, read_timeout: FAST }
}

#[tokio::test]
async fn ready_printer_is_ok_and_the_connection_is_released() {
    let p = FakePrinter::start(PrinterMode::Ready).await;
    assert_eq!(escpos(p.port, SLOW).run().await, CheckResult::ok());
    assert_eq!(escpos(p.port, SLOW).run().await, CheckResult::ok());
    assert_eq!(p.connections.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn paper_low_is_ok_with_a_note() {
    let p = FakePrinter::start(PrinterMode::PaperLow).await;
    assert_eq!(escpos(p.port, SLOW).run().await, CheckResult::Ok { notes: vec![Msg::key("reason.paperNearEnd")] });
}

#[tokio::test]
async fn printer_errors_are_reported() {
    for (mode, expected) in [
        (PrinterMode::CoverOpen, "The device reports: cover open."),
        (PrinterMode::PaperOut, "The device reports: out of paper."),
        (PrinterMode::CutterError, "The device reports: in error state, cutter error."),
    ] {
        let p = FakePrinter::start(mode).await;
        let r = escpos(p.port, SLOW).run().await;
        assert_eq!(r.failure(), Some(FailureCode::DeviceError), "{mode:?}");
        assert_eq!(text(&r), expected);
    }
}

#[tokio::test]
async fn silent_or_garbage_printer_is_not_responding() {
    let p = FakePrinter::start(PrinterMode::Silent).await;
    let r = escpos(p.port, SLOW).run().await;
    assert_eq!(r.failure(), Some(FailureCode::NoResponse));
    assert!(text(&r).contains("(no reply)"), "{}", text(&r));

    let p = FakePrinter::start(PrinterMode::Garbage).await;
    let r = escpos(p.port, SLOW).run().await;
    assert!(text(&r).contains("unexpected reply 0xFF"), "{}", text(&r));
}

#[tokio::test]
async fn automatic_status_packets_on_connect_are_ignored() {
    let p = FakePrinter::start(PrinterMode::AutoStatusOnConnect).await;
    assert_eq!(escpos(p.port, SLOW).run().await, CheckResult::ok());
}

#[tokio::test]
async fn closed_port_is_port_blocked_with_the_raw_print_hint() {
    let r = escpos(closed_port(), SLOW).run().await;
    assert_eq!(r.failure(), Some(FailureCode::PortBlocked));
    let t = text(&r);
    assert!(t.contains("connection refused") && t.contains("netstat -ano"), "{t}");
}

#[tokio::test]
async fn tcp_port_check() {
    let p = FakePrinter::start(PrinterMode::Ready).await;
    let ok = TcpPortCheck { host: "127.0.0.1".into(), ports: vec![p.port], timeout: SLOW };
    assert_eq!(ok.run().await, CheckResult::ok());
    let closed = closed_port();
    let r = TcpPortCheck { host: "127.0.0.1".into(), ports: vec![p.port, closed], timeout: SLOW }.run().await;
    assert_eq!(r.failure(), Some(FailureCode::PortBlocked));
    assert!(text(&r).contains(&format!("TCP port {closed}")));
}

async fn ipp_printer(reply: Vec<u8>, status: u16) -> FakeHttp {
    FakeHttp::start(move |_, _, _| (status, "application/ipp", reply.clone())).await
}

#[tokio::test]
async fn idle_ipp_printer_is_ok_and_warnings_become_notes() {
    let server = ipp_printer(ipp_response(0, 3, &["toner-low-warning"]), 200).await;
    let r = IppCheck::new("127.0.0.1", server.port, "/ipp/print", false, SLOW).run().await;
    assert_eq!(r, CheckResult::Ok { notes: vec![Msg::key("reason.tonerLow")] });
    let request = server.requests.lock().unwrap()[0].clone();
    assert_eq!((request.method.as_str(), request.path.as_str()), ("POST", "/ipp/print"));
    assert_eq!(request.header("content-type"), Some("application/ipp"));
    let parsed = parse_ipp_response(&request.body).unwrap();
    assert_eq!(parsed.attributes["printer-uri"], [IppValue::Text(format!("ipp://127.0.0.1:{}/ipp/print", server.port))]);
}

#[tokio::test]
async fn ipp_printer_errors() {
    let server = ipp_printer(ipp_response(0, 5, &["media-empty-error", "cover-open-error"]), 200).await;
    let r = IppCheck::new("127.0.0.1", server.port, "/ipp/print", false, SLOW).run().await;
    assert_eq!(r.failure(), Some(FailureCode::DeviceError));
    assert_eq!(text(&r), "The device reports: out of paper, cover open.");

    let server = ipp_printer(ipp_response(0, 5, &["none"]), 200).await;
    let r = IppCheck::new("127.0.0.1", server.port, "/ipp/print", false, SLOW).run().await;
    assert_eq!(text(&r), "The device reports: printer stopped.");
}

#[tokio::test]
async fn ipp_bad_answers_are_no_response() {
    for (reply, status, expected) in [
        (Vec::new(), 404, "HTTP status 404"),
        (ipp_response(0x0406, 3, &[]), 200, "IPP status 0x0406"),
        (vec![1, 2, 3], 200, "invalid response"),
    ] {
        let server = ipp_printer(reply, status).await;
        let r = IppCheck::new("127.0.0.1", server.port, "/ipp/print", false, SLOW).run().await;
        assert_eq!(r.failure(), Some(FailureCode::NoResponse));
        assert!(text(&r).contains(expected), "{}", text(&r));
    }
    let r = IppCheck::new("127.0.0.1", closed_port(), "/ipp/print", false, SLOW).run().await;
    assert_eq!(r.failure(), Some(FailureCode::PortBlocked));
}

async fn health(status: u16, body: &'static str) -> FakeHttp {
    FakeHttp::start(move |_, _, _| (status, "text/plain", body.as_bytes().to_vec())).await
}

fn http(port: u16, expect: Option<u16>, contains: Option<&str>, timeout: Duration) -> HttpCheck {
    HttpCheck::new(format!("http://127.0.0.1:{port}/health").parse().unwrap(), expect, contains.map(Into::into), false, timeout)
}

#[tokio::test]
async fn http_health_checks() {
    let s = health(204, "").await;
    assert_eq!(http(s.port, None, None, SLOW).run().await, CheckResult::ok());

    let s = health(503, "").await;
    let r = http(s.port, None, None, SLOW).run().await;
    assert_eq!(r.failure(), Some(FailureCode::HealthCheckFailed));
    assert_eq!(text(&r), format!("http://127.0.0.1:{}/health answered HTTP 503, expected 2xx.", s.port));

    let s = health(302, "").await;
    assert_eq!(http(s.port, Some(302), None, SLOW).run().await, CheckResult::ok());

    let s = health(200, "{\"status\":\"degraded\"}").await;
    assert_eq!(http(s.port, None, Some("\"ok\""), SLOW).run().await.failure(), Some(FailureCode::HealthCheckFailed));
    let s = health(200, "{\"status\":\"ok\"}").await;
    assert_eq!(http(s.port, None, Some("\"ok\""), SLOW).run().await, CheckResult::ok());
}

#[tokio::test]
async fn http_connection_refused_and_timeout() {
    let r = http(closed_port(), None, None, SLOW).run().await;
    assert_eq!(r.failure(), Some(FailureCode::PortBlocked));
    assert!(text(&r).contains("connection refused"), "{}", text(&r));

    // Accepts the connection and never answers.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _hold = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            held.push(s);
        }
    });
    let r = http(port, None, None, Duration::from_millis(300)).run().await;
    assert_eq!(r.failure(), Some(FailureCode::NoResponse));
}

/// Platform double for MAC, busy and queue checks.
struct FakePlatform {
    mac: Option<[u8; 6]>,
    busy: bool,
    queue: Option<PrintQueueProblem>,
}

impl Platform for FakePlatform {
    fn mac_address(&self, _ip: Ipv4Addr) -> Option<[u8; 6]> {
        self.mac
    }
    fn has_established_connection(&self, _addrs: &[IpAddr], _port: u16) -> bool {
        self.busy
    }
    fn print_queue_problem(&self, _queue: &str) -> Option<PrintQueueProblem> {
        self.queue.clone()
    }
    fn supports_print_queues(&self) -> bool {
        true
    }
}

fn fake(mac: Option<[u8; 6]>, busy: bool, queue: Option<PrintQueueProblem>) -> Arc<dyn Platform> {
    Arc::new(FakePlatform { mac, busy, queue })
}

#[tokio::test]
async fn mac_mismatch_is_an_ip_conflict() {
    let expected = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let r = MacCheck { host: "192.168.10.21".into(), expected, platform: fake(Some([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]), false, None) }
        .run()
        .await;
    assert_eq!(r.failure(), Some(FailureCode::IpConflict));
    assert_eq!(
        text(&r),
        "192.168.10.21 is answered by MAC address AA-BB-CC-DD-EE-FF, but the device has 00-11-22-33-44-55. Another device is using its IP address."
    );
    for mac in [Some(expected), None] {
        let r = MacCheck { host: "192.168.10.21".into(), expected, platform: fake(mac, false, None) }.run().await;
        assert_eq!(r, CheckResult::ok());
    }
}

#[tokio::test]
async fn open_print_connection_from_this_computer_means_busy() {
    let check = |busy| LocalConnectionBusyCheck { host: "192.168.10.21".into(), port: 9100, platform: fake(None, busy, None) };
    assert_eq!(check(true).run().await, CheckResult::Busy);
    assert_eq!(check(false).run().await, CheckResult::ok());
}

#[tokio::test]
async fn print_queue_problem_is_reported() {
    let problem = PrintQueueProblem { job_id: 7, document: "Receipt 42".into(), status: "Error".into() };
    let r = PrintQueueCheck { queue: "ELGIN i9".into(), platform: fake(None, false, Some(problem)) }.run().await;
    assert_eq!(r.failure(), Some(FailureCode::QueueError));
    assert_eq!(text(&r), "The device is OK, but in the Windows print queue \"ELGIN i9\" job 7 (\"Receipt 42\") is in state: Error.");
}

#[tokio::test]
async fn system_connection_table_sees_a_real_connection() {
    let p = FakePrinter::start(PrinterMode::Silent).await;
    let _client = tokio::net::TcpStream::connect(("127.0.0.1", p.port)).await.unwrap();
    let local = [IpAddr::V4(Ipv4Addr::LOCALHOST)];
    // Give the OS table a moment to list the new connection.
    tokio::time::sleep(Duration::from_millis(100)).await;
    if cfg!(any(target_os = "linux", windows)) {
        assert!(SystemPlatform.has_established_connection(&local, p.port));
    }
    assert!(!SystemPlatform.has_established_connection(&local, closed_port()));
}

#[test]
fn factory_builds_the_expected_checks_per_device_type() {
    let platform = fake(None, false, None);
    let t = Timeouts::default();
    let names = |d: Device| DeviceProbe::for_device(&d, &t, platform.clone()).names();
    let device = |kind: &str, host: &str| Device { name: "D".into(), kind: kind.into(), host: host.into(), ..Device::default() };

    assert_eq!(
        names(Device { mac: Some("00-11-22-33-44-55".into()), windows_queue: Some("Q".into()), ..device("escpos-printer", "10.0.0.1") }),
        ["busy:9100", "ping", "mac", "escpos:9100", "queue"]
    );
    assert_eq!(names(device("printer", "10.0.0.2")), ["ping", "ipp:631"]);
    assert_eq!(
        names(Device { ipp: Some(Ipp { enabled: false, ..Ipp::default() }), ..device("printer", "10.0.0.2") }),
        ["ping", "tcp:9100"]
    );
    assert_eq!(
        names(Device {
            ping: false,
            tcp_ports: vec![1883, 80, 1883],
            http: Some(HttpConfig { url: "http://10.0.0.3/".into(), ..HttpConfig::default() }),
            ..device("generic", "10.0.0.3")
        }),
        ["tcp:1883,80", "http"]
    );
    assert_eq!(names(Device { port: Some(502), tcp_ports: vec![80, 502], ..device("generic", "10.0.0.4") }), ["ping", "tcp:502", "tcp:80"]);
}

#[test]
fn sample_config_builds_probes() {
    let config = Config::parse(include_str!("../watchdog.json"), "sample").unwrap();
    for d in &config.devices {
        assert!(!DeviceProbe::for_device(d, &config.timeouts, fake(None, false, None)).checks.is_empty());
    }
}
