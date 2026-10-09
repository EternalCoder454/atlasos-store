//! An app's Flatpak permissions from its `metadata` (and the user's
//! overrides): each one with a stable code, a risk level and one plain
//! wording. The metadata comes from a remote and is untrusted: it is read
//! with [`KeyFile`] (as flatpak reads it), the text shown is cleaned, and a
//! value this module doesn't know is kept as an `unknown:` item, never
//! dropped. The rule behind the table: what is unknown, or could reach
//! outside the sandbox, is High.
//!
//! # Risk
//!
//! | Risk   | What |
//! |--------|------|
//! | High   | **Filesystem** (any mode, in every spelling): `host`, `host-os`, `host-etc`, `host-root`, `/`; home (also `home:ro`) and its ancestors; `/tmp`, `/mnt`, `/media`, `/opt`, `/srv`, `/lib32`, `/libx32`, `/nix`, `/sysroot`, `/boot`, `/etc`, `/usr`, `/var`, `/run` and anything under them (and `/bin`, `/sbin`, `/lib*`, `/dev`, `/proc`, `/sys`); `xdg-run` and `xdg-run/pipewire-0` (direct PipeWire access), `xdg-run/gvfsd`, other sub-paths of it, `xdg-run/speech-dispatcher`, `xdg-run/gvfs` and `xdg-run/app/ID` when writable, `xdg-run/app/ID` of a password or chat app (`org.keepassxc.KeePassXC`, `com.bitwarden.desktop`, `im.riot.Riot`, `org.signal.Signal`, `com.onepassword.OnePassword`, `org.telegram.desktop`, `io.element.Element`, `org.mozilla.Thunderbird`, `org.gnome.seahorse.Application`; a best-effort list) or of an invalid ID, and every `/run/user/UID` path; `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.mozilla`, `~/.local/share/keyrings`, `~/.local/share/kwalletd`, `~/.netrc`, `~/.git-credentials`, `~/.password-store`, `~/.docker`, `~/.kube`, `~/.thunderbird`, `~/.pki`, `~/.var/app`, `~/.config/{mozilla,vivaldi,microsoft-edge,opera,containers,Element,1Password,Bitwarden,keepassxc,gcloud,gh,rclone,chromium,google-chrome,BraveSoftware,Signal,discord,kdeconnect}`, and every folder that holds one (so `xdg-config`, `xdg-data`, `~/.local` even read-only). **Escape locations** when read-write or `create`: the bare `xdg-cache`, any folder that holds an escape location, any dot entry directly in `~` except `.config`, `.local`, `.cache` and the allowlist `.themes`, `.icons`, `.fonts` ("settings that other programs run"), the editor, KDE and tool files listed in `ESCAPES` (`.vimrc`, `.config/nvim`, `.config/konsolerc`, `.gitconfig`, `.cargo/bin`, `.steam`, `.wine`, `.minecraft`, `.local/share/Steam`, `.local/share/lutris`, `~/go/bin`, `.var/app` and more), `xdg-config/{autostart,systemd,environment.d,plasma-workspace,fish}`, `xdg-data/{flatpak,applications,dbus-1,systemd,kservices5,kservices6,plasma}`, `~/.local/bin`, `~/bin`, and the shell startup files `~/.bashrc`, `~/.bash_profile`, `~/.bash_login`, `~/.profile`, `~/.zshrc`, `~/.zprofile`, `~/.zshenv`. A `..` that leaves its base, or an item that can't be read, is unknown. **Other**: `devices` all, input, kvm, usb (all of `/dev/bus/usb`: a program can take a keyboard or a disk over); `sockets` session-bus, system-bus, ssh-auth, gpg-agent; x11 or fallback-x11 without wayland; session bus talk or own to `org.freedesktop.Flatpak`, `org.freedesktop.systemd1`, `org.freedesktop.secrets`, `org.kde.kwalletd5/6`, `org.gnome.keyring*`, `ca.desrt.dconf`, `org.kde.klauncher5/6`, `org.kde.KWin`, `org.kde.plasmashell`, `org.kde.kded5/6`, `org.gnome.Shell`, `org.freedesktop.PackageKit`, `org.freedesktop.impl.portal.*`, `org.kde.kdeconnect*`, `org.kde.ksmserver`, `org.kde.krunner`, `org.kde.konsole*`, `org.kde.yakuake`, `org.kde.kglobalaccel`, `org.gnome.SettingsDaemon.*`, `org.gnome.Mutter.*`, or a wildcard that covers one of them (`org.kde.*`, `org.*`); system bus talk or own to any name; talk or own on the accessibility bus; any other `... Bus Policy` group; every unknown item |
//! | Medium | read-only `xdg-run/speech-dispatcher`, `xdg-run/gvfs` and `xdg-run/app/ID`; any read-only path with a non-ASCII character that would be Low; the bare `xdg-cache` read-only; an unlisted dot entry in `~` read-only; other paths and xdg folders read-write; `shared` network; `sockets` pulseaudio, pcsc, cups, x11 next to wayland; `features` devel, bluetooth, canbus; other session bus names (talk or own); own on a portal name; `[Environment]` that sets `LD_*` or `PATH`; `extra-data` hosts |
//! | Low    | `sockets` wayland, inherit-wayland-socket, fallback-x11 next to wayland; `shared` ipc; `devices` dri, shm; `features` multiarch, per-app-dev-shm; other paths and xdg folders read-only; `see` on any bus; talk to portal names (including `org.freedesktop.portal.Flatpak`); persistent; other environment variables; unset-environment |
//!
//! The lists of escape locations and credential folders can't be complete: a
//! program can always keep something that runs or a secret somewhere not
//! listed. So the bare `xdg-config`, `xdg-data` and home folder grants are
//! High whatever the lists say, and an unlisted dot entry in `~` is High when
//! writable and Medium when read-only.
//!
//! # Codes
//!
//! `share:network`, `socket:x11`, `device:dri`, `feature:devel`,
//! `filesystem:home:ro` (the canonical path, then `ro`, `rw` or `create`),
//! `persistent:.mozilla`, `unset-env:NAME`, `env:NAME`,
//! `session-bus:talk:org.freedesktop.Notifications`, `system-bus:own:name`,
//! `extra-data:host`, `unknown:...` for what isn't recognized, `runtime:ID`
//! (only in [`Permissions::added_since`]). A code carries the full value,
//! never a cut one: a part that isn't plain printable ASCII without spaces
//! and colons is written as `hex` and its bytes in hex.
//!
//! # Filesystem paths
//!
//! Flatpak's `xdg-*` folders follow `$XDG_*` and `/home/NAME` is another
//! user's home, so an item's identity is its literal text (slashes
//! normalized, as in the Updater): `home`, `~` and `/home/bob` are three
//! grants, and `!x` removes only the same literal. The canonical form below is
//! for ranking, wording and the code; several literals of one place show as
//! one item at the highest access.
//!
//! A path is made canonical before it is ranked, with no real home folder
//! needed: `.`, `..` and `//` are resolved lexically (a `..` that would leave
//! `~` or an `xdg-` folder makes the item unknown), `~/.config`,
//! `~/.local/share` and `~/.cache` are `xdg-config`, `xdg-data` and
//! `xdg-cache`, `/home/NAME`, `/var/home/NAME`, `/root` and `/var/roothome`
//! are `~`, `/run/user/UID` is `xdg-run`, and `/var/run` is `/run`.
//!
//! # Showing
//!
//! Every text comes cleaned and capped, and a path in it isolated with
//! FSI/PDI when it has right-to-left letters. The dialog that shows them:
//!
//! - uses `textFormat: Text.PlainText` and `clip: true` on each text;
//! - keeps the risk badge in its own item, which no text can paint over;
//! - never elides a wording, so the "(contains unusual characters)" ending
//!   stays visible.
//!
//! # Updates
//!
//! [`Permissions::added_since`] compares item by item, never by what one
//! grant covers, so it reports at least what Telamon Updater's
//! `new_permissions` does (`xdg-download` to `home` is new, and so is
//! `xdg-download` after `home`):
//!
//! - an item the old version didn't have is added;
//! - a wider access to the same filesystem path is added (`ro` < `rw` <
//!   `create`), a narrower one is not;
//! - a bus name at a higher level is added (`see` < `talk` < `own`), a lower
//!   one is not; a new socket, device or feature is added even if names on
//!   that bus were allowed before;
//! - an environment variable or a value of an other group is added when it is
//!   new or its value changed;
//! - a different runtime or SDK ID is added (a new branch is not);
//! - a new extra-data host is added, one that stays is not;
//! - an extra-data URI whose host can't be parsed counts, like an odd item a
//!   runtime brought, only when the old version lacks the same one;
//! - anything that couldn't be read the way flatpak reads it, and every
//!   localized key (`key[de]`), counts every time, as it can't be compared: it
//!   is held back, never waved through. The one exception is an item that only
//!   a runtime brought in with [`Permissions::with_runtime`]: it counts unless
//!   the old version had the identical one (the Updater doesn't read runtime
//!   metadata, so no parity is lost);
//! - an `unset-environment` entry counts as written, even if `[Environment]`
//!   overrides it (it isn't listed by [`Permissions::permissions`] then);
//! - a grant that got riskier with nothing new added (`x11` once `wayland` is
//!   gone) counts as added;
//! - taking a permission away is never an addition, and neither is a grant
//!   that only got less risky.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::keyfile::{KeyFile, KeyFileError, Limits};
use crate::text::{LineBuf, valid_id};

/// How much a permission puts at risk. `Low < Medium < High`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Risk {
    Low,
    Medium,
    High,
}

/// Why metadata or an override file was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermError {
    /// The text isn't a readable key file.
    Unreadable(KeyFileError),
    /// A key file, but with no `[Application]` or `[Runtime]` group.
    NotMetadata,
}

impl fmt::Display for PermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermError::Unreadable(e) => write!(f, "The permissions can't be read: {e}"),
            PermError::NotMetadata => {
                f.write_str("This isn't an app's metadata: it has no Application or Runtime group")
            }
        }
    }
}

impl std::error::Error for PermError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PermError::Unreadable(e) => Some(e),
            PermError::NotMetadata => None,
        }
    }
}

const _: () = {
    const fn thread_safe<T: Send + Sync>() {}
    thread_safe::<Permissions>();
    thread_safe::<Permission>();
};

/// One permission: a stable code, a risk and one sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    code: String,
    risk: Risk,
    text: String,
}

impl Permission {
    fn new(code: String, risk: Risk, text: String) -> Permission {
        Permission { code, risk, text }
    }

    /// The stable code, such as `filesystem:home:ro`.
    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn risk(&self) -> Risk {
        self.risk
    }

    /// One short sentence, such as "Can read all files in your home folder".
    ///
    /// Computing [`Permissions::permissions`] walks the whole set: do it
    /// once on a worker thread and keep the result.
    pub fn describe(&self) -> &str {
        &self.text
    }

    /// Whether this is something the Store doesn't recognize.
    pub fn is_unknown(&self) -> bool {
        self.code.starts_with("unknown:")
    }
}

/// An item with what is needed to compare it with an older one.
struct Item {
    key: String,
    rank: u8,
    /// For values compared whole: changed means added.
    exact: Option<String>,
    /// Counts in every comparison.
    always: bool,
    /// Only a runtime brought it: counts unless the old one had it too.
    runtime_only: bool,
    /// For a filesystem item: the canonical place, so that several
    /// spellings of one place show as one item.
    place: Option<String>,
    perm: Permission,
}

/// One step that built a permission set, kept to replay it on a runtime.
#[derive(Debug, Clone)]
enum Op {
    /// `[Context]` key, one item as written (`!x` included).
    List(String, String),
    /// Scope (`item`, `value` or `key`), group, key, raw.
    Odd(&'static str, String, String, String),
    /// Bus policy group, name, level.
    Bus(String, String, String),
    Env(String, String),
    Other(String, String, String),
}

/// The steps taken. History, not part of what is granted: it never makes
/// two sets differ.
#[derive(Debug, Clone, Default)]
struct Ops(Vec<Op>);

impl PartialEq for Ops {
    fn eq(&self, _: &Ops) -> bool {
        true
    }
}
impl Eq for Ops {}

impl PartialEq for Permissions {
    fn eq(&self, o: &Permissions) -> bool {
        self.runtime == o.runtime
            && self.sdk == o.sdk
            && self.extra == o.extra
            && self.effective_lists() == o.effective_lists()
            && self.fs == o.fs
            && self.buses == o.buses
            && self.env == o.env
            && self.other == o.other
            && self.odd == o.odd
    }
}
impl Eq for Permissions {}

/// A permission set, folded the way flatpak folds a `[Context]`.
///
/// `==` compares the effective result, not how it came about: two sets that
/// grant the same are equal. It is not a cache key for what
/// [`Permissions::with_runtime`] returns, which depends on the runtime too.
#[derive(Debug, Clone, Default)]
pub struct Permissions {
    runtime: Option<String>,
    sdk: Option<String>,
    /// Hosts of `[Extra Data]` downloads.
    extra: BTreeSet<String>,
    ops: Ops,
    /// `[Context]` list keys other than `filesystems` → what is granted.
    lists: BTreeMap<String, BTreeSet<String>>,
    /// Path as written (slashes normalized) → where it points and access: 1 `ro`, 2 `rw`, 3 `create`.
    fs: BTreeMap<String, (Loc, u8)>,
    /// Bus policy group → name → level as written (never `none`).
    buses: BTreeMap<String, BTreeMap<String, String>>,
    env: BTreeMap<String, String>,
    /// (group, key) → value, for groups this module doesn't know.
    other: BTreeMap<(String, String), String>,
    /// (group, key, raw) of what couldn't be read the way flatpak does.
    odd: BTreeSet<(&'static str, String, String, String)>,
    /// The ones of `odd` that only a runtime brought (see `with_runtime`).
    rt_odd: BTreeSet<(&'static str, String, String, String)>,
}

const SESSION: &str = "Session Bus Policy";
const SYSTEM: &str = "System Bus Policy";

/// Groups that grant nothing.
fn harmless_group(g: &str) -> bool {
    matches!(
        g,
        "Application" | "Runtime" | "ExtensionOf" | "Build" | "Extra Data"
    ) || g.starts_with("Extension ")
}

// ---- text ------------------------------------------------------------

/// Plain printable ASCII with no space and no colon: safe to use as a part
/// of a code as it is.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b':')
}

/// `s` as a part of a code. Anything else is its bytes in hex after
/// `hex `: the space makes it differ from every plain part.
fn part(s: &str) -> String {
    if plain(s) {
        return s.to_string();
    }
    let mut out = String::from("hex ");
    for b in s.bytes() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Untrusted text for a sentence: cleaned, cut at `max` characters (with
/// `…`) and with `"` turned into `'`, so it can sit inside quotes.
fn shown(s: &str, max: usize) -> String {
    let mut b = LineBuf::new(max);
    b.push_str(s);
    let cut = b.truncated();
    let mut out = b.finish().replace('"', "'");
    if cut {
        out.push('…');
    }
    out
}

// ---- filesystems -----------------------------------------------------

/// Where a `filesystems` item points, canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Loc {
    /// `host`, `host-os` or `host-etc`.
    Host(&'static str),
    Root,
    /// Segments under the home folder (not `.config`, `.local/share` or
    /// `.cache`, which are `Xdg`).
    Home(Vec<String>),
    /// An xdg folder and the segments under it.
    Xdg(String, Vec<String>),
    /// An absolute path elsewhere.
    Abs(Vec<String>),
}

/// An item as flatpak resolves it.
struct Fs {
    /// The path as written, slashes normalized: what flatpak and the
    /// Updater tell apart.
    literal: String,
    loc: Loc,
    rank: u8,
    explicit_mode: bool,
}

const XDG: &[&str] = &[
    "desktop",
    "documents",
    "download",
    "music",
    "pictures",
    "public-share",
    "videos",
    "templates",
    "config",
    "cache",
    "data",
    "run",
];

/// Segments of a path below a base. `None` when a `..` leaves the base.
fn rel_segments(rest: &str) -> Option<Vec<String>> {
    let mut v: Vec<String> = Vec::new();
    for s in rest.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                v.pop()?;
            }
            s => v.push(s.to_string()),
        }
    }
    Some(v)
}

/// Segments of an absolute path: a `..` at the root stays at the root.
fn abs_segments(path: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for s in path.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                v.pop();
            }
            s => v.push(s.to_string()),
        }
    }
    v
}

fn home_loc(rel: Vec<String>) -> Loc {
    match rel.as_slice() {
        [a, rest @ ..] if a == ".config" => Loc::Xdg("config".into(), rest.to_vec()),
        [a, b, rest @ ..] if a == ".local" && b == "share" => {
            Loc::Xdg("data".into(), rest.to_vec())
        }
        [a, rest @ ..] if a == ".cache" => Loc::Xdg("cache".into(), rest.to_vec()),
        _ => Loc::Home(rel),
    }
}

fn abs_loc(path: &str) -> Loc {
    let mut s = abs_segments(path);
    if s.len() >= 2 && s[0] == "var" && s[1] == "run" {
        s.remove(0);
    }
    let w: Vec<&str> = s.iter().map(String::as_str).collect();
    match w.as_slice() {
        [] => Loc::Root,
        ["home", _, rest @ ..] | ["var", "home", _, rest @ ..] => {
            home_loc(rest.iter().map(|x| x.to_string()).collect())
        }
        ["root", rest @ ..] | ["var", "roothome", rest @ ..] => {
            home_loc(rest.iter().map(|x| x.to_string()).collect())
        }
        ["run", "user"] => Loc::Xdg("run".into(), Vec::new()),
        ["run", "user", _, rest @ ..] => {
            Loc::Xdg("run".into(), rest.iter().map(|x| x.to_string()).collect())
        }
        _ => Loc::Abs(s),
    }
}

fn parse_loc(path: &str) -> Option<Loc> {
    match path {
        "host" => return Some(Loc::Host("host")),
        "host-os" => return Some(Loc::Host("host-os")),
        "host-etc" => return Some(Loc::Host("host-etc")),
        "host-root" => return Some(Loc::Host("host-root")),
        "~" => return Some(Loc::Home(Vec::new())),
        "home" => return Some(Loc::Home(Vec::new())),
        _ => {}
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(home_loc(rel_segments(rest)?));
    }
    if path.starts_with('/') {
        return Some(abs_loc(path));
    }
    let rest = path.strip_prefix("xdg-")?;
    let (dir, sub) = rest.split_once('/').unwrap_or((rest, ""));
    if !XDG.contains(&dir) {
        return None;
    }
    Some(Loc::Xdg(dir.to_string(), rel_segments(sub)?))
}

fn canon(loc: &Loc) -> String {
    match loc {
        Loc::Host(n) => (*n).to_string(),
        Loc::Root => "/".to_string(),
        Loc::Home(r) if r.is_empty() => "home".to_string(),
        Loc::Home(r) => format!("~/{}", r.join("/")),
        Loc::Xdg(d, r) if r.is_empty() => format!("xdg-{d}"),
        Loc::Xdg(d, r) => format!("xdg-{d}/{}", r.join("/")),
        Loc::Abs(s) => format!("/{}", s.join("/")),
    }
}

/// Whether flatpak and the Store can take `c` in a path: any text except
/// controls, `\`, and the invisible format characters (bidi marks and
/// overrides, joiners, zero-width space, the BOM, line and paragraph
/// separators, tags), which could make a path read as another one. Telamon
/// Updater's `new_permissions` refuses only `\` and spaces at the edges of
/// an item, so every path it takes, this takes unless it hides something.
fn fs_char_ok(c: char) -> bool {
    let u = c as u32;
    !(c.is_control()
        || c == '\\'
        || matches!(u,
            0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x8E2 | 0x180E
            | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2064
            | 0x2066..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD
            | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F
            | 0x2800 | 0x13430..=0x1343F)
        || crate::text::class(c) == crate::text::Class::Drop)
}

/// Combining marks (the common blocks, and the nukta and virama of the
/// Indic scripts, which aren't letters).
fn is_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF
        | 0x093C | 0x094D | 0x09BC | 0x09CD | 0x0A3C | 0x0A4D | 0x0ABC | 0x0ACD
        | 0x0B3C | 0x0B4D | 0x0BCD | 0x0C4D | 0x0CBC | 0x0CCD | 0x0D4D | 0x0DCA
        | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE20..=0xFE2F)
}

/// Punctuation outside ASCII that real folder names use: en and em dash,
/// typographic quotes, guillemets, the middle dot.
fn ok_punct(c: char) -> bool {
    matches!(
        c as u32,
        0x2013 | 0x2014 | 0x2018 | 0x2019 | 0x201C | 0x201D | 0xAB | 0xBB | 0xB7
    )
}

/// Whether a segment holds something that could make it read as another
/// path. A non-ASCII character is taken only as a letter or digit, as a
/// listed combining mark right after a letter, digit or mark (at most two in
/// a row), as one of a few punctuation marks, or as a non-edge no-break
/// space. Anything else (other symbols, look-alike slashes, a listed mark
/// after a space or a dot) is refused. Signs that Unicode counts as letters
/// (Indic vowel signs, Tibetan) pass as letters and can stack; they can't
/// forge a `/` or `.`, and the dialog contains them (see Showing).
fn odd_chars(seg: &str) -> bool {
    let mut after_base = false;
    let mut run = 0;
    for c in seg.chars() {
        if c.is_ascii() {
            after_base = c.is_ascii_alphanumeric();
            run = 0;
        } else if is_mark(c) {
            run += 1;
            if !after_base || run > 2 {
                return true;
            }
        } else if c.is_alphanumeric() {
            after_base = true;
            run = 0;
        } else if ok_punct(c) || c == '\u{A0}' {
            after_base = false;
            run = 0;
        } else {
            return true;
        }
    }
    false
}

/// A strong right-to-left letter (Hebrew, Arabic, Syriac, Thaana, NKo).
fn is_rtl(c: char) -> bool {
    matches!(c as u32,
        0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF
        | 0x1E800..=0x1EFFF)
        && !is_mark(c)
}

/// A path for a sentence: as [`shown`], and isolated (FSI...PDI) when it has
/// right-to-left letters, so it can't reorder the words around it.
fn path_shown(s: &str, max: usize) -> String {
    let t = shown(s, max);
    if t.chars().any(is_rtl) {
        format!("\u{2068}{t}\u{2069}")
    } else {
        t
    }
}

fn parse_fs(item: &str) -> Option<Fs> {
    if item.is_empty() || item.trim() != item || !item.chars().all(fs_char_ok) {
        return None;
    }
    let (path, rank, explicit_mode) = match item.rsplit_once(':') {
        Some((p, "ro")) => (p, 1, true),
        Some((p, "rw")) => (p, 2, true),
        Some((p, "create")) => (p, 3, true),
        _ => (item, 2, false),
    };
    if path.contains(':') || path.trim() != path {
        return None;
    }
    let mut literal = path.to_string();
    while literal.contains("//") {
        literal = literal.replace("//", "/");
    }
    if literal.len() > 1 {
        literal = literal.trim_end_matches('/').to_string();
    }
    if literal.is_empty()
        || literal
            .split('/')
            .any(|seg| !seg.is_empty() && (seg.trim() != seg || odd_chars(seg)))
    {
        return None;
    }
    Some(Fs {
        loc: parse_loc(&literal)?,
        literal,
        rank,
        explicit_mode,
    })
}

fn mode_name(rank: u8) -> &'static str {
    match rank {
        1 => "ro",
        3 => "create",
        _ => "rw",
    }
}

/// Home-relative segments, for `Home` and for the xdg folders that live in
/// it.
fn home_rel(loc: &Loc) -> Option<Vec<&str>> {
    let (prefix, rest): (&[&str], &Vec<String>) = match loc {
        Loc::Home(r) => (&[], r),
        Loc::Xdg(d, r) if d == "config" => (&[".config"], r),
        Loc::Xdg(d, r) if d == "data" => (&[".local", "share"], r),
        Loc::Xdg(d, r) if d == "cache" => (&[".cache"], r),
        _ => return None,
    };
    Some(
        prefix
            .iter()
            .copied()
            .chain(rest.iter().map(String::as_str))
            .collect(),
    )
}

/// Places in the home folder where a write adds a program that runs outside
/// the sandbox, with what to call them.
const ESCAPES: &[(&str, &str)] = &[
    (".config/autostart", "autostart"),
    (".config/systemd", "systemd services"),
    (".config/environment.d", "session environment"),
    (".config/plasma-workspace", "Plasma startup scripts"),
    (".config/fish", "fish shell"),
    (".local/share/flatpak", "Flatpak"),
    (".local/share/applications", "app launchers"),
    (".local/share/dbus-1", "D-Bus services"),
    (".local/share/systemd", "systemd services"),
    (".local/share/kservices5", "KDE services"),
    (".local/share/kservices6", "KDE services"),
    (".local/share/plasma", "Plasma"),
    (".local/bin", "your programs folder"),
    ("bin", "your programs folder"),
    (".bashrc", "shell startup files"),
    (".bash_profile", "shell startup files"),
    (".bash_login", "shell startup files"),
    (".profile", "shell startup files"),
    (".zshrc", "shell startup files"),
    (".zprofile", "shell startup files"),
    (".zshenv", "shell startup files"),
    (".bashrc.d", "shell startup files"),
    (".bash_logout", "shell startup files"),
    (".zlogin", "shell startup files"),
    (".zlogout", "shell startup files"),
    (".xprofile", "session startup files"),
    (".xinitrc", "session startup files"),
    (".xsession", "session startup files"),
    (".xsessionrc", "session startup files"),
    (".pam_environment", "session environment"),
    (".vimrc", "editor settings"),
    (".vim", "editor plugins"),
    (".config/nvim", "editor plugins"),
    (".emacs.d", "editor plugins"),
    (".config/autostart-scripts", "autostart"),
    (".local/share/kwin", "KDE scripts"),
    (".local/share/kio", "KDE services"),
    (".config/kglobalshortcutsrc", "KDE shortcuts"),
    (".config/khotkeysrc", "KDE shortcuts"),
    (".config/konsolerc", "Konsole settings"),
    (".local/share/konsole", "Konsole profiles"),
    (".config/kdeglobals", "KDE settings"),
    (".local/lib", "your libraries folder"),
    (".gitconfig", "Git settings"),
    (".config/git", "Git settings"),
    (".cargo/bin", "your programs folder"),
    (".var/app", "other apps' data"),
    (".steam", "Steam"),
    (".wine", "Wine"),
    (".minecraft", "Minecraft"),
    (".local/share/Steam", "Steam"),
    (".local/share/lutris", "Lutris"),
    (".local/share/bash-completion", "shell completions"),
    (".local/share/fish", "fish shell"),
    (".local/share/nautilus-python", "file manager extensions"),
    (".local/share/nautilus/scripts", "file manager scripts"),
    ("go/bin", "your programs folder"),
    (".config/mimeapps.list", "default apps"),
    (
        ".config/plasma-org.kde.plasma.desktop-appletsrc",
        "Plasma panels",
    ),
];

/// Places that hold keys, passwords and logins.
const CREDENTIALS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".mozilla",
    ".local/share/keyrings",
    ".local/share/kwalletd",
    ".netrc",
    ".git-credentials",
    ".password-store",
    ".docker",
    ".kube",
    ".thunderbird",
    ".pki",
    ".config/gcloud",
    ".config/gh",
    ".config/rclone",
    ".config/chromium",
    ".config/google-chrome",
    ".config/BraveSoftware",
    ".config/Signal",
    ".config/discord",
    ".config/kdeconnect",
    ".var/app",
    ".config/mozilla",
    ".config/vivaldi",
    ".config/microsoft-edge",
    ".config/opera",
    ".config/containers",
    ".config/Element",
    ".config/1Password",
    ".config/Bitwarden",
    ".config/keepassxc",
];

/// Dot folders directly in the home folder that a write can't turn into a
/// way out: anything else there (a shell or tool config, a plugin folder) is
/// treated as settings that other programs run.
const BENIGN_DOT_DIRS: &[&str] = &[".themes", ".icons", ".fonts"];

/// Whether `path` is `entry` or below it (case does not matter: some disks
/// and homes fold it).
fn under(path: &[&str], entry: &str) -> bool {
    let e: Vec<&str> = entry.split('/').collect();
    path.len() >= e.len() && path.iter().zip(&e).all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Whether `path` is above `entry` (a folder that holds it).
fn above(path: &[&str], entry: &str) -> bool {
    let e: Vec<&str> = entry.split('/').collect();
    path.len() < e.len() && path.iter().zip(&e).all(|(a, b)| a.eq_ignore_ascii_case(b))
}

const SYSTEM_DIRS: &[&str] = &[
    "tmp", "mnt", "media", "opt", "srv", "lib32", "libx32", "nix", "sysroot", "boot", "etc", "usr",
    "var", "run", "bin", "sbin", "lib", "lib64", "proc", "sys", "dev",
];

/// Whose home folder a literal absolute path names, when it isn't yours
/// (the Store can't know who "you" are in the sandbox): `/home/NAME`,
/// `/var/home/NAME`, `/root`, `/var/roothome`.
fn home_owner(literal: &str) -> Option<String> {
    if !literal.starts_with('/') {
        return None;
    }
    let s = abs_segments(literal);
    let w: Vec<&str> = s.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["home", n, ..] | ["var", "home", n, ..] => Some(format!(
            "another user's home folder ({})",
            path_shown(n, 60)
        )),
        ["root", ..] | ["var", "roothome", ..] => Some("root's home folder".to_string()),
        _ => None,
    }
}

/// The runtime folder a literal path names.
fn run_owner(literal: &str) -> &'static str {
    if !literal.starts_with('/') {
        return "the runtime folder of your session";
    }
    let mut s = abs_segments(literal);
    if s.len() >= 2 && s[0] == "var" && s[1] == "run" {
        s.remove(0);
    }
    match s.as_slice() {
        [a, b] if a == "run" && b == "user" => "every user's runtime folder",
        _ => "a user's runtime folder",
    }
}

/// Apps whose shared files are secrets, whatever the access. A best-effort
/// list: an app that isn't on it is still Medium at most, and only read-only.
const SENSITIVE_APPS: &[&str] = &[
    "org.keepassxc.KeePassXC",
    "com.bitwarden.desktop",
    "im.riot.Riot",
    "org.signal.Signal",
    "com.onepassword.OnePassword",
    "org.telegram.desktop",
    "io.element.Element",
    "org.mozilla.Thunderbird",
    "org.gnome.seahorse.Application",
];

fn run_text(verb: &str, literal: &str) -> String {
    format!(
        "Can {verb} files in {}, which hold sockets that let it reach outside its sandbox",
        run_owner(literal)
    )
}

fn fs_text(literal: &str, loc: &Loc, rank: u8) -> (Risk, String) {
    use Risk::*;
    let verb = match rank {
        1 => "read",
        3 => "read, change and create",
        _ => "read and change",
    };
    let rw = rank > 1;
    let owner = home_owner(literal);
    let (mut risk, mut text) = match loc {
        Loc::Host("host") | Loc::Root => (High, format!("Can {verb} all files on your computer")),
        Loc::Host("host-os") => (High, format!("Can {verb} the system's own files")),
        Loc::Host("host-root") => (
            High,
            format!("Can {verb} every file on your computer, including other drives"),
        ),
        Loc::Host(_) => (High, format!("Can {verb} the system settings in /etc")),
        Loc::Home(r) if r.is_empty() => match &owner {
            Some(h) => (High, format!("Can {verb} files in {h}")),
            None => (High, format!("Can {verb} all files in your home folder")),
        },
        Loc::Xdg(d, rel) if d == "run" && !literal.starts_with('/') && !rel.is_empty() => {
            let generic = || (High, run_text(verb, literal));
            // Read-only, these are only reached; writing could replace the
            // socket in them.
            match (rel[0].as_str(), rel.len()) {
                ("pipewire-0", _) => (
                    High,
                    "Can use PipeWire directly, which can record audio and the screen without asking"
                        .to_string(),
                ),
                ("gvfsd", _) => (
                    High,
                    format!(
                        "Can {verb} files on your network shares and cloud drives, and mount new ones"
                    ),
                ),
                ("speech-dispatcher", _) if !rw => {
                    (Medium, "Can use the speech service".to_string())
                }
                ("gvfs", _) if !rw => (
                    Medium,
                    "Can use the file manager's network and device mounts".to_string(),
                ),
                ("app", n) if n >= 2 => {
                    let id = rel[1].as_str();
                    if SENSITIVE_APPS.iter().any(|a| a.eq_ignore_ascii_case(id)) {
                        (
                            High,
                            format!(
                                "Can {verb} files shared with {}, an app that holds your passwords or messages",
                                shown(id, 100)
                            ),
                        )
                    } else if !rw && valid_id(id) {
                        (
                            Medium,
                            format!("Shares files with {} while it runs", shown(id, 100)),
                        )
                    } else {
                        generic()
                    }
                }
                _ => generic(),
            }
        }
        Loc::Xdg(d, _) if d == "run" => (High, run_text(verb, literal)),
        Loc::Abs(s) if s.len() == 1 && s[0] == "home" => {
            (High, format!("Can {verb} the home folders of all users"))
        }
        Loc::Abs(s) if s.first().is_some_and(|f| f == "tmp") => (
            High,
            format!(
                "Can {verb} the shared temporary folder, which holds the sockets of other programs (keyboard capture, SSH keys)"
            ),
        ),
        Loc::Abs(s) if s.first().is_some_and(|f| SYSTEM_DIRS.contains(&f.as_str())) => (
            High,
            format!(
                "Can {verb} system files in \"{}\"",
                path_shown(&canon(loc), 120)
            ),
        ),
        _ => {
            let hr = home_rel(loc);
            // (what, name, whether `path` holds it rather than is in it)
            let cred = hr.as_ref().and_then(|p| {
                CREDENTIALS
                    .iter()
                    .find(|c| under(p, c) || above(p, c))
                    .map(|c| (*c, !under(p, c)))
            });
            let escape = hr.as_ref().filter(|_| rw).and_then(|p| {
                ESCAPES
                    .iter()
                    .find(|(e, _)| under(p, e) || above(p, e))
                    .map(|(e, l)| (*l, !under(p, e)))
            });
            let dot = matches!(loc, Loc::Home(r) if r.first().is_some_and(|f|
                f.starts_with('.')
                    && ![".config", ".local", ".cache"].contains(&f.as_str())
                    && !BENIGN_DOT_DIRS.contains(&f.as_str())));
            let cache_root = rw && hr.as_ref().is_some_and(|p| p.as_slice() == [".cache"]);
            let bare = hr.as_ref().is_some_and(|p| p.as_slice() == [".cache"]);
            let spot = match (&owner, &hr) {
                (Some(h), Some(p)) => format!("\"{}\" in {h}", path_shown(&p.join("/"), 100)),
                _ => place(loc),
            };
            let mine = owner.is_none();
            if let Some((l, holds)) = escape {
                if holds {
                    (
                        High,
                        format!(
                            "Can {verb} everything in {spot}, including {l}, which runs outside its sandbox"
                        ),
                    )
                } else {
                    (
                        High,
                        format!("Can add programs that run outside its sandbox ({l})"),
                    )
                }
            } else if let Some((c, holds)) = cred {
                let c = shown(c, 60);
                (
                    High,
                    match (holds, mine) {
                        (true, _) => format!(
                            "Can {verb} everything in {spot}, including saved passwords and keys ({c})"
                        ),
                        (false, true) => {
                            format!("Can {verb} your saved passwords and keys ({c})")
                        }
                        (false, false) => {
                            format!("Can {verb} saved passwords and keys in {spot} ({c})")
                        }
                    },
                )
            } else if dot && rw {
                (
                    High,
                    "Can change settings that other programs run".to_string(),
                )
            } else if dot {
                (
                    Medium,
                    "Can read settings and data of other programs".to_string(),
                )
            } else if cache_root {
                (
                    High,
                    format!("Can {verb} everything other programs keep in your cache folder"),
                )
            } else if bare {
                (Medium, format!("Can {verb} files in {spot}"))
            } else {
                (
                    if rw { Medium } else { Low },
                    format!("Can {verb} files in {spot}"),
                )
            }
        }
    };
    if !rw && !literal.is_ascii() {
        // Reading a place that can pass for another needs a second look.
        risk = risk.max(Medium);
        text.push_str(" (contains unusual characters)");
    }
    (risk, text)
}

fn fs_item(literal: &str, loc: &Loc, rank: u8) -> Item {
    let (risk, text) = fs_text(literal, loc, rank);
    // What the item says without its access: spellings that say the same
    // thing are one item, others stay apart.
    let said = if rank == 2 {
        text.clone()
    } else {
        fs_text(literal, loc, 2).1
    };
    // Another user's home and another user's runtime folder are not the
    // canonical place: their code names the path as written.
    let own_path = home_owner(literal).is_some()
        || (matches!(loc, Loc::Xdg(d, _) if d == "run") && literal.starts_with('/'));
    let shown_path = if own_path {
        literal.to_string()
    } else {
        canon(loc)
    };
    let code = format!("filesystem:{}:{}", part(&shown_path), mode_name(rank));
    Item {
        key: format!("filesystem:{literal}"),
        rank,
        exact: None,
        always: false,
        place: Some(format!("{shown_path}\n{said}")),
        runtime_only: false,
        perm: Permission::new(code, risk, text),
    }
}

/// One item per place, at its highest access, whatever the spelling, and
/// only when the wording is the same. Spellings of one place can thus give
/// fewer entries than the Updater lists (it counts each literal); what they
/// cover is the same.
fn collapse(items: Vec<Item>) -> Vec<Item> {
    let mut at: BTreeMap<String, usize> = BTreeMap::new();
    let mut out: Vec<Item> = Vec::new();
    for it in items {
        let Some(p) = it.place.clone() else {
            out.push(it);
            continue;
        };
        match at.get(&p) {
            Some(&i) => {
                if it.rank > out[i].rank {
                    out[i] = it;
                }
            }
            None => {
                at.insert(p, out.len());
                out.push(it);
            }
        }
    }
    out
}

/// How a path reads in a sentence.
fn place(loc: &Loc) -> String {
    match loc {
        Loc::Xdg(dir, rel) => {
            let name = match dir.as_str() {
                "desktop" => "your Desktop folder",
                "documents" => "your Documents folder",
                "download" => "your Downloads folder",
                "music" => "your Music folder",
                "pictures" => "your Pictures folder",
                "public-share" => "your Public folder",
                "videos" => "your Videos folder",
                "templates" => "your Templates folder",
                "config" => "your settings folder (~/.config)",
                "cache" => "your cache folder (~/.cache)",
                "data" => "your data folder (~/.local/share)",
                _ => "the runtime folder of your session",
            };
            if rel.is_empty() {
                name.to_string()
            } else {
                format!("\"{}\" in {name}", path_shown(&rel.join("/"), 100))
            }
        }
        Loc::Home(rel) => format!(
            "\"{}\" in your home folder",
            path_shown(&rel.join("/"), 100)
        ),
        other => format!("\"{}\"", path_shown(&canon(other), 120)),
    }
}

// ---- unknown items and bus names -------------------------------------

fn unknown_item(key: String, code: String, text: String) -> Item {
    Item {
        key,
        rank: 1,
        exact: None,
        always: false,
        place: None,
        runtime_only: false,
        perm: Permission::new(code, Risk::High, text),
    }
}

fn unknown_value(what: &str, raw: &str) -> String {
    let value = shown(raw, 120);
    if value.is_empty() {
        return format!(
            "Asks for something the Store doesn't know ({})",
            shown(what, 60)
        );
    }
    format!(
        "Asks for something the Store doesn't know ({}): {value}",
        shown(what, 60)
    )
}

/// Dot-separated segments of `[A-Za-z0-9_-]`, with an optional `.*` at the
/// end.
fn bus_name_ok(n: &str) -> bool {
    let base = n.strip_suffix(".*").unwrap_or(n);
    base.len() <= 255
        && base.split('.').all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// Session bus names that give a way out of the sandbox or to secrets:
/// name, whether it is a prefix, what it allows.
const SESSION_HIGH: &[(&str, bool, &str)] = &[
    (
        "org.freedesktop.Flatpak",
        false,
        "Can break out of its sandbox: it can run other programs outside it through the Flatpak service",
    ),
    (
        "org.freedesktop.systemd1",
        false,
        "Can break out of its sandbox: it can start programs through your session's systemd",
    ),
    (
        "org.freedesktop.secrets",
        false,
        "Can read your saved passwords",
    ),
    ("org.kde.kwalletd5", false, "Can read your saved passwords"),
    ("org.kde.kwalletd6", false, "Can read your saved passwords"),
    ("org.gnome.keyring", true, "Can read your saved passwords"),
    (
        "ca.desrt.dconf",
        false,
        "Can change your desktop settings, which can start programs outside its sandbox",
    ),
    (
        "org.kde.klauncher5",
        false,
        "Can break out of its sandbox: it can start programs outside it",
    ),
    (
        "org.kde.klauncher6",
        false,
        "Can break out of its sandbox: it can start programs outside it",
    ),
    (
        "org.kde.KWin",
        false,
        "Can break out of its sandbox: it can control your desktop and its windows",
    ),
    (
        "org.kde.plasmashell",
        false,
        "Can break out of its sandbox: it can control your desktop",
    ),
    (
        "org.kde.kded5",
        false,
        "Can break out of its sandbox: it can load code into your desktop",
    ),
    (
        "org.kde.kded6",
        false,
        "Can break out of its sandbox: it can load code into your desktop",
    ),
    (
        "org.gnome.Shell",
        false,
        "Can break out of its sandbox: it can control your desktop",
    ),
    (
        "org.freedesktop.PackageKit",
        false,
        "Can install software on your computer",
    ),
    (
        "org.kde.kdeconnect",
        true,
        "Can break out of its sandbox: it can run commands on your computer through KDE Connect",
    ),
    (
        "org.kde.ksmserver",
        false,
        "Can break out of its sandbox: it can control your session",
    ),
    (
        "org.kde.krunner",
        false,
        "Can break out of its sandbox: it can start programs through KRunner",
    ),
    (
        "org.kde.konsole",
        true,
        "Can break out of its sandbox: it can run commands in a terminal",
    ),
    (
        "org.kde.yakuake",
        false,
        "Can break out of its sandbox: it can run commands in a terminal",
    ),
    (
        "org.kde.kglobalaccel",
        false,
        "Can break out of its sandbox: it can set global shortcuts that start programs",
    ),
    (
        "org.gnome.SettingsDaemon.",
        true,
        "Can change your desktop settings, which can start programs outside its sandbox",
    ),
    (
        "org.gnome.Mutter.",
        true,
        "Can break out of its sandbox: it can control your desktop and its windows",
    ),
    (
        "org.freedesktop.impl.portal.",
        true,
        "Can break out of its sandbox by replacing a desktop portal",
    ),
];

/// Whether the bus name or `.*` pattern `n` covers the name `e` (or, for a
/// prefix, any name that starts with it).
fn bus_covers(n: &str, e: &str, prefix: bool) -> bool {
    match n.strip_suffix(".*") {
        Some(p) => {
            let pd = format!("{p}.");
            e == p || e.starts_with(&pd) || (prefix && pd.starts_with(e))
        }
        None => n == e || (prefix && n.starts_with(e)),
    }
}

fn level_rank(level: &str) -> u8 {
    match level {
        "see" => 1,
        "talk" => 2,
        "own" => 3,
        _ => 4,
    }
}

fn bus_item(group: &str, name: &str, level: &str) -> Item {
    let session = group == SESSION;
    let a11y = group == "Accessibility Bus Policy" || group == "A11y Bus Policy";
    let system = group == SYSTEM;
    let label = if session {
        "session-bus".to_string()
    } else if system {
        "system-bus".to_string()
    } else {
        format!("bus-{}", part(group.trim_end_matches(" Bus Policy")))
    };
    let rank = level_rank(level);
    if !(session || system || a11y)
        || !matches!(level, "see" | "talk" | "own")
        || !bus_name_ok(name)
    {
        let mut it = unknown_item(
            format!("{label}:{}", part(name)),
            format!("unknown:{label}:{}:{}", part(level), part(name)),
            unknown_value(&format!("{label} policy"), &format!("{name}={level}")),
        );
        it.rank = rank;
        it.exact = Some(level.to_string());
        return it;
    }
    let shown_name = shown(name, 120);
    let (target, wild) = match name.strip_suffix(".*") {
        Some(p) => (format!("all {} services", shown(p, 100)), true),
        None => (format!("the {shown_name} service"), false),
    };
    let mut text = match (level, system) {
        ("see", _) => format!("Can see whether {target} is running"),
        ("talk", false) => format!("Can talk to {target}"),
        ("talk", true) => format!("Can talk to {target} of your system"),
        (_, false) => format!("Can provide {target}"),
        (_, true) => format!("Can provide {target} on your system"),
    };
    let risk = if level == "see" {
        Risk::Low
    } else if a11y {
        text = "Can read every window and control input".to_string();
        Risk::High
    } else if system {
        text.push_str(", which can reach outside its sandbox");
        Risk::High
    } else if session {
        match SESSION_HIGH
            .iter()
            .find(|(e, p, _)| bus_covers(name, e, *p))
        {
            Some((_, _, why)) => {
                text = if wild {
                    format!("Can use {target}, including ones that let it break out of its sandbox")
                } else {
                    (*why).to_string()
                };
                Risk::High
            }
            None if name.starts_with("org.freedesktop.portal.") && level == "own" => Risk::Medium,
            None if name.starts_with("org.freedesktop.portal.") => Risk::Low,
            None => Risk::Medium,
        }
    } else {
        Risk::Medium
    };
    Item {
        key: format!("{label}:{name}"),
        rank,
        exact: None,
        always: false,
        place: None,
        runtime_only: false,
        perm: Permission::new(format!("{label}:{level}:{name}"), risk, text),
    }
}

/// The host of an `[Extra Data]` URI, lower case, or `None` when it isn't a
/// plain DNS name or IPv4 address.
fn host_of(uri: &str) -> Option<String> {
    let (scheme, rest) = uri.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+')
    {
        return None;
    }
    let auth = rest.split(['/', '?', '#']).next()?;
    let auth = auth.rsplit('@').next()?;
    let host = match auth.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        Some(_) => return None,
        None => auth,
    };
    let host = host.to_ascii_lowercase();
    let ok = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && !host.starts_with(['.', '-'])
        && !host.ends_with(['.', '-'])
        && !host.contains("..");
    ok.then_some(host)
}

impl Permissions {
    /// Reads an app's (or runtime's) `metadata`.
    pub fn from_metadata(bytes: &[u8]) -> Result<Permissions, PermError> {
        let kf = KeyFile::parse(bytes, &Limits::default()).map_err(PermError::Unreadable)?;
        if !kf.has_group("Application") && !kf.has_group("Runtime") {
            return Err(PermError::NotMetadata);
        }
        let mut p = Permissions::default();
        for key in ["runtime", "sdk"] {
            let id = match kf.string("Application", key) {
                Ok(Some(v)) => {
                    let id = v.split('/').next().unwrap_or_default().trim().to_string();
                    if id.is_empty() {
                        p.exec(Op::Odd("value", "Application".into(), key.into(), v));
                        None
                    } else {
                        Some(id)
                    }
                }
                Ok(None) => None,
                Err(_) => {
                    let raw = kf.raw("Application", key).unwrap_or_default().to_string();
                    p.exec(Op::Odd("value", "Application".into(), key.into(), raw));
                    None
                }
            };
            if key == "runtime" {
                p.runtime = id;
            } else {
                p.sdk = id;
            }
        }
        p.apply(&kf);
        let keys: Vec<&str> = kf.keys("Extra Data").collect();
        for key in keys.into_iter().filter(|k| k.starts_with("uri")) {
            match kf.string("Extra Data", key) {
                Ok(Some(uri)) => match host_of(&uri) {
                    Some(h) => {
                        p.extra.insert(h);
                    }
                    None => p.exec(Op::Odd("value", "Extra Data".into(), key.into(), uri)),
                },
                Ok(None) => {}
                Err(_) => {
                    let raw = kf.raw("Extra Data", key).unwrap_or_default().to_string();
                    p.exec(Op::Odd("value", "Extra Data".into(), key.into(), raw));
                }
            }
        }
        Ok(p)
    }

    /// Applies an override file (what `flatpak override` writes), the way
    /// flatpak merges it: a list adds, `!x` takes a granted `x` away, a bus
    /// policy replaces the old one (`none` removes it) and an environment
    /// variable is replaced.
    ///
    /// Flatpak builds what an app gets in this order, each step on top of the
    /// one before: the runtime's metadata ([`Permissions::with_runtime`]),
    /// the app's metadata ([`Permissions::from_metadata`]), the system-wide
    /// global override, the system-wide override of the app, the user's
    /// global override, the user's override of the app. Call this once per
    /// file that exists, in that order.
    pub fn with_overrides(&self, overrides: &[u8]) -> Result<Permissions, PermError> {
        let kf = KeyFile::parse(overrides, &Limits::default()).map_err(PermError::Unreadable)?;
        let mut p = self.clone();
        p.apply(&kf);
        Ok(p)
    }

    /// What the app gets with `runtime`'s permissions (its `[Context]`, bus
    /// policies and `[Environment]`) applied first and the app's on top, as
    /// flatpak does. The app's runtime, SDK and extra data stay the app's.
    pub fn with_runtime(&self, runtime: &Permissions) -> Permissions {
        let mut r = runtime.clone();
        r.rt_odd = runtime.odd.clone();
        r.runtime = self.runtime.clone();
        r.sdk = self.sdk.clone();
        r.extra = self.extra.clone();
        for op in &self.ops.0 {
            r.exec(op.clone());
        }
        r
    }

    fn apply(&mut self, kf: &KeyFile) {
        let mut groups: Vec<&str> = kf.groups().collect();
        // Flatpak reads `[Environment]` after the rest, so it wins over an
        // `unset-environment` of the same file.
        groups.sort_by_key(|g| *g == "Environment");
        for g in groups {
            if harmless_group(g) {
                continue;
            }
            let keys: Vec<&str> = kf.all_keys(g).collect();
            for key in keys {
                if key.contains('[') {
                    let raw = kf.raw(g, key).unwrap_or_default().to_string();
                    self.exec(Op::Odd("key", g.to_string(), key.to_string(), raw));
                    continue;
                }
                if g == "Context" {
                    self.apply_list(kf, key);
                    continue;
                }
                let v = match kf.string(g, key) {
                    Ok(Some(v)) => v,
                    Ok(None) => continue,
                    Err(_) => {
                        let raw = kf.raw(g, key).unwrap_or_default().to_string();
                        self.exec(Op::Odd("value", g.to_string(), key.to_string(), raw));
                        continue;
                    }
                };
                let op = if g.ends_with(" Bus Policy") {
                    Op::Bus(g.to_string(), key.to_string(), v)
                } else if g == "Environment" {
                    if plain(key) {
                        Op::Env(key.to_string(), v)
                    } else {
                        Op::Odd("key", g.to_string(), key.to_string(), v)
                    }
                } else {
                    Op::Other(g.to_string(), key.to_string(), v)
                };
                self.exec(op);
            }
        }
    }

    fn apply_list(&mut self, kf: &KeyFile, key: &str) {
        match kf.list("Context", key) {
            Ok(Some(items)) => {
                for item in items.into_iter().filter(|i| !i.is_empty()) {
                    self.exec(Op::List(key.to_string(), item));
                }
            }
            Ok(None) => {}
            Err(_) => {
                let raw = kf.raw("Context", key).unwrap_or_default().to_string();
                self.exec(Op::Odd("value", "Context".into(), key.to_string(), raw));
            }
        }
    }

    fn exec(&mut self, op: Op) {
        self.step(&op);
        self.ops.0.push(op);
    }

    fn step(&mut self, op: &Op) {
        match op {
            Op::Odd(scope, g, k, raw) => {
                let t = (*scope, g.clone(), k.clone(), raw.clone());
                self.rt_odd.remove(&t);
                self.odd.insert(t);
            }
            Op::Bus(g, name, level) => {
                if level == "none" {
                    if let Some(names) = self.buses.get_mut(g) {
                        names.remove(name);
                        if names.is_empty() {
                            self.buses.remove(g);
                        }
                    }
                } else {
                    self.buses
                        .entry(g.clone())
                        .or_default()
                        .insert(name.clone(), level.clone());
                }
            }
            Op::Env(k, v) => {
                self.env.insert(k.clone(), v.clone());
            }
            Op::Other(g, k, v) => {
                self.other.insert((g.clone(), k.clone()), v.clone());
            }
            Op::List(key, item) => {
                let (neg, name) = match item.strip_prefix('!') {
                    Some(n) => (true, n),
                    None => (false, item.as_str()),
                };
                let odd = |s: &mut Permissions| {
                    let t = ("item", "Context".into(), key.clone(), item.clone());
                    s.rt_odd.remove(&t);
                    s.odd.insert(t);
                };
                if key == "filesystems" {
                    match parse_fs(name) {
                        Some(f) if neg && f.explicit_mode => odd(self),
                        Some(f) if neg => {
                            self.fs.remove(&f.literal);
                        }
                        Some(f) => {
                            self.fs.insert(f.literal, (f.loc, f.rank));
                        }
                        None => odd(self),
                    }
                } else if !plain(name) {
                    odd(self);
                } else {
                    if neg {
                        if let Some(set) = self.lists.get_mut(key) {
                            set.remove(name);
                            if set.is_empty() {
                                self.lists.remove(key);
                            }
                        }
                    } else {
                        self.lists
                            .entry(key.clone())
                            .or_default()
                            .insert(name.to_string());
                        // One map in flatpak: the later of the two wins.
                        if key == "unset-environment" {
                            self.env.remove(name);
                        }
                    }
                }
            }
        }
    }

    /// The lists without an `unset-environment` that `[Environment]` overrides.
    fn effective_lists(&self) -> BTreeMap<&str, BTreeSet<&str>> {
        let mut m = BTreeMap::new();
        for (k, set) in &self.lists {
            let v: BTreeSet<&str> = set
                .iter()
                .map(String::as_str)
                .filter(|x| !(k == "unset-environment" && self.env.contains_key(*x)))
                .collect();
            if !v.is_empty() {
                m.insert(k.as_str(), v);
            }
        }
        m
    }

    fn has(&self, key: &str, v: &str) -> bool {
        self.lists.get(key).is_some_and(|s| s.contains(v))
    }

    /// Every permission, riskiest first, then by code.
    pub fn permissions(&self) -> Vec<Permission> {
        let mut v: Vec<Permission> = collapse(self.items()).into_iter().map(|i| i.perm).collect();
        v.sort_by(|a, b| b.risk.cmp(&a.risk).then_with(|| a.code.cmp(&b.code)));
        v
    }

    /// The highest risk of any permission, `None` when there are none.
    pub fn max_risk(&self) -> Option<Risk> {
        self.items().iter().map(|i| i.perm.risk).max()
    }

    /// What this grants that `old` doesn't (see the module's rules), riskiest
    /// first.
    pub fn added_since(&self, old: &Permissions) -> Vec<Permission> {
        let before: BTreeMap<String, Item> = old
            .items_with_runtime()
            .into_iter()
            .map(|i| (i.key.clone(), i))
            .collect();
        let added: Vec<Item> = self
            .items_with_runtime()
            .into_iter()
            .filter(|i| {
                match before.get(&i.key) {
                    None => true,
                    // What can't be compared counts, unless the same thing
                    // was there before (a runtime's, say).
                    Some(o) if i.always => !(i.runtime_only && o.always),
                    Some(o) => match &i.exact {
                        Some(e) => o.exact.as_ref() != Some(e),
                        // A wider grant, or the same one made riskier by
                        // something else (x11 once wayland is gone).
                        None => i.rank > o.rank || i.perm.risk > o.perm.risk,
                    },
                }
            })
            .collect();
        let mut out: Vec<Permission> = collapse(added).into_iter().map(|i| i.perm).collect();
        out.sort_by(|a, b| b.risk.cmp(&a.risk).then_with(|| a.code.cmp(&b.code)));
        out
    }

    fn items_with_runtime(&self) -> Vec<Item> {
        let mut v = self.items_with(false);
        for (kind, id) in [("runtime", &self.runtime), ("sdk", &self.sdk)] {
            if let Some(id) = id {
                v.push(Item {
                    key: kind.to_string(),
                    rank: 1,
                    exact: Some(id.clone()),
                    always: false,
                    place: None,
                    runtime_only: false,
                    perm: Permission::new(
                        format!("{kind}:{}", part(id)),
                        Risk::Medium,
                        format!("Uses a different {kind}: {}", shown(id, 120)),
                    ),
                });
            }
        }
        v
    }

    fn items(&self) -> Vec<Item> {
        self.items_with(true)
    }

    /// `hide_unset`: leave out an `unset-environment` that `[Environment]`
    /// overrides, as it is in effect; a comparison keeps it as written.
    fn items_with(&self, hide_unset: bool) -> Vec<Item> {
        let mut v = Vec::new();
        let wayland = self.has("sockets", "wayland");
        for (key, set) in &self.lists {
            for val in set {
                if hide_unset && key == "unset-environment" && self.env.contains_key(val) {
                    continue;
                }
                v.push(self.list_item(key, val, wayland));
            }
        }
        for (lit, (loc, rank)) in &self.fs {
            v.push(fs_item(lit, loc, *rank));
        }
        for (group, names) in &self.buses {
            for (name, level) in names {
                v.push(bus_item(group, name, level));
            }
        }
        for (name, val) in &self.env {
            let risk = if name.starts_with("LD_") || name == "PATH" {
                Risk::Medium
            } else {
                Risk::Low
            };
            v.push(Item {
                key: format!("env:{name}"),
                rank: 1,
                exact: Some(val.clone()),
                always: false,
                place: None,
                runtime_only: false,
                perm: Permission::new(
                    format!("env:{name}"),
                    risk,
                    format!(
                        "Sets the environment variable {} to \"{}\"",
                        shown(name, 80),
                        shown(val, 120)
                    ),
                ),
            });
        }
        for ((group, key), val) in &self.other {
            let mut it = unknown_item(
                format!("other:{}:{}", part(group), part(key)),
                format!("unknown:{}:{}", part(group), part(key)),
                unknown_value(&format!("{group} / {key}"), val.trim_end_matches(';')),
            );
            it.exact = Some(val.clone());
            v.push(it);
        }
        for t in &self.odd {
            let (scope, group, key, raw) = t;
            let code = format!(
                "unknown:{scope}:{}:{}:{}",
                part(group),
                part(key),
                part(raw)
            );
            let mut it = unknown_item(code.clone(), code, unknown_value("can't be read", raw));
            it.always = true;
            // A runtime's odd items, and extra-data URIs whose host can't be
            // parsed, count only when the old version lacks the same code:
            // the Updater can't see them either.
            it.runtime_only = self.rt_odd.contains(t) || group == "Extra Data";
            v.push(it);
        }
        for host in &self.extra {
            v.push(Item {
                key: format!("extra-data:{host}"),
                rank: 1,
                exact: None,
                always: false,
                place: None,
                runtime_only: false,
                perm: Permission::new(
                    format!("extra-data:{host}"),
                    Risk::Medium,
                    format!("Downloads extra files from {host} when installed"),
                ),
            });
        }
        v
    }

    fn list_item(&self, key: &str, val: &str, wayland: bool) -> Item {
        use Risk::*;
        let x11 = if wayland { Medium } else { High };
        let fx11 = if wayland { Low } else { High };
        let known: Option<(&str, Risk, &str)> = match (key, val) {
            ("shared", "network") => Some((
                "share:network",
                Medium,
                "Can access the internet, your local network and services running on this computer",
            )),
            ("shared", "ipc") => Some((
                "share:ipc",
                Low,
                "Can share memory with other processes on your computer",
            )),
            ("sockets", "wayland") => {
                Some(("socket:wayland", Low, "Can show windows on your desktop"))
            }
            ("sockets", "x11") => Some((
                "socket:x11",
                x11,
                "Can use the legacy X11 display, which lets it watch your keyboard and other windows",
            )),
            ("sockets", "fallback-x11") => Some((
                "socket:fallback-x11",
                fx11,
                "Can use the legacy X11 display when Wayland isn't available",
            )),
            ("sockets", "pulseaudio") => {
                Some(("socket:pulseaudio", Medium, "Can play and record sound"))
            }
            ("sockets", "system-bus") => Some((
                "socket:system-bus",
                High,
                "Can talk to every system service, which lets it break out of its sandbox",
            )),
            ("sockets", "session-bus") => Some((
                "socket:session-bus",
                High,
                "Can talk to every service of your session, which lets it break out of its sandbox",
            )),
            ("sockets", "ssh-auth") => Some(("socket:ssh-auth", High, "Can use your SSH keys")),
            ("sockets", "pcsc") => Some(("socket:pcsc", Medium, "Can use smart card readers")),
            ("sockets", "cups") => Some(("socket:cups", Medium, "Can use your printers")),
            ("sockets", "gpg-agent") => Some(("socket:gpg-agent", High, "Can use your GPG keys")),
            ("sockets", "inherit-wayland-socket") => Some((
                "socket:inherit-wayland-socket",
                Low,
                "Can use the Wayland connection of the app that started it",
            )),
            ("devices", "dri") => Some(("device:dri", Low, "Can use your graphics card")),
            ("devices", "input") => Some((
                "device:input",
                High,
                "Can read all keyboard, mouse and controller input, including what you type in other apps",
            )),
            ("devices", "usb") => Some((
                "device:usb",
                High,
                "Can talk to every USB device directly, including keyboards and storage",
            )),
            ("devices", "kvm") => Some(("device:kvm", High, "Can use hardware virtualization")),
            ("devices", "shm") => Some(("device:shm", Low, "Can use shared memory")),
            ("devices", "all") => {
                Some(("device:all", High, "Can use all devices, including disks"))
            }
            ("features", "devel") => Some((
                "feature:devel",
                Medium,
                "Can use development tools such as debuggers",
            )),
            ("features", "multiarch") => {
                Some(("feature:multiarch", Low, "Can run 32-bit programs"))
            }
            ("features", "bluetooth") => Some(("feature:bluetooth", Medium, "Can use Bluetooth")),
            ("features", "canbus") => Some(("feature:canbus", Medium, "Can use CAN bus networks")),
            ("features", "per-app-dev-shm") => Some((
                "feature:per-app-dev-shm",
                Low,
                "Has its own shared memory area",
            )),
            _ => None,
        };
        if let Some((code, risk, text)) = known {
            return Item {
                key: code.to_string(),
                rank: 1,
                exact: None,
                always: false,
                place: None,
                runtime_only: false,
                perm: Permission::new(code.to_string(), risk, text.to_string()),
            };
        }
        let simple = |code: String, risk: Risk, text: String| Item {
            key: code.clone(),
            rank: 1,
            exact: None,
            always: false,
            place: None,
            runtime_only: false,
            perm: Permission::new(code, risk, text),
        };
        let unknown = || {
            simple(
                format!("unknown:{}:{val}", part(key)),
                High,
                unknown_value(key, val),
            )
        };
        match key {
            "persistent" => {
                let segs: Vec<&str> = val.split('/').collect();
                if val.starts_with('/') || val == "~" || segs.contains(&"..") || segs == ["."] {
                    return unknown();
                }
                simple(
                    format!("persistent:{val}"),
                    Low,
                    format!(
                        "Keeps its own copy of \"{}\" in the app's private data",
                        shown(val, 120)
                    ),
                )
            }
            "unset-environment" => simple(
                format!("unset-env:{val}"),
                Low,
                format!("Clears the environment variable {}", shown(val, 80)),
            ),
            _ => unknown(),
        }
    }
}
