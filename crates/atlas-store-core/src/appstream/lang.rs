//! Which languages to show. The preference list comes from the locale
//! variables, and each `xml:lang` value in the AppStream data is ranked
//! against it.

use std::env;

/// The most languages kept, and the longest one.
const MAX_LANGS: usize = 32;
const MAX_LANG_LEN: usize = 64;

fn add(out: &mut Vec<String>, lang: &str) {
    if !lang.is_empty()
        && lang.len() <= MAX_LANG_LEN
        && out.len() < MAX_LANGS
        && !out.iter().any(|l| l == lang)
    {
        out.push(lang.to_string());
    }
}

/// Turns one locale (`pt_BR.UTF-8@euro`) into its languages, most specific
/// first (`pt_BR`, `pt`). `C`, `POSIX` and empty values give nothing.
fn expand(locale: &str, out: &mut Vec<String>) {
    let base = locale.split(['@', '.']).next().unwrap_or("").trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return;
    }
    add(out, base);
    if let Some((lang, _)) = base.split_once('_') {
        add(out, lang);
    }
}

/// The preference list from the values of LANGUAGE (a colon list), LC_ALL,
/// LC_MESSAGES and LANG. The first of the last three that is set decides.
pub fn langs_from_vars(
    language: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
) -> Vec<String> {
    let mut out = Vec::new();
    for l in language.unwrap_or("").split(':') {
        expand(l, &mut out);
    }
    let locale = [lc_all, lc_messages, lang]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty());
    if let Some(l) = locale {
        expand(l, &mut out);
    }
    out
}

/// The preference list from the process environment.
pub fn langs_from_env() -> Vec<String> {
    let get = |k: &str| env::var_os(k).map(|v| v.to_string_lossy().into_owned());
    let (a, b, c, d) = (
        get("LANGUAGE"),
        get("LC_ALL"),
        get("LC_MESSAGES"),
        get("LANG"),
    );
    langs_from_vars(a.as_deref(), b.as_deref(), c.as_deref(), d.as_deref())
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes().zip(b.bytes()).all(|(x, y)| {
            let n = |c: u8| {
                if c == b'_' {
                    b'-'
                } else {
                    c.to_ascii_lowercase()
                }
            };
            n(x) == n(y)
        })
}

/// A preference list ready for matching `xml:lang` values.
#[derive(Debug, Clone)]
pub struct LangPrefs {
    langs: Vec<String>,
}

impl LangPrefs {
    /// Takes the languages in order of preference.
    pub fn new(langs: &[String]) -> LangPrefs {
        LangPrefs {
            langs: langs.iter().take(MAX_LANGS).cloned().collect(),
        }
    }

    /// How good an element with this `xml:lang` is: lower is better, `None`
    /// means not wanted. The wanted languages rank first, then an element
    /// without `xml:lang`, then `en`. `-` and `_` and letter case don't matter.
    pub fn rank(&self, xml_lang: Option<&str>) -> Option<usize> {
        let n = self.langs.len();
        let Some(l) = xml_lang else { return Some(n) };
        if let Some(i) = self.langs.iter().position(|w| same(w, l)) {
            return Some(i);
        }
        same("en", l).then_some(n + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn locales() {
        assert_eq!(
            langs_from_vars(None, None, None, Some("pt_BR.UTF-8@euro")),
            v(&["pt_BR", "pt"])
        );
        assert_eq!(langs_from_vars(None, None, None, Some("C")), v(&[]));
        assert_eq!(
            langs_from_vars(None, Some("POSIX"), None, Some("de_DE")),
            v(&[])
        );
        assert_eq!(
            langs_from_vars(Some("fr:de_AT"), Some(""), None, Some("en_US.UTF-8")),
            v(&["fr", "de_AT", "de", "en_US", "en"])
        );
        assert_eq!(langs_from_vars(Some("de:de"), None, None, None), v(&["de"]));
    }

    #[test]
    fn ranking() {
        let p = LangPrefs::new(&v(&["pt_BR", "pt"]));
        assert_eq!(p.rank(Some("PT-br")), Some(0));
        assert_eq!(p.rank(Some("pt")), Some(1));
        assert_eq!(p.rank(None), Some(2));
        assert_eq!(p.rank(Some("en")), Some(3));
        assert_eq!(p.rank(Some("de")), None);
    }
}
