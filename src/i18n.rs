//! Localized strings, ported from the WPFLocalizeExtension `.resx` resources.
//!
//! Lookup falls back to English and finally to `Key: <name>`, matching the
//! behaviour of the old `LocalizedStrings` indexer.

use once_cell::sync::Lazy;
use parking_lot::RwLock;
use std::collections::HashMap;

macro_rules! bundled {
    ($($lang:literal),* $(,)?) => {
        &[$(($lang, include_str!(concat!("../assets/i18n/", $lang, ".json")))),*]
    };
}

/// Every translation shipped with the launcher.
const BUNDLES: &[(&str, &str)] =
    bundled!["en", "ar", "de", "es", "fr", "hr", "pt", "ru", "tr", "uk", "zh"];

type Catalog = HashMap<String, String>;

static CATALOGS: Lazy<HashMap<&'static str, Catalog>> = Lazy::new(|| {
    BUNDLES
        .iter()
        .map(|(lang, json)| {
            let map: Catalog = serde_json::from_str(json).unwrap_or_default();
            (*lang, map)
        })
        .collect()
});

static CURRENT: Lazy<RwLock<&'static str>> = Lazy::new(|| RwLock::new("en"));

/// Two-letter code of the OS UI language, or "en" when we have no translation.
pub fn system_language() -> &'static str {
    let locale = sys_locale::get_locale().unwrap_or_default().to_ascii_lowercase();
    let two = locale.split(['-', '_']).next().unwrap_or("en").to_owned();
    BUNDLES.iter().map(|(l, _)| *l).find(|l| *l == two).unwrap_or("en")
}

pub fn set_language(lang: &str) {
    let resolved =
        BUNDLES.iter().map(|(l, _)| *l).find(|l| l.eq_ignore_ascii_case(lang)).unwrap_or("en");
    *CURRENT.write() = resolved;
}

pub fn current_language() -> &'static str {
    *CURRENT.read()
}

/// The active language is process-global, so tests that change it must not run
/// concurrently. Any test whose expectations depend on the language takes this.
#[cfg(test)]
pub(crate) static TEST_LANG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the language lock, ignoring poisoning from an unrelated failed test.
#[cfg(test)]
pub(crate) fn lock_language() -> std::sync::MutexGuard<'static, ()> {
    TEST_LANG_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Look up `key` in the active language, then English.
pub fn tr(key: &str) -> String {
    let lang = current_language();
    if let Some(v) = CATALOGS.get(lang).and_then(|c| c.get(key)) {
        return v.clone();
    }
    if let Some(v) = CATALOGS.get("en").and_then(|c| c.get(key)) {
        return v.clone();
    }
    format!("Key: {key}")
}

/// `tr` with .NET-style `{0}`, `{1}`, ... placeholders substituted in order.
pub fn trf(key: &str, args: &[&str]) -> String {
    let mut s = tr(key);
    for (i, arg) in args.iter().enumerate() {
        s = s.replace(&format!("{{{i}}}"), arg);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_english_then_key() {
        let _guard = lock_language();
        set_language("ru");
        assert!(!tr("Launch").starts_with("Key:"));
        assert_eq!(tr("NoSuchKeyAnywhere"), "Key: NoSuchKeyAnywhere");
        set_language("en");
    }

    #[test]
    fn substitutes_positional_placeholders() {
        let _guard = lock_language();
        set_language("en");
        assert_eq!(trf("NotInstalled", &["ROTR"]), "ROTR was selected but not installed -  launch aborted!");
    }
}
