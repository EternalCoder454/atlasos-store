//! A GKeyFile reader for untrusted text: `.flatpakref`, `.flatpakrepo` and a
//! Flatpak app's `metadata`. It reads them the way flatpak (GLib) does, so the
//! Store never shows one thing while flatpak acts on another: a repeated key
//! keeps its last value, a repeated group merges into the first, `key[locale]`
//! is a separate key, and values are unescaped only when asked for. It is
//! stricter where GLib would accept something odd: invalid UTF-8, a NUL, a
//! line outside any group or over the limits fails the whole file.

use std::fmt;

/// Size limits for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_groups: usize,
    /// Keys in all groups together, translations included.
    pub max_keys: usize,
    /// Bytes of one raw value.
    pub max_value: usize,
}

impl Default for Limits {
    /// Room for a flatpakref with an inline GPG key (a few KiB of base64) and
    /// for any real app's metadata, which is under 4 KiB.
    fn default() -> Limits {
        Limits {
            max_bytes: 256 * 1024,
            max_lines: 4096,
            max_groups: 256,
            max_keys: 4096,
            max_value: 64 * 1024,
        }
    }
}

/// Why a file or a value was refused. `line` is 1-based, 0 for the whole file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFileError {
    pub line: usize,
    pub kind: ErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    TooLarge,
    NotUtf8,
    Nul,
    TooManyLines,
    TooManyGroups,
    TooManyKeys,
    ValueTooLong,
    BadGroupName,
    BadKeyName,
    /// A line that is neither a comment, a group nor `key=value`.
    BadLine,
    /// A key before the first group.
    NoGroup,
    /// An escape GLib doesn't know, or a `\` ending the value.
    BadEscape,
    /// A boolean that isn't `true`, `false`, `1` or `0`.
    NotBool,
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = match self.kind {
            ErrorKind::TooLarge => "the file is too large",
            ErrorKind::NotUtf8 => "the file is not UTF-8 text",
            ErrorKind::Nul => "the file contains a NUL byte",
            ErrorKind::TooManyLines => "the file has too many lines",
            ErrorKind::TooManyGroups => "the file has too many groups",
            ErrorKind::TooManyKeys => "the file has too many keys",
            ErrorKind::ValueTooLong => "a value is too long",
            ErrorKind::BadGroupName => "a group name is not valid",
            ErrorKind::BadKeyName => "a key name is not valid",
            ErrorKind::BadLine => "a line is not a group, a key or a comment",
            ErrorKind::NoGroup => "a key comes before the first group",
            ErrorKind::BadEscape => "a value has an invalid escape",
            ErrorKind::NotBool => "a value is not true or false",
        };
        if self.line == 0 {
            f.write_str(what)
        } else {
            write!(f, "{what} (line {})", self.line)
        }
    }
}

impl std::error::Error for KeyFileError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    name: String,
    /// Key (with any `[locale]`), raw value and the line it came from.
    entries: Vec<(String, String, usize)>,
}

/// A parsed key file. Groups and keys keep the order they first appeared in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFile {
    groups: Vec<Group>,
}

fn err(line: usize, kind: ErrorKind) -> KeyFileError {
    KeyFileError { line, kind }
}

/// GLib's `g_ascii_isspace`: unlike `char::is_ascii_whitespace` it includes
/// the vertical tab, so a value starting with one reads the same here.
fn glib_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

/// GLib's `g_key_file_is_group_name`: not empty, no `[`, `]` or control.
fn group_name_ok(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c == '[' || c == ']' || c.is_control())
}

/// A key, optionally `name[locale]`: the name not empty, not starting with a
/// space, with no `=`, `[`, `]` or control; the locale not empty and without
/// `]` or control.
fn key_name_ok(s: &str) -> bool {
    let (name, locale) = match s.find('[') {
        Some(i) => match s[i + 1..].strip_suffix(']') {
            Some(l) => (&s[..i], Some(l)),
            None => return false,
        },
        None => (s, None),
    };
    let name_ok = !name.is_empty()
        && !name.starts_with(' ')
        && !name
            .chars()
            .any(|c| matches!(c, '=' | '[' | ']') || c.is_control());
    let locale_ok = locale.is_none_or(|l| {
        !l.is_empty() && !l.chars().any(|c| matches!(c, '[' | ']') || c.is_control())
    });
    name_ok && locale_ok
}

impl KeyFile {
    /// Parses a whole file with `limits`.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<KeyFile, KeyFileError> {
        if bytes.len() > limits.max_bytes {
            return Err(err(0, ErrorKind::TooLarge));
        }
        if bytes.contains(&0) {
            return Err(err(0, ErrorKind::Nul));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| err(0, ErrorKind::NotUtf8))?;
        let mut kf = KeyFile { groups: Vec::new() };
        let mut cur: Option<usize> = None;
        let mut keys = 0usize;
        // A final newline ends the last line rather than starting one, and,
        // as in GLib, `\r` is dropped only before `\n`.
        for (i, raw) in text.split_inclusive('\n').enumerate() {
            let n = i + 1;
            if n > limits.max_lines {
                return Err(err(n, ErrorKind::TooManyLines));
            }
            let line = match raw.strip_suffix('\n') {
                Some(l) => l.strip_suffix('\r').unwrap_or(l),
                None => raw,
            };
            let line = line.trim_start_matches(glib_space);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let Some(end) = rest.find(']') else {
                    return Err(err(n, ErrorKind::BadLine));
                };
                if !rest[end + 1..].trim_end_matches([' ', '\t']).is_empty() {
                    return Err(err(n, ErrorKind::BadLine));
                }
                let name = &rest[..end];
                if !group_name_ok(name) {
                    return Err(err(n, ErrorKind::BadGroupName));
                }
                cur = Some(match kf.groups.iter().position(|g| g.name == name) {
                    Some(g) => g,
                    None => {
                        if kf.groups.len() >= limits.max_groups {
                            return Err(err(n, ErrorKind::TooManyGroups));
                        }
                        kf.groups.push(Group {
                            name: name.to_string(),
                            entries: Vec::new(),
                        });
                        kf.groups.len() - 1
                    }
                });
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(err(n, ErrorKind::BadLine));
            };
            let key = key.trim_end_matches(glib_space);
            if !key_name_ok(key) {
                return Err(err(n, ErrorKind::BadKeyName));
            }
            let value = value.trim_start_matches(glib_space);
            if value.len() > limits.max_value {
                return Err(err(n, ErrorKind::ValueTooLong));
            }
            let Some(g) = cur else {
                return Err(err(n, ErrorKind::NoGroup));
            };
            let entries = &mut kf.groups[g].entries;
            match entries.iter_mut().find(|(k, _, _)| k == key) {
                Some(e) => {
                    e.1 = value.to_string();
                    e.2 = n;
                }
                None => {
                    keys += 1;
                    if keys > limits.max_keys {
                        return Err(err(n, ErrorKind::TooManyKeys));
                    }
                    entries.push((key.to_string(), value.to_string(), n));
                }
            }
        }
        Ok(kf)
    }

    fn group(&self, group: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == group)
    }

    fn entry(&self, group: &str, key: &str) -> Option<(&str, usize)> {
        self.group(group)?
            .entries
            .iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, v, n)| (v.as_str(), *n))
    }

    /// The group names, in order.
    pub fn groups(&self) -> impl Iterator<Item = &str> {
        self.groups.iter().map(|g| g.name.as_str())
    }

    pub fn has_group(&self, group: &str) -> bool {
        self.group(group).is_some()
    }

    /// The keys of `group` without translations (`key[locale]`), in order.
    pub fn keys(&self, group: &str) -> impl Iterator<Item = &str> {
        self.group(group)
            .into_iter()
            .flat_map(|g| g.entries.iter())
            .map(|(k, _, _)| k.as_str())
            .filter(|k| !k.contains('['))
    }

    /// Every key of `group`, translations (`key[locale]`) included, in order.
    pub fn all_keys(&self, group: &str) -> impl Iterator<Item = &str> {
        self.group(group)
            .into_iter()
            .flat_map(|g| g.entries.iter())
            .map(|(k, _, _)| k.as_str())
    }

    /// The value as written, escapes and all.
    pub fn raw(&self, group: &str, key: &str) -> Option<&str> {
        self.entry(group, key).map(|(v, _)| v)
    }

    /// The value unescaped (`\s`, `\n`, `\t`, `\r`, `\\`), as
    /// `g_key_file_get_string` returns it.
    pub fn string(&self, group: &str, key: &str) -> Result<Option<String>, KeyFileError> {
        let Some((v, n)) = self.entry(group, key) else {
            return Ok(None);
        };
        let mut out = unescape(v, false).map_err(|k| err(n, k))?;
        Ok(out.pop())
    }

    /// The value as a `;`-separated list, as `g_key_file_get_string_list`
    /// returns it: `\;` is a literal `;` and an empty last element is dropped.
    pub fn list(&self, group: &str, key: &str) -> Result<Option<Vec<String>>, KeyFileError> {
        let Some((v, n)) = self.entry(group, key) else {
            return Ok(None);
        };
        unescape(v, true).map(Some).map_err(|k| err(n, k))
    }

    /// `true`/`1` or `false`/`0`, as `g_key_file_get_boolean` reads them.
    /// Anything else is an error; flatpak, reading with no error, would take
    /// it as false, so a caller refuses rather than guess.
    pub fn bool(&self, group: &str, key: &str) -> Result<Option<bool>, KeyFileError> {
        let Some((v, n)) = self.entry(group, key) else {
            return Ok(None);
        };
        // Exactly these four, as GLib compares them: `true ` is not true.
        match v {
            "true" | "1" => Ok(Some(true)),
            "false" | "0" => Ok(Some(false)),
            _ => Err(err(n, ErrorKind::NotBool)),
        }
    }
}

/// GLib's value unescaping. As a list, an unescaped `;` ends an element and
/// `\;` is kept as `;`; a non-empty tail is the last element. As a string the
/// result is one element, and `\;` is an invalid escape, as in GLib.
fn unescape(v: &str, list: bool) -> Result<Vec<String>, ErrorKind> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = v.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some('s') => cur.push(' '),
                Some('n') => cur.push('\n'),
                Some('t') => cur.push('\t'),
                Some('r') => cur.push('\r'),
                Some('\\') => cur.push('\\'),
                Some(';') if list => cur.push(';'),
                _ => return Err(ErrorKind::BadEscape),
            },
            ';' if list => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if !list || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<KeyFile, KeyFileError> {
        KeyFile::parse(s.as_bytes(), &Limits::default())
    }

    #[test]
    fn reads_groups_keys_and_values_like_glib() {
        let kf = p(
            "# c\n\n[Flatpak Ref]\r\nName = org.a.B\n  Title=Hi\\sthere\nTitle[de]=Hallo\n\
                    [Context]\nshared=network;ipc;\nempty=\n",
        )
        .unwrap();
        assert_eq!(kf.groups().collect::<Vec<_>>(), ["Flatpak Ref", "Context"]);
        assert_eq!(kf.raw("Flatpak Ref", "Name"), Some("org.a.B"));
        assert_eq!(
            kf.string("Flatpak Ref", "Title").unwrap().unwrap(),
            "Hi there"
        );
        assert_eq!(kf.raw("Flatpak Ref", "Title[de]"), Some("Hallo"));
        assert_eq!(
            kf.keys("Flatpak Ref").collect::<Vec<_>>(),
            ["Name", "Title"]
        );
        assert_eq!(
            kf.all_keys("Flatpak Ref").collect::<Vec<_>>(),
            ["Name", "Title", "Title[de]"]
        );
        assert_eq!(
            kf.list("Context", "shared").unwrap().unwrap(),
            ["network", "ipc"]
        );
        assert_eq!(
            kf.list("Context", "empty").unwrap().unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(kf.string("Context", "empty").unwrap().unwrap(), "");
        assert_eq!(kf.raw("Context", "missing"), None);
        assert_eq!(kf.raw("Nope", "x"), None);
        assert!(kf.string("Nope", "x").unwrap().is_none());
    }

    #[test]
    fn repeats_resolve_as_glib_does() {
        let kf = p("[A]\nk=1\n[B]\nx=y\n[A]\nk=2\nj=3\n").unwrap();
        assert_eq!(kf.groups().collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(kf.raw("A", "k"), Some("2"));
        assert_eq!(kf.keys("A").collect::<Vec<_>>(), ["k", "j"]);
    }

    #[test]
    fn lists_and_escapes() {
        let kf = p("[G]\na=x\\;y;z;\nb=a;;b\nc=one\\stwo;\nd=bad\\q\ne=end\\\nf=a\\;b\n").unwrap();
        assert_eq!(kf.list("G", "a").unwrap().unwrap(), ["x;y", "z"]);
        assert_eq!(kf.list("G", "b").unwrap().unwrap(), ["a", "", "b"]);
        assert_eq!(kf.list("G", "c").unwrap().unwrap(), ["one two"]);
        let bad = kf.list("G", "d").unwrap_err();
        assert_eq!((bad.line, bad.kind), (5, ErrorKind::BadEscape));
        assert_eq!(kf.string("G", "e").unwrap_err().kind, ErrorKind::BadEscape);
        // `\;` is only an escape in a list, as in GLib.
        assert_eq!(kf.string("G", "f").unwrap_err().kind, ErrorKind::BadEscape);
        assert_eq!(kf.list("G", "f").unwrap().unwrap(), ["a;b"]);
    }

    #[test]
    fn booleans() {
        let kf = p("[G]\na=true\nb=0\nc=yes\nd=True\ne=true \n").unwrap();
        assert_eq!(kf.bool("G", "a").unwrap(), Some(true));
        assert_eq!(kf.bool("G", "b").unwrap(), Some(false));
        assert_eq!(kf.bool("G", "c").unwrap_err().kind, ErrorKind::NotBool);
        assert_eq!(kf.bool("G", "d").unwrap_err().kind, ErrorKind::NotBool);
        assert_eq!(kf.bool("G", "e").unwrap_err().kind, ErrorKind::NotBool);
        assert_eq!(kf.bool("G", "z").unwrap(), None);
    }

    #[test]
    fn refuses_what_is_not_a_key_file() {
        let kind = |s: &str| p(s).unwrap_err().kind;
        assert_eq!(kind("k=v\n[G]\n"), ErrorKind::NoGroup);
        assert_eq!(kind("[G]\njust text\n"), ErrorKind::BadLine);
        assert_eq!(kind("[G\n"), ErrorKind::BadLine);
        assert_eq!(kind("[G] x\n"), ErrorKind::BadLine);
        assert_eq!(kind("[]\n"), ErrorKind::BadGroupName);
        assert_eq!(kind("[G\u{1b}]\n"), ErrorKind::BadGroupName);
        assert_eq!(kind("[G]\n=v\n"), ErrorKind::BadKeyName);
        assert_eq!(kind("[G]\nk[=v\n"), ErrorKind::BadKeyName);
        assert_eq!(kind("[G]\nk[]=v\n"), ErrorKind::BadKeyName);
        assert_eq!(kind("[G]\nk]x=v\n"), ErrorKind::BadKeyName);
        assert_eq!(kind("[G]\nk\u{7}=v\n"), ErrorKind::BadKeyName);
        assert_eq!(kind("[G]\nk=a\0b\n"), ErrorKind::Nul);
        assert_eq!(
            KeyFile::parse(b"[G]\nk=\xff\n", &Limits::default())
                .unwrap_err()
                .kind,
            ErrorKind::NotUtf8
        );
        // Trailing spaces and tabs after a group are fine, as in GLib; other
        // blanks there are not.
        assert!(p("[G] \t\nk=v").is_ok());
        assert_eq!(kind("[G]\x0C\n"), ErrorKind::BadLine);
        // A vertical tab is GLib whitespace: trimmed before a value and a line.
        let kf = p("\x0B[G]\nk =\x0B\x0C v\n").unwrap();
        assert_eq!(kf.raw("G", "k"), Some("v"));
        assert!(p("").unwrap().groups().next().is_none());
    }

    #[test]
    fn limits_hold() {
        let l = Limits {
            max_bytes: 64,
            max_lines: 5,
            max_groups: 2,
            max_keys: 2,
            max_value: 8,
        };
        let kind = |s: &str| KeyFile::parse(s.as_bytes(), &l).unwrap_err().kind;
        assert_eq!(kind(&"x".repeat(65)), ErrorKind::TooLarge);
        assert_eq!(kind("\n\n\n\n\n\n"), ErrorKind::TooManyLines);
        assert_eq!(kind("\n\n\n\n\nx"), ErrorKind::TooManyLines);
        assert!(KeyFile::parse(b"\n\n\n\n\n", &l).is_ok());
        assert!(KeyFile::parse(b"#\n#\n#\n#\n[E]\n", &l).is_ok());
        // `\r` is a line ending only before `\n`.
        assert!(KeyFile::parse(b"[A]\r\n", &l).is_ok());
        assert!(KeyFile::parse(b"[A]\r", &l).is_err());
        assert_eq!(kind("[A]\n[B]\n[C]\n"), ErrorKind::TooManyGroups);
        assert_eq!(kind("[A]\na=1\nb=2\nc=3\n"), ErrorKind::TooManyKeys);
        assert_eq!(kind("[A]\na=123456789\n"), ErrorKind::ValueTooLong);
        // A repeated key or group doesn't count twice.
        assert!(KeyFile::parse(b"[A]\na=1\na=2\n[A]\nb=3", &l).is_ok());
        assert!(KeyFile::parse(b"[A]\na=12345678", &l).is_ok());
    }
}
