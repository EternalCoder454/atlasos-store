//! What a launch asks for: the command line of the first launch, or of a
//! second one forwarded by the single-instance service. Everything here comes
//! from outside (a browser hands over links, a file manager hands over
//! files), so nothing is trusted: names are checked, URLs are taken apart by
//! hand and only the forms below are accepted.

use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// Longest argument looked at. A Flatpak name is at most 255 bytes; a URL or
/// path longer than this is refused rather than parsed.
const MAX_ARG: usize = 4096;

/// Arguments looked at per launch; the rest are dropped unread. Any process in
/// the session can forward arguments, so one call can't flood the window.
pub const MAX_ARGS: usize = 64;

/// Requests and refusals kept per launch; more are counted in `dropped`.
const MAX_REQUESTS: usize = 8;
const MAX_REFUSED: usize = 8;

/// A page the window can open on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Installed,
    Updates,
    Sources,
}

impl Page {
    pub fn name(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Installed => "installed",
            Page::Updates => "updates",
            Page::Sources => "sources",
        }
    }

    fn parse(s: &str) -> Option<Page> {
        match s {
            "home" => Some(Page::Home),
            "installed" => Some(Page::Installed),
            "updates" => Some(Page::Updates),
            "sources" => Some(Page::Sources),
            _ => None,
        }
    }
}

/// What the files we are handed hold, by their name. The content is checked
/// again when it is read: the name only picks the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// `.flatpakref`: one app and where it comes from.
    Ref,
    /// `.flatpakrepo`: a remote.
    Repo,
    /// `.flatpak`: a single-file bundle.
    Bundle,
    /// `.rpm`: can't be installed on Telamon OS; the Store explains why.
    Rpm,
}

/// One request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Page(Page),
    /// An app's page, by Flatpak or AppStream ID.
    App(String),
    /// An app's page with its Remove confirmation open (the launcher's
    /// Uninstall). Nothing is removed until the user confirms there.
    Remove(String),
    Search(String),
    File(FileKind, PathBuf),
    /// A `.flatpakref` to download (from a `flatpak+https:` link).
    RefUrl(String),
}

/// Why an argument was refused, in words the window can show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub arg: String,
    pub reason: &'static str,
}

/// What one launch asked for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The requests, in order.
    pub requests: Vec<Request>,
    /// What was refused, in order.
    pub refused: Vec<Refused>,
    /// Arguments, requests and refusals over the limits, not looked at.
    pub dropped: usize,
}

/// The requests in `args` (without the program name), in order, and what was
/// refused. `cwd` resolves relative file paths; when it isn't absolute (a
/// forwarded launch that sent none), relative paths are refused. With no
/// argument at all the answer is the home page. `--` ends the options.
pub fn parse(args: &[String], cwd: &Path) -> Launch {
    let mut launch = Launch {
        dropped: args.len().saturating_sub(MAX_ARGS),
        ..Launch::default()
    };
    let mut it = args.iter().take(MAX_ARGS);
    let mut options = true;
    while let Some(arg) = it.next() {
        let result = if arg.len() > MAX_ARG {
            Err("too long")
        } else if options && arg == "--" {
            options = false;
            continue;
        } else if options && arg.starts_with('-') {
            option(arg, &mut it)
        } else {
            positional(arg, cwd)
        };
        match result {
            Ok(r) if launch.requests.len() < MAX_REQUESTS => launch.requests.push(r),
            Err(reason) if launch.refused.len() < MAX_REFUSED => launch.refused.push(Refused {
                arg: shown(arg),
                reason,
            }),
            _ => launch.dropped += 1,
        }
    }
    if args.is_empty() {
        launch.requests.push(Request::Page(Page::Home));
    }
    launch
}

/// `--app`, `--search`, `--page` or `--remove`, with its value inline
/// (`--app=ID`) or next. Anything else starting with `-` is refused, so a
/// link or file name can never become an option.
fn option<'a>(
    arg: &str,
    rest: &mut impl Iterator<Item = &'a String>,
) -> Result<Request, &'static str> {
    let (flag, inline) = match arg.split_once('=') {
        Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
        _ => (arg, None),
    };
    if !matches!(flag, "--app" | "--remove" | "--search" | "--page") {
        return Err("unknown option");
    }
    let value = inline
        .or_else(|| rest.next().cloned())
        .ok_or("needs a value")?;
    if value.len() > MAX_ARG {
        return Err("too long");
    }
    match flag {
        "--app" => app_id(&value).map(Request::App).ok_or("not an app ID"),
        "--remove" => app_id(&value).map(Request::Remove).ok_or("not an app ID"),
        "--search" => search_text(&value)
            .map(Request::Search)
            .ok_or("empty search"),
        _ => Page::parse(&value).map(Request::Page).ok_or("no such page"),
    }
}

/// A URL or a file.
fn positional(arg: &str, cwd: &Path) -> Result<Request, &'static str> {
    if arg.chars().any(hidden) {
        return Err("has hidden or control characters");
    }
    if let Some((scheme, rest)) = scheme(arg) {
        match scheme.as_str() {
            "appstream" => {
                // appstream://org.gimp.GIMP, appstream:org.gimp.GIMP, and
                // the ".desktop" IDs older components still have.
                let id = rest.trim_start_matches("//").trim_end_matches('/');
                return app_id(id).map(Request::App).ok_or("not an app ID");
            }
            "flatpak+https" => {
                return https_url(&format!("https:{rest}"))
                    .map(Request::RefUrl)
                    .ok_or("not a usable link");
            }
            "file" => return file(&file_url_path(rest)?),
            // Any other link. Without `//` it may be a file name with a
            // colon in it (`notes:v2.flatpakref`), read as a path below.
            _ if rest.starts_with("//") => return Err("unsupported link"),
            _ => {}
        }
    }
    let path = Path::new(arg);
    if path.is_absolute() {
        file(path)
    } else if cwd.is_absolute() {
        // `./a.flatpakref` and `Downloads//a.flatpakref` are how people type
        // paths, and a forwarded folder may end in `/`: written plainly
        // before the check. A trailing `/` or `/.` still means a folder.
        if arg.ends_with('/') || arg.ends_with("/.") || arg == "." {
            return Err("is a folder");
        }
        file(&cwd.join(path).components().collect::<PathBuf>())
    } else {
        Err("relative path with no folder to find it in")
    }
}

/// A file the Store opens, by its name: an absolute path to a file (not a
/// folder: no trailing `/`) with no `..` in it, at most `MAX_ARG` bytes.
fn file(path: &Path) -> Result<Request, &'static str> {
    if !path.is_absolute() || path.as_os_str().len() > MAX_ARG {
        return Err("not a file");
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err("has \"..\" in it");
    }
    if path.as_os_str().as_bytes().ends_with(b"/") {
        return Err("is a folder");
    }
    // Written the one way `components()` reads it: no `//`, `/./` or
    // trailing `/.`, which would hide what the name really ends with.
    if path.components().collect::<PathBuf>().as_os_str() != path.as_os_str() {
        return Err("not a plain path");
    }
    if path.to_string_lossy().chars().any(hidden) {
        return Err("has hidden or control characters");
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("not a file")?;
    let lower = name.to_ascii_lowercase();
    let kind = if lower.ends_with(".flatpakref") {
        FileKind::Ref
    } else if lower.ends_with(".flatpakrepo") {
        FileKind::Repo
    } else if lower.ends_with(".flatpak") {
        FileKind::Bundle
    } else if lower.ends_with(".rpm") {
        FileKind::Rpm
    } else {
        return Err("not a file the Store opens");
    };
    Ok(Request::File(kind, path.to_path_buf()))
}

/// `scheme:rest` when `s` starts with a URL scheme (RFC 3986: a letter, then
/// letters, digits, `+`, `-`, `.`), lowercased. A one-letter scheme is taken
/// as no scheme, so nothing like `C:` is ever read as one.
fn scheme(s: &str) -> Option<(String, &str)> {
    let (scheme, rest) = s.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if scheme.len() < 2
        || !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), rest))
}

/// The path of a `file:` URL's rest (`///path` or `//localhost/path`),
/// percent-decoded. Another host, a bad escape, or a path that decodes to
/// something that isn't UTF-8 or holds a hidden or control character, is
/// refused.
fn file_url_path(rest: &str) -> Result<PathBuf, &'static str> {
    let path = if let Some(p) = rest.strip_prefix("//") {
        let (host, path) = p.split_at(p.find('/').ok_or("not a local file")?);
        if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
            return Err("not a local file");
        }
        path
    } else if rest.starts_with('/') {
        rest
    } else {
        return Err("not a local file");
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let decoded = percent_decode(path).ok_or("not a usable link")?;
    if decoded.chars().any(hidden) {
        return Err("has hidden or control characters");
    }
    Ok(PathBuf::from(decoded))
}

/// `%XX` escapes decoded; an escape that isn't `%` and two hex digits fails.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = hex(*bytes.get(i + 1)?)?;
            let lo = hex(*bytes.get(i + 2)?)?;
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// An `https:` URL to a public host name, in one plain form that every URL
/// parser reads the same way, or nothing:
/// - printable ASCII only, and none of `\ " < > ` { } | ^` (a parser that
///   reads `\` as `/` would see another host);
/// - no user name or password, port 443 or none;
/// - the host a DNS name of two or more labels with a letters-only top
///   level, so no IP address, and none of the names kept for local networks,
///   tests and Tor (`localhost`, `.local`, `.lan`, `.internal`, `.onion`...);
/// - no `.` or `..` path segment, plain or percent-encoded, which a fetcher
///   would resolve to another path than the one shown.
///
/// This checks the name only. The fetcher must still refuse to connect to a
/// loopback, private or link-local address the name resolves to, and run
/// every redirect through here again.
///
/// The answer has the host lowercased and the port dropped; only it is used
/// to show, check and fetch.
pub fn https_url(s: &str) -> Option<String> {
    if s.len() > MAX_ARG
        || !s.bytes().all(|b| b.is_ascii_graphic())
        || s.contains(['\\', '"', '<', '>', '`', '{', '}', '|', '^'])
    {
        return None;
    }
    let scheme = s.get(..8)?;
    if !scheme.eq_ignore_ascii_case("https://") {
        return None;
    }
    let rest = &s[8..];
    let (authority, tail) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let host = match authority.split_once(':') {
        Some((host, "443")) => host,
        Some(_) => return None,
        None => authority,
    };
    let path = tail.split(['?', '#']).next().unwrap_or_default();
    let dots = path
        .to_ascii_lowercase()
        .replace("%2e", ".")
        .split('/')
        .any(|seg| seg == "." || seg == "..");
    let host = host.to_ascii_lowercase();
    (!dots && public_host(&host)).then(|| format!("https://{host}{tail}"))
}

/// A lowercase DNS name on the internet: see `https_url`.
fn public_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    let label_ok = |l: &&str| {
        (1..=63).contains(&l.len())
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(label_ok)
        && labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_lowercase()))
        && !matches!(
            labels.last(),
            Some(
                &("local"
                    | "localhost"
                    | "localdomain"
                    | "lan"
                    | "home"
                    | "internal"
                    | "intranet"
                    | "private"
                    | "corp"
                    | "arpa"
                    | "test"
                    | "example"
                    | "invalid"
                    | "onion"
                    | "alt")
            )
        )
}

/// A Flatpak application name, or an AppStream component ID that may name
/// one (older ones end in `.desktop`): three or more non-empty parts split by
/// dots, of ASCII letters, digits, `_` and `-`, the first not starting with a
/// digit (later parts may: `io.github.0x7c13.Pal`), at most 255 bytes. Only
/// the form is checked here; the ID is looked up in the index after.
pub fn app_id(s: &str) -> Option<String> {
    if s.is_empty() || s.len() > 255 {
        return None;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    for (i, part) in parts.iter().enumerate() {
        let mut chars = part.chars();
        let first = chars.next()?;
        if i == 0 && !(first.is_ascii_alphabetic() || first == '_') {
            return None;
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return None;
        }
    }
    Some(s.to_string())
}

/// Search text as typed: hidden and control characters out, spaces
/// collapsed, at most 200 characters.
pub fn search_text(s: &str) -> Option<String> {
    let words: Vec<&str> = s
        .split(|c: char| c.is_whitespace() || hidden(c))
        .filter(|w| !w.is_empty())
        .collect();
    let joined: String = words.join(" ").chars().take(200).collect();
    (!joined.is_empty()).then_some(joined)
}

/// Characters that change how text around them looks without showing
/// themselves: controls; the bidi embeddings, overrides, isolates and marks;
/// zero-width space, word joiner, invisible operators and the deprecated
/// format characters; soft hyphen, combining grapheme joiner and the blank
/// Hangul and Khmer letters; line and paragraph separators; interlinear
/// annotations, Mongolian free variation selectors, variation selectors,
/// musical formatting controls, shorthand format controls, tag characters and
/// the byte-order mark. A name or path holding one could show as something it
/// isn't. (Zero-width joiner and non-joiner stay: scripts need them. Best
/// effort: what matters is that a request shows as what it is.)
pub fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E007F}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

/// An argument as it may be shown or logged: hidden characters out, a link's
/// user name and password and its query and fragment (which can carry
/// tokens) dropped, an option's inline value left out, cut to 120 characters.
fn shown(s: &str) -> String {
    let s = if s.starts_with('-') {
        match s.split_once('=') {
            Some((flag, _)) => format!("{flag}=…"),
            None => s.to_string(),
        }
    } else if scheme(s).is_some() {
        without_userinfo(s.split(['?', '#']).next().unwrap_or_default())
    } else {
        s.to_string()
    };
    s.chars()
        .map(|c| if hidden(c) { ' ' } else { c })
        .take(120)
        .collect()
}

/// `scheme://user:password@host/path` as `scheme://host/path`, and
/// `user:password@host/path` (no `//`) as `user:host/path`.
fn without_userinfo(link: &str) -> String {
    let Some(colon) = link.find(':') else {
        return link.to_string();
    };
    let Some(rest) = link[colon + 1..].strip_prefix("//") else {
        let rest = &link[colon + 1..];
        let end = rest.find('/').unwrap_or(rest.len());
        return match rest[..end].rfind('@') {
            Some(at) => format!("{}{}", &link[..=colon], &rest[at + 1..]),
            None => link.to_string(),
        };
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{}//{}", &link[..=colon], &rest[at + 1..]),
        None => link.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(args: &[&str], cwd: &str) -> Launch {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&args, Path::new(cwd))
    }

    fn p(args: &[&str]) -> (Vec<Request>, Vec<Refused>) {
        let l = launch(args, "/home/u");
        (l.requests, l.refused)
    }

    #[test]
    fn nothing_is_home() {
        assert_eq!(p(&[]).0, vec![Request::Page(Page::Home)]);
        // Only refusals: no page change behind the message.
        assert!(p(&["--evil"]).0.is_empty());
    }

    #[test]
    fn options() {
        assert_eq!(
            p(&["--app", "org.gimp.GIMP"]).0,
            vec![Request::App("org.gimp.GIMP".into())]
        );
        assert_eq!(
            p(&["--remove", "org.gimp.GIMP"]).0,
            vec![Request::Remove("org.gimp.GIMP".into())]
        );
        assert_eq!(
            p(&["--remove=org.gimp.GIMP"]).0,
            vec![Request::Remove("org.gimp.GIMP".into())]
        );
        assert!(p(&["--remove", "../evil"]).0.is_empty());
        assert!(p(&["--remove"]).0.is_empty());
        assert_eq!(p(&["--", "--remove", "org.gimp.GIMP"]).0.len(), 0);
        assert_eq!(
            p(&["--app=org.kde.kate"]).0,
            vec![Request::App("org.kde.kate".into())]
        );
        assert_eq!(
            p(&["--search", "  photo\teditor "]).0,
            vec![Request::Search("photo editor".into())]
        );
        assert_eq!(
            p(&["--search", "a\u{202E}b\u{2028}c"]).0,
            vec![Request::Search("a b c".into())]
        );
        assert_eq!(
            p(&["--page", "updates"]).0,
            vec![Request::Page(Page::Updates)]
        );
        // After `--`, a name starting with `-` is a file.
        assert_eq!(
            p(&["--", "-x.flatpakref"]).0,
            vec![Request::File(FileKind::Ref, "/home/u/-x.flatpakref".into())]
        );
    }

    #[test]
    fn refused() {
        for args in [
            &["--app"][..],
            &["--app", "gimp"],
            &["--app", "org..x"],
            &["--app", "1org.x.y"],
            &["--app", "org.x.y z"],
            &["-app", "x"],
            &["--page", "root"],
            &["--search", "   "],
            &["--evil"],
            &["notes.txt"],
            &["https://example.com/x.flatpakref"],
            &["flatpak+https://user@host/x"],
            &["file://evil.example/x.flatpakref"],
            &["javascript://alert(1)"],
            &["/x/\u{202E}fer.kaptalf"],
            &["/x/../etc/a.flatpakref"],
            &["/x/dir.flatpakref/"],
            &["/x/dir.flatpakref/."],
            &["/x/./a.flatpakref"],
            &["//x/a.flatpakref"],
            &["/x/a\u{E0041}.flatpakref"],
            &["/x/a\u{FE0F}.flatpakref"],
        ] {
            let (ok, bad) = p(args);
            let extra = usize::from(args == ["-app", "x"]);
            assert!(ok.is_empty(), "{args:?} gave {ok:?}");
            assert_eq!(bad.len(), 1 + extra, "{args:?}");
        }
        let long = "a".repeat(MAX_ARG + 1);
        assert_eq!(p(&[&long]).1[0].reason, "too long");
    }

    #[test]
    fn links() {
        assert_eq!(
            p(&["appstream://org.gimp.GIMP"]).0,
            vec![Request::App("org.gimp.GIMP".into())]
        );
        assert_eq!(
            p(&["appstream:org.kde.kate.desktop"]).0,
            vec![Request::App("org.kde.kate.desktop".into())]
        );
        assert_eq!(
            p(&["flatpak+https://dl.flathub.org/repo/appstream/org.gimp.GIMP.flatpakref"]).0,
            vec![Request::RefUrl(
                "https://dl.flathub.org/repo/appstream/org.gimp.GIMP.flatpakref".into()
            )]
        );
    }

    #[test]
    fn https_urls() {
        assert_eq!(
            https_url("HTTPS://DL.Flathub.org:443/a?b=c#d").as_deref(),
            Some("https://dl.flathub.org/a?b=c#d")
        );
        assert_eq!(
            https_url("https://flathub.org/a..b/.x?q=..").as_deref(),
            Some("https://flathub.org/a..b/.x?q=..")
        );
        assert_eq!(
            https_url("https://flathub.org").as_deref(),
            Some("https://flathub.org")
        );
        for bad in [
            "http://flathub.org/x",
            "https://evil.example\\.flathub.org/x",
            "https://user@flathub.org/x",
            "https://user:pw@flathub.org/x",
            "https://flathub.org:8443/x",
            "https://flathub.org:/x",
            "https://127.0.0.1/x",
            "https://[::1]/x",
            "https://localhost/x",
            "https://printer.local/x",
            "https://intranet/x",
            "https://fl\u{0430}thub.org/x",
            "https://flathub.org/a b",
            "https://flathub.org/<x>",
            "https://-a.org/x",
            "https://a..org/x",
            "https:///x",
            "https://%66lathub.org/x",
            "https://router.lan/x",
            "https://db.internal/x",
            "https://x.home.arpa/x",
            "https://abc.onion/x",
            "https://a.test/x",
            "https://flathub.org/a/../x",
            "https://flathub.org/a/%2E%2e/x",
            "https://flathub.org/./x",
            "https://flathub.org/a/..",
        ] {
            assert_eq!(https_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn files() {
        assert_eq!(
            p(&["Downloads/GIMP.flatpakref"]).0,
            vec![Request::File(
                FileKind::Ref,
                "/home/u/Downloads/GIMP.flatpakref".into()
            )]
        );
        assert_eq!(
            p(&["file:///tmp/a%20b.flatpakrepo"]).0,
            vec![Request::File(FileKind::Repo, "/tmp/a b.flatpakrepo".into())]
        );
        assert_eq!(
            p(&["file://localhost/x/app.FLATPAK"]).0,
            vec![Request::File(FileKind::Bundle, "/x/app.FLATPAK".into())]
        );
        for typed in ["./Downloads/GIMP.flatpakref", "Downloads//GIMP.flatpakref"] {
            assert_eq!(
                p(&[typed]).0,
                vec![Request::File(
                    FileKind::Ref,
                    "/home/u/Downloads/GIMP.flatpakref".into()
                )],
                "{typed}"
            );
        }
        assert_eq!(
            launch(&["a.flatpakref"], "/home/u/").requests,
            vec![Request::File(FileKind::Ref, "/home/u/a.flatpakref".into())]
        );
        for folder in ["d.flatpakref/", "d.flatpakref/.", "./../a.flatpakref"] {
            assert_eq!(p(&[folder]).1.len(), 1, "{folder}");
        }
        assert_eq!(
            p(&["/x/thing.rpm"]).0,
            vec![Request::File(FileKind::Rpm, "/x/thing.rpm".into())]
        );
        // A colon in a file name is not a link.
        assert_eq!(
            p(&["notes:v2.flatpakref"]).0,
            vec![Request::File(
                FileKind::Ref,
                "/home/u/notes:v2.flatpakref".into()
            )]
        );
        // Smuggled through percent-encoding: NUL, newline, an override, `..`,
        // and escapes that aren't two hex digits.
        for bad in [
            "file:///x%00.flatpakref",
            "file:///x%0A.flatpakref",
            "file:///x%E2%80%AEfer.flatpakref",
            "file:///x/%2E%2E/y.flatpakref",
            "file:///x%+A.flatpakref",
            "file:///x%A.flatpakref",
            "file:///x%.flatpakref",
        ] {
            assert_eq!(p(&[bad]).1.len(), 1, "{bad}");
        }
    }

    #[test]
    fn relative_paths_need_a_folder() {
        // The folder comes from outside too.
        assert_eq!(launch(&["a.flatpakref"], "/x/\u{202E}y").refused.len(), 1);
        let l = launch(&["a.flatpakref", "/abs/b.flatpakref"], "");
        assert_eq!(
            l.requests,
            vec![Request::File(FileKind::Ref, "/abs/b.flatpakref".into())]
        );
        assert_eq!(l.refused.len(), 1);
    }

    #[test]
    fn shown_is_safe() {
        let l = launch(
            &[
                "https://x.org/a?token=secret",
                "/x/\u{202E}a.txt",
                "flatpak+https://user:pw@host/x.flatpakref",
                "flatpak+https://u@h",
                "--app=https://x.org/?token=secret",
            ],
            "/",
        );
        assert_eq!(l.refused[4].arg, "--app=…");
        assert_eq!(
            launch(&["user:pw@host/x"], "/").refused[0].arg,
            "user:host/x"
        );
        assert_eq!(l.refused[0].arg, "https://x.org/a");
        assert!(!l.refused[1].arg.contains('\u{202E}'));
        assert_eq!(l.refused[2].arg, "flatpak+https://host/x.flatpakref");
        assert_eq!(l.refused[3].arg, "flatpak+https://h");
    }

    #[test]
    fn limits() {
        let many: Vec<String> = (0..200).map(|i| format!("--app=org.a.A{i}")).collect();
        let l = parse(&many, Path::new("/"));
        assert_eq!(l.requests.len(), MAX_REQUESTS);
        assert_eq!(l.dropped, 200 - MAX_REQUESTS);
        let bad: Vec<String> = (0..20).map(|_| "--evil".to_string()).collect();
        let l = parse(&bad, Path::new("/"));
        assert_eq!(
            (l.refused.len(), l.dropped),
            (MAX_REFUSED, 20 - MAX_REFUSED)
        );
    }

    #[test]
    fn several_in_order() {
        let (ok, bad) = p(&["--page", "installed", "--bogus", "appstream://org.a.B"]);
        assert_eq!(
            ok,
            vec![
                Request::Page(Page::Installed),
                Request::App("org.a.B".into())
            ]
        );
        assert_eq!(bad.len(), 1);
    }
}
