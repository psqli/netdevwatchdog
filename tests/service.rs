//! The service end to end: scripted device checks, recording channels.

mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use netdevwatchdog::checks::{CheckResult, DeviceCheck, FailureCode};
use netdevwatchdog::config::{CommandNotification, Config};
use netdevwatchdog::i18n::{Localizer, Msg};
use netdevwatchdog::monitor::{DeviceMonitor, Health, Thresholds};
use netdevwatchdog::msg;
use netdevwatchdog::notify::{CommandNotifier, Kind, LogSink, Notification, Notifier, NtfyNotifier};
use netdevwatchdog::probe::DeviceProbe;
use netdevwatchdog::service::Service;
use tokio_util::sync::CancellationToken;

/// Returns the scripted results in order, then keeps returning the last one.
struct Scripted(Mutex<VecDeque<CheckResult>>);

#[async_trait]
impl DeviceCheck for Scripted {
    fn name(&self) -> String {
        "scripted".into()
    }
    async fn run(&self) -> CheckResult {
        let mut q = self.0.lock().unwrap();
        if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap() }
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Notification>>);

#[async_trait]
impl Notifier for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }
    async fn notify(&self, n: &Notification) -> Result<(), String> {
        self.0.lock().unwrap().push(n.clone());
        Ok(())
    }
}

struct Broken;

#[async_trait]
impl Notifier for Broken {
    fn name(&self) -> &str {
        "broken"
    }
    async fn notify(&self, _: &Notification) -> Result<(), String> {
        Err("no route to host".into())
    }
}

#[derive(Default)]
struct MemoryLog(Mutex<Vec<String>>);

impl LogSink for MemoryLog {
    fn write(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_owned());
    }
}

fn cover_open() -> CheckResult {
    CheckResult::fail(FailureCode::DeviceError, msg!("msg.deviceError", reasons = vec![Msg::key("reason.coverOpen")]))
}

struct Fixture {
    service: Service,
    recorder: Arc<Recorder>,
    log: Arc<MemoryLog>,
}

fn fixture(language: &str, results: Vec<CheckResult>, extra: Vec<Arc<dyn Notifier>>) -> Fixture {
    let recorder = Arc::new(Recorder::default());
    let log = Arc::new(MemoryLog::default());
    let probe = DeviceProbe { checks: vec![Box::new(Scripted(Mutex::new(results.into())))] };
    let monitor = DeviceMonitor::new("Caixa 1", "192.168.10.21", probe, Thresholds { fail: 2, busy: 3 });
    let mut notifiers: Vec<Arc<dyn Notifier>> = extra;
    notifiers.push(recorder.clone());
    let config = Config { interval_seconds: 1, ..Config::default() };
    let service = Service::new(config, Localizer::new(Some(language), None), vec![monitor], notifiers, log.clone());
    Fixture { service, recorder, log }
}

#[tokio::test]
async fn alerts_once_after_the_threshold_and_then_reports_recovery_in_portuguese() {
    let f = fixture("pt", vec![cover_open(), cover_open(), cover_open(), CheckResult::ok()], vec![]);
    f.service.check_all().await;
    assert!(f.recorder.0.lock().unwrap().is_empty(), "one failure is below the threshold");
    f.service.check_all().await;
    f.service.check_all().await;
    let snapshots = f.service.check_all().await;
    assert_eq!(snapshots[0].health, Health::Ok);

    let sent = f.recorder.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 2, "{sent:#?}");
    assert_eq!(sent[0].kind, Kind::Problem);
    assert_eq!(sent[0].title, "Caixa 1: Erro no dispositivo");
    assert!(sent[0].body.contains("tampa aberta"), "{}", sent[0].body);
    assert_eq!(sent[1].kind, Kind::Recovery);
    assert_eq!(sent[1].title, "Caixa 1 voltou a funcionar");
    assert_eq!(sent[1].body, "192.168.10.21 está funcionando de novo (problema anterior: Erro no dispositivo).");
    assert!(f.log.0.lock().unwrap().iter().any(|l| l.starts_with("ALERTA Caixa 1")));
}

#[tokio::test]
async fn a_failure_below_the_threshold_is_only_logged() {
    let f = fixture("en", vec![cover_open(), CheckResult::ok()], vec![]);
    f.service.check_all().await;
    f.service.check_all().await;
    assert!(f.recorder.0.lock().unwrap().is_empty());
    let log = f.log.0.lock().unwrap();
    assert!(log.iter().any(|l| l == "Caixa 1: Device error - The device reports: cover open."), "{log:#?}");
    assert!(log.iter().any(|l| l.contains("temporary problem")), "{log:#?}");
}

#[tokio::test]
async fn a_failing_channel_does_not_stop_the_others() {
    let f = fixture("en", vec![cover_open()], vec![Arc::new(Broken)]);
    f.service.check_all().await;
    f.service.check_all().await;
    assert_eq!(f.recorder.0.lock().unwrap().len(), 1);
    let log = f.log.0.lock().unwrap();
    assert!(log.iter().any(|l| l.contains("broken") && l.contains("no route to host")), "{log:#?}");
}

#[tokio::test]
async fn test_alert_is_localized() {
    let f = fixture("zh", vec![CheckResult::ok()], vec![]);
    f.service.send_test_alert().await;
    let sent = f.recorder.0.lock().unwrap();
    assert_eq!(sent[0].kind, Kind::Test);
    assert_eq!(sent[0].title, "测试：端口被占用");
}

#[tokio::test]
async fn status_listeners_see_every_round_and_run_stops_on_cancel() {
    let f = fixture("en", vec![CheckResult::ok()], vec![]);
    let rounds = Arc::new(Mutex::new(0));
    let counter = rounds.clone();
    f.service.on_status(move |s| {
        assert_eq!(s[0].device, "Caixa 1");
        *counter.lock().unwrap() += 1;
    });
    let stop = CancellationToken::new();
    let cancel = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        cancel.cancel();
    });
    tokio::time::timeout(Duration::from_secs(5), f.service.run(stop)).await.expect("run stops when cancelled");
    assert!(*rounds.lock().unwrap() >= 2);
    let log = f.log.0.lock().unwrap();
    assert!(log.first().unwrap().starts_with("Watchdog started: 1 device(s)"), "{log:#?}");
    assert_eq!(log.last().unwrap(), "Watchdog stopped.");
}

fn notification() -> Notification {
    Notification {
        kind: Kind::Problem,
        device: "Caixa 1".into(),
        host: "192.168.10.21".into(),
        failure: Some(FailureCode::DeviceError),
        title: "Caixa 1: Não responde".into(),
        body: "O dispositivo informa: tampa aberta.".into(),
        time: std::time::SystemTime::now(),
    }
}

#[tokio::test]
async fn ntfy_posts_the_alert_with_an_encoded_title() {
    let server = common::FakeHttp::start(|_, _, _| (200, "application/json", b"{}".to_vec())).await;
    let url = format!("http://127.0.0.1:{}/watchdog", server.port).parse().unwrap();
    NtfyNotifier::new(url, Some("tk_secret".into())).notify(&notification()).await.unwrap();
    let request = server.requests.lock().unwrap()[0].clone();
    assert_eq!((request.method.as_str(), request.path.as_str()), ("POST", "/watchdog"));
    assert_eq!(std::str::from_utf8(&request.body).unwrap(), "O dispositivo informa: tampa aberta.");
    // Non-ASCII titles are RFC 2047 encoded; HTTP headers are ASCII.
    assert_eq!(request.header("title"), Some("=?UTF-8?B?Q2FpeGEgMTogTsOjbyByZXNwb25kZQ==?="));
    assert_eq!(request.header("priority"), Some("high"));
    assert_eq!(request.header("authorization"), Some("Bearer tk_secret"));

    let failing = common::FakeHttp::start(|_, _, _| (403, "text/plain", Vec::new())).await;
    let url = format!("http://127.0.0.1:{}/watchdog", failing.port).parse().unwrap();
    assert!(NtfyNotifier::new(url, None).notify(&notification()).await.unwrap_err().contains("403"));
}

#[cfg(unix)]
#[tokio::test]
async fn command_gets_placeholders_and_environment() {
    let dir = std::env::temp_dir().join(format!("ndw-cmd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("out.txt");
    let command = CommandNotification {
        path: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            format!("printf '%s|%s|%s' \"$1\" \"$NDW_FAILURE\" \"$NDW_TITLE\" > {}", out.display()),
            "sh".into(),
            "{kind}".into(),
        ],
        timeout_seconds: 5,
    };
    CommandNotifier(command).notify(&notification()).await.unwrap();
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "problem|deviceError|Caixa 1: Não responde");

    let slow = CommandNotification { path: "/bin/sh".into(), arguments: vec!["-c".into(), "sleep 10".into()], timeout_seconds: 1 };
    assert!(CommandNotifier(slow).notify(&notification()).await.unwrap_err().contains("did not finish"));
    let failing = CommandNotification { path: "/bin/sh".into(), arguments: vec!["-c".into(), "exit 3".into()], timeout_seconds: 5 };
    assert!(CommandNotifier(failing).notify(&notification()).await.is_err());
    let _ = std::fs::remove_dir_all(dir);
}
