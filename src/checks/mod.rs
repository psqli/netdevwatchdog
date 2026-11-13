//! Device checks. Each check either passes (possibly with notes), reports the
//! device as busy, or fails with a [`FailureCode`] and an explanation.

mod escpos;
mod http;
mod ipp;
mod net;
mod ping;
mod simple;

pub use escpos::{EscPosStatus, EscPosStatusCheck, interpret_escpos_status, is_valid_escpos_reply};
pub use http::HttpCheck;
pub(crate) use http::http_client;
pub use ipp::{IppCheck, IppResponse, IppValue, build_get_printer_state, classify_ipp_reasons, parse_ipp_response};
pub use ping::PingCheck;
pub use simple::{LocalConnectionBusyCheck, MacCheck, PrintQueueCheck, TcpPortCheck};

use async_trait::async_trait;

use crate::i18n::Msg;

/// What kind of failure was found. Each has a localized title (`failure.*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureCode {
    /// No ping reply.
    Unreachable,
    /// The IP answers with another device's MAC address.
    IpConflict,
    /// Ping works but a TCP port refuses or ignores connections.
    PortBlocked,
    /// The port connects but the device does not answer its protocol.
    NoResponse,
    /// The device reports an error (paper out, cover open...).
    DeviceError,
    /// A Windows print-queue job is in an error state.
    QueueError,
    /// This computer's print connection stayed open too long.
    JobStuck,
    /// HTTP health check returned the wrong status or content.
    HealthCheckFailed,
    /// The check itself crashed.
    CheckFailed,
}

impl FailureCode {
    pub fn title_key(self) -> &'static str {
        match self {
            FailureCode::Unreachable => "failure.unreachable",
            FailureCode::IpConflict => "failure.ipConflict",
            FailureCode::PortBlocked => "failure.portBlocked",
            FailureCode::NoResponse => "failure.noResponse",
            FailureCode::DeviceError => "failure.deviceError",
            FailureCode::QueueError => "failure.queueError",
            FailureCode::JobStuck => "failure.jobStuck",
            FailureCode::HealthCheckFailed => "failure.healthCheckFailed",
            FailureCode::CheckFailed => "failure.checkFailed",
        }
    }

    pub fn title(self) -> Msg {
        Msg::key(self.title_key())
    }

    /// Stable identifier for logs and external commands.
    pub fn id(self) -> &'static str {
        &self.title_key()["failure.".len()..]
    }
}

/// Result of a check or of a whole device probe.
#[derive(Clone, Debug, PartialEq)]
pub enum CheckResult {
    /// Passed. Notes are warnings that are not failures (e.g. paper running low).
    Ok {
        notes: Vec<Msg>,
    },
    /// This computer is printing to the device; skip the round.
    Busy,
    Failed {
        code: FailureCode,
        message: Msg,
        notes: Vec<Msg>,
    },
}

impl CheckResult {
    pub fn ok() -> Self {
        CheckResult::Ok { notes: Vec::new() }
    }

    pub fn fail(code: FailureCode, message: Msg) -> Self {
        CheckResult::Failed { code, message, notes: Vec::new() }
    }

    pub fn failure(&self) -> Option<FailureCode> {
        match self {
            CheckResult::Failed { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn message(&self) -> Option<&Msg> {
        match self {
            CheckResult::Failed { message, .. } => Some(message),
            _ => None,
        }
    }

    pub fn notes(&self) -> &[Msg] {
        match self {
            CheckResult::Ok { notes } | CheckResult::Failed { notes, .. } => notes,
            CheckResult::Busy => &[],
        }
    }

    /// Prepends notes gathered by earlier checks.
    pub fn with_notes_before(mut self, earlier: Vec<Msg>) -> Self {
        if let CheckResult::Ok { notes } | CheckResult::Failed { notes, .. } = &mut self {
            let mut all = earlier;
            all.append(notes);
            *notes = all;
        }
        self
    }
}

/// One check of a device.
#[async_trait]
pub trait DeviceCheck: Send + Sync {
    /// Short name for diagnostics, e.g. `ping`, `escpos:9100`.
    fn name(&self) -> String;

    async fn run(&self) -> CheckResult;
}
