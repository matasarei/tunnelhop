//! Which language the app speaks, and its text in that language. Windows' display language
//! decides, and English stands in for a language the app does not have. The texts live in
//! `app/locales/<tag>.json`, one file per language, which the window reads too: this side
//! picks the language for both, so the tray and the window never disagree.
//!
//! Only the app's own text is here. What the service says (why a connect failed, the lines of
//! an import error) arrives in English and is shown as it is.

use std::collections::HashMap;
use std::sync::OnceLock;

pub const ENGLISH: &str = "en";

/// Every language the app has, by the name of its file.
pub const LANGUAGES: [&str; 1] = [ENGLISH];

const CATALOGUES: [(&str, &str); 1] = [(ENGLISH, include_str!("../../locales/en.json"))];

/// Set to one of [`LANGUAGES`] to see the app in it whatever Windows speaks: for checking a
/// translation.
const OVERRIDE: &str = "TUNNELHOP_LANG";

/// The app's language for one Windows language tag such as `uk-UA`, if it has one.
fn matching(tag: &str) -> Option<&'static str> {
    let tag = tag.trim().to_ascii_lowercase();
    let mut parts = tag.split(['-', '_']);
    let wanted = match parts.next()? {
        // Readers of Traditional Chinese are better served by English than by Simplified.
        "zh" => {
            let rest: Vec<&str> = parts.collect();
            let traditional = ["hant", "tw", "hk", "mo"];
            if rest.iter().any(|part| traditional.contains(part)) {
                return None;
            }
            "zh-hans"
        }
        // One Portuguese for every region.
        "pt" => "pt-br",
        primary => primary,
    };
    LANGUAGES
        .into_iter()
        .find(|language| language.eq_ignore_ascii_case(wanted))
}

/// The app's language for a user who prefers `preferred`, most wanted first: the first one
/// the app has, or English.
pub fn pick<S: AsRef<str>>(preferred: &[S]) -> &'static str {
    preferred
        .iter()
        .find_map(|tag| matching(tag.as_ref()))
        .unwrap_or(ENGLISH)
}

/// The display languages the signed-in user chose in Windows, most wanted first.
#[cfg(windows)]
fn system() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let mut count = 0u32;
    let mut length = 0u32;
    // SAFETY: with no buffer and a zero length the call only reports the length it needs.
    let sized = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            std::ptr::null_mut(),
            &mut length,
        )
    };
    if sized == 0 || length == 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: `buffer` is `length` UTF-16 units long, which is what the call is told.
    let filled = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &mut count,
            buffer.as_mut_ptr(),
            &mut length,
        )
    };
    if filled == 0 {
        return Vec::new();
    }
    // The names come back one after another, each ended by a zero.
    buffer
        .split(|&unit| unit == 0)
        .filter(|name| !name.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(not(windows))]
fn system() -> Vec<String> {
    Vec::new()
}

/// The language this run of the app speaks. Decided once: a change in Windows takes effect
/// when the app next starts, as it does for other apps.
pub fn current() -> &'static str {
    static CURRENT: OnceLock<&'static str> = OnceLock::new();
    CURRENT.get_or_init(|| {
        // Tests read English whatever the PC they run on speaks.
        if cfg!(test) {
            return ENGLISH;
        }
        match std::env::var(OVERRIDE) {
            Ok(tag) => pick(&[tag]),
            Err(_) => pick(&system()),
        }
    })
}

fn catalogues() -> &'static HashMap<&'static str, HashMap<String, String>> {
    static PARSED: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        CATALOGUES
            .into_iter()
            .map(|(language, json)| {
                let texts = serde_json::from_str(json)
                    .unwrap_or_else(|e| panic!("locales/{language}.json is not valid: {e}"));
                (language, texts)
            })
            .collect()
    })
}

/// `template` with each `{name}` replaced by its value.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    // One pass, so a value that itself reads `{name}` is never filled in again.
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let known = after.find('}').and_then(|close| {
            let value = values.iter().find(|(name, _)| *name == &after[..close])?;
            Some((close, value.1))
        });
        match known {
            Some((close, value)) => {
                text.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                text.push('{');
                rest = after;
            }
        }
    }
    text.push_str(rest);
    text
}

/// The text for `key` in `language`, with `values` put in its `{placeholders}`. English when
/// the language lacks the key, and the key itself when English does too.
pub fn text_in(language: &str, key: &str, values: &[(&str, &str)]) -> String {
    let all = catalogues();
    let template = [language, ENGLISH]
        .into_iter()
        .find_map(|language| all.get(language)?.get(key))
        .map_or(key, String::as_str);
    fill(template, values)
}

/// The text for `key` in the app's language.
pub fn text(key: &str, values: &[(&str, &str)]) -> String {
    text_in(current(), key, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_fills_placeholders_and_falls_back() {
        assert_eq!(
            text_in(
                ENGLISH,
                "tray.status.connected_ip",
                &[("profile", "ua-kyiv-01"), ("ip", "203.0.113.7")]
            ),
            "Connected: ua-kyiv-01 (203.0.113.7)"
        );
        // A language the app does not have reads as English; a key nobody has, as itself.
        assert_eq!(text_in("xx", "tray.exit", &[]), "Exit");
        assert_eq!(text_in(ENGLISH, "no.such.key", &[]), "no.such.key");
        // A value is put in as it is, even when it looks like a placeholder.
        assert_eq!(
            text_in(
                ENGLISH,
                "tray.status.connected_ip",
                &[("profile", "{ip}"), ("ip", "203.0.113.7")]
            ),
            "Connected: {ip} (203.0.113.7)"
        );
    }

    #[test]
    fn tests_read_english() {
        assert_eq!(current(), ENGLISH);
        assert_eq!(text("tray.status.blocked", &[]), "Blocked");
    }
}
