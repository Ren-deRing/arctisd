//! Built-in translations for user-facing text (tray, notifications, CLI output).
//! Logs stay in English.

use std::collections::HashMap;
use std::sync::OnceLock;

const EN: &str = include_str!("../locales/en.txt");
const KO: &str = include_str!("../locales/ko.txt");
const SUPPORTED: [(&str, &str); 2] = [("en", EN), ("ko", KO)];

/// `key = text` lines; blank lines and `#` comments are skipped.
fn parse(src: &'static str) -> HashMap<&'static str, &'static str> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim(), v.trim())))
        .collect()
}

/// `ko_KR.UTF-8` → `ko`, if we ship it.
fn supported(locale: &str) -> Option<&'static str> {
    let code = locale.split(['_', '.', '@']).next().unwrap_or("");
    SUPPORTED.iter().map(|(c, _)| *c).find(|c| *c == code)
}

/// gettext-like choice: LANGUAGE list first, then the first set of LC_ALL,
/// LC_MESSAGES, LANG. Unsupported or `C` → English.
pub fn detect(get: impl Fn(&str) -> Option<String>) -> &'static str {
    let set = |k: &str| get(k).filter(|v| !v.is_empty());
    if let Some(list) = set("LANGUAGE")
        && let Some(c) = list.split(':').find_map(supported)
    {
        return c;
    }
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| set(k))
        .and_then(|l| supported(&l))
        .unwrap_or("en")
}

pub struct Tr {
    lang: HashMap<&'static str, &'static str>,
    en: HashMap<&'static str, &'static str>,
}

impl Tr {
    fn from_sources(lang: &'static str, en: &'static str) -> Self {
        Self { lang: parse(lang), en: parse(en) }
    }

    pub fn for_lang(code: &str) -> Self {
        let src = SUPPORTED.iter().find(|(c, _)| *c == code).map_or(EN, |(_, s)| *s);
        Self::from_sources(src, EN)
    }

    /// Translated text; falls back to English, then to the key itself.
    pub fn t(&self, key: &str) -> String {
        self.lang.get(key).or_else(|| self.en.get(key)).copied().unwrap_or(key).to_string()
    }

    /// `t` with `{name}` placeholders filled in.
    pub fn f(&self, key: &str, args: &[(&str, &str)]) -> String {
        args.iter().fold(self.t(key), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
    }
}

/// Translations for the user's locale, chosen once.
pub fn tr() -> &'static Tr {
    static TR: OnceLock<Tr> = OnceLock::new();
    TR.get_or_init(|| Tr::for_lang(detect(|k| std::env::var(k).ok())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn detect_order_and_fallback() {
        assert_eq!(detect(env(&[("LANG", "ko_KR.UTF-8")])), "ko");
        assert_eq!(detect(env(&[("LANG", "ko_KR.UTF-8"), ("LC_MESSAGES", "en_US.UTF-8")])), "en");
        assert_eq!(detect(env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "ko_KR.UTF-8")])), "ko");
        assert_eq!(detect(env(&[("LC_ALL", "en_US.UTF-8"), ("LANGUAGE", "ko:en")])), "ko");
        assert_eq!(detect(env(&[("LANGUAGE", "fr:ko"), ("LANG", "fr_FR.UTF-8")])), "ko");
        assert_eq!(detect(env(&[("LANG", "fr_FR.UTF-8")])), "en");
        assert_eq!(detect(env(&[("LANG", "C.UTF-8")])), "en");
        assert_eq!(detect(env(&[])), "en");
        assert_eq!(detect(env(&[("LANGUAGE", ""), ("LANG", "ko_KR")])), "ko");
    }

    #[test]
    fn lookup_and_placeholders() {
        let ko = Tr::for_lang("ko");
        let en = Tr::for_lang("en");
        assert_eq!(ko.t("menu.quit"), "종료");
        assert_eq!(en.t("menu.quit"), "Quit");
        assert_eq!(ko.f("status.battery", &[("pct", "85")]), "배터리 85%");
        assert_eq!(en.f("status.battery", &[("pct", "85")]), "Battery 85%");
        assert_eq!(en.t("no.such.key"), "no.such.key");
    }

    #[test]
    fn missing_key_falls_back_to_english() {
        let t = Tr::from_sources("x = only here\n", "a = A\n");
        assert_eq!(t.t("a"), "A");
        assert_eq!(t.t("x"), "only here");
    }

    #[test]
    fn locale_files_have_same_keys_and_placeholders() {
        let en = parse(EN);
        let ko = parse(KO);
        let keys = |m: &HashMap<&'static str, &'static str>| -> Vec<&'static str> {
            let mut k: Vec<&str> = m.keys().copied().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&en), keys(&ko));
        let holes = |s: &str| {
            let mut v: Vec<String> =
                s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string())).collect();
            v.sort();
            v
        };
        for (k, v) in &en {
            assert_eq!(holes(v), holes(ko[k]), "placeholders differ for {k}");
        }
    }
}
