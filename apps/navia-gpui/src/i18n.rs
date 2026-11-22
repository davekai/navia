//! Translations. The catalog lives in `locales/*.json` at the repo root and is shared with the
//! Mac app. `tools/i18n.py` keeps it tidy. Keys are the English text.
//!
//! - `tr("Settings")` for fixed text.
//! - `trf("Added to {name}", &[("name", &value)])` for text with values.
//! - `trn("{count} song", "{count} songs", n)` for counted text, `trnf` when there are more values.
//! - `n_("...")` marks a key in a `const` or table. Show it later with `tr(value)`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// The languages Navia is translated into. The first is the source and the fallback.
pub const LANGUAGES: [&str; 4] = ["en", "fr", "es", "de"];
/// Each language's name as written in that language, for the Language setting. Never translated.
pub const LANGUAGE_NAMES: [(&str, &str); 4] = [("en", "English"), ("fr", "Français"), ("es", "Español"), ("de", "Deutsch")];

static ACTIVE: AtomicU8 = AtomicU8::new(0);

enum Text {
    Plain(&'static str),
    Plural { one: &'static str, many: Option<&'static str>, other: &'static str },
}

fn catalogs() -> &'static [HashMap<&'static str, Text>; 4] {
    static CATALOGS: OnceLock<[HashMap<&'static str, Text>; 4]> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        [
            parse(include_str!("../../../locales/en.json")),
            parse(include_str!("../../../locales/fr.json")),
            parse(include_str!("../../../locales/es.json")),
            parse(include_str!("../../../locales/de.json")),
        ]
    })
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// One catalog as parsed JSON, leaked so `tr` can return `'static` text. There are at most four
/// catalogs for the life of the app, so the leak is bounded.
fn parse(json: &str) -> HashMap<&'static str, Text> {
    let value: serde_json::Value = serde_json::from_str(json).expect("catalog is valid JSON");
    let map = value.as_object().expect("catalog is an object");
    let mut out = HashMap::with_capacity(map.len());
    for (key, value) in map {
        let text = match value {
            serde_json::Value::String(s) => Text::Plain(leak(s)),
            serde_json::Value::Object(forms) => {
                let form = |name: &str| forms.get(name).and_then(|v| v.as_str()).map(leak);
                Text::Plural { one: form("one").unwrap_or(leak(key)), many: form("many"), other: form("other").unwrap_or(leak(key)) }
            }
            _ => continue,
        };
        out.insert(leak(key), text);
    }
    out
}

/// The English text for a key. The context after `||` is never shown.
fn english(key: &'static str) -> &'static str {
    match key.split_once("||") {
        Some((text, _)) => text,
        None => key,
    }
}

fn lookup(key: &'static str) -> &'static str {
    let cats = catalogs();
    let active = ACTIVE.load(Ordering::Relaxed) as usize;
    for c in [active, 0] {
        if let Some(Text::Plain(s)) = cats[c].get(key) {
            return s;
        }
    }
    english(key)
}

/// Marks `key` as translatable in a `const` or table. Show it later with `tr(value)`.
pub const fn n_(key: &'static str) -> &'static str {
    key
}

/// The active language's text for `key`, falling back to English and never showing a raw key.
pub fn tr(key: &'static str) -> &'static str {
    lookup(key)
}

/// Same as `tr` for text known only at run time, like a value from a table of `n_` keys.
pub fn tr_dyn(key: &str) -> String {
    let cats = catalogs();
    let active = ACTIVE.load(Ordering::Relaxed) as usize;
    for c in [active, 0] {
        if let Some(Text::Plain(s)) = cats[c].get(key) {
            return (*s).to_string();
        }
    }
    key.split("||").next().unwrap_or(key).to_string()
}

fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else { break };
        out.push_str(&rest[..open]);
        let name = &rest[open + 1..open + close];
        match vars.iter().find(|(k, _)| *k == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[open..=open + close]),
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// `tr` for text with `{name}` placeholders. Values are inserted as they are.
pub fn trf(key: &'static str, vars: &[(&str, &str)]) -> String {
    fill(lookup(key), vars)
}

/// The CLDR plural category of `n` in the active language.
fn plural(active: usize, n: u64) -> &'static str {
    let m = n % 1_000_000;
    match LANGUAGES[active] {
        // French: 0 and 1 are singular. Exact millions take "de" ("1 000 000 de titres").
        "fr" => {
            if n < 2 {
                "one"
            } else if n != 0 && m == 0 {
                "many"
            } else {
                "other"
            }
        }
        "es" => {
            if n == 1 {
                "one"
            } else if n != 0 && m == 0 {
                "many"
            } else {
                "other"
            }
        }
        // English and German: only 1 is singular.
        _ => {
            if n == 1 {
                "one"
            } else {
                "other"
            }
        }
    }
}

fn plural_text(other: &'static str, one: &'static str, n: u64) -> &'static str {
    let cats = catalogs();
    let active = ACTIVE.load(Ordering::Relaxed) as usize;
    let category = plural(active, n);
    let pick = |c: usize| match cats[c].get(other) {
        Some(Text::Plural { one: o, many, other: rest }) => Some(match category {
            "one" => *o,
            "many" => many.unwrap_or(rest),
            _ => *rest,
        }),
        _ => None,
    };
    pick(active).or_else(|| pick(0)).unwrap_or(if n == 1 { english(one) } else { english(other) })
}

/// The active language's form of a counted text for `n`, with `{count}` still in it. For keys
/// known only at run time.
pub fn plural_dyn(one: &str, other: &str, n: u64) -> String {
    let cats = catalogs();
    let active = ACTIVE.load(Ordering::Relaxed) as usize;
    let category = plural(active, n);
    let pick = |c: usize| match cats[c].get(other) {
        Some(Text::Plural { one: o, many, other: rest }) => Some(match category {
            "one" => *o,
            "many" => many.unwrap_or(rest),
            _ => *rest,
        }),
        _ => None,
    };
    pick(active).or_else(|| pick(0)).map_or_else(|| if n == 1 { one.to_string() } else { other.to_string() }, str::to_string)
}

/// Counted text. `one` and `other` are the English forms, and the catalog key is `other`.
pub fn trn(one: &'static str, other: &'static str, n: usize) -> String {
    trnf(one, other, n, &[])
}

/// `trn` with more values than `{count}`.
pub fn trnf(one: &'static str, other: &'static str, n: usize, vars: &[(&str, &str)]) -> String {
    let count = fmt_int(n as i64);
    let mut all: Vec<(&str, &str)> = Vec::with_capacity(vars.len() + 1);
    all.push(("count", &count));
    all.extend_from_slice(vars);
    fill(plural_text(other, one, n as u64), &all)
}

/// The language's thousands separator and decimal mark. French uses a non-breaking space.
fn separators(lang: &str) -> (&'static str, char) {
    match lang {
        "fr" => ("\u{a0}", ','),
        "de" => (".", ','),
        "es" => (".", ','),
        _ => (",", '.'),
    }
}

/// A whole number with the language's grouping, like 12,345 or 12.345.
pub fn fmt_int(value: i64) -> String {
    let lang = LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize];
    let (sep, _) = separators(lang);
    let digits = value.unsigned_abs().to_string();
    // Spanish doesn't group four-digit numbers.
    let min = if lang == "es" { 5 } else { 4 };
    let mut out = if value < 0 { "-".to_string() } else { String::new() };
    if digits.len() < min {
        out.push_str(&digits);
        return out;
    }
    let lead = digits.len() % 3;
    if lead > 0 {
        out.push_str(&digits[..lead]);
    }
    for (i, chunk) in digits[lead..].as_bytes().chunks(3).enumerate() {
        if i > 0 || lead > 0 {
            out.push_str(sep);
        }
        out.push_str(std::str::from_utf8(chunk).expect("ASCII digits"));
    }
    out
}

/// A day with a short month, like "Mar 4, 2025", "4 mars 2025" or "4. März 2025". `unix` is in
/// seconds and the day is taken in UTC. Empty for 0.
pub fn fmt_date(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    let (y, m, d) = civil_from_days(unix.div_euclid(86_400));
    let lang = LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize];
    let months: [&str; 12] = match lang {
        "fr" => ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."],
        "es" => ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"],
        "de" => ["Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.", "Nov.", "Dez."],
        _ => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],
    };
    let month = months[(m - 1) as usize];
    match lang {
        "fr" | "es" => format!("{d} {month} {y}"),
        "de" => format!("{d}. {month} {y}"),
        _ => format!("{month} {d}, {y}"),
    }
}

/// A local date and 24-hour time with a short month, like "Apr 7, 2026, 23:05". `unix` is in seconds.
pub fn fmt_date_time(unix: i64) -> String {
    let local = unix + navia_core::library::taste::utc_offset(unix);
    let minutes = local.rem_euclid(86_400) / 60;
    format!("{}, {:02}:{:02}", fmt_date(local), minutes / 60, minutes % 60)
}

/// A release date with the full month, at whatever precision the server has. For example
/// "2011", "March 2011" or "March 4, 2011".
pub fn fmt_release(year: i64, month: Option<u32>, day: Option<u32>) -> String {
    let Some(m) = month.filter(|m| (1..=12).contains(m)) else { return year.to_string() };
    let lang = LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize];
    let month_day = fmt_day_month(days_from_civil(year, m, day.unwrap_or(1)));
    match day {
        Some(_) => match lang {
            "fr" | "de" => format!("{month_day} {year}"),
            "es" => format!("{month_day} de {year}"),
            _ => format!("{month_day}, {year}"),
        },
        None => {
            let name = full_month(lang, m);
            if lang == "es" { format!("{name} de {year}") } else { format!("{name} {year}") }
        }
    }
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// a day of the year with the full month, like "November 11" or "11 novembre". `days` counts from 1970-01-01.
pub fn fmt_day_month(days: i64) -> String {
    let (_, m, d) = civil_from_days(days);
    let lang = LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize];
    let month = full_month(lang, m);
    match lang {
        "fr" => format!("{d} {month}"),
        "es" => format!("{d} de {month}"),
        "de" => format!("{d}. {month}"),
        _ => format!("{month} {d}"),
    }
}

/// A month's full name in the language, 1 to 12.
fn full_month(lang: &str, m: u32) -> &'static str {
    let months: [&str; 12] = match lang {
        "fr" => ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"],
        "es" => ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"],
        "de" => ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"],
        _ => ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"],
    };
    months[(m.clamp(1, 12) - 1) as usize]
}

/// Year, month and day of a count of days since 1970-01-01 (Howard Hinnant's algorithm).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// A decimal number with the language's mark, like 12.5 or 12,5.
pub fn fmt_dec(value: f64, digits: usize) -> String {
    let lang = LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize];
    let (_, mark) = separators(lang);
    let text = format!("{value:.digits$}");
    if mark == '.' { text } else { text.replace('.', &mark.to_string()) }
}

/// Puts the language's decimal mark into already formatted text, like `2.5` for `2,5`.
pub fn decimal_text(text: &str) -> String {
    let (_, mark) = separators(LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize]);
    if mark == '.' { text.to_string() } else { text.replace('.', &mark.to_string()) }
}

/// The language code for a saved choice. `"system"` or nothing means the system's language.
pub fn resolve(choice: Option<&str>) -> &'static str {
    let wanted = match choice {
        Some(c) if c != "system" => Some(c.to_string()),
        _ => None,
    };
    let wanted = wanted.or_else(|| std::env::var("NAVIA_LANG").ok().filter(|l| !l.is_empty()));
    let candidates = match wanted {
        Some(w) => vec![w],
        None => system_languages(),
    };
    for candidate in &candidates {
        let code = candidate.split(['_', '-', '.', '@']).next().unwrap_or("").to_ascii_lowercase();
        if let Some(lang) = LANGUAGES.iter().find(|l| **l == code) {
            return lang;
        }
    }
    LANGUAGES[0]
}

/// Sets the interface language from a saved choice, `"system"` or a language code.
pub fn apply(choice: Option<&str>) {
    let lang = resolve(choice);
    let index = LANGUAGES.iter().position(|l| *l == lang).unwrap_or(0);
    ACTIVE.store(index as u8, Ordering::Relaxed);
}

pub fn current() -> &'static str {
    LANGUAGES[ACTIVE.load(Ordering::Relaxed) as usize]
}

#[cfg(windows)]
fn system_languages() -> Vec<String> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
    }
    let mut buf = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    if len <= 1 {
        return Vec::new();
    }
    vec![std::ffi::OsString::from_wide(&buf[..(len as usize - 1)]).to_string_lossy().into_owned()]
}

#[cfg(not(windows))]
fn system_languages() -> Vec<String> {
    // Same order as gettext. LANGUAGE is a colon-separated list.
    let mut list = Vec::new();
    if let Ok(languages) = std::env::var("LANGUAGE") {
        list.extend(languages.split(':').map(str::to_string).filter(|l| !l.is_empty()));
    }
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(value) = std::env::var(var) {
            if !value.is_empty() {
                list.push(value);
            }
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2025-02-26 and the leap day 2024-02-29.
        assert_eq!(civil_from_days(20_145), (2025, 2, 26));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    fn placeholders(text: &str) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else { break };
            let name = &rest[open + 1..open + close];
            if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                out.insert(name.to_string());
            }
            rest = &rest[open + close + 1..];
        }
        out
    }

    fn catalog(text: &str) -> serde_json::Map<String, Value> {
        serde_json::from_str::<Value>(text).unwrap().as_object().unwrap().clone()
    }

    #[test]
    fn every_language_matches_english() {
        let en = catalog(include_str!("../../../locales/en.json"));
        let others = [("fr", include_str!("../../../locales/fr.json")), ("es", include_str!("../../../locales/es.json")), ("de", include_str!("../../../locales/de.json"))];
        for (lang, text) in others {
            let cat = catalog(text);
            for key in en.keys() {
                assert!(cat.contains_key(key), "{lang} is missing {key:?}");
            }
            for key in cat.keys() {
                assert!(en.contains_key(key), "{lang} has an unknown key {key:?}");
            }
            for (key, value) in &cat {
                let english = &en[key];
                if let (Some(a), Some(b)) = (english.as_str(), value.as_str()) {
                    assert_eq!(placeholders(a), placeholders(b), "{lang}: placeholders differ in {key:?}");
                    continue;
                }
                let (a, b) = (english.as_object().unwrap_or_else(|| panic!("{key:?} isn't plural in en")), value.as_object().unwrap_or_else(|| panic!("{key:?} isn't plural in {lang}")));
                assert!(b.contains_key("one") && b.contains_key("other"), "{lang}: {key:?} needs one and other");
                let want = placeholders(a["other"].as_str().unwrap());
                for (form, text) in b {
                    let got = placeholders(text.as_str().unwrap());
                    if form == "one" {
                        let mut allowed = want.clone();
                        allowed.remove("count");
                        assert!(got.is_superset(&allowed) && got.is_subset(&want), "{lang}: {key:?} {form}");
                    } else {
                        assert_eq!(got, want, "{lang}: {key:?} {form}");
                    }
                }
            }
        }
    }

    #[test]
    fn plural_rules() {
        assert_eq!(plural(1, 0), "one");
        assert_eq!(plural(1, 1), "one");
        assert_eq!(plural(1, 2), "other");
        assert_eq!(plural(1, 1_000_000), "many");
        assert_eq!(plural(2, 0), "other");
        assert_eq!(plural(2, 1), "one");
        assert_eq!(plural(2, 3_000_000), "many");
        assert_eq!(plural(0, 1), "one");
        assert_eq!(plural(3, 0), "other");
    }

    #[test]
    fn numbers_group_by_language() {
        ACTIVE.store(0, Ordering::Relaxed);
        assert_eq!(fmt_int(1234), "1,234");
        assert_eq!(fmt_int(-999), "-999");
        assert_eq!(fmt_int(12_345_678), "12,345,678");
        assert_eq!(fmt_dec(12.5, 1), "12.5");
        ACTIVE.store(1, Ordering::Relaxed);
        assert_eq!(fmt_int(1234), "1\u{a0}234");
        assert_eq!(fmt_dec(12.5, 1), "12,5");
        ACTIVE.store(2, Ordering::Relaxed);
        assert_eq!(fmt_int(1234), "1234");
        assert_eq!(fmt_int(12345), "12.345");
        ACTIVE.store(3, Ordering::Relaxed);
        assert_eq!(fmt_int(1234), "1.234");
        ACTIVE.store(0, Ordering::Relaxed);
    }

    #[test]
    fn missing_keys_show_english_not_a_raw_key() {
        assert_eq!(tr("Not in any catalog||context"), "Not in any catalog");
        assert_eq!(tr_dyn("Also missing||context"), "Also missing");
    }

    #[test]
    fn placeholders_fill_once_and_keep_unknown_text() {
        assert_eq!(fill("Added to {name}", &[("name", "{x}")]), "Added to {x}");
        assert_eq!(fill("a {x} b", &[]), "a {x} b");
    }
}
