//! The Updates place: app updates through Telamon Updater's engine.
//!
//! The Store does not update apps itself. The list, the check of what an
//! update asks for, the update and the history are `telamon-updater-core`'s
//! `apps` and `apphistory` (the code Telamon Settings' Updates page and the
//! Updater's tray drive), called in the order Settings calls them:
//!
//! 1. check: `apps::list(refresh)`, then, when something waits,
//!    `apps::check`, which says which updates ask for NEW permissions;
//! 2. update: `apps::unseen(apps::check(..), &shown)` (nothing is installed
//!    that the page did not show with what it asks for), then
//!    `apps::update`, `apphistory::record`, and a fresh `apps::list`.
//!
//! Both run on a worker thread holding the Store's `OperationLock`, which
//! takes Telamon Updater's apps lock (so the tray's background rounds, Settings
//! and the Store never change installations together) and the Store's own.
//! The engine's own `lock::take` is not called: it would wait for the lock
//! this worker already holds.
//!
//! The engine can update everything that waits, not one app: the framework's
//! `UpdateOptions` has no filter of refs. So there is no per-row Update and no
//! Cancel once the engine runs (`apps::update` does not take a token); Cancel
//! is offered only while the worker waits for another update to finish.
//!
//! Nothing here runs in the background: a check starts when the page is
//! opened (and the last one this session is older than ten minutes) or when
//! the user asks, and never when the Store is closed.
//!
//! "Last checked" is the later of the Store's own record of its last
//! successful check (`$XDG_STATE_HOME/telamon-store/updates-checked`) and the
//! Updater's last background round (`RoundAt` in `telamon-updaterrc`). The
//! engine keeps no time for a check by hand, and a round that skipped its
//! check on a metered connection still sets `RoundAt`, which is why the later
//! of two is only an estimate.
//!
//! Every text from a remote (names, permissions, errors, release notes) is
//! cleaned here and shown as plain text by the QML.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        /// The object is started.
        #[qproperty(bool, ready)]
        /// A check or an update runs.
        #[qproperty(bool, busy)]
        /// "check", "update" or "" while idle.
        #[qproperty(QString, op)]
        /// The worker waits for another update to finish (Cancel works).
        #[qproperty(bool, waiting)]
        /// One plain line about what the worker does.
        #[qproperty(QString, status)]
        /// 0 to 100 while a figure is known, else -1.
        #[qproperty(i32, percent)]
        /// A check ended well this session: the lists below are known.
        #[qproperty(bool, loaded)]
        /// JSON array of the apps with an update waiting.
        #[qproperty(QString, apps_json, cxx_name = "appsJson")]
        /// JSON array of the runtimes and other components that update too.
        #[qproperty(QString, others_json, cxx_name = "othersJson")]
        #[qproperty(i32, app_count, cxx_name = "appCount")]
        #[qproperty(i32, other_count, cxx_name = "otherCount")]
        /// How many waiting updates ask for new permissions: Update All needs
        /// the user's confirmation, with them listed, while this is not 0.
        #[qproperty(i32, review_count, cxx_name = "reviewCount")]
        /// What an update downloads in all, or "" when not known.
        #[qproperty(QString, download_text, cxx_name = "downloadText")]
        /// Why the last check or update failed, in plain words, or "".
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// The engine's own words for it, cleaned, or "".
        #[qproperty(QString, error_detail, cxx_name = "errorDetail")]
        /// What the last update did, in plain words, or "".
        #[qproperty(QString, notice)]
        /// Unix seconds of the last check (the Store's or the Updater's), or 0.
        #[qproperty(i64, last_checked, cxx_name = "lastChecked")]
        /// Telamon Updater updates apps in the background.
        #[qproperty(bool, auto_updates, cxx_name = "autoUpdates")]
        /// Why Telamon Settings could not be opened, or "".
        #[qproperty(QString, settings_note, cxx_name = "settingsNote")]
        /// Goes up each time an update installed something.
        #[qproperty(i32, revision)]
        #[namespace = "telamon_store"]
        type AppUpdates = super::AppUpdatesRust;

        /// A check ended well; `count` is the apps waiting. The Store reads
        /// the catalog again, which the check's refresh may have changed.
        #[qsignal]
        fn listed(self: Pin<&mut AppUpdates>, count: i32);
    }

    unsafe extern "RustQt" {
        /// Starts the object. Once.
        #[qinvokable]
        fn start(self: Pin<&mut AppUpdates>);

        /// The page was opened: reads the settings, and checks when no check
        /// ended this session or the last is older than ten minutes.
        #[qinvokable]
        #[cxx_name = "pageOpened"]
        fn page_opened(self: Pin<&mut AppUpdates>);

        /// Looks for updates now.
        #[qinvokable]
        fn check(self: Pin<&mut AppUpdates>);

        /// Updates every app (and the components they need) that waits. The
        /// page asks first when any asks for new permissions (`reviewCount`).
        #[qinvokable]
        #[cxx_name = "updateAll"]
        fn update_all(self: Pin<&mut AppUpdates>);

        /// Updates what does not ask for new permissions and leaves the rest
        /// as it is (the engine's `hold_new_permissions`).
        #[qinvokable]
        #[cxx_name = "updateWithoutNewPermissions"]
        fn update_without_new_permissions(self: Pin<&mut AppUpdates>);

        /// Stops waiting for another update to finish.
        #[qinvokable]
        #[cxx_name = "cancelWait"]
        fn cancel_wait(self: Pin<&mut AppUpdates>);

        /// Forgets the error and the notice.
        #[qinvokable]
        #[cxx_name = "clearMessages"]
        fn clear_messages(self: Pin<&mut AppUpdates>);

        /// The catalog changed: names, icons and release notes are read again.
        #[qinvokable]
        #[cxx_name = "libraryChanged"]
        fn library_changed(self: Pin<&mut AppUpdates>);

        /// Opens Telamon Settings on its Updates page.
        #[qinvokable]
        #[cxx_name = "openSettings"]
        fn open_settings(self: Pin<&mut AppUpdates>);

        /// The same, with the window system's activation token ("" for
        /// none), so the window comes to the front.
        #[qinvokable]
        #[cxx_name = "openSettingsWithToken"]
        fn open_settings_with_token(self: Pin<&mut AppUpdates>, token: &QString);
    }

    impl cxx_qt::Threading for AppUpdates {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn app_updates_make_unique() -> UniquePtr<AppUpdates>;
    }
}

use core::pin::Pin;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::{Value, json};
use telamon_store_core::appstream::{Block, Release, ReleaseKind, Span};
use telamon_store_core::catalog::Library;
use telamon_store_core::flatpak::{
    CancelToken, LockError, OperationLock, RefKind, Scope, list_installed_all,
    valid_activation_token,
};
use telamon_store_core::text::clean;
use telamon_updater_core::apps::{self, Done, HeldApp, Row};
use telamon_updater_core::{apphistory, rc};

use crate::catalog::{ICON_SIZE, library, safe_icon};

/// A check older than this is made again when the page is opened.
const STALE_AFTER: Duration = Duration::from_secs(600);
/// How long a worker waits for another update before it gives up.
const LOCK_GIVE_UP: Duration = Duration::from_secs(600);
/// Release notes in a row are cut at this many characters.
const NOTES_MAX: usize = 300;
/// The program that shows Telamon Settings. Fixed: no user input in it.
const SETTINGS_BIN: &str = "/usr/bin/telamon-settings";
/// Its arguments: the Updates page, at the app updates (as the tray opens it).
const SETTINGS_ARGS: [&str; 2] = ["updates", "apps"];
/// The Store's record of its last successful check, in its state folder.
const CHECKED_FILE: &str = "updates-checked";

// ---- pure logic (tested below) ----

/// What a row shows besides the engine's own fields.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The catalog's name; "" when the app is not in the catalog.
    pub name: String,
    /// A `file:` URL or "".
    pub icon: String,
    /// The catalog's newest release, as (version, plain text).
    pub release: Option<(String, String)>,
}

/// Flattens a release description into one plain line.
fn plain_blocks(blocks: &[Block]) -> String {
    let spans = |s: &[Span]| s.iter().map(|x| x.text.as_str()).collect::<String>();
    let parts: Vec<String> = blocks
        .iter()
        .take(8)
        .map(|b| match b {
            Block::Paragraph(s) => spans(s),
            Block::List { items, .. } => items
                .iter()
                .map(|it| format!("\u{2022} {}", spans(it)))
                .collect::<Vec<_>>()
                .join(" "),
        })
        .filter(|p| !p.trim().is_empty())
        .collect();
    clean(&parts.join(" "), NOTES_MAX)
}

/// What the catalog knows about the newest stable release, when it is not
/// the version that is installed. The catalog may be older than the check
/// (it is read again after one), and an app with no installed version can't be
/// compared: both give nothing, never a guess.
pub fn whats_new(releases: &[Release], installed: &str) -> Option<(String, String)> {
    let newest = releases.iter().find(|r| r.kind == ReleaseKind::Stable)?;
    let version = clean(&newest.version, 40);
    if version.is_empty() || installed.is_empty() || version == clean(installed, 40) {
        return None;
    }
    Some((version, plain_blocks(&newest.description)))
}

/// "stable · for you · 12.3 MB download".
pub fn detail_line(r: &Row) -> String {
    let mut parts: Vec<String> = Vec::new();
    let branch = clean(&r.branch, 40);
    if !branch.is_empty() {
        parts.push(branch);
    }
    parts.push(scope_words(r.system).to_string());
    let size = apps::format_size(r.size);
    if !size.is_empty() {
        parts.push(format!("{size} download"));
    }
    parts.join(" \u{b7} ")
}

pub fn scope_words(system: bool) -> &'static str {
    if system {
        "for everyone on this computer"
    } else {
        "for you"
    }
}

/// An app's row for the page.
pub fn app_json(r: &Row, held: Option<&HeldApp>, facts: &Facts) -> Value {
    let name = if facts.name.is_empty() {
        clean(&r.name, 80)
    } else {
        clean(&facts.name, 80)
    };
    let name = if name.is_empty() {
        clean(&r.id, 80)
    } else {
        name
    };
    let notes = match &facts.release {
        Some((v, t)) if t.is_empty() => format!("New version: {v}"),
        Some((v, t)) => format!("What's new in {v}: {t}"),
        None => String::new(),
    };
    json!({
        "appId": clean(&r.id, 255),
        "name": name,
        "iconSource": facts.icon,
        "detail": detail_line(r),
        // What it asks for that the installed version doesn't have: a few
        // words for the row, all of them for the confirmation.
        "asks": held.map(|h| apps::asks_text(&h.asks)).unwrap_or_default(),
        "asksList": held
            .map(|h| h.asks.iter().take(60).map(|a| clean(a, 200)).collect::<Vec<_>>())
            .unwrap_or_default(),
        "review": held.is_some(),
        "notes": notes,
    })
}

/// A runtime or other component that is updated along with the apps.
pub fn other_json(r: &Row) -> Value {
    let name = clean(&r.name, 80);
    let size = apps::format_size(r.size);
    json!({
        "name": if name.is_empty() { clean(&r.id, 80) } else { name },
        "detail": if size.is_empty() {
            scope_words(r.system).to_string()
        } else {
            format!("{} \u{b7} {size}", scope_words(r.system))
        },
    })
}

/// The key `apps::row_key` makes, from an installed ref.
fn version_key(id: &str, branch: &str, system: bool) -> String {
    format!("{id}/{branch}/{}", if system { 's' } else { 'u' })
}

/// What the page holds between the engine's calls. No Qt in it.
#[derive(Debug, Default, Clone)]
pub struct Model {
    /// Updates waiting: apps first, then components.
    pub rows: Vec<Row>,
    /// Apps that ask for new permissions, by `apps::row_key`: what the page
    /// shows and what `apps::unseen` compares an update with.
    pub held: HashMap<String, HeldApp>,
    /// Installed versions, by `apps::row_key`.
    pub versions: HashMap<String, String>,
}

impl Model {
    /// Notes apps that ask for new permissions.
    fn note(&mut self, held: &[HeldApp]) {
        for h in held {
            self.held.insert(h.key.clone(), h.clone());
        }
    }

    /// A check ended: its list, and what the waiting updates ask for. A check
    /// of permissions that ran to the end replaces the earlier notes; one
    /// that failed leaves them (the page then says it couldn't check). Notes
    /// of updates no longer waiting go.
    pub fn listed(&mut self, rows: Vec<Row>, checked: Option<&Done>) {
        if let Some(c) = checked {
            if c.error.is_none() {
                self.held.clear();
            }
            self.note(&c.held_back);
        }
        self.rows = rows;
        self.forget_gone();
    }

    fn forget_gone(&mut self) {
        let keys: std::collections::HashSet<String> = self.rows.iter().map(apps::row_key).collect();
        self.held.retain(|k, _| keys.contains(k));
    }

    /// An update run ended. `left` is the list afterwards (without a refresh,
    /// so sizes are zero: the old ones are kept), or why it could not be read:
    /// then what was waiting, less what was installed.
    pub fn updated(&mut self, left: Result<Vec<Row>, String>, done: &Done) {
        let before = std::mem::take(&mut self.rows);
        let mut rows = match left {
            Ok(rows) => rows,
            Err(_) => before
                .iter()
                .filter(|r| {
                    !done
                        .updated
                        .iter()
                        .any(|u| u.id == r.id && u.branch == r.branch && u.system == r.system)
                })
                .cloned()
                .collect(),
        };
        for r in &mut rows {
            if r.size == 0
                && let Some(b) = before.iter().find(|b| apps::row_key(b) == apps::row_key(r))
            {
                r.size = b.size;
                r.size_text = b.size_text.clone();
            }
        }
        self.rows = rows;
        self.note(&done.held_back);
        self.forget_gone();
    }

    pub fn app_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| !r.runtime)
    }

    pub fn other_rows(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().filter(|r| r.runtime)
    }

    /// The installed version of a waiting update, or "".
    pub fn version_of(&self, r: &Row) -> &str {
        self.versions
            .get(&apps::row_key(r))
            .map_or("", String::as_str)
    }

    /// What an update downloads in all.
    pub fn download_text(&self) -> String {
        apps::format_size(self.rows.iter().map(|r| r.size).sum())
    }

    /// How many waiting updates ask for new permissions. Update All needs
    /// the user's confirmation, with them listed, when this is not 0. Every
    /// row counts, not only apps, so nothing can slip past the dialog.
    pub fn review_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| self.held.contains_key(&apps::row_key(r)))
            .count()
    }
}

/// What the page shows of a [`Model`], built off the GUI thread: the
/// catalog lookups stat icon files.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct View {
    pub apps_json: String,
    pub others_json: String,
    pub app_count: i32,
    pub other_count: i32,
    pub download_text: String,
    pub review_count: i32,
}

/// Builds the page's lists. `facts` is the catalog's word on an app (its
/// installed version is the second argument).
pub fn render(m: &Model, facts: impl Fn(&Row, &str) -> Facts) -> View {
    let apps_v: Vec<Value> = m
        .app_rows()
        .map(|r| app_json(r, m.held.get(&apps::row_key(r)), &facts(r, m.version_of(r))))
        .collect();
    let others_v: Vec<Value> = m.other_rows().map(other_json).collect();
    View {
        app_count: i32::try_from(apps_v.len()).unwrap_or(i32::MAX),
        other_count: i32::try_from(others_v.len()).unwrap_or(i32::MAX),
        apps_json: Value::Array(apps_v).to_string(),
        others_json: Value::Array(others_v).to_string(),
        download_text: m.download_text(),
        review_count: i32::try_from(m.review_count()).unwrap_or(i32::MAX),
    }
}

/// What an update run may install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateMode {
    /// Everything that waits, also what asks for new permissions: only after
    /// the page showed what they ask for (and the user confirmed it, when
    /// any does).
    All,
    /// Everything that doesn't ask for new permissions; the engine leaves
    /// the others out and reports them.
    LeaveOutNewPermissions,
}

impl UpdateMode {
    /// `hold` of `apps::update` (the engine's `hold_new_permissions`).
    pub fn hold(self) -> bool {
        self == UpdateMode::LeaveOutNewPermissions
    }
}

/// The guard before an update, as Settings has it: with everything to be
/// installed, look again at what the updates ask for and stop if it is more
/// than the page showed (`apps::unseen`). Leaving out what asks for new
/// permissions needs no look: the engine does that itself, and says which.
/// `check` runs only when it is needed.
pub fn guard_for(
    mode: UpdateMode,
    check: impl FnOnce() -> Done,
    shown: &HashMap<String, HeldApp>,
) -> Result<(), Box<Done>> {
    if mode.hold() {
        return Ok(());
    }
    guard_update(check(), shown)
}

/// Waits for a worker for at most `cap`: `true` when it ended.
pub fn join_capped(h: std::thread::JoinHandle<()>, cap: Duration) -> bool {
    let start = Instant::now();
    while !h.is_finished() {
        if start.elapsed() > cap {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = h.join();
    true
}

/// What the page does with an update request: go on, or stop because the
/// check found updates asking for more than the page showed (they are
/// returned, to be shown, and nothing is installed).
pub fn guard_update(checked: Done, shown: &HashMap<String, HeldApp>) -> Result<(), Box<Done>> {
    match apps::unseen(checked, shown) {
        None => Ok(()),
        Some(stop) => Err(Box::new(stop)),
    }
}

/// The later of the Store's and the Updater's last time, ignoring none and
/// times in the future (a wrong clock or a hand-edited file).
pub fn effective_last_checked(store: Option<i64>, round: Option<i64>, now: i64) -> i64 {
    [store, round]
        .into_iter()
        .flatten()
        .filter(|t| *t > 0 && *t <= now + 300)
        .max()
        .unwrap_or(0)
}

/// Whether a list from `age` ago is old enough to look again.
pub fn is_stale(age: Option<Duration>) -> bool {
    age.is_none_or(|a| a > STALE_AFTER)
}

/// The engine's progress line ("Installing org.example.App… 37%") with the
/// app's name for its ID, cleaned, and the percent (-1 when none).
pub fn progress_line(raw: &str, names: &[(String, String)]) -> (String, i32) {
    let percent = raw
        .trim_end()
        .strip_suffix('%')
        .and_then(|s| {
            let digits: String = s
                .chars()
                .rev()
                .take_while(char::is_ascii_digit)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            digits.parse::<i32>().ok()
        })
        .filter(|p| (0..=100).contains(p))
        .unwrap_or(-1);
    let mut line = clean(raw, 200);
    for (id, name) in names {
        if !id.is_empty() && !name.is_empty() && line.contains(id.as_str()) {
            line = line.replacen(id.as_str(), name, 1);
            break;
        }
    }
    (line, percent)
}

/// What the page says about an update run: the apps that were updated.
pub fn updated_notice(updated: &[apphistory::Entry], left_out: usize) -> String {
    let base = updated_names(updated);
    let out = match left_out {
        0 => String::new(),
        1 => "1 app that asks for new permissions was left out.".to_string(),
        n => format!("{n} apps that ask for new permissions were left out."),
    };
    match (base.is_empty(), out.is_empty()) {
        (_, true) => base,
        (true, false) => out,
        (false, false) => format!("{base} {out}"),
    }
}

fn updated_names(updated: &[apphistory::Entry]) -> String {
    let names: Vec<String> = updated
        .iter()
        .filter(|u| !u.runtime)
        .map(|u| {
            let n = clean(&u.name, 60);
            if n.is_empty() { clean(&u.id, 60) } else { n }
        })
        .collect();
    match names.as_slice() {
        [] if updated.is_empty() => String::new(),
        [] => "Components were updated.".to_string(),
        [a] => format!("Updated {a}."),
        [a, b] => format!("Updated {a} and {b}."),
        [a, b, c] => format!("Updated {a}, {b} and {c}."),
        [a, b, rest @ ..] => format!("Updated {a}, {b} and {} more apps.", rest.len()),
    }
}

/// Which request the words belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ctx {
    Check,
    Update,
}

/// An error in plain words, and the engine's own words (cleaned) for those
/// who want them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub text: String,
    pub detail: String,
}

/// Plain words for what the engine, libflatpak or a remote said. The engine's
/// strings are English text from other programs: they are matched loosely and
/// shown cleaned, whatever they say.
pub fn plain_error(ctx: Ctx, raw: &str) -> Shown {
    let detail = clean(raw, 240);
    let low = raw.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| low.contains(w));
    let head = match ctx {
        Ctx::Check => "Couldn't check for updates.",
        Ctx::Update => "The update didn't finish.",
    };
    let reason = if has(&["no space left", "not enough space", "disk full"]) {
        Some("There isn't enough free disk space.")
    } else if has(&[
        "could not resolve",
        "name or service not known",
        "temporary failure in name resolution",
        "network is unreachable",
        "no route to host",
        "connection refused",
        "connection reset",
        "unable to connect",
        "failed to connect",
        "timed out",
        "timeout",
        "offline",
    ]) {
        Some(
            "The Store couldn't reach the app source. Check your internet connection and try again.",
        )
    } else if has(&[
        "not authorized",
        "not allowed",
        "authorization",
        "authentication",
        "polkit",
        "permission denied",
    ]) {
        Some("This computer didn't allow the change.")
    } else if has(&["signature", "gpg"]) {
        Some("An app source's signature didn't check out, so nothing was changed.")
    } else if has(&["cancelled", "canceled"]) {
        Some("It was cancelled.")
    } else {
        Some(match ctx {
            Ctx::Check => "Something went wrong. Try again in a moment.",
            Ctx::Update => "Something went wrong. Look at what is installed, then try again.",
        })
    };
    Shown {
        text: match reason {
            Some(r) => format!("{head} {r}"),
            None => head.to_string(),
        },
        detail,
    }
}

/// A lock that could not be taken, in plain words.
pub fn lock_error(e: &LockError) -> Shown {
    let text = match e {
        LockError::Busy(_) => "Another update is still running. Try again when it is done.",
        LockError::Cancelled => "",
        _ => "The Store couldn't make sure no other update is running, so it did not start.",
    };
    Shown {
        text: text.to_string(),
        detail: clean(&e.to_string(), 240),
    }
}

// ---- files and Telamon Settings ----

fn state_base() -> Option<PathBuf> {
    let abs = |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    abs(std::env::var_os("XDG_STATE_HOME"))
        .or_else(|| abs(std::env::var_os("HOME")).map(|h| h.join(".local/state")))
}

/// Reads the Store's last check in `base/telamon-store`: one line, Unix
/// seconds. A link, a file that is not a regular file of ours, a long file or
/// anything but digits is not read.
pub fn read_checked_in(base: &Path) -> Option<i64> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(
            base.join(telamon_store_core::legacy::NAME)
                .join(CHECKED_FILE),
        )
        .ok()?;
    let md = f.metadata().ok()?;
    // SAFETY: geteuid has no arguments and cannot fail.
    if !md.is_file() || md.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let mut text = String::new();
    f.take(32).read_to_string(&mut text).ok()?;
    let t = text.trim();
    (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .then(|| t.parse().ok())
        .flatten()
}

/// Writes it: the folder is made (0700) if needed, the file replaced
/// atomically, nothing followed through a link.
pub fn write_checked_in(base: &Path, at: i64) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    // A folder under the old name moves first, so it is not left behind.
    let _ = telamon_store_core::legacy::move_once(base);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(base)?;
    let dir = base.join(telamon_store_core::legacy::NAME);
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let tmp = dir.join(format!("{CHECKED_FILE}.new"));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    writeln!(f, "{at}")?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(CHECKED_FILE))
}

/// The command that opens Telamon Settings on the Updates page: a fixed
/// program and fixed arguments, with the activation token (if it is valid)
/// in the environment, and no shell.
pub fn settings_command(bin: &Path, token: Option<&str>) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(bin);
    cmd.args(SETTINGS_ARGS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    match token.and_then(valid_activation_token) {
        Some(t) => cmd.env("XDG_ACTIVATION_TOKEN", t),
        None => cmd.env_remove("XDG_ACTIVATION_TOKEN"),
    };
    cmd.env_remove("DESKTOP_STARTUP_ID");
    cmd
}

/// Starts Telamon Settings and waits for it (so it is not left a zombie).
/// The error is in plain words.
fn run_settings(bin: &Path, token: Option<&str>) -> Result<(), String> {
    if !bin.is_file() {
        return Err("Telamon Settings isn't installed on this computer.".into());
    }
    let mut child = settings_command(bin, token)
        .spawn()
        .map_err(|_| "Telamon Settings could not be started.".to_string())?;
    let _ = child.wait();
    Ok(())
}

// ---- the workers ----

/// What a worker read from the settings and the state folder.
struct Info {
    auto: bool,
    round_at: Option<i64>,
    store_at: Option<i64>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

fn read_info() -> Info {
    Info {
        auto: rc::apps_automatic(),
        round_at: rc::get(rc::APPS, telamon_updater_core::worker::ROUND_AT)
            .and_then(|v| v.trim().parse().ok()),
        store_at: state_base().and_then(|b| read_checked_in(&b)),
    }
}

/// The result of a check. The model and the page's lists are built on the
/// worker, so the GUI thread only takes them over.
struct Checked {
    /// The new model and what it shows; `None` when the list failed.
    listed: Option<(Model, View)>,
    /// Why the list failed (the engine's words).
    list_error: Option<String>,
    /// Why what the updates ask for could not be checked.
    asks_error: Option<String>,
    /// Why the worker did not run (the lock), in plain words.
    refused: Option<Shown>,
    cancelled: bool,
}

/// The result of an update.
struct Updated {
    /// The check before updating found more than the page showed.
    stopped: bool,
    mode: UpdateMode,
    done: Done,
    model: Model,
    view: View,
    refused: Option<Shown>,
    cancelled: bool,
}

fn say(thread: &CxxQtThread<qobject::AppUpdates>, waiting: bool, line: &str) {
    let line = line.to_string();
    let _ = thread.queue(move |mut o| {
        o.as_mut().set_waiting(waiting);
        o.as_mut().set_status(QString::from(line.as_str()));
        o.as_mut().set_percent(-1);
    });
}

/// Takes the shared lock; says so while another update holds it.
fn take_lock(
    thread: &CxxQtThread<qobject::AppUpdates>,
    cancel: &CancelToken,
) -> Result<OperationLock, LockError> {
    match OperationLock::acquire(Duration::from_millis(400), cancel) {
        Ok(l) => return Ok(l),
        Err(LockError::Busy(_)) => {}
        Err(e) => return Err(e),
    }
    say(thread, true, "Another update is running");
    let start = Instant::now();
    loop {
        match OperationLock::acquire(Duration::from_secs(2), cancel) {
            Ok(l) => {
                return Ok(l);
            }
            Err(LockError::Busy(_)) if start.elapsed() < LOCK_GIVE_UP => {}
            Err(e) => return Err(e),
        }
    }
}

/// The installed versions of everything the lists name. Best effort: a
/// failure leaves the release notes out, nothing more.
fn installed_versions() -> HashMap<String, String> {
    let out = list_installed_all(&CancelToken::new());
    out.refs
        .iter()
        .filter(|r| r.kind == RefKind::App && !r.version.is_empty())
        .map(|r| {
            (
                version_key(&r.id, &r.branch, r.scope == Scope::System),
                r.version.clone(),
            )
        })
        .collect()
}

/// The catalog's view of the model, as the page shows it.
fn view_of(m: &Model) -> View {
    let lib = library();
    render(m, |r, v| facts_of(lib.as_deref(), r, v))
}

fn check_job(
    thread: &CxxQtThread<qobject::AppUpdates>,
    cancel: &CancelToken,
    prev: Model,
) -> Checked {
    let lock = match take_lock(thread, cancel) {
        Ok(l) => l,
        Err(e) => {
            return Checked {
                listed: None,
                list_error: None,
                asks_error: None,
                cancelled: e == LockError::Cancelled,
                refused: Some(lock_error(&e)),
            };
        }
    };
    say(thread, false, "Looking for app updates\u{2026}");
    // The engine's own calls, as Settings makes them: a refresh, and then
    // what the waiting updates ask for (nothing is downloaded or installed).
    let rows = apps::list(true, false, None);
    let asks = match &rows {
        Ok(r) if !r.is_empty() => Some(apps::check(None)),
        _ => None,
    };
    drop(lock);
    if let Some(e) = asks.as_ref().and_then(|c| c.error.as_ref()) {
        log::warn!("could not check what app updates ask for: {e}");
    }
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            return Checked {
                listed: None,
                list_error: Some(e),
                asks_error: None,
                refused: None,
                cancelled: false,
            };
        }
    };
    let mut model = prev;
    model.versions = if rows.is_empty() {
        HashMap::new()
    } else {
        installed_versions()
    };
    model.listed(rows, asks.as_ref());
    let view = view_of(&model);
    Checked {
        listed: Some((model, view)),
        list_error: None,
        asks_error: asks.and_then(|c| c.error),
        refused: None,
        cancelled: false,
    }
}

fn update_job(
    thread: &CxxQtThread<qobject::AppUpdates>,
    cancel: &CancelToken,
    mode: UpdateMode,
    prev: Model,
    names: Vec<(String, String)>,
) -> Updated {
    let lock = match take_lock(thread, cancel) {
        Ok(l) => l,
        Err(e) => {
            return Updated {
                stopped: false,
                mode,
                done: Done::default(),
                model: prev,
                view: View::default(),
                cancelled: e == LockError::Cancelled,
                refused: Some(lock_error(&e)),
            };
        }
    };
    say(thread, false, "Updating apps\u{2026}");
    // The page's notes may be hours old: look again, and install nothing if
    // something now asks for more than the page showed.
    let (stopped, done) = match guard_for(mode, || apps::check(None), &prev.held) {
        Err(stop) => (true, *stop),
        Ok(()) => {
            let qt = thread.clone();
            let mut last = String::new();
            let done = apps::update(None, false, mode.hold(), move |raw| {
                let (line, percent) = progress_line(&raw, &names);
                if line == last {
                    return;
                }
                last.clone_from(&line);
                let _ = qt.queue(move |mut o| {
                    o.as_mut().set_status(QString::from(line.as_str()));
                    o.as_mut().set_percent(percent);
                });
            });
            (false, done)
        }
    };
    if let Err(e) = apphistory::record(&done.updated, None) {
        log::warn!("could not save the app update history: {e}");
    }
    let left = apps::list(false, false, None);
    drop(lock);
    let mut model = prev;
    model.versions = match &left {
        Ok(r) if !r.is_empty() => installed_versions(),
        _ => HashMap::new(),
    };
    model.updated(left, &done);
    let view = view_of(&model);
    Updated {
        stopped,
        mode,
        done,
        model,
        view,
        refused: None,
        cancelled: false,
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> bool {
    spawn_handle(name, f).is_some()
}

fn spawn_handle(
    name: &str,
    f: impl FnOnce() + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .spawn(f)
        .ok()
}

// ---- the QObject ----

pub struct AppUpdatesRust {
    ready: bool,
    busy: bool,
    op: QString,
    waiting: bool,
    status: QString,
    percent: i32,
    loaded: bool,
    apps_json: QString,
    others_json: QString,
    app_count: i32,
    other_count: i32,
    download_text: QString,
    error_text: QString,
    error_detail: QString,
    notice: QString,
    last_checked: i64,
    auto_updates: bool,
    settings_note: QString,
    revision: i32,
    review_count: i32,
    model: Model,
    cancel: CancelToken,
    /// The check or update worker, joined when the object goes.
    worker: Option<std::thread::JoinHandle<()>>,
    /// Counts the page's lists: a list built from an older model is dropped.
    render_seq: u64,
    /// When the list was last read this session.
    listed_at: Option<Instant>,
    round_at: Option<i64>,
    store_at: Option<i64>,
}

impl Default for AppUpdatesRust {
    fn default() -> Self {
        Self {
            ready: false,
            busy: false,
            op: QString::default(),
            waiting: false,
            status: QString::default(),
            percent: -1,
            loaded: false,
            apps_json: QString::from("[]"),
            others_json: QString::from("[]"),
            app_count: 0,
            other_count: 0,
            download_text: QString::default(),
            error_text: QString::default(),
            error_detail: QString::default(),
            notice: QString::default(),
            last_checked: 0,
            auto_updates: false,
            settings_note: QString::default(),
            revision: 0,
            review_count: 0,
            model: Model::default(),
            cancel: CancelToken::new(),
            worker: None,
            render_seq: 0,
            listed_at: None,
            round_at: None,
            store_at: None,
        }
    }
}

/// How long quitting waits for a running check or update.
const QUIT_WAIT: Duration = Duration::from_secs(10);

impl Drop for AppUpdatesRust {
    /// Quitting stops a wait for the lock at once. The engine cannot be
    /// stopped once it runs, so a running update is waited for, up to ten
    /// seconds, as the other workers are.
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(h) = self.worker.take()
            && !join_capped(h, QUIT_WAIT)
        {
            log::error!("the app update did not finish within 10 s; leaving it");
        }
    }
}

/// The catalog's facts about an app, for its row.
fn facts_of(lib: Option<&Library>, row: &Row, installed: &str) -> Facts {
    let Some(lib) = lib else {
        return Facts::default();
    };
    let Some(id) = lib.find(&row.id) else {
        return Facts::default();
    };
    let c = lib.component(id);
    Facts {
        name: c.name.clone(),
        icon: lib
            .source(id)
            .icon_path(c, ICON_SIZE)
            .and_then(|p| safe_icon(&p))
            .unwrap_or_default(),
        release: whats_new(&c.releases, installed),
    }
}

impl qobject::AppUpdates {
    pub fn start(mut self: Pin<&mut Self>) {
        if !*self.ready() {
            self.as_mut().set_ready(true);
        }
    }

    pub fn page_opened(self: Pin<&mut Self>) {
        if *self.busy() {
            return;
        }
        let age = self.rust().listed_at.map(|t| t.elapsed());
        if is_stale(age) {
            self.check();
            return;
        }
        // The list is this session's and recent; only the settings and the
        // time may have changed (the tray's rounds).
        let qt = self.qt_thread();
        spawn("telamon-updates-info", move || {
            let info = catch_unwind(read_info).ok();
            let _ = qt.queue(move |mut o| {
                if let Some(i) = info {
                    o.as_mut().apply_info(i);
                }
            });
        });
    }

    fn apply_info(mut self: Pin<&mut Self>, info: Info) {
        self.as_mut().set_auto_updates(info.auto);
        {
            let mut r = self.as_mut().rust_mut();
            r.round_at = info.round_at;
            r.store_at = info.store_at;
        }
        self.as_mut().show_last_checked();
    }

    fn show_last_checked(mut self: Pin<&mut Self>) {
        let t = effective_last_checked(self.rust().store_at, self.rust().round_at, now_unix());
        if *self.last_checked() != t {
            self.as_mut().set_last_checked(t);
        }
    }

    fn begin(mut self: Pin<&mut Self>, op: &str, status: &str) -> CancelToken {
        let cancel = CancelToken::new();
        self.as_mut().rust_mut().cancel = cancel.clone();
        self.as_mut().set_busy(true);
        self.as_mut().set_op(QString::from(op));
        self.as_mut().set_waiting(false);
        self.as_mut().set_percent(-1);
        self.as_mut().set_status(QString::from(status));
        self.as_mut().set_error_text(QString::default());
        self.as_mut().set_error_detail(QString::default());
        self.as_mut().set_notice(QString::default());
        cancel
    }

    fn end(mut self: Pin<&mut Self>) {
        self.as_mut().set_busy(false);
        self.as_mut().set_op(QString::default());
        self.as_mut().set_waiting(false);
        self.as_mut().set_status(QString::default());
        self.as_mut().set_percent(-1);
    }

    fn fail(mut self: Pin<&mut Self>, shown: &Shown) {
        self.as_mut()
            .set_error_text(QString::from(shown.text.as_str()));
        self.as_mut()
            .set_error_detail(QString::from(shown.detail.as_str()));
    }

    pub fn check(mut self: Pin<&mut Self>) {
        if *self.busy() {
            return;
        }
        let cancel = self
            .as_mut()
            .begin("check", "Looking for app updates\u{2026}");
        let prev = self.rust().model.clone();
        let qt = self.qt_thread();
        let qt_fail = qt.clone();
        let handle = spawn_handle("telamon-updates-check", move || {
            let info = catch_unwind(read_info).ok();
            let _ = qt.queue(move |mut o| {
                if let Some(i) = info {
                    o.as_mut().apply_info(i);
                }
            });
            let out = catch_unwind(AssertUnwindSafe(|| check_job(&qt, &cancel, prev)));
            let _ = qt.queue(move |mut o| o.as_mut().finish_check(out.ok()));
        });
        if handle.is_none() {
            let _ = qt_fail.queue(|mut o| {
                o.as_mut().end();
                o.as_mut().fail(&Shown {
                    text: "Couldn't check for updates. The Store could not start a worker.".into(),
                    detail: String::new(),
                });
            });
        }
        self.as_mut().rust_mut().worker = handle;
    }

    fn finish_check(mut self: Pin<&mut Self>, out: Option<Checked>) {
        self.as_mut().end();
        let Some(out) = out else {
            log::error!("the update check panicked");
            self.fail(&Shown {
                text: "Couldn't check for updates. Something went wrong inside the Store.".into(),
                detail: String::new(),
            });
            return;
        };
        if out.cancelled {
            self.as_mut().set_notice(QString::from("Cancelled."));
            return;
        }
        if let Some(s) = out.refused {
            self.fail(&s);
            return;
        }
        let Some((model, view)) = out.listed else {
            let e = out.list_error.unwrap_or_default();
            self.fail(&plain_error(Ctx::Check, &e));
            return;
        };
        self.as_mut().rust_mut().model = model;
        self.as_mut().rust_mut().listed_at = Some(Instant::now());
        self.as_mut().set_loaded(true);
        // Only a check that listed counts as one, also when the question
        // what the updates ask for failed (said below).
        let now = now_unix();
        self.as_mut().rust_mut().store_at = Some(now);
        if let Some(base) = state_base() {
            spawn("telamon-updates-time", move || {
                if let Err(e) = write_checked_in(&base, now) {
                    log::warn!("could not save the time of the update check: {e}");
                }
            });
        }
        self.as_mut().show_last_checked();
        self.as_mut().show_view(&view);
        if let Some(e) = out.asks_error {
            let mut s = plain_error(Ctx::Check, &e);
            s.text = "Couldn't check which updates ask for new permissions. They can't be installed from here until that works.".into();
            self.as_mut().fail(&s);
        }
        let n = *self.app_count();
        self.listed(n);
    }

    pub fn update_all(self: Pin<&mut Self>) {
        self.start_update(UpdateMode::All);
    }

    pub fn update_without_new_permissions(self: Pin<&mut Self>) {
        self.start_update(UpdateMode::LeaveOutNewPermissions);
    }

    fn start_update(mut self: Pin<&mut Self>, mode: UpdateMode) {
        if *self.busy() || self.rust().model.rows.is_empty() {
            return;
        }
        let cancel = self.as_mut().begin("update", "Updating apps\u{2026}");
        let prev = self.rust().model.clone();
        let names: Vec<(String, String)> = prev
            .rows
            .iter()
            .map(|r| (r.id.clone(), clean(&r.name, 80)))
            .collect();
        let qt = self.qt_thread();
        let qt_fail = qt.clone();
        let handle = spawn_handle("telamon-updates-update", move || {
            let out = catch_unwind(AssertUnwindSafe(|| {
                update_job(&qt, &cancel, mode, prev, names)
            }));
            let _ = qt.queue(move |mut o| o.as_mut().finish_update(out.ok()));
        });
        if handle.is_none() {
            let _ = qt_fail.queue(|mut o| {
                o.as_mut().end();
                o.as_mut().fail(&Shown {
                    text: "The update didn't finish. The Store could not start a worker.".into(),
                    detail: String::new(),
                });
            });
        }
        self.as_mut().rust_mut().worker = handle;
    }

    fn finish_update(mut self: Pin<&mut Self>, out: Option<Updated>) {
        self.as_mut().end();
        let Some(out) = out else {
            log::error!("the app update panicked");
            self.fail(&Shown {
                text: "The update didn't finish. Something went wrong inside the Store.".into(),
                detail: String::new(),
            });
            return;
        };
        if out.cancelled {
            self.as_mut().set_notice(QString::from("Cancelled."));
            return;
        }
        if let Some(s) = out.refused {
            self.fail(&s);
            return;
        }
        let done = out.done;
        let installed = !done.updated.is_empty();
        self.as_mut().rust_mut().model = out.model;
        self.as_mut().rust_mut().listed_at = Some(Instant::now());
        self.as_mut().show_view(&out.view);
        if out.stopped && done.error.is_none() {
            self.as_mut().set_notice(QString::from(
                "Some updates ask for new permissions. Look at them below, then press Update All again.",
            ));
        } else {
            let left_out = if out.mode.hold() {
                done.held_back.len()
            } else {
                0
            };
            let text = updated_notice(&done.updated, left_out);
            if !text.is_empty() {
                self.as_mut().set_notice(QString::from(text.as_str()));
            }
        }
        if let Some(e) = &done.error {
            self.as_mut().fail(&plain_error(Ctx::Update, e));
        }
        if installed {
            let rev = self.revision().wrapping_add(1);
            self.as_mut().set_revision(rev);
        }
    }

    pub fn cancel_wait(self: Pin<&mut Self>) {
        if *self.waiting() {
            self.rust().cancel.cancel();
        }
    }

    pub fn clear_messages(mut self: Pin<&mut Self>) {
        self.as_mut().set_error_text(QString::default());
        self.as_mut().set_error_detail(QString::default());
        self.as_mut().set_notice(QString::default());
    }

    /// The catalog changed: names, icons and release notes are looked up
    /// again, on a worker (the lookups stat icon files). Not while a check or
    /// an update runs: it ends with fresh lists of its own.
    pub fn library_changed(mut self: Pin<&mut Self>) {
        if !*self.loaded() || *self.busy() {
            return;
        }
        let seq = self.rust().render_seq.wrapping_add(1);
        self.as_mut().rust_mut().render_seq = seq;
        let model = self.rust().model.clone();
        let qt = self.qt_thread();
        spawn("telamon-updates-view", move || {
            if let Ok(view) = catch_unwind(AssertUnwindSafe(|| view_of(&model))) {
                let _ = qt.queue(move |mut o| {
                    // Dropped when a newer list came meanwhile.
                    if o.rust().render_seq == seq {
                        o.as_mut().show_view_only(&view);
                    }
                });
            }
        });
    }

    /// Takes over lists a worker built; any list still being built from an
    /// older model is dropped.
    fn show_view(mut self: Pin<&mut Self>, view: &View) {
        let seq = self.rust().render_seq.wrapping_add(1);
        self.as_mut().rust_mut().render_seq = seq;
        self.show_view_only(view);
    }

    fn show_view_only(mut self: Pin<&mut Self>, v: &View) {
        self.as_mut()
            .set_apps_json(QString::from(v.apps_json.as_str()));
        self.as_mut()
            .set_others_json(QString::from(v.others_json.as_str()));
        self.as_mut().set_app_count(v.app_count);
        self.as_mut().set_other_count(v.other_count);
        self.as_mut().set_review_count(v.review_count);
        self.as_mut()
            .set_download_text(QString::from(v.download_text.as_str()));
    }

    pub fn open_settings(self: Pin<&mut Self>) {
        self.start_settings(None);
    }

    pub fn open_settings_with_token(self: Pin<&mut Self>, token: &QString) {
        let t = token.to_string();
        self.start_settings(valid_activation_token(&t).map(str::to_string));
    }

    fn start_settings(mut self: Pin<&mut Self>, token: Option<String>) {
        self.as_mut().set_settings_note(QString::default());
        let qt = self.qt_thread();
        let qt_fail = qt.clone();
        if !spawn("telamon-open-settings", move || {
            if let Err(e) = run_settings(Path::new(SETTINGS_BIN), token.as_deref()) {
                let _ =
                    qt.queue(move |mut o| o.as_mut().set_settings_note(QString::from(e.as_str())));
            }
        }) {
            let _ = qt_fail.queue(|mut o| {
                o.as_mut()
                    .set_settings_note(QString::from("Telamon Settings could not be started."));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telamon_store_core::appstream::Style;

    fn row(id: &str, name: &str, system: bool, runtime: bool, size: u64) -> Row {
        Row {
            name: name.into(),
            id: id.into(),
            branch: "stable".into(),
            system,
            runtime,
            size,
            size_text: apps::format_size(size),
            asks: String::new(),
        }
    }

    fn held(r: &Row, asks: &[&str]) -> HeldApp {
        HeldApp {
            key: apps::row_key(r),
            name: r.name.clone(),
            asks: asks.iter().map(|a| a.to_string()).collect(),
            raw: asks.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn release(version: &str, kind: ReleaseKind, text: &str) -> Release {
        Release {
            version: version.into(),
            timestamp: 1,
            kind,
            description: if text.is_empty() {
                Vec::new()
            } else {
                vec![Block::Paragraph(vec![Span {
                    text: text.into(),
                    style: Style::Plain,
                }])]
            },
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "telamon-store-updates-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn size_and_scope_in_plain_words() {
        let user = row("org.a.A", "A", false, false, 12_300_000);
        assert_eq!(
            detail_line(&user),
            "stable \u{b7} for you \u{b7} 12.3 MB download"
        );
        let system = row("org.a.A", "A", true, false, 0);
        assert_eq!(
            detail_line(&system),
            "stable \u{b7} for everyone on this computer"
        );
    }

    #[test]
    fn a_row_has_name_icon_notes_and_what_it_asks_for() {
        let r = row("org.test.Hello", "org.test.Hello", false, false, 1_500_000);
        let h = held(&r, &["the network", "your home folder"]);
        let facts = Facts {
            name: "Hello".into(),
            icon: "file:///icons/hello.png".into(),
            release: Some(("1.1".into(), "Now with the network.".into())),
        };
        let v = app_json(&r, Some(&h), &facts);
        assert_eq!(v["appId"], "org.test.Hello");
        assert_eq!(v["name"], "Hello");
        assert_eq!(v["iconSource"], "file:///icons/hello.png");
        assert_eq!(v["asks"], "the network, your home folder");
        assert_eq!(v["notes"], "What's new in 1.1: Now with the network.");
        assert_eq!(v["detail"], "stable \u{b7} for you \u{b7} 1.5 MB download");
        // Nothing known: the engine's name, no notes, nothing it asks for.
        let bare = app_json(&r, None, &Facts::default());
        assert_eq!(bare["name"], "org.test.Hello");
        assert_eq!(bare["notes"], "");
        assert_eq!(bare["asks"], "");
        assert_eq!(bare["iconSource"], "");
        // A release without text still says the version.
        let facts = Facts {
            release: Some(("2.0".into(), String::new())),
            ..Facts::default()
        };
        assert_eq!(app_json(&r, None, &facts)["notes"], "New version: 2.0");
    }

    #[test]
    fn text_from_a_remote_is_cleaned() {
        let r = row("org.a.A", "Evil\u{202e}Name\nline", false, false, 0);
        let v = app_json(&r, None, &Facts::default());
        assert_eq!(v["name"], "EvilName line");
        let h = held(&r, &["x"]);
        // asks come cleaned from the engine's `describe`.
        assert_eq!(app_json(&r, Some(&h), &Facts::default())["asks"], "x");
    }

    #[test]
    fn components_are_listed_apart() {
        let mut m = Model::default();
        m.listed(
            vec![
                row("org.a.A", "A", true, false, 10),
                row("org.p.Platform", "Platform", true, true, 90),
            ],
            None,
        );
        assert_eq!(m.app_rows().count(), 1);
        assert_eq!(m.other_rows().count(), 1);
        assert_eq!(m.download_text(), "100 B");
        let o = other_json(m.other_rows().next().unwrap());
        assert_eq!(o["name"], "Platform");
        assert_eq!(o["detail"], "for everyone on this computer \u{b7} 90 B");
    }

    #[test]
    fn whats_new_is_only_what_the_catalog_knows_to_be_newer() {
        let rel = [
            release("1.2-dev", ReleaseKind::Development, "Unstable."),
            release("1.1", ReleaseKind::Stable, "Fixes."),
            release("1.0", ReleaseKind::Stable, "First."),
        ];
        assert_eq!(
            whats_new(&rel, "1.0"),
            Some(("1.1".into(), "Fixes.".into()))
        );
        // The catalog is not newer than what is installed: nothing.
        assert_eq!(whats_new(&rel, "1.1"), None);
        // Can't compare: nothing.
        assert_eq!(whats_new(&rel, ""), None);
        assert_eq!(whats_new(&[], "1.0"), None);
        assert_eq!(whats_new(&rel[..1], "1.0"), None);
    }

    #[test]
    fn release_notes_become_one_short_plain_line() {
        let long = "word ".repeat(200);
        let rel = [release("2", ReleaseKind::Stable, &long)];
        let (_, text) = whats_new(&rel, "1").unwrap();
        assert!(text.chars().count() <= NOTES_MAX, "{}", text.len());
        let list = Release {
            description: vec![
                Block::Paragraph(vec![Span {
                    text: "Changes\u{202e}:".into(),
                    style: Style::Plain,
                }]),
                Block::List {
                    ordered: false,
                    items: vec![
                        vec![Span {
                            text: "one".into(),
                            style: Style::Plain,
                        }],
                        vec![Span {
                            text: "two".into(),
                            style: Style::Plain,
                        }],
                    ],
                },
            ],
            ..release("2", ReleaseKind::Stable, "")
        };
        assert_eq!(
            whats_new(&[list], "1").unwrap().1,
            "Changes: \u{2022} one \u{2022} two"
        );
    }

    #[test]
    fn errors_are_plain_and_the_engines_words_are_cleaned() {
        let s = plain_error(
            Ctx::Check,
            "While fetching https://x/summary: Could not resolve host\u{202e}: x",
        );
        assert_eq!(
            s.text,
            "Couldn't check for updates. The Store couldn't reach the app source. Check your internet connection and try again."
        );
        assert!(!s.detail.contains('\u{202e}'));
        assert!(s.detail.starts_with("While fetching"));
        let s = plain_error(Ctx::Update, "write error: No space left on device");
        assert_eq!(
            s.text,
            "The update didn't finish. There isn't enough free disk space."
        );
        assert_eq!(
            plain_error(Ctx::Update, "something odd\nhappened").text,
            "The update didn't finish. Something went wrong. Look at what is installed, then try again."
        );
        assert_eq!(
            plain_error(Ctx::Update, "something odd\nhappened").detail,
            "something odd happened"
        );
        let long = "x".repeat(1000);
        assert!(plain_error(Ctx::Check, &long).detail.chars().count() <= 240);
        // The engine's own sentence when a check of permissions fails.
        assert_eq!(
            plain_error(
                Ctx::Update,
                "couldn't check what the updates ask for: not authorized"
            )
            .text,
            "The update didn't finish. This computer didn't allow the change."
        );
    }

    #[test]
    fn a_busy_lock_is_said_in_plain_words() {
        let s = lock_error(&LockError::Busy(
            telamon_store_core::flatpak::LockName::Updater,
        ));
        assert_eq!(
            s.text,
            "Another update is still running. Try again when it is done."
        );
        assert!(
            lock_error(&LockError::Io("denied".into()))
                .text
                .contains("did not start")
        );
        assert_eq!(lock_error(&LockError::Cancelled).text, "");
    }

    #[test]
    fn last_checked_is_the_later_of_the_two_and_never_the_future() {
        let now = 1_000_000;
        assert_eq!(effective_last_checked(None, None, now), 0);
        assert_eq!(effective_last_checked(Some(900), None, now), 900);
        assert_eq!(effective_last_checked(None, Some(950), now), 950);
        assert_eq!(effective_last_checked(Some(900), Some(950), now), 950);
        assert_eq!(effective_last_checked(Some(990), Some(950), now), 990);
        // A time far ahead (a wrong clock, an edited file) is ignored.
        assert_eq!(
            effective_last_checked(Some(900), Some(now + 10_000), now),
            900
        );
        assert_eq!(effective_last_checked(Some(0), Some(-5), now), 0);
    }

    #[test]
    fn a_list_is_old_after_ten_minutes_or_when_there_is_none() {
        assert!(is_stale(None));
        assert!(!is_stale(Some(Duration::from_secs(30))));
        assert!(!is_stale(Some(Duration::from_secs(600))));
        assert!(is_stale(Some(Duration::from_secs(601))));
    }

    #[test]
    fn progress_shows_the_apps_name_and_a_percent() {
        let names = vec![("org.test.Hello".to_string(), "Hello".to_string())];
        assert_eq!(
            progress_line("Installing org.test.Hello\u{2026} 37%", &names),
            ("Installing Hello\u{2026} 37%".to_string(), 37)
        );
        assert_eq!(
            progress_line("Updating\u{2026}", &names),
            ("Updating\u{2026}".to_string(), -1)
        );
        assert_eq!(progress_line("Installing x\u{2026} 100%", &[]).1, 100);
        assert_eq!(progress_line("Installing x\u{2026} 400%", &[]).1, -1);
        assert_eq!(
            progress_line("Installing\u{202e}\n x\u{2026} 5%", &[]).0,
            "Installing x\u{2026} 5%"
        );
    }

    fn entry(id: &str, name: &str, runtime: bool) -> apphistory::Entry {
        apphistory::Entry {
            at: 1,
            id: id.into(),
            name: name.into(),
            branch: "stable".into(),
            runtime,
            system: false,
            from: None,
            to: None,
            auto: false,
        }
    }

    #[test]
    fn what_was_updated_is_said_briefly() {
        assert_eq!(updated_notice(&[], 0), "");
        assert_eq!(
            updated_notice(&[entry("a.A", "A", false), entry("p.P", "P", true)], 0),
            "Updated A."
        );
        assert_eq!(
            updated_notice(&[entry("p.P", "P", true)], 0),
            "Components were updated."
        );
        let many: Vec<_> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|n| entry(&format!("a.{n}"), n, false))
            .collect();
        assert_eq!(updated_notice(&many[..3], 0), "Updated A, B and C.");
        assert_eq!(updated_notice(&many, 0), "Updated A, B and 3 more apps.");
    }

    #[test]
    fn notes_of_held_apps_stay_until_a_whole_check_replaces_them() {
        let kate = row("org.k.Kate", "Kate", true, false, 5);
        let okular = row("org.o.Okular", "Okular", true, false, 7);
        let mut m = Model::default();
        let found = Done {
            held_back: vec![held(&kate, &["the network"])],
            ..Default::default()
        };
        m.listed(vec![kate.clone(), okular.clone()], Some(&found));
        assert!(m.held.contains_key(&apps::row_key(&kate)));
        // A check of permissions that failed keeps the notes.
        let failed = Done {
            error: Some("offline".into()),
            ..Default::default()
        };
        m.listed(vec![kate.clone(), okular.clone()], Some(&failed));
        assert!(m.held.contains_key(&apps::row_key(&kate)));
        // One that ran to the end and found nothing clears them.
        m.listed(vec![kate.clone(), okular.clone()], Some(&Done::default()));
        assert!(m.held.is_empty());
        // Notes of an app no longer waiting go.
        m.listed(vec![kate.clone()], Some(&found));
        m.listed(vec![okular], None);
        assert!(m.held.is_empty());
    }

    #[test]
    fn an_update_the_page_did_not_show_is_stopped_for_review() {
        let kate = row("org.k.Kate", "Kate", true, false, 5);
        let asks = held(&kate, &["Session Bus Policy: org.x=talk"]);
        let check = |h: Vec<HeldApp>, error: Option<&str>| Done {
            held_back: h,
            error: error.map(String::from),
            ..Default::default()
        };
        let mut shown = HashMap::new();
        // Nothing asks for anything: go on.
        assert!(guard_update(check(vec![], None), &shown).is_ok());
        // Something asks and the page did not show it: stop, and give it back.
        let stop = guard_update(check(vec![asks.clone()], None), &shown).unwrap_err();
        assert!(stop.updated.is_empty());
        assert_eq!(stop.held_back, vec![asks.clone()]);
        // The page showed exactly that: go on.
        shown.insert(asks.key.clone(), asks.clone());
        assert!(guard_update(check(vec![asks.clone()], None), &shown).is_ok());
        // It asks for more than was shown: stop.
        let mut more = asks.clone();
        more.raw.push("Context: devices=all".into());
        assert!(guard_update(check(vec![more], None), &shown).is_err());
        // The check itself failed: stop, never install blind.
        let blind = guard_update(check(vec![], Some("offline")), &shown).unwrap_err();
        assert!(blind.error.unwrap().contains("couldn't check"));
    }

    #[test]
    fn after_an_update_what_is_left_keeps_its_size_and_notes() {
        let kate = row("org.k.Kate", "Kate", false, false, 5_000_000);
        let okular = row("org.o.Okular", "Okular", false, false, 7_000_000);
        let mut m = Model::default();
        m.listed(vec![kate.clone(), okular.clone()], None);
        let done = Done {
            updated: vec![apphistory::Entry {
                system: false,
                ..entry("org.k.Kate", "Kate", false)
            }],
            ..Default::default()
        };
        // Listed again without a refresh: sizes are 0 there.
        let mut left = okular.clone();
        left.size = 0;
        left.size_text = String::new();
        m.updated(Ok(vec![left]), &done);
        assert_eq!(m.rows.len(), 1);
        assert_eq!(m.rows[0].size, 7_000_000);
        assert_eq!(m.rows[0].size_text, "7.0 MB");
        // Could not be listed: what waited, less what was installed.
        let mut m2 = Model::default();
        m2.listed(vec![kate, okular], None);
        m2.updated(Err("x".into()), &done);
        assert_eq!(
            m2.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["org.o.Okular"]
        );
    }

    #[test]
    fn the_engines_fixture_rows_make_the_page_json() {
        let dir = scratch("fixture");
        std::fs::write(
            dir.join("flatpak.json"),
            r#"[{"name":"Hello","id":"org.test.Hello","branch":"stable","system":false,"runtime":false,"size":2500000},
                {"name":"Platform","id":"org.test.Platform","branch":"stable","system":false,"runtime":true,"size":9000000}]"#,
        )
        .unwrap();
        let rows = apps::list(false, true, Some(&dir)).unwrap();
        let mut m = Model::default();
        m.listed(rows, None);
        let apps_v: Vec<Value> = m
            .app_rows()
            .map(|r| app_json(r, None, &Facts::default()))
            .collect();
        assert_eq!(apps_v.len(), 1);
        assert_eq!(apps_v[0]["name"], "Hello");
        assert_eq!(
            apps_v[0]["detail"],
            "stable \u{b7} for you \u{b7} 2.5 MB download"
        );
        assert_eq!(m.other_rows().count(), 1);
        assert_eq!(m.download_text(), "11.5 MB");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stores_last_check_is_kept_in_its_state_folder() {
        let base = scratch("state");
        assert_eq!(read_checked_in(&base), None);
        write_checked_in(&base, 1_700_000_000).unwrap();
        assert_eq!(read_checked_in(&base), Some(1_700_000_000));
        write_checked_in(&base, 1_700_000_500).unwrap();
        assert_eq!(read_checked_in(&base), Some(1_700_000_500));
        let file = base.join("telamon-store").join(CHECKED_FILE);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(base.join("telamon-store"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        // Not read: not digits, too long, a link.
        for bad in ["abc\n", "-5\n", "12 34\n", &"9".repeat(100)] {
            std::fs::write(&file, bad).unwrap();
            assert_eq!(read_checked_in(&base), None, "{bad}");
        }
        std::fs::remove_file(&file).unwrap();
        let other = base.join("other");
        std::fs::write(&other, "1700000000\n").unwrap();
        std::os::unix::fs::symlink(&other, &file).unwrap();
        assert_eq!(read_checked_in(&base), None);
        // Writing replaces the link, it does not follow it.
        write_checked_in(&base, 42).unwrap();
        assert_eq!(read_checked_in(&base), Some(42));
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "1700000000\n");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn settings_open_on_the_apps_updates_with_fixed_arguments() {
        let dir = scratch("settings");
        let out = dir.join("argv");
        let bin = dir.join("fake-telamon-settings");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\n{{ for a in \"$@\"; do echo \"arg:$a\"; done; echo \"token:${{XDG_ACTIVATION_TOKEN-unset}}\"; }} > '{}'\n",
                out.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        run_settings(&bin, Some("tok123")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "arg:updates\narg:apps\ntoken:tok123\n"
        );
        // A token with a line break is not passed on.
        run_settings(&bin, Some("bad\ntoken")).unwrap();
        assert!(
            std::fs::read_to_string(&out)
                .unwrap()
                .ends_with("token:unset\n")
        );
        // Not installed: said in plain words.
        assert_eq!(
            run_settings(&dir.join("missing"), None).unwrap_err(),
            "Telamon Settings isn't installed on this computer."
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_was_left_out_is_said_after_what_was_updated() {
        let a = [entry("a.A", "A", false)];
        assert_eq!(
            updated_notice(&a, 1),
            "Updated A. 1 app that asks for new permissions was left out."
        );
        assert_eq!(
            updated_notice(&[], 2),
            "2 apps that ask for new permissions were left out."
        );
        assert_eq!(updated_notice(&a, 0), "Updated A.");
    }

    fn kate_asking() -> (Model, Row) {
        let kate = row("org.k.Kate", "Kate", false, false, 5_000_000);
        let okular = row("org.o.Okular", "Okular", false, false, 7_000_000);
        let mut m = Model::default();
        let found = Done {
            held_back: vec![held(
                &kate,
                &[
                    "your home folder",
                    "the network",
                    "all devices, such as cameras and USB",
                    "a",
                    "b",
                    "c",
                    "d",
                ],
            )],
            ..Default::default()
        };
        m.listed(vec![kate.clone(), okular], Some(&found));
        (m, kate)
    }

    #[test]
    fn update_all_needs_the_confirmation_exactly_when_something_asks() {
        let (m, _) = kate_asking();
        assert_eq!(m.review_count(), 1);
        let v = render(&m, |_, _| Facts::default());
        assert_eq!(v.review_count, 1);
        // Nothing asks: one press.
        let mut quiet = Model::default();
        quiet.listed(vec![row("org.o.Okular", "Okular", false, false, 1)], None);
        assert_eq!(quiet.review_count(), 0);
        assert_eq!(render(&quiet, |_, _| Facts::default()).review_count, 0);
        // A component that asks counts too: nothing slips past the dialog.
        let rt = row("org.p.Platform", "Platform", true, true, 1);
        let mut m2 = Model::default();
        m2.listed(
            vec![rt.clone()],
            Some(&Done {
                held_back: vec![held(&rt, &["x"])],
                ..Default::default()
            }),
        );
        assert_eq!(m2.review_count(), 1);
    }

    #[test]
    fn the_confirmation_gets_every_permission_not_five() {
        let (m, _) = kate_asking();
        let v = render(&m, |_, _| Facts::default());
        let apps_v: Vec<Value> = serde_json::from_str(&v.apps_json).unwrap();
        let kate = apps_v.iter().find(|a| a["name"] == "Kate").unwrap();
        assert_eq!(kate["review"], true);
        assert_eq!(kate["asksList"].as_array().unwrap().len(), 7);
        // The row's own line stays short.
        assert!(kate["asks"].as_str().unwrap().ends_with(", more"));
        let okular = apps_v.iter().find(|a| a["name"] == "Okular").unwrap();
        assert_eq!(okular["review"], false);
        assert_eq!(okular["asksList"].as_array().unwrap().len(), 0);
        assert_eq!(v.download_text, "12.0 MB");
    }

    #[test]
    fn leaving_out_new_permissions_is_the_engines_hold() {
        assert!(!UpdateMode::All.hold());
        assert!(UpdateMode::LeaveOutNewPermissions.hold());
    }

    #[test]
    fn the_guard_runs_for_everything_and_not_when_the_engine_holds() {
        let kate = row("org.k.Kate", "Kate", true, false, 5);
        let asks = held(&kate, &["Session Bus Policy: org.x=talk"]);
        let shown = HashMap::new();
        let found = || Done {
            held_back: vec![asks.clone()],
            ..Default::default()
        };
        // Everything: the check runs, and what the page did not show stops it.
        let mut ran = false;
        let stop = guard_for(
            UpdateMode::All,
            || {
                ran = true;
                found()
            },
            &shown,
        );
        assert!(ran);
        assert!(stop.is_err());
        // Leaving them out: no look is needed, the engine holds them.
        let mut ran = false;
        let go = guard_for(
            UpdateMode::LeaveOutNewPermissions,
            || {
                ran = true;
                found()
            },
            &shown,
        );
        assert!(!ran);
        assert!(go.is_ok());
    }

    #[test]
    fn what_the_engine_held_back_keeps_its_note_and_the_confirmation() {
        let (mut m, kate) = kate_asking();
        // The run left Kate out: she is still listed, still asking.
        let done = Done {
            held_back: vec![held(&kate, &["your home folder"])],
            ..Default::default()
        };
        let okular = row("org.o.Okular", "Okular", false, false, 0);
        m.updated(Ok(vec![kate.clone(), okular]), &done);
        assert_eq!(m.review_count(), 1);
        assert!(m.held.contains_key(&apps::row_key(&kate)));
    }

    #[test]
    fn quitting_waits_for_a_worker_but_not_for_ever() {
        // A worker that ends on its own is joined.
        let h = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(60)));
        assert!(join_capped(h, Duration::from_secs(5)));
        // One that does not is left after the cap.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        let start = Instant::now();
        assert!(!join_capped(h, Duration::from_millis(100)));
        assert!(start.elapsed() < Duration::from_secs(3));
        drop(tx);
    }

    #[test]
    fn dropping_the_object_cancels_a_wait_and_joins_the_worker() {
        let mut obj = AppUpdatesRust::default();
        let token = obj.cancel.clone();
        let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ended.clone();
        // A worker that waits for the lock until it is cancelled.
        obj.worker = Some(std::thread::spawn(move || {
            while !token.is_cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(50));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        drop(obj);
        // Drop returned only after the worker ended.
        assert!(ended.load(std::sync::atomic::Ordering::SeqCst));
    }
}
