//! Search text: lowercase, accent-folded words. Done once when the library is
//! built, and for each query; never allocates per entry while searching.

/// The ASCII a Latin letter with a diacritic folds to, or `None` to keep it.
/// Covers Latin-1 Supplement and Latin Extended-A (lowercase: the input is
/// lowercased first), which is what app names in the catalogs use.
fn fold(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' => "t",
        'þ' => "th",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

/// Appends ` word` to `out` for every word of `s`: letters and digits,
/// lowercased and accent-folded, split at everything else (so `org.gnome.Foo`
/// has three words and `Café` becomes `cafe`). Combining marks are dropped.
/// A word is capped at 64 characters.
pub(super) fn push_words(s: &str, out: &mut String) {
    let mut in_word = false;
    let mut len = 0usize;
    for c in s.chars() {
        // Marks are dropped after lowercasing too: `İ` lowercases to `i`
        // and U+0307, which must not split the word.
        for lc in c.to_lowercase() {
            if ('\u{300}'..='\u{36f}').contains(&lc) {
                continue;
            }
            let folded = fold(lc);
            let mut one = [0u8; 4];
            let piece: &str = folded.unwrap_or_else(|| lc.encode_utf8(&mut one));
            if piece.chars().all(char::is_alphanumeric) {
                if !in_word {
                    out.push(' ');
                    in_word = true;
                    len = 0;
                }
                if len < 64 {
                    out.push_str(piece);
                    len += 1;
                }
            } else {
                in_word = false;
            }
        }
    }
}

/// The words of `s` as a ` w1 w2` haystack string.
pub(super) fn haystack(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 1);
    push_words(s, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_fold_and_split() {
        assert_eq!(haystack("Café Über-Tool 2"), " cafe uber tool 2");
        assert_eq!(haystack("org.gnome.Foo"), " org gnome foo");
        assert_eq!(haystack("e\u{301}tude"), " etude");
        assert_eq!(haystack("  ++ "), "");
        assert_eq!(haystack("ÆON Straße"), " aeon strasse");
        assert_eq!(haystack("İstanbul"), " istanbul");
    }
}
