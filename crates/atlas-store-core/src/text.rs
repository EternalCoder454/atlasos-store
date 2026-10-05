//! Text from outside, made safe to show: control characters and bidi
//! overrides are removed, whitespace is collapsed and every field has a length
//! cap. The checks for IDs, URLs, icon file names and Flatpak references live
//! here too, so the parser and the index reader apply the same rules.

/// What to do with one character of untrusted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Remove it: a control, a bidi embedding/override/isolate, a BOM or a
    /// noncharacter.
    Drop,
    /// Whitespace: runs collapse to one space, and none starts or ends a text.
    Space,
    /// Keep it. This includes the left-to-right and right-to-left marks and
    /// the zero-width joiners, which scripts need.
    Keep,
}

/// Sorts one character. Whitespace is checked first, so tab and newline
/// become spaces rather than being removed.
pub fn class(c: char) -> Class {
    if c.is_whitespace() {
        return Class::Space;
    }
    let u = c as u32;
    let bad =
        c.is_control() || invisible(u) || (0xFDD0..=0xFDEF).contains(&u) || (u & 0xFFFE) == 0xFFFE;
    if bad { Class::Drop } else { Class::Keep }
}

/// Characters that show as nothing or reorder what is around them, so text
/// could read as something it isn't: soft hyphen, combining grapheme joiner,
/// Arabic letter mark, Hangul and Halfwidth fillers, Khmer inherent vowels,
/// Mongolian free variation selectors, zero-width space, the general
/// punctuation format block (word joiner, invisible operators, bidi isolates,
/// deprecated format characters), embeddings and overrides, variation
/// selectors (so an emoji shows in its default form), the BOM, interlinear
/// annotation and object-replacement characters, musical and shorthand format
/// controls and the tag characters. Zero-width joiner and non-joiner and the
/// left-to-right and right-to-left marks stay: scripts need them. This list
/// follows `launch::hidden`.
fn invisible(u: u32) -> bool {
    matches!(u,
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5
        | 0x180B..=0x180F | 0x200B | 0x202A..=0x202E | 0x2060..=0x206F
        | 0x3164 | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFC
        | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0000..=0xE007F
        | 0xE0100..=0xE01EF)
}

/// Builds one line of cleaned text from pieces, stopping at a cap.
#[derive(Debug, Default)]
pub struct LineBuf {
    s: String,
    pending: bool,
    chars: usize,
    max_chars: usize,
    over: bool,
    altered: bool,
}

impl LineBuf {
    /// An empty buffer that takes at most `max_chars` characters.
    pub fn new(max_chars: usize) -> LineBuf {
        LineBuf {
            max_chars,
            ..LineBuf::default()
        }
    }

    /// Adds a piece of text. Past the cap the rest is ignored (and
    /// [`LineBuf::truncated`] turns true); this is never an error.
    pub fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            if self.over {
                return;
            }
            match class(c) {
                Class::Drop => self.altered = true,
                Class::Space => self.pending = self.chars > 0,
                Class::Keep => {
                    let extra = usize::from(self.pending) + 1;
                    if self.chars + extra > self.max_chars {
                        self.over = true;
                        return;
                    }
                    if self.pending {
                        self.s.push(' ');
                        self.pending = false;
                    }
                    self.s.push(c);
                    self.chars += extra;
                }
            }
        }
    }

    /// True when text was cut off at the cap.
    pub fn truncated(&self) -> bool {
        self.over
    }

    /// True when a control, bidi or other removed character was in the text.
    /// IDs, file names and URLs are refused then instead of being repaired.
    pub fn altered(&self) -> bool {
        self.altered
    }

    /// The cleaned text.
    pub fn finish(self) -> String {
        self.s
    }
}

/// Cleans a whole string as one line, cut at `max_chars` characters.
pub fn clean(s: &str, max_chars: usize) -> String {
    let mut b = LineBuf::new(max_chars);
    b.push_str(s);
    b.finish()
}

fn id_chars(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// A component or Flatpak ID: `[A-Za-z0-9_.-]`, at most 255 bytes, at least two
/// dot-separated parts and none of them empty. A `.desktop` suffix passes.
pub fn valid_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 255 || !id_chars(s) {
        return false;
    }
    let mut parts = 0;
    for p in s.split('.') {
        if p.is_empty() {
            return false;
        }
        parts += 1;
    }
    parts >= 2
}

/// A bare icon file name: only `[A-Za-z0-9._+-]`, 1 to 255 bytes, no leading
/// `.` or `-`, no `..`, ending `.png` or `.svg`.
pub fn valid_icon_file(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with(['.', '-'])
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
        && (s.ends_with(".png") || s.ends_with(".svg"))
}

/// A web URL the Store may keep: `https://` (or `http://` unless `https_only`),
/// a host of `[A-Za-z0-9.-]` with an optional port up to 65535, no userinfo, no whitespace, control or bidi characters, at most
/// 2048 bytes.
pub fn valid_url(s: &str, https_only: bool) -> bool {
    if s.len() > 2048 {
        return false;
    }
    let lower = s.get(..8).unwrap_or("").to_ascii_lowercase();
    let rest = if lower == "https://" {
        &s[8..]
    } else if !https_only && lower.starts_with("http://") {
        &s[7..]
    } else {
        return false;
    };
    if s.chars().any(|c| c == '\\' || class(c) != Class::Keep) {
        return false;
    }
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    if auth.is_empty() || auth.contains('@') || auth.contains('[') || auth.contains(']') {
        return false;
    }
    let host = match auth.split_once(':') {
        Some((h, port)) => {
            if port.is_empty()
                || port.len() > 5
                || !port.bytes().all(|b| b.is_ascii_digit())
                || port.parse::<u32>().is_ok_and(|p| p > 65535)
            {
                return false;
            }
            h
        }
        None => auth,
    };
    !host.is_empty()
        && !host.starts_with('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn arch_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn branch_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && id_chars(s)
}

/// `ID/arch/branch`, as a Flatpak runtime or SDK is named.
pub fn valid_flatpak_target(s: &str) -> bool {
    let mut it = s.split('/');
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(id), Some(arch), Some(branch), None) => {
            valid_id(id) && arch_ok(arch) && branch_ok(branch)
        }
        _ => false,
    }
}

/// `app|runtime/ID/arch/branch`.
pub fn valid_bundle_ref(s: &str) -> bool {
    match s.split_once('/') {
        Some(("app" | "runtime", rest)) => valid_flatpak_target(rest),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_removes_controls_and_bidi() {
        assert_eq!(clean("  a\u{0}b\u{202E}c \t\n d\u{7}  ", 100), "abc d");
        assert_eq!(
            clean("x\u{200E}y\u{200D}z\u{FEFF}\u{FFFF}", 100),
            "x\u{200E}y\u{200D}z"
        );
        assert_eq!(clean("abcdef", 3), "abc");
        assert_eq!(clean("ab cd", 3), "ab");
        assert_eq!(clean("é€😀x", 3), "é€😀");
    }

    #[test]
    fn urls() {
        assert!(valid_url("https://example.org/a?b#c", false));
        assert!(valid_url("http://example.org:8080/", false));
        assert!(valid_url("https://example.org:65535/", false));
        assert!(!valid_url("http://example.org/", true));
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "https://user:pw@example.org/",
            "https://@example.org/",
            "https:///path",
            "https://exa mple.org/",
            "https://example.org/\u{7}",
            "https://example.org\\@evil.org/",
            "https://example.org:/",
            "https://example.org:80:90/",
            "https://example.org:65536/",
            "https://exa%6Dple.org/",
            "https://exa_mple.org/",
            "https://ex\u{e4}mple.org/",
            "",
        ] {
            assert!(!valid_url(bad, false), "{bad}");
        }
        let long = format!("https://example.org/{}", "a".repeat(2048));
        assert!(!valid_url(&long, false));
    }

    #[test]
    fn ids_icons_refs() {
        assert!(valid_id("org.gnome.Nautilus.desktop"));
        for bad in [
            "", "nodots", "a..b", ".a.b", "a.b.", "a.b/c", "a b.c", "../x.y",
        ] {
            assert!(!valid_id(bad), "{bad}");
        }
        assert!(valid_icon_file("org.x.Y.png"));
        assert!(valid_icon_file("a+b_c-d.svg"));
        for bad in [
            "../a.png",
            "/etc/passwd",
            "a/b.png",
            ".hidden.png",
            "a.jpg",
            "a\\b.svg",
            "a\0.png",
            "-a.png",
            "a..b.png",
            "a b.png",
            "a%2e.png",
            "\u{e4}.png",
            "a\u{202E}.png",
        ] {
            assert!(!valid_icon_file(bad), "{bad}");
        }
        assert!(valid_bundle_ref("app/ai.jan.Jan/x86_64/stable"));
        assert!(valid_bundle_ref(
            "runtime/org.freedesktop.Sdk.Extension.golang/x86_64/18.08"
        ));
        assert!(!valid_bundle_ref("app/ai.jan.Jan/x86_64"));
        assert!(!valid_bundle_ref("bundle/ai.jan.Jan/x86_64/stable"));
        assert!(!valid_bundle_ref("app/../x86_64/stable"));
    }
}
