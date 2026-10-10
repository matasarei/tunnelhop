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
pub const LANGUAGES: [&str; 10] = [
    ENGLISH, "uk", "es", "fr", "de", "pt-BR", "it", "pl", "zh-Hans", "ja",
];

const CATALOGUES: [(&str, &str); 10] = [
    (ENGLISH, include_str!("../../locales/en.json")),
    ("uk", include_str!("../../locales/uk.json")),
    ("es", include_str!("../../locales/es.json")),
    ("fr", include_str!("../../locales/fr.json")),
    ("de", include_str!("../../locales/de.json")),
    ("pt-BR", include_str!("../../locales/pt-BR.json")),
    ("it", include_str!("../../locales/it.json")),
    ("pl", include_str!("../../locales/pl.json")),
    ("zh-Hans", include_str!("../../locales/zh-Hans.json")),
    ("ja", include_str!("../../locales/ja.json")),
];

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
    use std::collections::BTreeSet;

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
    fn the_language_follows_what_windows_prefers() {
        for (preferred, language) in [
            (vec!["uk-UA"], "uk"),
            (vec!["uk"], "uk"),
            (vec!["UK_ua"], "uk"),
            (vec!["de-AT"], "de"),
            (vec!["es-419"], "es"),
            (vec!["fr-CA"], "fr"),
            (vec!["it-IT"], "it"),
            (vec!["pl-PL"], "pl"),
            (vec!["ja-JP"], "ja"),
            (vec!["en-GB"], "en"),
            // One Portuguese for every region.
            (vec!["pt-BR"], "pt-BR"),
            (vec!["pt-PT"], "pt-BR"),
            // Simplified Chinese however it is named; Traditional reads English.
            (vec!["zh-CN"], "zh-Hans"),
            (vec!["zh-Hans-SG"], "zh-Hans"),
            (vec!["zh"], "zh-Hans"),
            (vec!["zh-TW"], "en"),
            (vec!["zh-Hant-HK"], "en"),
            // The first language the app has wins, not the first one listed.
            (vec!["ko-KR", "uk-UA", "de-DE"], "uk"),
            (vec!["zh-TW", "ja-JP"], "ja"),
            // Nothing the app has, nothing at all, or nonsense: English.
            (vec!["ko-KR", "ar-SA"], "en"),
            (vec![], "en"),
            (vec!["", "-", "ukr"], "en"),
        ] {
            assert_eq!(pick(&preferred), language, "{preferred:?}");
        }
    }

    /// `key` without the ending that names a plural form, and that ending.
    fn plural(key: &str) -> (&str, Option<&str>) {
        for form in ["_one", "_few", "_many", "_other"] {
            if let Some(base) = key.strip_suffix(form) {
                return (base, Some(&form[1..]));
            }
        }
        (key, None)
    }

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}'))
            .map(|(name, _)| name)
            .collect()
    }

    #[test]
    fn every_language_has_every_text() {
        let all = catalogues();
        assert_eq!(all.len(), LANGUAGES.len());
        let english = &all[ENGLISH];
        let bases = |texts: &HashMap<String, String>| -> BTreeSet<String> {
            texts.keys().map(|key| plural(key).0.to_string()).collect()
        };
        for language in LANGUAGES {
            let texts = &all[language];
            assert_eq!(bases(texts), bases(english), "the keys of {language}");
            // The forms a number can ask for in each language (Unicode's plural rules).
            let forms: &[&str] = match language {
                "uk" | "pl" => &["one", "few", "many", "other"],
                "zh-Hans" | "ja" => &["other"],
                _ => &["one", "other"],
            };
            for (key, text) in texts {
                assert!(!text.trim().is_empty(), "{language}: {key} is empty");
                let (base, form) = plural(key);
                // What the text may fill in is what English fills in; a plural form may
                // leave the number out ("Add a server").
                let source = match form {
                    Some(_) => &english[&format!("{base}_other")],
                    None => &english[key],
                };
                let mut wanted = placeholders(source);
                let mut found = placeholders(text);
                if form.is_some() {
                    wanted.insert("n");
                    found.insert("n");
                }
                assert_eq!(found, wanted, "{language}: the placeholders of {key}");
                if form.is_some() {
                    for form in forms {
                        let needed = format!("{base}_{form}");
                        assert!(
                            texts.contains_key(&needed),
                            "{language}: {needed} is missing"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_translation_is_used() {
        assert_eq!(text_in("uk", "tray.status.blocked", &[]), "Заблоковано");
        assert_eq!(
            text_in("ja", "tray.status.connected", &[("profile", "ua-kyiv-01")]),
            "接続済み: ua-kyiv-01"
        );
    }

    /// Every Windows has a display language. An empty list would mean the call failed, and
    /// the app would then speak English to everyone without a word in any log.
    #[cfg(windows)]
    #[test]
    fn windows_names_its_display_languages() {
        let languages = system();
        assert!(!languages.is_empty());
        for language in &languages {
            assert!(
                language.len() >= 2 && language.is_ascii(),
                "not a language tag: {language:?}"
            );
        }
    }

    #[test]
    fn tests_read_english() {
        assert_eq!(current(), ENGLISH);
        assert_eq!(text("tray.status.blocked", &[]), "Blocked");
    }
}
