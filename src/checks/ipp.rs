//! Minimal IPP (RFC 8010/8011) Get-Printer-Attributes client for office printers.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;

use super::http::{classify, http_client};
use super::simple::port_blocked;
use super::{CheckResult, DeviceCheck, FailureCode};
use crate::i18n::Msg;
use crate::msg;

pub const GET_PRINTER_ATTRIBUTES: u16 = 0x000B;

const OPERATION_ATTRIBUTES: u8 = 0x01;
const END_OF_ATTRIBUTES: u8 = 0x03;
const TAG_INTEGER: u8 = 0x21;
const TAG_ENUM: u8 = 0x23;
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;

fn attribute(out: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

/// Get-Printer-Attributes asking for printer-state, -reasons and -message.
pub fn build_get_printer_state(printer_uri: &str, request_id: u32) -> Vec<u8> {
    let mut out = vec![0x02, 0x00];
    out.extend_from_slice(&GET_PRINTER_ATTRIBUTES.to_be_bytes());
    out.extend_from_slice(&request_id.to_be_bytes());
    out.push(OPERATION_ATTRIBUTES);
    attribute(&mut out, TAG_CHARSET, "attributes-charset", b"utf-8");
    attribute(&mut out, TAG_LANGUAGE, "attributes-natural-language", b"en");
    attribute(&mut out, TAG_URI, "printer-uri", printer_uri.as_bytes());
    attribute(&mut out, TAG_KEYWORD, "requested-attributes", b"printer-state");
    attribute(&mut out, TAG_KEYWORD, "", b"printer-state-reasons"); // additional values
    attribute(&mut out, TAG_KEYWORD, "", b"printer-state-message");
    out.push(END_OF_ATTRIBUTES);
    out
}

/// An attribute value: integers/enums as numbers, everything else as text.
#[derive(Clone, Debug, PartialEq)]
pub enum IppValue {
    Int(i32),
    Text(String),
}

#[derive(Debug, PartialEq)]
pub struct IppResponse {
    pub status: u16,
    pub attributes: HashMap<String, Vec<IppValue>>,
}

impl IppResponse {
    pub fn is_success(&self) -> bool {
        self.status < 0x0100
    }

    /// 3 idle, 4 processing, 5 stopped.
    pub fn printer_state(&self) -> Option<i32> {
        match self.attributes.get("printer-state")?.first()? {
            IppValue::Int(i) => Some(*i),
            IppValue::Text(_) => None,
        }
    }

    pub fn state_reasons(&self) -> Vec<&str> {
        self.attributes
            .get("printer-state-reasons")
            .map(|v| v.iter().filter_map(|x| if let IppValue::Text(t) = x { Some(t.as_str()) } else { None }).collect())
            .unwrap_or_default()
    }
}

/// Parses an IPP message (request or response share the format).
pub fn parse_ipp_response(data: &[u8]) -> Result<IppResponse, &'static str> {
    if data.len() < 9 {
        return Err("IPP response too short");
    }
    let status = u16::from_be_bytes([data[2], data[3]]);
    let mut attributes: HashMap<String, Vec<IppValue>> = HashMap::new();
    let mut pos = 8;
    let mut current: Option<String> = None;
    let take = |pos: &mut usize, n: usize| -> Result<&[u8], &'static str> {
        let s = data.get(*pos..*pos + n).ok_or("IPP response truncated")?;
        *pos += n;
        Ok(s)
    };
    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        if tag == END_OF_ATTRIBUTES {
            break;
        }
        if tag < 0x10 {
            current = None; // a new attribute group
            continue;
        }
        let name_len = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap()) as usize;
        let name = String::from_utf8_lossy(take(&mut pos, name_len)?).into_owned();
        let value_len = u16::from_be_bytes(take(&mut pos, 2)?.try_into().unwrap()) as usize;
        let value = take(&mut pos, value_len)?;
        if name_len > 0 {
            current = Some(name);
        }
        let key = current.clone().ok_or("IPP additional value without an attribute")?;
        let v = if matches!(tag, TAG_INTEGER | TAG_ENUM) && value.len() == 4 {
            IppValue::Int(i32::from_be_bytes(value.try_into().unwrap()))
        } else {
            IppValue::Text(String::from_utf8_lossy(value).into_owned())
        };
        attributes.entry(key).or_default().push(v);
    }
    Ok(IppResponse { status, attributes })
}

/// Splits printer-state-reasons into (errors, warnings). Per RFC 8011 a keyword
/// without suffix is an error; `-report` keywords are informational.
pub fn classify_ipp_reasons<'a>(reasons: impl IntoIterator<Item = &'a str>) -> (Vec<Msg>, Vec<Msg>) {
    let (mut errors, mut warnings) = (Vec::new(), Vec::new());
    for raw in reasons {
        let reason = raw.trim().to_ascii_lowercase();
        if reason.is_empty() || reason == "none" || reason.ends_with("-report") {
            continue;
        }
        let (keyword, warning) = if let Some(k) = reason.strip_suffix("-warning") {
            (k, true)
        } else {
            (reason.strip_suffix("-error").unwrap_or(&reason), false)
        };
        let text = describe(keyword);
        if warning { warnings.push(text) } else { errors.push(text) }
    }
    (errors, warnings)
}

fn describe(keyword: &str) -> Msg {
    Msg::key(match keyword {
        "media-empty" | "media-needed" => "reason.paperEnd",
        "media-low" => "reason.paperNearEnd",
        "media-jam" => "reason.paperJam",
        "toner-empty" | "marker-supply-empty" => "reason.tonerEmpty",
        "toner-low" | "marker-supply-low" => "reason.tonerLow",
        "door-open" => "reason.doorOpen",
        "cover-open" => "reason.coverOpen",
        "paused" | "moving-to-paused" => "reason.paused",
        "output-area-full" | "output-tray-missing" => "reason.outputFull",
        "input-tray-missing" => "reason.trayMissing",
        "offline" | "shutdown" => "reason.offline",
        other => return Msg::raw(other),
    })
}

/// Asks the printer for its state over IPP; `DeviceError` when stopped or
/// reporting an error reason.
pub struct IppCheck {
    client: reqwest::Client,
    host: String,
    port: u16,
    endpoint: String,
    printer_uri: String,
}

impl IppCheck {
    pub fn new(host: &str, port: u16, path: &str, tls: bool, timeout: Duration) -> Self {
        let host_part = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
        IppCheck {
            // Printers ship self-signed certificates; the IPP status is not sensitive.
            client: http_client(timeout, tls),
            host: host.to_owned(),
            port,
            endpoint: format!("{}://{host_part}:{port}{path}", if tls { "https" } else { "http" }),
            printer_uri: format!("{}://{host_part}:{port}{path}", if tls { "ipps" } else { "ipp" }),
        }
    }

    fn no_response(detail: Msg) -> CheckResult {
        CheckResult::fail(FailureCode::NoResponse, msg!("msg.noResponse.ipp", detail = detail))
    }
}

#[async_trait]
impl DeviceCheck for IppCheck {
    fn name(&self) -> String {
        format!("ipp:{}", self.port)
    }

    async fn run(&self) -> CheckResult {
        let request = self
            .client
            .post(&self.endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/ipp")
            .body(build_get_printer_state(&self.printer_uri, 1));
        let reply = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                return match classify(&e) {
                    (FailureCode::PortBlocked, detail) => {
                        CheckResult::fail(FailureCode::PortBlocked, port_blocked(&self.host, self.port, detail, false))
                    }
                    (_, detail) => Self::no_response(detail),
                };
            }
        };
        if !reply.status().is_success() {
            return Self::no_response(msg!("detail.httpStatus", status = reply.status().as_u16()));
        }
        let body = match reply.bytes().await {
            Ok(b) => b,
            Err(e) => return Self::no_response(classify(&e).1),
        };
        let Ok(response) = parse_ipp_response(&body) else {
            return Self::no_response(Msg::key("detail.invalidResponse"));
        };
        if !response.is_success() {
            return Self::no_response(msg!("detail.ippStatus", status = format!("{:04X}", response.status)));
        }
        let (mut errors, warnings) = classify_ipp_reasons(response.state_reasons());
        if response.printer_state() == Some(5) && errors.is_empty() {
            errors.push(Msg::key("reason.stopped"));
        }
        if errors.is_empty() {
            CheckResult::Ok { notes: warnings }
        } else {
            CheckResult::Failed { code: FailureCode::DeviceError, message: msg!("msg.deviceError", reasons = errors), notes: warnings }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_the_parser() {
        let req = build_get_printer_state("ipp://printer.local:631/ipp/print", 7);
        assert_eq!(&req[..2], &[2, 0]);
        assert_eq!(u16::from_be_bytes([req[2], req[3]]), GET_PRINTER_ATTRIBUTES);
        assert_eq!(u32::from_be_bytes(req[4..8].try_into().unwrap()), 7);
        assert_eq!(*req.last().unwrap(), END_OF_ATTRIBUTES);
        let parsed = parse_ipp_response(&req).unwrap();
        assert_eq!(parsed.attributes["printer-uri"], [IppValue::Text("ipp://printer.local:631/ipp/print".into())]);
        assert_eq!(parsed.attributes["requested-attributes"].len(), 3);
    }

    #[test]
    fn truncated_data_is_rejected() {
        let req = build_get_printer_state("ipp://x/ipp/print", 1);
        assert!(parse_ipp_response(&req[..req.len() - 6]).is_err());
    }

    #[test]
    fn reasons_are_classified_by_suffix() {
        let (errors, warnings) =
            classify_ipp_reasons(["none", "media-jam-error", "toner-low-warning", "other-report", "door-open", "spaceship-error"]);
        assert_eq!(errors, [Msg::key("reason.paperJam"), Msg::key("reason.doorOpen"), Msg::raw("spaceship")]);
        assert_eq!(warnings, [Msg::key("reason.tonerLow")]);
    }
}
