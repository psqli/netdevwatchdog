//! JSON configuration: model, loading and validation.
//!
//! The file is JSON with comments and trailing commas allowed. Unknown settings
//! are rejected so typos are reported instead of silently ignored.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Root of the configuration file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Config {
    /// `auto` (operating system language) or a tag such as `pt-BR`.
    pub language: String,
    pub interval_seconds: u64,
    /// Failed checks in a row before alerting.
    pub fail_threshold: u32,
    /// ESC/POS: rounds with this computer's print connection open before "job stuck".
    pub busy_threshold: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locales_directory: Option<String>,
    pub notifications: Notifications,
    pub timeouts: Timeouts,
    pub devices: Vec<Device>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: "auto".into(),
            interval_seconds: 60,
            fail_threshold: 3,
            busy_threshold: 10,
            log_directory: None,
            locales_directory: None,
            notifications: Notifications::default(),
            timeouts: Timeouts::default(),
            devices: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Notifications {
    /// Desktop notification (Windows notification area).
    pub desktop: bool,
    /// Always-on-top alert window that stays until someone presses OK.
    pub alert_window: bool,
    pub alert_font_size: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ntfy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ntfy_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<CommandNotification>,
    pub notify_recovery: bool,
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            desktop: true,
            alert_window: true,
            alert_font_size: 18,
            ntfy_url: None,
            ntfy_token: None,
            command: None,
            notify_recovery: true,
        }
    }
}

/// Runs a program per notification; see the README for placeholders.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct CommandNotification {
    pub path: String,
    pub arguments: Vec<String>,
    pub timeout_seconds: u64,
}

impl Default for CommandNotification {
    fn default() -> Self {
        CommandNotification { path: String::new(), arguments: Vec::new(), timeout_seconds: 30 }
    }
}

/// Timeouts in milliseconds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Timeouts {
    pub ping_ms: u64,
    pub connect_ms: u64,
    pub read_ms: u64,
    pub http_ms: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts { ping_ms: 1000, connect_ms: 3000, read_ms: 2000, http_ms: 5000 }
    }
}

/// One monitored device.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Device {
    pub name: String,
    /// `escpos-printer`, `printer` or `generic`.
    #[serde(rename = "type")]
    pub kind: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    pub ping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows_queue: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tcp_ports: Vec<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipp: Option<Ipp>,
    pub enabled: bool,
}

impl Default for Device {
    fn default() -> Self {
        Device {
            name: String::new(),
            kind: "generic".into(),
            host: String::new(),
            port: None,
            mac: None,
            ping: true,
            windows_queue: None,
            tcp_ports: Vec::new(),
            http: None,
            ipp: None,
            enabled: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct HttpCheck {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_body_contains: Option<String>,
    pub ignore_certificate_errors: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Ipp {
    /// Set to false for printers without IPP: only the TCP port is checked.
    pub enabled: bool,
    pub path: String,
    pub tls: bool,
}

impl Default for Ipp {
    fn default() -> Self {
        Ipp { enabled: true, path: "/ipp/print".into(), tls: false }
    }
}

/// Device types and their default checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    /// Receipt/kitchen printer speaking ESC/POS on a raw TCP port.
    EscPosPrinter,
    /// Office printer reporting its state over IPP.
    Printer,
    /// Any networked device.
    Generic,
}

impl DeviceKind {
    pub const NAMES: [&str; 3] = ["escpos-printer", "printer", "generic"];

    pub fn parse(text: &str) -> Option<DeviceKind> {
        match text.trim().to_ascii_lowercase().as_str() {
            "escpos-printer" | "escpos" | "receipt-printer" | "thermal-printer" => Some(DeviceKind::EscPosPrinter),
            "printer" | "ipp-printer" | "office-printer" => Some(DeviceKind::Printer),
            "generic" | "device" | "embedded" => Some(DeviceKind::Generic),
            _ => None,
        }
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            DeviceKind::EscPosPrinter => Some(9100),
            DeviceKind::Printer => Some(631),
            DeviceKind::Generic => None,
        }
    }
}

/// Configuration errors: every problem found, in file order.
#[derive(Debug, thiserror::Error)]
#[error("invalid configuration in {source_name}:\n  - {}", errors.join("\n  - "))]
pub struct ConfigError {
    pub source_name: String,
    pub errors: Vec<String>,
}

impl Config {
    /// Loads and validates a configuration file.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError { source_name: path.display().to_string(), errors: vec![e.to_string()] })?;
        Config::parse(&text, &path.display().to_string())
    }

    /// Parses and validates configuration text.
    pub fn parse(text: &str, source_name: &str) -> Result<Config, ConfigError> {
        let fail = |errors| ConfigError { source_name: source_name.to_owned(), errors };
        let json = strip_jsonc(text);
        if json.trim().is_empty() {
            return Err(fail(vec!["The file is empty.".into()]));
        }
        let config: Config = serde_json::from_str(&json)
            .map_err(|e| fail(vec![format!("JSON error (line {}, column {}): {}", e.line(), e.column(), strip_position(&e))]))?;
        let errors = config.validate();
        if errors.is_empty() { Ok(config) } else { Err(fail(errors)) }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("configuration serializes")
    }

    /// Returns every problem; empty when valid.
    pub fn validate(&self) -> Vec<String> {
        let mut e = Vec::new();
        if self.interval_seconds < 5 {
            e.push("intervalSeconds must be at least 5.".into());
        }
        if self.fail_threshold < 1 {
            e.push("failThreshold must be at least 1.".into());
        }
        if self.busy_threshold < 1 {
            e.push("busyThreshold must be at least 1.".into());
        }
        if !(8..=72).contains(&self.notifications.alert_font_size) {
            e.push("notifications.alertFontSize must be between 8 and 72.".into());
        }
        if let Some(url) = &self.notifications.ntfy_url
            && !is_http_url(url)
        {
            e.push("notifications.ntfyUrl must be an http(s) URL.".into());
        }
        if let Some(cmd) = &self.notifications.command {
            if cmd.path.trim().is_empty() {
                e.push("notifications.command.path is required when notifications.command is set.".into());
            }
            if cmd.timeout_seconds < 1 {
                e.push("notifications.command.timeoutSeconds must be at least 1.".into());
            }
        }
        let t = &self.timeouts;
        for (name, v) in [("pingMs", t.ping_ms), ("connectMs", t.connect_ms), ("readMs", t.read_ms), ("httpMs", t.http_ms)] {
            if !(100..=120_000).contains(&v) {
                e.push(format!("timeouts.{name} must be between 100 and 120000."));
            }
        }
        if self.devices.is_empty() {
            e.push("devices is empty: add at least one device.".into());
        }

        let mut names = HashSet::new();
        for (i, d) in self.devices.iter().enumerate() {
            let at = if d.name.trim().is_empty() { format!("devices[{i}]") } else { format!("devices[{i}] \"{}\"", d.name) };
            if d.name.trim().is_empty() {
                e.push(format!("{at}: name is required."));
            } else if !names.insert(d.name.trim().to_lowercase()) {
                e.push(format!("{at}: the name is used by another device."));
            }
            if d.host.trim().is_empty() {
                e.push(format!("{at}: host is required."));
            } else if !is_valid_host(d.host.trim()) {
                e.push(format!("{at}: host \"{}\" is not a valid IP address or host name.", d.host));
            }
            let Some(kind) = DeviceKind::parse(&d.kind) else {
                e.push(format!("{at}: type \"{}\" is unknown; use one of: {}.", d.kind, DeviceKind::NAMES.join(", ")));
                continue;
            };
            if d.port == Some(0) {
                e.push(format!("{at}: port 0 is out of range (1-65535)."));
            }
            if d.tcp_ports.contains(&0) {
                e.push(format!("{at}: tcpPorts contains 0, which is out of range (1-65535)."));
            }
            if let Some(mac) = &d.mac
                && !mac.trim().is_empty()
                && parse_mac(mac).is_none()
            {
                e.push(format!("{at}: mac \"{mac}\" is not a valid MAC address (use 00-11-22-33-44-55)."));
            }
            if let Some(h) = &d.http {
                if !is_http_url(&h.url) {
                    e.push(format!("{at}: http.url must be an absolute http(s) URL."));
                }
                if let Some(s) = h.expect_status
                    && !(100..=599).contains(&s)
                {
                    e.push(format!("{at}: http.expectStatus must be an HTTP status code (100-599)."));
                }
            }
            if let (DeviceKind::Printer, Some(ipp)) = (kind, &d.ipp)
                && ipp.enabled
                && !ipp.path.starts_with('/')
            {
                e.push(format!("{at}: ipp.path must start with '/'."));
            }
            if kind == DeviceKind::Generic && !d.ping && d.tcp_ports.is_empty() && d.http.is_none() && d.port.is_none() {
                e.push(format!("{at}: a generic device needs at least one check: ping, port, tcpPorts or http."));
            }
        }
        e
    }
}

/// Parses `00-11-22-33-44-55`, `00:11:22:33:44:55`, `0011.2233.4455` or `001122334455`.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    if !text.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, '-' | ':' | '.' | ' ')) {
        return None;
    }
    let hex: String = text.chars().filter(char::is_ascii_hexdigit).collect();
    if hex.len() != 12 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(mac)
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join("-")
}

fn is_http_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
}

fn is_valid_host(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

/// serde_json appends " at line X column Y" to its messages; we print it separately.
fn strip_position(e: &serde_json::Error) -> String {
    let s = e.to_string();
    match s.rfind(" at line ") {
        Some(i) => s[..i].to_owned(),
        None => s,
    }
}

/// Turns JSON-with-comments into JSON: `//` and `/* */` comments become spaces
/// (newlines kept, so error line numbers stay right) and trailing commas are removed.
pub fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    out.push(' ');
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                out.push_str("  ");
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    out.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                out.push_str("  ");
                i += 2;
            }
            (',', _) => {
                // Trailing comma: the next meaningful character closes the object/array.
                let mut j = i + 1;
                loop {
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'/') {
                        while j < chars.len() && chars[j] != '\n' {
                            j += 1;
                        }
                    } else if chars.get(j) == Some(&'/') && chars.get(j + 1) == Some(&'*') {
                        j += 2;
                        while j < chars.len() && !(chars[j] == '*' && chars.get(j + 1) == Some(&'/')) {
                            j += 1;
                        }
                        j += 2;
                    } else {
                        break;
                    }
                }
                out.push(if matches!(chars.get(j), Some('}' | ']')) { ' ' } else { ',' });
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<Config, ConfigError> {
        Config::parse(json, "test")
    }

    #[test]
    fn sample_configuration_is_valid() {
        let c = parse(include_str!("../watchdog.json")).unwrap();
        assert_eq!(c.devices.len(), 4);
        assert_eq!(c.devices[0].kind, "escpos-printer");
    }

    #[test]
    fn defaults_apply_when_omitted() {
        let c = parse(r#"{ "devices": [ { "name": "A", "host": "10.0.0.1" } ] }"#).unwrap();
        assert_eq!(c.language, "auto");
        assert_eq!((c.interval_seconds, c.fail_threshold, c.busy_threshold), (60, 3, 10));
        assert!(c.notifications.alert_window);
        assert_eq!(c.notifications.alert_font_size, 18);
        assert!(c.devices[0].ping && c.devices[0].enabled);
        assert_eq!(c.devices[0].kind, "generic");
    }

    #[test]
    fn comments_and_trailing_commas_are_allowed() {
        let c = parse("// leading\n{\n \"devices\": [\n  { \"name\": \"A, b\", \"host\": \"10.0.0.1\", }, /* c */\n ],\n}\n").unwrap();
        assert_eq!(c.devices[0].name, "A, b");
    }

    #[test]
    fn comment_markers_inside_strings_are_kept() {
        let c = parse(r#"{ "devices": [ { "name": "a//b /* c */", "host": "h", "http": { "url": "http://h/x" } } ] }"#).unwrap();
        assert_eq!(c.devices[0].name, "a//b /* c */");
        assert_eq!(c.devices[0].http.as_ref().unwrap().url, "http://h/x");
    }

    #[test]
    fn unknown_properties_are_rejected() {
        let e = parse(r#"{ "intervalSecond": 30, "devices": [ { "name": "A", "host": "10.0.0.1" } ] }"#).unwrap_err();
        assert!(e.errors[0].contains("intervalSecond"), "{}", e.errors[0]);
    }

    #[test]
    fn malformed_json_reports_the_line() {
        let e = parse("{\n  \"devices\": [\n    { \"name\": \"A\" \"host\": \"x\" }\n  ]\n}").unwrap_err();
        assert!(e.errors[0].contains("line 3"), "{}", e.errors[0]);
    }

    #[test]
    fn validation_collects_every_problem() {
        let e = parse(
            r#"{
              "intervalSeconds": 1, "failThreshold": 0,
              "notifications": { "ntfyUrl": "not a url", "alertFontSize": 200 },
              "devices": [
                { "name": "", "host": "" },
                { "name": "Dup", "host": "10.0.0.1", "type": "toaster" },
                { "name": "dup", "host": "10.0.0.2", "port": 0, "tcpPorts": [0], "mac": "zz" },
                { "name": "Web", "host": "10.0.0.3", "http": { "url": "ftp://x" } },
                { "name": "Nothing", "host": "10.0.0.4", "ping": false },
                { "name": "Bad host", "host": "a b" }
              ]
            }"#,
        )
        .unwrap_err();
        let all = e.errors.join("\n");
        for needle in [
            "intervalSeconds",
            "failThreshold",
            "ntfyUrl",
            "alertFontSize",
            "name is required",
            "host is required",
            "\"toaster\" is unknown",
            "used by another device",
            "port 0",
            "tcpPorts contains 0",
            "mac \"zz\"",
            "http.url",
            "needs at least one check",
            "\"a b\" is not a valid",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in:\n{all}");
        }
    }

    #[test]
    fn port_out_of_u16_range_is_a_json_error() {
        let e = parse(r#"{ "devices": [ { "name": "A", "host": "h", "port": 70000 } ] }"#).unwrap_err();
        assert!(e.errors[0].contains("70000"), "{}", e.errors[0]);
    }

    #[test]
    fn mac_formats() {
        for ok in ["00-11-22-33-44-55", "00:11:22:33:44:55", "0011.2233.4455", "001122334455", "aa-bb-cc-dd-ee-ff"] {
            assert!(parse_mac(ok).is_some(), "{ok}");
        }
        for bad in ["00-11-22-33-44", "00-11-22-33-44-55-66", "gg-11-22-33-44-55"] {
            assert!(parse_mac(bad).is_none(), "{bad}");
        }
        assert_eq!(format_mac(&parse_mac("aa:bb:cc:dd:ee:ff").unwrap()), "AA-BB-CC-DD-EE-FF");
    }

    #[test]
    fn device_type_aliases() {
        assert_eq!(DeviceKind::parse("ESCPOS"), Some(DeviceKind::EscPosPrinter));
        assert_eq!(DeviceKind::parse("ipp-printer"), Some(DeviceKind::Printer));
        assert_eq!(DeviceKind::parse("embedded"), Some(DeviceKind::Generic));
        assert_eq!(DeviceKind::parse("toaster"), None);
    }

    #[test]
    fn serialize_then_parse_round_trips() {
        let c = parse(
            r#"{ "language": "pt", "devices": [ { "name": "A", "type": "printer", "host": "10.0.0.9", "ipp": { "path": "/ipp" } } ] }"#,
        )
        .unwrap();
        let again = parse(&c.to_json()).unwrap();
        assert_eq!(again.language, "pt");
        assert_eq!(again.devices[0].ipp.as_ref().unwrap().path, "/ipp");
    }
}
