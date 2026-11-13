//! ESC/POS real-time status (`DLE EOT n` = `10 04 n`) over the raw print port.

use std::time::Duration;

use async_trait::async_trait;

use super::net;
use super::simple::port_blocked;
use super::{CheckResult, DeviceCheck, FailureCode};
use crate::i18n::Msg;
use crate::msg;

/// Queries an ESC/POS printer.
///
/// Fails with `PortBlocked` when the port refuses the connection (these printers
/// typically accept a single client), `NoResponse` when it connects but does not
/// answer, and `DeviceError` when the printer reports an error. The socket is
/// always closed so the printer's single connection slot is released.
pub struct EscPosStatusCheck {
    pub host: String,
    pub port: u16,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
}

#[async_trait]
impl DeviceCheck for EscPosStatusCheck {
    fn name(&self) -> String {
        format!("escpos:{}", self.port)
    }

    async fn run(&self) -> CheckResult {
        let mut stream = match net::connect(&self.host, self.port, self.connect_timeout).await {
            Ok(s) => s,
            Err(detail) => return CheckResult::fail(FailureCode::PortBlocked, port_blocked(&self.host, self.port, detail, true)),
        };
        // Some printers push automatic status packets right after connecting.
        net::drain(&mut stream, Duration::from_millis(50)).await;

        let mut query = async |n: u8| {
            if !net::write_all(&mut stream, &[0x10, 0x04, n]).await {
                return None;
            }
            net::read_byte(&mut stream, self.read_timeout).await
        };

        let printer = query(1).await;
        let Some(s1) = printer.filter(|b| is_valid_escpos_reply(*b)) else {
            let detail = match printer {
                None => Msg::key("detail.noReply"),
                Some(b) => msg!("detail.unexpectedReply", value = format!("{b:02X}")),
            };
            return CheckResult::fail(FailureCode::NoResponse, msg!("msg.noResponse.escpos", port = self.port, detail = detail));
        };
        let paper = query(4).await;
        let (offline_cause, error_cause) = if s1 & 0x08 != 0 { (query(2).await, query(3).await) } else { (None, None) };

        let status = interpret_escpos_status(Some(s1), offline_cause, error_cause, paper);
        if status.errors.is_empty() {
            CheckResult::Ok { notes: status.warnings }
        } else {
            CheckResult::Failed {
                code: FailureCode::DeviceError,
                message: msg!("msg.deviceError", reasons = status.errors),
                notes: status.warnings,
            }
        }
        // `stream` is dropped here, closing the connection.
    }
}

/// Every status byte has bits 1 and 4 set and bits 0 and 7 clear.
pub fn is_valid_escpos_reply(b: u8) -> bool {
    b & 0x93 == 0x12
}

/// Decoded status: errors make the device fail, warnings are informational.
#[derive(Debug, Default, PartialEq)]
pub struct EscPosStatus {
    pub errors: Vec<Msg>,
    pub warnings: Vec<Msg>,
}

/// Interprets replies to n=1 (printer), n=2 (offline cause), n=3 (error cause)
/// and n=4 (paper sensors). `None` = not read; invalid bytes are ignored.
pub fn interpret_escpos_status(printer: Option<u8>, offline: Option<u8>, error: Option<u8>, paper: Option<u8>) -> EscPosStatus {
    let valid = |b: Option<u8>| b.filter(|b| is_valid_escpos_reply(*b));
    let mut s = EscPosStatus::default();
    let has = |errors: &[Msg], key| errors.iter().any(|m| m.key == key);

    if let Some(p) = valid(paper) {
        if p & 0x60 != 0 {
            s.errors.push(Msg::key("reason.paperEnd"));
        } else if p & 0x0C != 0 {
            s.warnings.push(Msg::key("reason.paperNearEnd"));
        }
    }
    if valid(printer).is_some_and(|p| p & 0x08 != 0) {
        let before = s.errors.len();
        if let Some(o) = valid(offline) {
            if o & 0x04 != 0 {
                s.errors.push(Msg::key("reason.coverOpen"));
            }
            if o & 0x08 != 0 {
                s.errors.push(Msg::key("reason.feedButton"));
            }
            if o & 0x20 != 0 && !has(&s.errors, "reason.paperEnd") {
                s.errors.push(Msg::key("reason.stoppedPaperEnd"));
            }
            if o & 0x40 != 0 {
                s.errors.push(Msg::key("reason.errorState"));
            }
        }
        if let Some(e) = valid(error) {
            if e & 0x08 != 0 {
                s.errors.push(Msg::key("reason.cutterError"));
            }
            if e & 0x20 != 0 {
                s.errors.push(Msg::key("reason.unrecoverable"));
            }
            if e & 0x40 != 0 {
                s.errors.push(Msg::key("reason.overheated"));
            }
        }
        if s.errors.len() == before && !has(&s.errors, "reason.paperEnd") {
            s.errors.push(Msg::key("reason.offlineUnknown"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &[Msg]) -> Vec<&str> {
        v.iter().map(|m| m.key).collect()
    }

    #[test]
    fn valid_status_bytes() {
        for (b, ok) in [(0x12, true), (0x16, true), (0x1E, true), (0x10, false), (0x13, false), (0x92, false), (0xFF, false)] {
            assert_eq!(is_valid_escpos_reply(b), ok, "{b:#x}");
        }
    }

    #[test]
    fn ready_printer() {
        assert_eq!(interpret_escpos_status(Some(0x16), None, None, Some(0x12)), EscPosStatus::default());
    }

    #[test]
    fn paper_near_end_is_only_a_warning() {
        let s = interpret_escpos_status(Some(0x16), None, None, Some(0x12 | 0x0C));
        assert!(s.errors.is_empty());
        assert_eq!(keys(&s.warnings), ["reason.paperNearEnd"]);
    }

    #[test]
    fn offline_causes() {
        let s = interpret_escpos_status(Some(0x1E), Some(0x12 | 0x04 | 0x40), Some(0x12 | 0x08 | 0x40), Some(0x12));
        assert_eq!(keys(&s.errors), ["reason.coverOpen", "reason.errorState", "reason.cutterError", "reason.overheated"]);
    }

    #[test]
    fn paper_out_is_not_reported_twice() {
        let s = interpret_escpos_status(Some(0x1E), Some(0x12 | 0x20), Some(0x12), Some(0x12 | 0x60));
        assert_eq!(keys(&s.errors), ["reason.paperEnd"]);
    }

    #[test]
    fn offline_without_cause() {
        let s = interpret_escpos_status(Some(0x1E), Some(0x12), Some(0x12), Some(0x12));
        assert_eq!(keys(&s.errors), ["reason.offlineUnknown"]);
    }
}
