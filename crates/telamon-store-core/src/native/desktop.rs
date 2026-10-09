//! What a bundle shows the rest of the desktop, and the rewrite that makes it
//! work from where the Store puts it.
//!
//! A bundle's tree is a prefix (`bin/`, `share/`...). The desktop only looks
//! in `~/.local/share/{applications,icons,metainfo,dbus-1/services,
//! knotifications6}`, so the Store copies a few files there. Which files, and
//! under which names, is fixed here; anything else under `share/` stays in the
//! app's own folder and is read by the app itself.
//!
//! | In the bundle | Copied to (`$XDG_DATA_HOME/`) | Name rule |
//! |---|---|---|
//! | `share/applications/<id>.desktop` | `applications/` | exactly this one file |
//! | `share/icons/hicolor/<size>/apps/<icon>` | `icons/hicolor/<size>/apps/` | `.png`/`.svg` named `<id>` or `<id>-...`/`<id>_...` |
//! | `share/metainfo/<id>.metainfo.xml` | `metainfo/` | `<id>.metainfo.xml` or `<id>.appdata.xml` |
//! | `share/dbus-1/services/<name>.service` | `dbus-1/services/` | `<name>` is `<id>` or `<id>.<more>` |
//! | `share/knotifications6/telamon-<last part of id>[-_...].notifyrc` | `knotifications6/` | `telamon-` and the last part of the ID, as the framework names it |
//!
//! The names are the rule that keeps a bundle from replacing something that
//! is not its own (the user's folder comes before `/usr` in every search
//! path): a bundle can only ever write files that carry its app ID. On top of
//! the names:
//!
//! - **Reserved names**: an app ID or D-Bus name below `org.freedesktop.`,
//!   `org.kde.`, `org.gnome.` and the other [`RESERVED_NAMESPACES`] is
//!   refused.
//! - **The system's files** ([`check_system`]): nothing is copied to a path
//!   that exists in a system data folder, and the app ID and the D-Bus names a
//!   bundle declares must not be claimed by a system D-Bus service file (by
//!   its name or by the `Name` inside it).
//! - **Text files** (desktop entry, D-Bus service, notification file) are read
//!   strictly ([`strict_scan`]): a control character but TAB, a `\r` that is
//!   not part of a line end, a line that starts with white space, an odd key
//!   name, a repeated group or key refuses the file, so that GLib, KDE and the
//!   D-Bus daemon cannot read it differently from the Store. The desktop entry
//!   and the D-Bus service are rewritten: the first word of every `Exec` (a
//!   bare program name that must be a program in the bundle's `bin/`) becomes
//!   the absolute path under `<app>/current/bin/`, `TryExec`, `Path` and the
//!   keys that load or run something of the bundle's choosing
//!   (`X-KDE-Library`, `Implements`, `X-KDE-Wayland-Interfaces`... see
//!   `DROPPED_KEYS`) and any marker of another tool (`X-Telamon-*`,
//!   `X-Flatpak*`...) are dropped, and the Store's own `X-Telamon-Native-App`
//!   and `-Version` are added. `MimeType` and the other plain metadata are
//!   kept: a bundle may offer itself as a handler for a file type (the user's
//!   choice in `mimeapps.list` still decides the default).
//! - **Notification files** may not run a command (`Execute`) or write a log
//!   (`Logfile`); **metainfo** is one `<component>` for the app's own ID that
//!   replaces, extends and provides nothing else (read with limits, no
//!   DOCTYPE or entity); **icons** are PNGs of a sane size in the header or
//!   plain SVGs (`appimage::meta::icon_kind`), because Qt decodes them.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use super::dirfd::Dir;
use super::manifest::Manifest;
use super::{Error, err};
use crate::appimage::install::{escape_value, exec_arg};
use crate::keyfile::{KeyFile, Limits};
use crate::launch::hidden;

/// Largest file copied out of a bundle.
const MAX_EXPORT: usize = 1024 * 1024;
/// Most icons copied.
const MAX_ICONS: usize = 64;
/// Most files copied in all (the record holds 256).
const MAX_EXPORTS: usize = 200;
/// The marker the Store writes into the desktop entry and the D-Bus service.
pub const MARKER: &str = "X-Telamon-Native-App";
pub const MARKER_VERSION: &str = "X-Telamon-Native-Version";

/// Name spaces that belong to the desktop, the OS and other vendors' apps. An
/// app ID or a D-Bus name below one of them is refused: the user's folders come
/// before `/usr` in every search path and the session bus starts the first
/// service file it finds for a name, so a bundle that took one of these names
/// could stand in for a system component.
pub const RESERVED_NAMESPACES: &[&str] = &[
    "org.freedesktop.",
    "org.kde.",
    "org.gnome.",
    "org.gtk.",
    "org.mate.",
    "org.xfce.",
    "org.flatpak.",
    "org.fedoraproject.",
    "org.mozilla.",
    "com.canonical.",
];

/// Keys of a desktop entry that make another component load or run
/// something of the bundle's choosing, or hand the app privileges, and so are
/// not copied (with any `[locale]` suffix): KDE's service loader
/// (`X-KDE-Library`, `X-KDE-ServiceTypes`, `X-KDE-Protocols`, `X-KDE-Init`),
/// GNOME's search provider hook (`Implements`), KDE's request to run as
/// another user (`X-KDE-SubstituteUID`, `X-KDE-Username`) and its grant of
/// privileged Wayland and D-Bus interfaces (`X-KDE-Wayland-Interfaces`,
/// `X-KDE-DBUS-Restricted-Interfaces`).
const DROPPED_KEYS: &[&str] = &[
    "X-KDE-Library",
    "X-KDE-ServiceTypes",
    "Implements",
    "X-KDE-Protocols",
    "X-KDE-Init",
    "X-KDE-SubstituteUID",
    "X-KDE-Username",
    "X-KDE-Wayland-Interfaces",
    "X-KDE-DBUS-Restricted-Interfaces",
];

/// Key prefixes that are other tools' markers: the Store's own (written
/// afresh), Flatpak's, Snap's and the AppImage tools', which could make
/// another tool treat the entry as its own.
const DROPPED_PREFIXES: &[&str] = &[
    "X-Telamon-",
    "X-Flatpak",
    "X-Snap",
    "X-AppImage",
    "X-KDE-PluginInfo",
];

/// A file the Store copies out of the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// Where it was in the tree.
    pub from: String,
    /// Where it goes, relative to `$XDG_DATA_HOME`.
    pub to: String,
    /// Its content as it will be written (rewritten for the desktop entry and
    /// the D-Bus service).
    pub bytes: Vec<u8>,
}

/// What `plan` found out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub exports: Vec<Export>,
    /// The program Open runs: `bin/<name>`, the first word of the desktop
    /// entry's `Exec`.
    pub exe: String,
    /// The D-Bus names the bundle's service files own.
    pub bus_names: Vec<String>,
}

fn limits() -> Limits {
    Limits {
        max_bytes: 64 * 1024,
        max_lines: 1000,
        max_groups: 32,
        max_keys: 400,
        max_value: 8192,
    }
}

fn bare_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 100
        && !n.starts_with('.')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// The programs in `bin/` (directly below it): files the manifest marks
/// executable, and links there that lead to one of those.
pub fn programs(m: &Manifest) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = m
        .files
        .iter()
        .filter(|f| f.executable)
        .filter_map(|f| f.path.strip_prefix("bin/"))
        .filter(|n| !n.contains('/'))
        .map(str::to_string)
        .collect();
    for l in &m.links {
        let Some(name) = l.path.strip_prefix("bin/").filter(|n| !n.contains('/')) else {
            continue;
        };
        // The target, read from the link's folder, lexically.
        let mut parts: Vec<&str> = vec!["bin"];
        for part in l.target.split('/') {
            match part {
                ".." => {
                    parts.pop();
                }
                p => parts.push(p),
            }
        }
        let target = parts.join("/");
        if m.files.iter().any(|f| f.executable && f.path == target) {
            out.insert(name.to_string());
        }
    }
    out
}

/// Splits an `Exec` value into its first word and the rest (kept as written).
/// The first word must be a bare name (no quotes, no path).
fn first_word(value: &str) -> Result<(&str, &str), Error> {
    let end = value.find([' ', '\t']).unwrap_or(value.len());
    let (word, rest) = value.split_at(end);
    if !bare_name_ok(word) {
        return Err(err(
            "The bundle's launcher must start its program by its plain name from bin/.",
        ));
    }
    Ok((word, rest))
}

/// The new `Exec` value: the program at `<prefix>/bin/<word>`, quoted as the
/// Desktop Entry spec wants, then the original arguments. A folder name that
/// is not text or holds a control character cannot be quoted so that it reads
/// back the same, and is refused.
fn exec_value(
    value: &str,
    prefix: &Path,
    programs: &BTreeSet<String>,
) -> Result<(String, String), Error> {
    let (word, rest) = first_word(value)?;
    if !programs.contains(word) {
        return Err(err(
            "The bundle's launcher starts a program the bundle does not contain.",
        ));
    }
    let path = prefix.join("bin").join(word);
    let Some(path) = path.to_str().filter(|p| !p.chars().any(hidden)) else {
        return Err(err(
            "The folder the app goes in has a name the Store cannot put in a launcher.",
        ));
    };
    Ok((
        format!("{}{rest}", escape_value(&exec_arg(path))),
        word.to_string(),
    ))
}

/// Checks the text of a desktop entry, a D-Bus service or a notification file
/// before anything is read from it, and refuses what different readers (GLib,
/// KDE's `KConfig`, the D-Bus daemon, scripts that split lines their own way)
/// could read differently, so the Store never copies one thing while a reader
/// sees another:
///
/// - any control character but TAB, and a `\r` that is not part of `\r\n`,
///   and the characters that end or hide a line (see `launch::hidden`);
/// - a line that starts with white space;
/// - a key that is not ASCII letters, digits and `-` (and an optional
///   `[locale]`), a group header that is not exactly `[name]`;
/// - the same group twice, or the same key twice in a group.
fn strict_scan<'a>(src: &'a [u8], what: &str) -> Result<&'a str, Error> {
    let bad = || {
        err(format!(
            "The bundle's {what} has a character or a line the Store won't copy."
        ))
    };
    let text =
        std::str::from_utf8(src).map_err(|_| err(format!("The bundle's {what} is not text.")))?;
    let mut after_cr = false;
    for c in text.chars() {
        if after_cr && c != '\n' {
            return Err(bad());
        }
        after_cr = c == '\r';
        if !after_cr && c != '\n' && c != '\t' && hidden(c) {
            return Err(bad());
        }
    }
    if after_cr {
        return Err(bad());
    }
    let mut groups: HashSet<&str> = HashSet::new();
    let mut keys: HashSet<(&str, &str)> = HashSet::new();
    let mut group = "";
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Some(first) = line.chars().next() else {
            continue;
        };
        if first.is_whitespace() {
            return Err(bad());
        }
        if first == '#' {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some(name) = rest.strip_suffix(']') else {
                return Err(bad());
            };
            if name.is_empty() || name.contains(['[', ']']) || !groups.insert(name) {
                return Err(bad());
            }
            group = name;
            continue;
        }
        let Some((key, _)) = line.split_once('=') else {
            return Err(bad());
        };
        let key = key.trim_end_matches([' ', '\t']);
        if !simple_key(key) || !keys.insert((group, key)) {
            return Err(bad());
        }
    }
    Ok(text)
}

/// `Name` or `Name[locale]`: ASCII letters, digits and `-` for the name, and
/// letters, digits and `_@.$-` for the locale (`$` for KDE's `[$e]`).
fn simple_key(key: &str) -> bool {
    let (name, locale) = match key.split_once('[') {
        Some((n, l)) => match l.strip_suffix(']') {
            Some(l) => (n, Some(l)),
            None => return false,
        },
        None => (key, None),
    };
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && locale.is_none_or(|l| {
            !l.is_empty()
                && l.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'@' | b'.' | b'$' | b'-')
                })
        })
}

/// Whether a desktop-entry key is one the Store does not copy.
fn dropped_key(key: &str) -> bool {
    let base = key.split('[').next().unwrap_or(key);
    base == "TryExec"
        || base == "Path"
        || DROPPED_KEYS.contains(&base)
        || DROPPED_PREFIXES.iter().any(|p| base.starts_with(p))
}

/// Rewrites the bundle's desktop entry. Returns the text and the program the
/// first `Exec` of the main group starts.
pub fn rewrite_desktop(
    src: &[u8],
    id: &str,
    version: &str,
    prefix: &Path,
    programs: &BTreeSet<String>,
) -> Result<(Vec<u8>, String), Error> {
    let bad = |_| err("The bundle's desktop entry is not valid.");
    let text = strict_scan(src, "desktop entry")?;
    let kf = KeyFile::parse(src, &limits()).map_err(bad)?;
    if kf.groups().next() != Some("Desktop Entry") {
        return Err(err(
            "The bundle's desktop entry does not start with [Desktop Entry].",
        ));
    }
    if kf.raw("Desktop Entry", "Type") != Some("Application") {
        return Err(err("The bundle's desktop entry is not an application."));
    }
    let mut out = String::new();
    let mut group = String::new();
    let mut main_done = false;
    let mut exe: Option<String> = None;
    let mut exe_main: Option<String> = None;
    let marker = |out: &mut String| {
        out.push_str(&format!("{MARKER}={id}\n{MARKER_VERSION}={version}\n"));
    };
    for raw in text.split_inclusive('\n') {
        // The scan allows `\r` only in front of `\n`.
        let line = raw.trim_end_matches(['\n', '\r']);
        if let Some(rest) = line.strip_prefix('[')
            && let Some(name) = rest.split(']').next()
        {
            if group == "Desktop Entry" && !main_done {
                marker(&mut out);
                main_done = true;
            }
            group = name.to_string();
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(err("The bundle's desktop entry is not valid."));
        };
        let key = key.trim_end_matches([' ', '\t']);
        let value = value.trim_start_matches([' ', '\t']);
        if key.starts_with("Exec[") || key.starts_with("TryExec[") || key.starts_with("Path[") {
            return Err(err(
                "The bundle's desktop entry has a launcher key the Store won't copy.",
            ));
        }
        if key == "Exec" {
            let (new, word) = exec_value(value, prefix, programs)?;
            if group == "Desktop Entry" {
                exe_main = Some(word.clone());
            }
            exe.get_or_insert(word);
            out.push_str(&format!("Exec={new}\n"));
        } else if !dropped_key(key) {
            out.push_str(&format!("{key}={value}\n"));
        }
    }
    if group == "Desktop Entry" && !main_done {
        marker(&mut out);
    }
    let exe = exe_main
        .or(exe)
        .ok_or_else(|| err("The bundle's desktop entry has no Exec."))?;
    // What was written must read back as a valid entry with our marker.
    let check = KeyFile::parse(out.as_bytes(), &limits()).map_err(bad)?;
    if check.raw("Desktop Entry", MARKER) != Some(id) {
        return Err(err("The bundle's desktop entry is not valid."));
    }
    Ok((out.into_bytes(), exe))
}

/// Rewrites the bundle's D-Bus service file: `Name` and `Exec` only.
pub fn rewrite_dbus(
    src: &[u8],
    id: &str,
    version: &str,
    prefix: &Path,
    programs: &BTreeSet<String>,
) -> Result<(Vec<u8>, String), Error> {
    let bad = |_| err("The bundle's D-Bus service file is not valid.");
    strict_scan(src, "D-Bus service file")?;
    let kf = KeyFile::parse(src, &limits()).map_err(bad)?;
    let name = kf
        .raw("D-BUS Service", "Name")
        .ok_or_else(|| err("The bundle's D-Bus service file has no Name."))?;
    let exec = kf
        .raw("D-BUS Service", "Exec")
        .ok_or_else(|| err("The bundle's D-Bus service file has no Exec."))?;
    let owns = name == id || name.strip_prefix(id).is_some_and(|r| r.starts_with('.'));
    if !owns || !super::valid_app_id(name) {
        return Err(err(
            "The bundle's D-Bus service is not named after the app.",
        ));
    }
    let (value, _) = exec_value(exec, prefix, programs)?;
    let text = format!(
        "[D-BUS Service]\nName={name}\nExec={value}\n{MARKER}={id}\n{MARKER_VERSION}={version}\n"
    );
    Ok((text.into_bytes(), name.to_string()))
}

/// A notification file may name sounds, icons and the places a notification
/// appears (`Popup`, `Sound`, `Taskbar`). It may not run a command
/// (`Execute`, `Action=Execute`) or write to a file (`Logfile`): the file is
/// read by the notification service whenever the app, or another app that
/// shares its name, notifies.
fn check_notifyrc(bytes: &[u8]) -> Result<(), Error> {
    let what = "notification file";
    let text = strict_scan(bytes, what)?;
    let kf = KeyFile::parse(bytes, &limits())
        .map_err(|_| err("The bundle's notification file is not valid."))?;
    let refused = |s: &str| {
        let s = s.to_ascii_lowercase();
        s.contains("execute") || s.contains("logfile")
    };
    // By lines as well as by the parsed keys, in case a reader splits them
    // differently.
    if text.lines().any(|l| {
        l.split_once('=')
            .is_some_and(|(k, v)| refused(k) || (k.trim().starts_with("Action") && refused(v)))
    }) {
        return Err(err(
            "The bundle's notification file runs a command or writes a log file.",
        ));
    }
    let groups: Vec<String> = kf.groups().map(str::to_string).collect();
    for g in groups {
        for key in kf.all_keys(&g) {
            let base = key.split('[').next().unwrap_or(key);
            let action = base == "Action" && kf.raw(&g, key).is_some_and(refused);
            if refused(base) || action {
                return Err(err(
                    "The bundle's notification file runs a command or writes a log file.",
                ));
            }
        }
    }
    Ok(())
}

/// A metainfo file describes the bundle's own app to software centers, which
/// read the user's folder before the system's. It must be one `<component>`
/// whose `<id>` is the app ID (with an optional `.desktop`), replace or extend
/// nothing, and provide only its own IDs and bus names. Read with limits: at
/// most 1 MiB, 32 levels deep, no DOCTYPE, no entity but the predefined five,
/// no processing instruction.
fn check_metainfo(bytes: &[u8], id: &str) -> Result<(), Error> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    if bytes.len() > MAX_EXPORT {
        return Err(metainfo_bad("is too large"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| metainfo_bad("is not text"))?;
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return Err(metainfo_bad("has a control character"));
    }
    let mut rd = Reader::from_str(text);
    let mut st = MetaState {
        id,
        stack: Vec::new(),
        roots: 0,
        component_id: false,
        capture: None,
    };
    loop {
        match rd
            .read_event()
            .map_err(|_| metainfo_bad("is not valid XML"))?
        {
            Event::Eof => break,
            Event::DocType(_) => return Err(metainfo_bad("has a DOCTYPE")),
            Event::PI(_) => return Err(metainfo_bad("has a processing instruction")),
            Event::Decl(_) | Event::Comment(_) => {}
            Event::GeneralRef(r) => {
                if st.capture.is_some() {
                    return Err(metainfo_bad("has an entity where an ID is read"));
                }
                if !r.is_char_ref() && quick_xml::escape::resolve_predefined_entity(&r).is_none() {
                    return Err(metainfo_bad("has an entity"));
                }
            }
            Event::Text(t) => {
                if let Some((_, c)) = st.capture.as_mut() {
                    c.push_str(&t);
                }
            }
            Event::CData(t) => {
                if let Some((_, c)) = st.capture.as_mut() {
                    c.push_str(&t);
                }
            }
            Event::Start(e) => st.start(&e)?,
            Event::Empty(e) => {
                st.start(&e)?;
                st.finish()?;
            }
            Event::End(_) => st.finish()?,
        }
    }
    if st.roots != 1 || !st.component_id {
        return Err(metainfo_bad("must be one <component> with the app's ID"));
    }
    Ok(())
}

fn metainfo_bad(why: &str) -> Error {
    err(format!("The bundle's metainfo file {why}."))
}

/// Where `check_metainfo` is in the document.
struct MetaState<'a> {
    id: &'a str,
    stack: Vec<String>,
    roots: usize,
    component_id: bool,
    /// What the text of the open element is for, and the text so far.
    capture: Option<(&'static str, String)>,
}

impl MetaState<'_> {
    fn own(&self, s: &str) -> bool {
        s == self.id || s.strip_suffix(".desktop") == Some(self.id)
    }

    fn own_bus(&self, s: &str) -> bool {
        s == self.id || s.strip_prefix(self.id).is_some_and(|r| r.starts_with('.'))
    }

    fn start(&mut self, e: &quick_xml::events::BytesStart<'_>) -> Result<(), Error> {
        let name = e.name().as_ref().to_string();
        if self.capture.is_some() {
            return Err(metainfo_bad("has markup where an ID is read"));
        }
        if self.stack.len() >= 32 {
            return Err(metainfo_bad("is nested too deeply"));
        }
        if self.stack.is_empty() {
            self.roots += 1;
            if name != "component" || self.roots > 1 {
                return Err(metainfo_bad("must be one <component>"));
            }
        } else if name == "component" {
            return Err(metainfo_bad("has a component inside a component"));
        }
        if name == "replaces" || name == "extends" {
            return Err(metainfo_bad("replaces or extends another component"));
        }
        let path: Vec<&str> = self.stack.iter().map(String::as_str).collect();
        self.capture = match (path.as_slice(), name.as_str()) {
            (["component"], "id") => Some(("id", String::new())),
            (["component", "provides"], "id") => Some(("provides-id", String::new())),
            (["component", "provides"], "dbus") => Some(("dbus", String::new())),
            (["component"], "launchable") => {
                let mut desktop = false;
                for a in e.attributes().flatten() {
                    if a.key.as_ref() == "type" {
                        if a.value.contains('&') {
                            return Err(metainfo_bad("has an entity where an ID is read"));
                        }
                        desktop = a.value == "desktop-id";
                    }
                }
                desktop.then(|| ("launchable", String::new()))
            }
            _ => None,
        };
        self.stack.push(name);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Error> {
        self.stack.pop();
        let Some((kind, text)) = self.capture.take() else {
            return Ok(());
        };
        let text = text.trim();
        match kind {
            "id" => {
                if self.component_id || !self.own(text) {
                    return Err(metainfo_bad("must name the app's own ID"));
                }
                self.component_id = true;
            }
            "provides-id" if !self.own(text) => {
                return Err(metainfo_bad("provides an ID that is not the app's"));
            }
            "dbus" if !self.own_bus(text) => {
                return Err(metainfo_bad("provides a bus name that is not the app's"));
            }
            "launchable" if text != format!("{}.desktop", self.id) => {
                return Err(metainfo_bad("launches something that is not the app"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// Icons are decoded by Qt in the Store and in the desktop shell: a PNG with
/// a sane size in its header, or a small plain SVG (the checks of
/// `appimage::meta::icon_kind`), and the file extension says which.
fn check_icon(file: &str, bytes: &[u8]) -> Result<(), Error> {
    use crate::appimage::meta::{IconKind, icon_kind};
    let want = if file.ends_with(".png") {
        IconKind::Png
    } else {
        IconKind::Svg
    };
    if icon_kind(bytes) == Some(want) {
        Ok(())
    } else {
        Err(err(
            "An icon in the bundle is not a plain PNG (up to 2048 pixels each way) or a plain SVG.",
        ))
    }
}

/// `telamon-<last part of the ID>.notifyrc`, optionally with a `-` or `_`
/// suffix before the extension: the name the framework gives an app's
/// notification events, so one app cannot take another's.
fn notifyrc_name_ok(file: &str, id: &str) -> bool {
    let Some(stem) = file.strip_suffix(".notifyrc") else {
        return false;
    };
    let last = id.rsplit('.').next().unwrap_or("");
    let base = format!("telamon-{last}");
    bare_name_ok(stem)
        && (stem == base
            || stem
                .strip_prefix(&base)
                .is_some_and(|r| r.starts_with('-') || r.starts_with('_')))
}

fn icon_name_ok(file: &str, id: &str) -> bool {
    let Some((stem, ext)) = file.rsplit_once('.') else {
        return false;
    };
    matches!(ext, "png" | "svg")
        && (stem == id
            || stem
                .strip_prefix(id)
                .is_some_and(|r| r.starts_with('-') || r.starts_with('_')))
}

fn icon_dir_ok(dir: &str) -> bool {
    dir == "scalable"
        || dir.split_once('x').is_some_and(|(w, h)| {
            let digits =
                |s: &str| !s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit());
            digits(w) && digits(h)
        })
}

/// Whether `name` is in one of the [`RESERVED_NAMESPACES`].
pub fn reserved_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    RESERVED_NAMESPACES.iter().any(|p| lower.starts_with(p))
}

/// Decides what is copied out of the bundle at `tree` (an open folder: every
/// file is read through it without following a link), and reads, checks and
/// rewrites it. `prefix` is the app's folder as the desktop will run it
/// (`<data>/telamon-apps/<id>/current`). Errors name the first thing that is
/// not allowed.
pub fn plan(tree: &Dir, m: &Manifest, prefix: &Path) -> Result<Plan, Error> {
    let id = &m.id;
    if reserved_name(id) {
        return Err(err(format!(
            "The app ID {id} is in a name space that belongs to the desktop or another vendor."
        )));
    }
    let progs = programs(m);
    let mut exports = Vec::new();
    let mut bus_names = Vec::new();
    let mut exe = None;
    let mut icons = 0usize;
    let mut desktop = false;
    let read = |rel: &str, max: usize| -> Result<Vec<u8>, Error> {
        match tree.read_at(rel, max as u64) {
            Ok(Some(b)) => Ok(b),
            _ => Err(err("A file the bundle shows the desktop cannot be read.")),
        }
    };
    let listed = m
        .files
        .iter()
        .map(|f| f.path.as_str())
        .chain(m.links.iter().map(|l| l.path.as_str()));
    for path in listed {
        let Some(rest) = path.strip_prefix("share/") else {
            continue;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        let is_link = m.links.iter().any(|l| l.path == path);
        match parts.as_slice() {
            ["applications", file] => {
                if *file != format!("{id}.desktop") || is_link {
                    return Err(err(
                        "The bundle's share/applications may hold only <app ID>.desktop.",
                    ));
                }
                let (bytes, e) =
                    rewrite_desktop(&read(path, 64 * 1024)?, id, &m.version, prefix, &progs)?;
                exe = Some(e);
                desktop = true;
                exports.push(Export {
                    from: path.into(),
                    to: format!("applications/{file}"),
                    bytes,
                });
            }
            ["icons", "hicolor", dir, "apps", file]
                if icon_dir_ok(dir) && icon_name_ok(file, id) =>
            {
                if is_link {
                    return Err(err("The bundle's icons must be files, not links."));
                }
                icons += 1;
                if icons > MAX_ICONS {
                    return Err(err("The bundle has too many icons."));
                }
                let bytes = read(path, MAX_EXPORT)?;
                check_icon(file, &bytes)?;
                exports.push(Export {
                    from: path.into(),
                    to: format!("icons/hicolor/{dir}/apps/{file}"),
                    bytes,
                });
            }
            ["icons", ..] => {
                return Err(err(
                    "The bundle's share/icons may hold only icons in hicolor/<size>/apps named after the app ID.",
                ));
            }
            ["metainfo", file] => {
                if (*file != format!("{id}.metainfo.xml") && *file != format!("{id}.appdata.xml"))
                    || is_link
                {
                    return Err(err(
                        "The bundle's share/metainfo may hold only <app ID>.metainfo.xml.",
                    ));
                }
                let bytes = read(path, MAX_EXPORT)?;
                check_metainfo(&bytes, id)?;
                exports.push(Export {
                    from: path.into(),
                    to: format!("metainfo/{file}"),
                    bytes,
                });
            }
            ["dbus-1", "services", file] => {
                let Some(name) = file.strip_suffix(".service") else {
                    return Err(err(
                        "The bundle's share/dbus-1/services holds a file that is not a .service.",
                    ));
                };
                if is_link {
                    return Err(err("The bundle's D-Bus services must be files, not links."));
                }
                let (bytes, bus) =
                    rewrite_dbus(&read(path, 64 * 1024)?, id, &m.version, prefix, &progs)?;
                if bus != name {
                    return Err(err(
                        "The bundle's D-Bus service file is not named after its bus name.",
                    ));
                }
                if reserved_name(&bus) {
                    return Err(err(format!(
                        "The D-Bus name {bus} is in a name space that belongs to the desktop or another vendor."
                    )));
                }
                bus_names.push(bus);
                exports.push(Export {
                    from: path.into(),
                    to: format!("dbus-1/services/{file}"),
                    bytes,
                });
            }
            ["dbus-1", ..] => {
                return Err(err(
                    "The bundle's share/dbus-1 may hold only session services.",
                ));
            }
            ["knotifications6", file] => {
                if !notifyrc_name_ok(file, id) || is_link {
                    return Err(err(
                        "The bundle's share/knotifications6 may hold only telamon-<app name>.notifyrc files named after the app.",
                    ));
                }
                let bytes = read(path, MAX_EXPORT)?;
                check_notifyrc(&bytes)?;
                exports.push(Export {
                    from: path.into(),
                    to: format!("knotifications6/{file}"),
                    bytes,
                });
            }
            _ => {}
        }
    }
    if exports.len() > MAX_EXPORTS {
        return Err(err("The bundle shows the desktop too many files."));
    }
    let (true, Some(exe)) = (desktop, exe) else {
        return Err(err(
            "The bundle has no desktop entry (share/applications/<app ID>.desktop).",
        ));
    };
    exports.sort_by(|a, b| a.to.cmp(&b.to));
    bus_names.sort();
    Ok(Plan {
        exports,
        exe: format!("bin/{exe}"),
        bus_names,
    })
}

/// Most `.service` files read from the system's D-Bus folders, and the most
/// bytes of each.
const MAX_SYSTEM_SERVICES: usize = 2000;
const MAX_SYSTEM_SERVICE_BYTES: u64 = 64 * 1024;

/// The bundle must not stand in for something the system already provides.
/// The user's data folder comes first in every search path, so a file of the
/// bundle with the same relative path as one in a system data folder would
/// replace it, and the session bus starts the first service file it finds for
/// a name. Refused when:
///
/// - any file the bundle would copy exists, at the same relative path, in a
///   system data folder (`dirs`: `/usr/share`, `/usr/local/share`, Flatpak's
///   exports...);
/// - the app ID, or a D-Bus name the bundle declares, is claimed (by file name
///   or by the `Name` inside) by a system `dbus-1/services/*.service` file;
///   a service file the Store cannot read or understand claims nothing, and
///   more than 2000 of them are too many to check (refused).
pub fn check_system(plan: &Plan, id: &str, system: &[PathBuf]) -> Result<(), Error> {
    for dir in system {
        if dir
            .join("applications")
            .join(format!("{id}.desktop"))
            .exists()
            || dir
                .join("dbus-1/services")
                .join(format!("{id}.service"))
                .exists()
        {
            return Err(err(format!(
                "An app with the ID {id} is already on this computer. The Store won't install over it."
            )));
        }
        for e in &plan.exports {
            if std::fs::symlink_metadata(dir.join(&e.to)).is_ok() {
                return Err(err(format!(
                    "{} is already provided by the system. The Store won't install over it.",
                    e.to
                )));
            }
        }
    }
    let mut names: Vec<&str> = vec![id];
    names.extend(plan.bus_names.iter().map(String::as_str));
    let mut seen = 0usize;
    for dir in system {
        let svc = dir.join("dbus-1/services");
        let Ok(rd) = std::fs::read_dir(&svc) else {
            continue;
        };
        for entry in rd.flatten() {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            let Some(stem) = file.strip_suffix(".service") else {
                continue;
            };
            seen += 1;
            if seen > MAX_SYSTEM_SERVICES {
                return Err(err(
                    "There are too many D-Bus services on this computer to check the app's name against.",
                ));
            }
            if names.contains(&stem) {
                return Err(err(format!(
                    "The D-Bus name {stem} is already used by a service on this computer."
                )));
            }
            let Some(claimed) = system_service_name(&entry.path()) else {
                continue;
            };
            if names.contains(&claimed.as_str()) {
                return Err(err(format!(
                    "The D-Bus name {claimed} is already used by a service on this computer."
                )));
            }
        }
    }
    Ok(())
}

/// The `Name` of a system D-Bus service file, if it can be read (at most
/// 64 KiB; links are followed, Flatpak's exports are links).
fn system_service_name(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_SYSTEM_SERVICE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_SYSTEM_SERVICE_BYTES {
        return None;
    }
    let kf = KeyFile::parse(&bytes, &limits()).ok()?;
    kf.raw("D-BUS Service", "Name").map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "net.eterneon.telamon.gates";

    fn progs() -> BTreeSet<String> {
        ["telamon-gates".to_string()].into()
    }

    fn rewrite(text: &str) -> Result<(String, String), Error> {
        rewrite_desktop(
            text.as_bytes(),
            ID,
            "0.2.0",
            Path::new("/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current"),
            &progs(),
        )
        .map(|(b, e)| (String::from_utf8(b).unwrap(), e))
    }

    const GOOD: &str = "[Desktop Entry]\nType=Application\nName=Telamon Gates\nName[de]=Tor\nExec=telamon-gates %U\nTryExec=telamon-gates\nIcon=net.eterneon.telamon.gates\nTerminal=false\n\n[Desktop Action new]\nName=New Chat\nExec=telamon-gates --new\n";

    #[test]
    fn exec_becomes_absolute_and_marked() {
        let (text, exe) = rewrite(GOOD).unwrap();
        assert_eq!(exe, "telamon-gates");
        assert!(text.contains("Exec=/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current/bin/telamon-gates %U\n"), "{text}");
        assert!(text.contains("Exec=/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current/bin/telamon-gates --new\n"));
        assert!(!text.contains("TryExec"));
        assert!(text.contains("Name[de]=Tor"));
        assert!(text.contains(&format!("{MARKER}={ID}\n{MARKER_VERSION}=0.2.0\n")));
        // The marker sits in the main group, before the action.
        let marker = text.find(MARKER).unwrap();
        assert!(marker < text.find("[Desktop Action new]").unwrap());
        let kf = KeyFile::parse(text.as_bytes(), &limits()).unwrap();
        assert_eq!(kf.raw("Desktop Entry", MARKER), Some(ID));
    }

    #[test]
    fn a_home_with_a_space_is_quoted() {
        let (text, _) = rewrite_desktop(
            GOOD.as_bytes(),
            ID,
            "1",
            Path::new("/home/my user/apps/x/current"),
            &progs(),
        )
        .map(|(b, e)| (String::from_utf8(b).unwrap(), e))
        .unwrap();
        assert!(
            text.contains("Exec=\"/home/my user/apps/x/current/bin/telamon-gates\" %U"),
            "{text}"
        );
        // And it reads back as one argument.
        let line = text.lines().find(|l| l.starts_with("Exec=")).unwrap();
        let args = crate::appimage::install::split_exec(&line[5..].replace("\\\\", "\\")).unwrap();
        assert_eq!(args[0], "/home/my user/apps/x/current/bin/telamon-gates");
    }

    #[test]
    fn a_main_group_only_file_gets_its_marker() {
        let (text, _) =
            rewrite("[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates").unwrap();
        assert!(text.contains(MARKER), "{text}");
    }

    #[test]
    fn the_bundle_cannot_bring_its_own_marker_or_launcher_tricks() {
        let (text, _) = rewrite("[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nX-Telamon-Native-App=org.evil.app\nPath=/etc\n").unwrap();
        assert_eq!(text.matches(MARKER).count(), 1);
        assert!(text.contains(&format!("{MARKER}={ID}")));
        assert!(!text.contains("Path="));
    }

    #[test]
    fn launchers_that_leave_the_bundle_are_refused() {
        for exec in [
            "/usr/bin/konsole",
            "../../bin/sh",
            "\"telamon-gates\"",
            "env FOO=1 telamon-gates",
            "sh -c telamon-gates",
            "bin/telamon-gates",
            "other-program",
            "",
            ".hidden",
        ] {
            let text = format!("[Desktop Entry]\nType=Application\nName=X\nExec={exec}\n");
            assert!(rewrite(&text).is_err(), "{exec:?}");
        }
        for text in [
            "[Desktop Entry]\nType=Application\nName=X\n",
            "[Desktop Entry]\nType=Link\nName=X\nExec=telamon-gates\n",
            "Name=X\n[Desktop Entry]\nType=Application\nExec=telamon-gates\n",
            "[Other]\nType=Application\n[Desktop Entry]\nExec=telamon-gates\n",
            "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nExec[de]=sh\n",
        ] {
            assert!(rewrite(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn dbus_services_are_rewritten_to_name_and_exec() {
        let src = "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates --gapplication-service\nUser=root\nSystemdService=evil.service\n";
        let (bytes, name) =
            rewrite_dbus(src.as_bytes(), ID, "1", Path::new("/p/current"), &progs()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(name, ID);
        assert!(text.contains("Exec=/p/current/bin/telamon-gates --gapplication-service"));
        assert!(!text.contains("root") && !text.contains("Systemd"));
        for bad in [
            "[D-BUS Service]\nName=org.freedesktop.Notifications\nExec=telamon-gates\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gatesX\nExec=telamon-gates\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=/usr/bin/sh\n",
            "[D-BUS Service]\nExec=telamon-gates\n",
        ] {
            assert!(
                rewrite_dbus(bad.as_bytes(), ID, "1", Path::new("/p"), &progs()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_link_in_bin_to_a_program_is_a_program() {
        use crate::native::manifest::{FileEntry, LinkEntry};
        let mut m = crate::native::manifest::tests_sample();
        m.files = vec![FileEntry {
            path: "bin/real".into(),
            size: 1,
            sha256: "a".repeat(64),
            executable: true,
        }];
        m.links = vec![
            LinkEntry {
                path: "bin/alias".into(),
                target: "real".into(),
            },
            LinkEntry {
                path: "bin/other".into(),
                target: "../share/x".into(),
            },
        ];
        let p = programs(&m);
        assert!(
            p.contains("real") && p.contains("alias") && !p.contains("other"),
            "{p:?}"
        );
    }

    #[test]
    fn notification_files_are_named_after_the_app() {
        assert!(notifyrc_name_ok("telamon-gates.notifyrc", ID));
        assert!(notifyrc_name_ok("telamon-gates-extra.notifyrc", ID));
        for bad in [
            "telamon-store.notifyrc",
            "telamon-updater.notifyrc",
            "telamon-gatesx.notifyrc",
            "plasma.notifyrc",
            "telamon-gates.txt",
            "telamon-.notifyrc",
        ] {
            assert!(!notifyrc_name_ok(bad, ID), "{bad}");
        }
    }

    #[test]
    fn icons_must_carry_the_app_id() {
        assert!(icon_name_ok("net.eterneon.telamon.gates.svg", ID));
        assert!(icon_name_ok("net.eterneon.telamon.gates-symbolic.svg", ID));
        assert!(!icon_name_ok("document-save.svg", ID));
        assert!(!icon_name_ok("net.eterneon.telamon.gatesX.svg", ID));
        assert!(!icon_name_ok("net.eterneon.telamon.gates.xpm", ID));
        assert!(
            icon_dir_ok("48x48")
                && icon_dir_ok("scalable")
                && !icon_dir_ok("../x")
                && !icon_dir_ok("48")
        );
    }

    // ---- the text the Store copies is read the same by everyone ----

    #[test]
    fn control_characters_and_odd_line_ends_are_refused() {
        for (what, text) in [
            (
                "vertical tab",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nComment=a\x0Bb\n",
            ),
            (
                "form feed",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nComment=a\x0Cb\n",
            ),
            (
                "lone CR in a comment",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n# a\rExec=sh\n",
            ),
            (
                "lone CR before a key",
                "[Desktop Entry]\rType=Application\nName=X\nExec=telamon-gates\n",
            ),
            (
                "CR at the end",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\r",
            ),
            (
                "escape",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nComment=\x1b[2J\n",
            ),
            (
                "DEL",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nComment=\x7f\n",
            ),
            (
                "NEL",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nComment=a\u{85}Exec=sh\n",
            ),
            (
                "line separator",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nComment=a\u{2028}Exec=sh\n",
            ),
            (
                "paragraph separator",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n#a\u{2029}Exec=sh\n",
            ),
            (
                "byte order mark",
                "\u{feff}[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n",
            ),
            (
                "bidi override",
                "[Desktop Entry]\nType=Application\nName=X\u{202e}\nExec=telamon-gates\n",
            ),
            (
                "NBSP before a key",
                "[Desktop Entry]\nType=Application\nName=X\n\u{a0}Exec=telamon-gates\n",
            ),
            (
                "space before a key",
                "[Desktop Entry]\nType=Application\nName=X\n Exec=telamon-gates\n",
            ),
            (
                "tab before a key",
                "[Desktop Entry]\nType=Application\nName=X\n\tExec=telamon-gates\n",
            ),
            (
                "space before a group",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n [Other]\nExec=sh\n",
            ),
        ] {
            assert!(rewrite(text).is_err(), "{what}");
        }
        // TAB and CRLF are fine.
        let ok = "[Desktop Entry]\r\nType=Application\r\nName=\tX\r\nExec=telamon-gates\r\n";
        assert!(rewrite(ok).is_ok());
        // A TAB in the folder name cannot be written in a launcher.
        assert!(rewrite_desktop(GOOD.as_bytes(), ID, "1", Path::new("/a\tb"), &progs()).is_err());
    }

    #[test]
    fn a_service_file_with_control_characters_is_refused() {
        for bad in [
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates\n# x\rExec=sh\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates\x0B\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates\n\u{a0}User=root\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nName=net.eterneon.telamon.gates.Other\nExec=telamon-gates\n",
            "[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates\n[D-BUS Service]\nExec=sh\n",
        ] {
            assert!(
                rewrite_dbus(bad.as_bytes(), ID, "1", Path::new("/p"), &progs()).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn repeated_groups_and_keys_and_odd_keys_are_refused() {
        for (what, text) in [
            (
                "two main groups",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n[Desktop Entry]\nName=Y\n",
            ),
            (
                "two groups of another name",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n[Other]\na=1\n[Other]\nb=2\n",
            ),
            (
                "a repeated key",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nName=Y\n",
            ),
            (
                "a repeated Exec",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nExec=telamon-gates --x\n",
            ),
            (
                "a key with a space",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nExec [de]=sh\n",
            ),
            (
                "a key with a dot",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nA.B=1\n",
            ),
            (
                "a key with a non-ASCII letter",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\nÉxec=1\n",
            ),
            (
                "a group header with text after it",
                "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n[Other] x\n",
            ),
        ] {
            assert!(rewrite(text).is_err(), "{what}");
        }
        // Spaces around the equals sign, a locale and a TAB value are fine.
        assert!(
            rewrite("[Desktop Entry]\nType = Application\nName[zh_CN]=Z\nName = X\nExec =\ttelamon-gates\n").is_ok()
        );
    }

    #[test]
    fn an_exec_in_any_group_is_rewritten_or_refused() {
        let ok = "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n[X-Other Thing]\nExec=telamon-gates --y\n";
        let (text, _) = rewrite(ok).unwrap();
        assert_eq!(text.matches("Exec=/home/u/").count(), 2, "{text}");
        let bad = "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n[X-Other Thing]\nExec=sh -c evil\n";
        assert!(rewrite(bad).is_err());
        let bad = "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n[Desktop Action a]\nExec[de]=sh\n";
        assert!(rewrite(bad).is_err());
    }

    #[test]
    fn keys_that_load_or_run_something_are_not_copied() {
        let text = "[Desktop Entry]\nType=Application\nName=X\nExec=telamon-gates\n\
            X-KDE-Library=evil\nX-KDE-Library[de]=evil\nX-KDE-ServiceTypes=KParts/ReadOnlyPart\nImplements=org.gnome.Shell.SearchProvider2\n\
            X-KDE-Protocols=http,https\nX-KDE-Init=evil\nX-KDE-SubstituteUID=true\nX-KDE-Username=root\n\
            X-KDE-Wayland-Interfaces=org_kde_kwin_fake_input\nX-KDE-DBUS-Restricted-Interfaces=org.kde.kwin.Screenshot\n\
            X-Flatpak=org.mozilla.firefox\nX-Flatpak-Tags=x\nX-SnapInstanceName=x\nX-AppImage-Version=1\nX-Telamon-AppImage=true\n\
            X-KDE-PluginInfo-Name=x\n\
            MimeType=text/plain;\nKeywords=a;b;\nX-KDE-StartupNotify=true\nCategories=Utility;\nStartupNotify=true\n";
        let (out, _) = rewrite(text).unwrap();
        for dropped in [
            "X-KDE-Library",
            "X-KDE-ServiceTypes",
            "Implements",
            "X-KDE-Protocols",
            "X-KDE-Init",
            "X-KDE-SubstituteUID",
            "X-KDE-Username",
            "X-KDE-Wayland-Interfaces",
            "X-KDE-DBUS-Restricted-Interfaces",
            "X-Flatpak",
            "X-SnapInstanceName",
            "X-AppImage",
            "X-Telamon-AppImage",
            "X-KDE-PluginInfo",
        ] {
            assert!(!out.contains(dropped), "{dropped} was copied:\n{out}");
        }
        for kept in [
            "MimeType=text/plain;",
            "Keywords=a;b;",
            "X-KDE-StartupNotify=true",
            "Categories=Utility;",
            "StartupNotify=true",
        ] {
            assert!(out.contains(kept), "{kept} was dropped:\n{out}");
        }
        // The Store's marker is the only X-Telamon key.
        assert_eq!(out.matches("X-Telamon-").count(), 2, "{out}");
    }

    // ---- the quoted program path, against two independent readers ----

    /// The Desktop Entry specification's unquoting of an `Exec` value (after
    /// the key-file string was unescaped): double quotes group, inside them a
    /// backslash makes the next character literal, `%%` is a `%`.
    fn unquote_spec(v: &str) -> Option<Vec<String>> {
        let mut args = Vec::new();
        let mut cur = String::new();
        let mut started = false;
        let mut quoted = false;
        let mut it = v.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '"' => {
                    quoted = !quoted;
                    started = true;
                }
                '\\' if quoted => cur.push(it.next()?),
                '%' if it.peek() == Some(&'%') => {
                    it.next();
                    cur.push('%');
                    started = true;
                }
                ' ' | '\t' if !quoted => {
                    if started {
                        args.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                c => {
                    cur.push(c);
                    started = true;
                }
            }
        }
        if quoted {
            return None;
        }
        if started {
            args.push(cur);
        }
        Some(args)
    }

    /// GLib's `g_shell_parse_argv` (what GIO's launcher uses) after the same
    /// `%%` step: in double quotes a backslash escapes only `$`, `` ` ``, `"`,
    /// `\` and a newline, otherwise it stays; outside quotes it escapes
    /// anything; single quotes are literal.
    fn unquote_glib(v: &str) -> Option<Vec<String>> {
        let v = v.replace("%%", "%");
        let mut args = Vec::new();
        let mut cur = String::new();
        let mut started = false;
        let mut it = v.chars();
        while let Some(c) = it.next() {
            match c {
                ' ' | '\t' | '\n' => {
                    if started {
                        args.push(std::mem::take(&mut cur));
                        started = false;
                    }
                }
                '\'' => {
                    started = true;
                    loop {
                        match it.next()? {
                            '\'' => break,
                            c => cur.push(c),
                        }
                    }
                }
                '"' => {
                    started = true;
                    loop {
                        match it.next()? {
                            '"' => break,
                            '\\' => match it.next()? {
                                c @ ('$' | '`' | '"' | '\\') => cur.push(c),
                                '\n' => {}
                                c => {
                                    cur.push('\\');
                                    cur.push(c);
                                }
                            },
                            c => cur.push(c),
                        }
                    }
                }
                '\\' => {
                    started = true;
                    cur.push(it.next()?);
                }
                c => {
                    started = true;
                    cur.push(c);
                }
            }
        }
        if started {
            args.push(cur);
        }
        Some(args)
    }

    /// xorshift: a few thousand reproducible random folder names.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn the_program_path_always_comes_back_as_one_argument() {
        const ALPHABET: &[char] = &[
            ' ', ' ', '"', '\'', '$', '`', '\\', '\\', '%', '%', ';', 'é', '日', '#', '~', '(',
            ')', '>', '<', '|', '&', '*', '?', '{', '}', '!', '=', ',', '[', ']', 'a', 'b', 'Z',
            '1', '-', '.', '_',
        ];
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut prefixes: Vec<String> = Vec::new();
        for _ in 0..3000 {
            let parts = 1 + rng.next() % 4;
            let mut p = String::new();
            for _ in 0..parts {
                p.push('/');
                let n = 1 + rng.next() % 12;
                for _ in 0..n {
                    p.push(ALPHABET[(rng.next() % ALPHABET.len() as u64) as usize]);
                }
            }
            prefixes.push(p);
        }
        // Long ones: 2000 characters of everything awkward, and a deep one.
        prefixes.push(format!("/{}", "a b\"$`\\%;é".repeat(180)));
        prefixes.push(
            format!("/{}", "x/".repeat(900))
                .trim_end_matches('/')
                .to_string(),
        );
        let mut done = 0;
        for prefix in prefixes {
            let (bytes, _) =
                match rewrite_desktop(GOOD.as_bytes(), ID, "1", Path::new(&prefix), &progs()) {
                    Ok(v) => v,
                    Err(e) => panic!("{prefix:?}: {e}"),
                };
            let text = String::from_utf8(bytes).unwrap();
            for line in text.lines().filter(|l| l.starts_with("Exec=")) {
                let kf_text = format!("[Desktop Entry]\n{line}\n");
                let exec = KeyFile::parse(kf_text.as_bytes(), &limits())
                    .unwrap()
                    .string("Desktop Entry", "Exec")
                    .unwrap()
                    .unwrap();
                let a = unquote_spec(&exec).unwrap();
                let b = unquote_glib(&exec).unwrap();
                assert_eq!(a, b, "{exec:?}");
                assert_eq!(a[0], format!("{prefix}/bin/telamon-gates"), "{line}");
                assert_eq!(a.len(), 2, "{line}");
                assert!(a[1] == "%U" || a[1] == "--new", "{line}");
            }
            done += 1;
        }
        assert_eq!(done, 3002);
    }

    #[test]
    fn a_folder_name_that_cannot_be_quoted_is_refused_not_changed() {
        for prefix in [
            "/home/a\nb/x",
            "/home/a\tb\x07/x",
            "/home/\u{202e}x",
            "/home/a\u{2028}b",
        ] {
            assert!(
                rewrite_desktop(GOOD.as_bytes(), ID, "1", Path::new(prefix), &progs()).is_err(),
                "{prefix:?}"
            );
        }
        // Not text.
        use std::os::unix::ffi::OsStrExt;
        let odd = Path::new(std::ffi::OsStr::from_bytes(b"/home/\xffx"));
        assert!(rewrite_desktop(GOOD.as_bytes(), ID, "1", odd, &progs()).is_err());
        // Too long to read back (the key-file value limit): an error, never a wrong path.
        let long = format!("/{}", "\\".repeat(5000));
        assert!(rewrite_desktop(GOOD.as_bytes(), ID, "1", Path::new(&long), &progs()).is_err());
    }

    // ---- notification files and metainfo ----

    #[test]
    fn a_notification_file_may_not_run_a_command_or_write_a_log() {
        let ok = b"[Global]\nIconName=x\nComment=Telamon Gates\n\n[Event/m]\nName=Message\nAction=Popup|Sound\nSound=message-new\n";
        assert!(check_notifyrc(ok).is_ok());
        for bad in [
            "[Event/m]\nAction=Execute\nExecute=/usr/bin/konsole\n",
            "[Event/m]\nAction=Popup|Execute\n",
            "[Event/m]\nAction=popup|EXECUTE\n",
            "[Event/m]\nExecute=sh\n",
            "[Event/m]\nExecute[de]=sh\n",
            "[Event/m]\nexecute=sh\n",
            "[Event/m]\nAction[$e]=Execute\n",
            "[Event/m]\nAction=Logfile\nLogfile=/home/u/.bashrc\n",
            "[Event/m]\nLogfile=/home/u/.bashrc\n",
            "[Event/m]\nAction=Popup\n[Event/m]\nAction=Execute\n",
            "[Event/m]\nAction=Popup\n# x\rExecute=sh\n",
            "not a key file\n",
            "Action=Popup\n",
        ] {
            assert!(check_notifyrc(bad.as_bytes()).is_err(), "{bad:?}");
        }
    }

    fn meta(body: &str) -> Vec<u8> {
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<component type=\"desktop-application\">{body}</component>\n").into_bytes()
    }

    #[test]
    fn metainfo_is_one_component_with_the_apps_own_id() {
        for ok in [
            meta(&format!("<id>{ID}</id><name>x</name>")),
            meta(&format!("<id>{ID}.desktop</id>")),
            meta(&format!(
                "<id>{ID}</id><provides><id>{ID}.desktop</id><dbus type=\"session\">{ID}.Service</dbus><binary>telamon-gates</binary></provides><launchable type=\"desktop-id\">{ID}.desktop</launchable>"
            )),
            meta(&format!("<id><![CDATA[{ID}]]></id>")),
            meta(&format!(
                "<id>{ID}</id><description><p>a &amp; b &lt; &#65;</p></description><launchable type=\"service\">x.service</launchable>"
            )),
        ] {
            check_metainfo(&ok, ID)
                .unwrap_or_else(|e| panic!("{}: {e}", String::from_utf8_lossy(&ok)));
        }
        let long_ok = format!("<id>{ID}</id>{}", "<a>".repeat(30) + &"</a>".repeat(30));
        check_metainfo(&meta(&long_ok), ID).unwrap();
        let deep = format!("<id>{ID}</id>{}", "<a>".repeat(40) + &"</a>".repeat(40));
        let huge = format!(
            "<id>{ID}</id><description>{}</description>",
            "x".repeat(1024 * 1024)
        );
        let nested = |inner: &str| meta(&format!("<id>{ID}</id>{inner}"));
        for (what, bad) in [
            ("no id", meta("<name>x</name>")),
            ("another id", meta("<id>org.mozilla.firefox</id>")),
            ("an id that only starts with ours", meta(&format!("<id>{ID}x</id>"))),
            ("two ids", meta(&format!("<id>{ID}</id><id>{ID}</id>"))),
            ("an id with a child", meta(&format!("<id>{ID}<b>x</b></id>"))),
            ("an id with an entity", meta(&format!("<id>{ID}&#x2e;x</id>"))),
            ("replaces", nested("<replaces><id>org.kde.dolphin</id></replaces>")),
            ("extends", nested("<extends>org.kde.dolphin</extends>")),
            ("a foreign provided id", nested("<provides><id>org.kde.dolphin</id></provides>")),
            ("a foreign provided bus name", nested("<provides><dbus type=\"session\">org.freedesktop.Notifications</dbus></provides>")),
            ("a foreign launchable", nested("<launchable type=\"desktop-id\">org.kde.dolphin.desktop</launchable>")),
            ("a launchable type hidden in an entity", nested("<launchable type=\"desktop&#45;id\">org.kde.dolphin.desktop</launchable>")),
            ("a component in a component", nested("<component><id>x</id></component>")),
            ("too deep", meta(&deep)),
            ("too large", meta(&huge)),
            ("a doctype", format!("<?xml version=\"1.0\"?><!DOCTYPE component SYSTEM \"http://evil/x.dtd\"><component><id>{ID}</id></component>").into_bytes()),
            ("an internal entity", format!("<?xml version=\"1.0\"?><!DOCTYPE c [<!ENTITY e \"x\">]><component><id>{ID}</id><name>&e;</name></component>").into_bytes()),
            ("an undefined entity", meta(&format!("<id>{ID}</id><name>&e;</name>"))),
            ("a processing instruction", format!("<?xml version=\"1.0\"?><?xml-stylesheet href=\"http://evil/s.xsl\"?><component><id>{ID}</id></component>").into_bytes()),
            ("two roots", format!("<component><id>{ID}</id></component><component><id>{ID}</id></component>").into_bytes()),
            ("a root that is not a component", format!("<components><component><id>{ID}</id></component></components>").into_bytes()),
            ("not XML", b"<component><id>".to_vec()),
            ("mismatched tags", meta(&format!("<id>{ID}</ide>"))),
            ("not text", vec![0xff, 0xfe, b'<']),
            ("a NUL", meta(&format!("<id>{ID}</id><name>a\0b</name>"))),
        ] {
            assert!(check_metainfo(&bad, ID).is_err(), "{what}");
        }
    }

    #[test]
    fn icons_are_checked_for_what_qt_will_decode() {
        let png = |w: u32, h: u32| {
            let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
            b.extend_from_slice(&w.to_be_bytes());
            b.extend_from_slice(&h.to_be_bytes());
            b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
            b
        };
        assert!(check_icon("a.png", &png(512, 512)).is_ok());
        assert!(check_icon("a.png", &png(2048, 2048)).is_ok());
        for (what, name, bytes) in [
            ("too wide", "a.png", png(2049, 16)),
            ("a huge image", "a.png", png(60_000, 60_000)),
            ("an empty image", "a.png", png(0, 16)),
            ("not an image", "a.png", b"png".to_vec()),
            ("an SVG named png", "a.png", b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec()),
            ("a PNG named svg", "a.svg", png(16, 16)),
            ("an SVG that loads a file", "a.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"file:///etc/passwd\"/></svg>".to_vec()),
            ("an SVG with a script", "a.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>x</script></svg>".to_vec()),
            ("an SVG with an entity", "a.svg", b"<!DOCTYPE svg [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><svg/>".to_vec()),
            ("empty", "a.svg", Vec::new()),
        ] {
            assert!(check_icon(name, &bytes).is_err(), "{what}");
        }
    }

    #[test]
    fn reserved_names() {
        for r in [
            "org.freedesktop.portal",
            "org.freedesktop.portal.Desktop",
            "org.kde.dolphin",
            "ORG.GNOME.Shell",
            "org.gtk.Settings",
            "org.mate.x",
            "org.xfce.x",
            "org.flatpak.Helper",
            "org.fedoraproject.x.y",
            "org.mozilla.firefox",
            "com.canonical.x.y",
        ] {
            assert!(reserved_name(r), "{r}");
        }
        for ok in [
            "net.eterneon.telamon.gates",
            "org.example.App",
            "org.kdex.a.b",
            "io.github.x.y",
        ] {
            assert!(!reserved_name(ok), "{ok}");
        }
    }
}
