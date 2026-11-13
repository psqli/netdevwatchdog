//! HTTP(S) health check for embedded devices and services.

use std::time::Duration;

use async_trait::async_trait;

use super::{CheckResult, DeviceCheck, FailureCode};
use crate::i18n::Msg;
use crate::msg;

const MAX_BODY: usize = 1 << 20;

/// Fails with `PortBlocked` when the connection fails, `NoResponse` on timeout,
/// and `HealthCheckFailed` on an unexpected status or body.
pub struct HttpCheck {
    client: reqwest::Client,
    url: reqwest::Url,
    expect_status: Option<u16>,
    expect_body: Option<String>,
}

impl HttpCheck {
    pub fn new(
        url: reqwest::Url,
        expect_status: Option<u16>,
        expect_body: Option<String>,
        ignore_cert_errors: bool,
        timeout: Duration,
    ) -> Self {
        let client = http_client(timeout, ignore_cert_errors);
        HttpCheck { client, url, expect_status, expect_body: expect_body.filter(|b| !b.is_empty()) }
    }
}

/// HTTP client with connect and total timeouts.
pub(crate) fn http_client(timeout: Duration, accept_invalid_certs: bool) -> reqwest::Client {
    crate::install_crypto_provider();
    reqwest::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .danger_accept_invalid_certs(accept_invalid_certs)
        .no_proxy()
        .build()
        .expect("HTTP client configuration is valid")
}

/// Turns a reqwest error into (failure code, detail).
pub(crate) fn classify(e: &reqwest::Error) -> (FailureCode, Msg) {
    if e.is_timeout() {
        return (FailureCode::NoResponse, Msg::key("detail.timeout"));
    }
    // Walk the source chain for the underlying socket error.
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<std::io::Error>() {
            return (FailureCode::PortBlocked, super::net::describe(io));
        }
        source = s.source();
    }
    let code = if e.is_connect() { FailureCode::PortBlocked } else { FailureCode::NoResponse };
    (code, Msg::raw(e.to_string()))
}

#[async_trait]
impl DeviceCheck for HttpCheck {
    fn name(&self) -> String {
        "http".into()
    }

    async fn run(&self) -> CheckResult {
        let url = self.url.to_string();
        let response = match self.client.get(self.url.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                return match classify(&e) {
                    (FailureCode::NoResponse, detail) => {
                        CheckResult::fail(FailureCode::NoResponse, msg!("msg.noResponse.http", url = &url, detail = detail))
                    }
                    (code, detail) => CheckResult::fail(code, msg!("msg.http.connect", url = &url, detail = detail)),
                };
            }
        };
        let status = response.status().as_u16();
        let ok = match self.expect_status {
            Some(expected) => status == expected,
            None => (200..300).contains(&status),
        };
        if !ok {
            let expected = self.expect_status.map_or("2xx".to_owned(), |s| s.to_string());
            return CheckResult::fail(
                FailureCode::HealthCheckFailed,
                msg!("msg.http.status", url = &url, status = status, expected = expected),
            );
        }
        if let Some(text) = &self.expect_body {
            let body = read_limited(response).await;
            if !body.contains(text.as_str()) {
                return CheckResult::fail(FailureCode::HealthCheckFailed, msg!("msg.http.body", url = &url, text = text));
            }
        }
        CheckResult::ok()
    }
}

async fn read_limited(mut response: reqwest::Response) -> String {
    let mut body = Vec::new();
    while body.len() < MAX_BODY {
        match response.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            _ => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}
