//! Localized text.
//!
//! Messages are a key plus named arguments ([`Msg`]) and are turned into text
//! only when shown, by a [`Localizer`]. Named placeholders (`{host}`, `{port}`)
//! let translators reorder words freely. Locale files are JSON documents
//! embedded at compile time; a directory of extra or overriding files can be
//! supplied at runtime.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, LazyLock};

use base64::Engine;
use regex::Regex;
use serde::Deserialize;

/// The language used when nothing else matches.
pub const DEFAULT_LANGUAGE: &str = "en";

/// Locale files compiled into the binary.
const BUILT_IN: &[(&str, &str)] = &[
    ("ar", include_str!("../locales/ar.json")),
    ("en", include_str!("../locales/en.json")),
    ("es", include_str!("../locales/es.json")),
    ("hi", include_str!("../locales/hi.json")),
    ("pt", include_str!("../locales/pt.json")),
    ("zh", include_str!("../locales/zh.json")),
];

static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{([A-Za-z][A-Za-z0-9_]*)\}").unwrap());

/// An argument of a [`Msg`].
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Text(String),
    Int(i64),
    Msg(Msg),
    /// Joined with the locale's list separator.
    List(Vec<Msg>),
}

impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::Text(v.to_owned())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::Text(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::Text(v.clone())
    }
}
impl From<Msg> for Arg {
    fn from(v: Msg) -> Self {
        Arg::Msg(v)
    }
}
impl From<Vec<Msg>> for Arg {
    fn from(v: Vec<Msg>) -> Self {
        Arg::List(v)
    }
}
macro_rules! int_arg {
    ($($t:ty),*) => { $(impl From<$t> for Arg { fn from(v: $t) -> Self { Arg::Int(v as i64) } })* };
}
int_arg!(i32, i64, u16, u32, u64, usize);

/// A localizable message: a key such as `msg.unreachable` plus named arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub key: &'static str,
    pub args: Vec<(&'static str, Arg)>,
}

impl Msg {
    pub const fn key(key: &'static str) -> Self {
        Msg { key, args: Vec::new() }
    }

    /// Text shown as-is in every language (e.g. an unknown IPP keyword).
    pub fn raw(text: impl Into<String>) -> Self {
        Msg { key: "raw", args: vec![("text", Arg::Text(text.into()))] }
    }
}

/// Builds a [`Msg`]: `msg!("msg.unreachable", host = "10.0.0.1")`.
#[macro_export]
macro_rules! msg {
    ($key:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::i18n::Msg { key: $key, args: vec![$((stringify!($name), $crate::i18n::Arg::from($value))),*] }
    };
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocaleJson {
    language: Option<String>,
    direction: Option<String>,
    font_family: Option<String>,
    #[serde(default)]
    strings: HashMap<String, String>,
}

/// A parsed locale file.
#[derive(Clone, Debug)]
pub struct Locale {
    pub code: String,
    pub display_name: Option<String>,
    pub right_to_left: Option<bool>,
    pub font_family: Option<String>,
    pub strings: HashMap<String, String>,
}

impl Locale {
    pub fn parse(code: &str, json: &str) -> Result<Locale, serde_json::Error> {
        let f: LocaleJson = serde_json::from_str(json)?;
        Ok(Locale {
            code: code.to_owned(),
            display_name: f.language,
            right_to_left: f.direction.map(|d| d.eq_ignore_ascii_case("rtl")),
            font_family: f.font_family,
            strings: f.strings,
        })
    }

    /// Built-in locale by code (`en`, `pt`, ...).
    pub fn built_in(code: &str) -> Option<Locale> {
        BUILT_IN.iter().find(|(c, _)| *c == code).map(|(c, json)| Locale::parse(c, json).expect("built-in locale files are valid JSON"))
    }

    /// Codes of all built-in locales.
    pub fn built_in_codes() -> impl Iterator<Item = &'static str> {
        BUILT_IN.iter().map(|(c, _)| *c)
    }

    /// Overlays `custom` on `self`.
    fn merged(mut self, custom: Option<&Locale>) -> Locale {
        if let Some(c) = custom {
            self.strings.extend(c.strings.iter().map(|(k, v)| (k.clone(), v.clone())));
            self.display_name = c.display_name.clone().or(self.display_name);
            self.right_to_left = c.right_to_left.or(self.right_to_left);
            self.font_family = c.font_family.clone().or(self.font_family);
        }
        self
    }
}

/// Looks up and formats text. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Localizer {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    language: String,
    display_name: String,
    right_to_left: bool,
    font_family: String,
    strings: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

impl Localizer {
    /// Creates a localizer for `language` (`auto`/`None` = operating system language,
    /// or a tag like `pt-BR`), optionally merging locale files from `override_dir`.
    pub fn new(language: Option<&str>, override_dir: Option<&Path>) -> Localizer {
        let overrides = load_overrides(override_dir);
        let english = Locale::built_in(DEFAULT_LANGUAGE).unwrap().merged(overrides.get(DEFAULT_LANGUAGE));

        let chosen = candidates(language)
            .into_iter()
            .find_map(|code| match (Locale::built_in(&code), overrides.get(&code)) {
                (Some(b), custom) => Some(b.merged(custom)),
                (None, Some(custom)) => Some(custom.clone()),
                (None, None) => None,
            })
            .unwrap_or_else(|| english.clone());

        Localizer {
            inner: Arc::new(Inner {
                display_name: chosen.display_name.clone().unwrap_or_else(|| chosen.code.clone()),
                right_to_left: chosen.right_to_left.unwrap_or(false),
                font_family: chosen.font_family.clone().unwrap_or_else(|| "Segoe UI".to_owned()),
                language: chosen.code,
                strings: chosen.strings,
                fallback: english.strings,
            }),
        }
    }

    /// Code of the locale in use, e.g. `pt`.
    pub fn language(&self) -> &str {
        &self.inner.language
    }

    pub fn display_name(&self) -> &str {
        &self.inner.display_name
    }

    pub fn is_right_to_left(&self) -> bool {
        self.inner.right_to_left
    }

    /// Preferred UI font for the script.
    pub fn font_family(&self) -> &str {
        &self.inner.font_family
    }

    /// Raw text for a key (placeholders not substituted). Unknown keys return the key.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.inner.strings.get(key).or_else(|| self.inner.fallback.get(key)).map(String::as_str).unwrap_or(key)
    }

    /// Localizes a message.
    pub fn format(&self, msg: &Msg) -> String {
        let template = self.get(msg.key);
        if msg.args.is_empty() {
            return template.to_owned();
        }
        PLACEHOLDER
            .replace_all(template, |c: &regex::Captures| {
                let name = &c[1];
                match msg.args.iter().find(|(n, _)| *n == name) {
                    Some((_, arg)) => self.format_arg(arg),
                    None => c[0].to_owned(),
                }
            })
            .into_owned()
    }

    fn format_arg(&self, arg: &Arg) -> String {
        match arg {
            Arg::Text(s) => s.clone(),
            Arg::Int(i) => i.to_string(),
            Arg::Msg(m) => self.format(m),
            Arg::List(items) => items.iter().map(|m| self.format(m)).collect::<Vec<_>>().join(self.get("list.separator")),
        }
    }
}

/// Placeholder names used in a template.
pub fn placeholders(template: &str) -> BTreeSet<String> {
    PLACEHOLDER.captures_iter(template).map(|c| c[1].to_owned()).collect()
}

/// Codes to try, most specific first: `pt-BR` → `pt-br`, `pt`, then `en`.
pub(crate) fn candidates(language: Option<&str>) -> Vec<String> {
    let tag = match language.map(str::trim) {
        None | Some("") => sys_locale::get_locale().unwrap_or_default(),
        Some(l) if l.eq_ignore_ascii_case("auto") => sys_locale::get_locale().unwrap_or_default(),
        Some(l) => l.to_owned(),
    };
    // POSIX locales look like pt_BR.UTF-8.
    let mut tag = tag.split(['.', '@']).next().unwrap_or_default().replace('_', "-").to_ascii_lowercase();
    let mut out = Vec::new();
    while !tag.is_empty() {
        out.push(tag.clone());
        match tag.rfind('-') {
            Some(i) => tag.truncate(i),
            None => break,
        }
    }
    out.push(DEFAULT_LANGUAGE.to_owned());
    out
}

fn load_overrides(dir: Option<&Path>) -> HashMap<String, Locale> {
    let mut map = HashMap::new();
    let Some(entries) = dir.and_then(|d| std::fs::read_dir(d).ok()) else {
        return map;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let code = path.file_stem().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
            if let Ok(locale) = std::fs::read_to_string(&path).map_err(|_| ()).and_then(|t| Locale::parse(&code, &t).map_err(|_| ())) {
                map.insert(code, locale);
            }
        }
    }
    map
}

/// Encodes text for an HTTP header: RFC 2047 when not plain ASCII.
pub fn header_value(text: &str) -> String {
    if text.bytes().all(|b| (0x20..0x7F).contains(&b)) {
        text.to_owned()
    } else {
        format!("=?UTF-8?B?{}?=", base64::engine::general_purpose::STANDARD.encode(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_locale_has_exactly_the_english_keys_and_placeholders() {
        let en = Locale::built_in("en").unwrap();
        for code in Locale::built_in_codes() {
            let l = Locale::built_in(code).unwrap();
            let missing: Vec<_> = en.strings.keys().filter(|k| !l.strings.contains_key(*k)).collect();
            let extra: Vec<_> = l.strings.keys().filter(|k| !en.strings.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{code} is missing {missing:?}");
            assert!(extra.is_empty(), "{code} has unknown keys {extra:?}");
            for (key, text) in &en.strings {
                assert_eq!(placeholders(text), placeholders(&l.strings[key]), "{code}:{key}");
            }
            assert!(l.display_name.is_some() && l.right_to_left.is_some() && l.font_family.is_some(), "{code}: metadata");
        }
    }

    #[test]
    fn all_requested_languages_are_built_in() {
        let codes: Vec<_> = Locale::built_in_codes().collect();
        assert_eq!(codes, ["ar", "en", "es", "hi", "pt", "zh"]);
    }

    #[test]
    fn language_tags_fall_back_to_the_best_locale() {
        for (tag, expected) in [
            ("pt-BR", "pt"),
            ("pt_BR.UTF-8", "pt"),
            ("es-MX", "es"),
            ("zh-Hans-CN", "zh"),
            ("hi-IN", "hi"),
            ("ar-SA", "ar"),
            ("en-US", "en"),
            ("fr-FR", "en"),
            ("not-a-language", "en"),
        ] {
            assert_eq!(Localizer::new(Some(tag), None).language(), expected, "{tag}");
        }
    }

    #[test]
    fn arabic_is_right_to_left() {
        assert!(Localizer::new(Some("ar"), None).is_right_to_left());
        assert!(!Localizer::new(Some("zh"), None).is_right_to_left());
    }

    #[test]
    fn formats_named_arguments_nested_messages_and_lists() {
        let m = msg!("msg.deviceError", reasons = vec![Msg::key("reason.coverOpen"), Msg::key("reason.paperEnd")]);
        assert_eq!(Localizer::new(Some("en"), None).format(&m), "The device reports: cover open, out of paper.");
        assert_eq!(Localizer::new(Some("zh"), None).format(&m), "设备报告：机盖打开、缺纸。");
        assert_eq!(
            Localizer::new(Some("pt"), None).format(&msg!("app.configValid", count = 1234)),
            "Configuração OK: 1234 dispositivo(s)."
        );
    }

    #[test]
    fn unknown_keys_and_missing_arguments_are_visible_but_harmless() {
        let l = Localizer::new(Some("en"), None);
        assert_eq!(l.get("no.such.key"), "no.such.key");
        assert_eq!(l.format(&Msg::key("alert.title.recovered")), "{device} is working again");
    }

    #[test]
    fn override_directory_adds_languages_and_overrides_strings() {
        let dir = std::env::temp_dir().join(format!("ndw-locales-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pt.json"), r#"{ "strings": { "status.ok": "Tudo certo" } }"#).unwrap();
        std::fs::write(dir.join("de.json"), r#"{ "language": "Deutsch", "strings": { "status.ok": "In Ordnung" } }"#).unwrap();

        let pt = Localizer::new(Some("pt"), Some(&dir));
        assert_eq!(pt.get("status.ok"), "Tudo certo");
        assert_eq!(pt.get("failure.unreachable"), "Sem rede");
        let de = Localizer::new(Some("de-DE"), Some(&dir));
        assert_eq!(de.language(), "de");
        assert_eq!(de.get("status.ok"), "In Ordnung");
        assert_eq!(de.get("failure.unreachable"), "No network");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn header_values_are_rfc2047_encoded_only_when_needed() {
        assert_eq!(header_value("Kitchen: No network"), "Kitchen: No network");
        let encoded = header_value("Cozinha: Não responde");
        let payload = encoded.strip_prefix("=?UTF-8?B?").unwrap().strip_suffix("?=").unwrap();
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(payload).unwrap(), "Cozinha: Não responde".as_bytes());
    }
}
