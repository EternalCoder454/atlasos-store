//! Installing a native app for the user, updating it, and taking it away.
//!
//! Layout (under `$XDG_DATA_HOME`):
//!
//! ```text
//! telamon-apps/<id>/<version>/      the bundle's tree (bin/, share/, ...)
//! telamon-apps/<id>/current         link to <version>, switched in one rename
//! telamon-apps/<id>/install.json    what the Store installed (the record)
//! applications/<id>.desktop         copied out of the bundle (see desktop.rs)
//! icons/, metainfo/, dbus-1/services/, knotifications6/
//! ```
//!
//! **Update** = unpack the new version beside the old one, check it, write the
//! copied files, point `current` at it, write the record, and only then tidy:
//! the version before the old one and stale copies go; the old version stays
//! until the next update, so an app that is still running keeps every file it
//! may read lazily. If anything fails before the record is written, the copied
//! files are put back as they were, `current` still names the old version and
//! the new folder is removed: **rollback**.
//!
//! **Uninstall** removes exactly what the record lists (a copied file only if
//! it still has the content the Store wrote) and the app's folder. The app's
//! own data (`~/.local/share/<app>`, settings) is never touched.
//!
//! **Folders are held open, not walked by name** (`dirfd::Dir`): the data
//! folder (a link is followed there, a dotfile manager may have made one) and
//! `telamon-apps` below it (never a link, this user's, 0700) are opened once
//! under the lock, and everything else is `openat`/`renameat2`/`unlinkat`
//! relative to a descriptor with `O_NOFOLLOW`. The export roots
//! (`applications`, `icons`...) may be links; every folder below them may not.
//! A file the Store replaces or removes is checked on the very object it
//! touches: swapped (`RENAME_EXCHANGE`) or moved aside first, then read and
//! hashed through its descriptor, and put back if it is not what was written.
//! The undo of a failed install is a `Drop`, so a panic undoes it too.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::archive::{self, hex};
use super::desktop;
use super::dirfd::{Dir, Expect, Kind, Refused, RemoveError, ReplaceError};
use super::manifest::{Host, Manifest};
use super::version::Version;
use super::{APPS_DIR, Error, err, io_err, valid_app_id};
use crate::appimage::fsutil;

const RECORD: &str = "install.json";
const MAX_RECORD: u64 = 256 * 1024;
const MAX_LISTED: usize = 200;
/// The most of a replaced copied file that is kept to put it back.
const MAX_COPY: u64 = 1024 * 1024;
/// The folders of the user's data directory a bundle may put files in.
const EXPORT_ROOTS: [&str; 5] = [
    "applications",
    "icons",
    "metainfo",
    "dbus-1",
    "knotifications6",
];

/// Tests make an install fail at a chosen step of the commit, to see that it
/// is undone. Per thread, so tests that run side by side do not meet.
#[cfg(feature = "test-hooks")]
pub mod test_hooks {
    use std::cell::{Cell, RefCell};
    /// What a test runs at a step.
    pub type Callback = Box<dyn Fn(&str)>;
    thread_local! {
        /// `"exports"`, `"switch"` or `"record"`: fail after that step.
        pub static FAIL_AFTER: Cell<Option<&'static str>> = const { Cell::new(None) };
        /// `"exports"`, `"switch"`, `"record"`: panic after that step instead.
        pub static PANIC_AFTER: Cell<Option<&'static str>> = const { Cell::new(None) };
        /// Called with the name of a step of an install (`"staged"`,
        /// `"planned"`, `"exports"`, `"switch"`, `"record"`) before it runs;
        /// a test uses it to change the folders under the install's feet.
        pub static ON_STEP: RefCell<Option<Callback>> = const { RefCell::new(None) };
    }
}

#[cfg(feature = "test-hooks")]
fn hook(step: &str) -> Result<(), Error> {
    test_hooks::ON_STEP.with(|f| {
        if let Some(f) = f.borrow().as_ref() {
            f(step)
        }
    });
    if test_hooks::PANIC_AFTER.with(|f| f.get()) == Some(step) {
        panic!("test panic after {step}");
    }
    if test_hooks::FAIL_AFTER.with(|f| f.get()) == Some(step) {
        return Err(err(format!("test failure after {step}")));
    }
    Ok(())
}

#[cfg(not(feature = "test-hooks"))]
fn hook(_step: &str) -> Result<(), Error> {
    Ok(())
}

/// Where things go. [`Dirs::from_env`] is the user's; tests make their own.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// `$XDG_DATA_HOME`
    pub data: PathBuf,
    /// `$HOME`
    pub home: PathBuf,
    /// The system's data folders (`XDG_DATA_DIRS`, `/usr/share`,
    /// `/usr/local/share`, Flatpak's exports): nothing a bundle would copy
    /// may exist there at the same relative path, and an app whose menu entry
    /// or D-Bus name is already there is not installed, so a bundle cannot
    /// replace or shadow what is not its own.
    pub system: Vec<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Option<Dirs> {
        let home = fsutil::home()?;
        let mut system: Vec<PathBuf> = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_default()
            .split(':')
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .collect();
        // The ones every session searches, whatever the variable says.
        for fixed in [
            "/usr/local/share".into(),
            "/usr/share".into(),
            "/var/lib/flatpak/exports/share".into(),
            home.join(".local/share/flatpak/exports/share"),
        ] {
            if !system.contains(&fixed) {
                system.push(fixed);
            }
        }
        Some(Dirs {
            data: fsutil::data_home()?,
            home,
            system,
        })
    }

    pub fn apps(&self) -> PathBuf {
        self.data.join(APPS_DIR)
    }

    pub fn app(&self, id: &str) -> PathBuf {
        self.apps().join(id)
    }
}

/// Where an installed app came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    /// `release` (the catalog) or `local` (a file the user opened).
    pub kind: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    /// The key ID (16 upper-case hex digits) of the key whose signature the
    /// release was installed on; none for a local file and for installs made
    /// before releases were signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer: Option<String>,
}

impl Origin {
    pub fn release(repo: &str, tag: &str) -> Origin {
        Origin {
            kind: "release".into(),
            repo: Some(repo.into()),
            tag: Some(tag.into()),
            signer: None,
        }
    }

    /// A release whose manifest was verified with the key `signer`.
    pub fn signed_release(repo: &str, tag: &str, signer: &str) -> Origin {
        Origin {
            signer: Some(signer.into()),
            ..Origin::release(repo, tag)
        }
    }

    pub fn local() -> Origin {
        Origin {
            kind: "local".into(),
            repo: None,
            tag: None,
            signer: None,
        }
    }
}

/// A file copied out of the bundle, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copied {
    /// Relative to `$XDG_DATA_HOME`.
    pub to: String,
    pub sha256: String,
}

/// `telamon-apps/<id>/install.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub schema: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub summary: String,
    pub version: String,
    #[serde(default)]
    pub previous: Option<String>,
    pub origin: Origin,
    /// The program Open runs: `bin/<name>`.
    pub exe: String,
    pub copied: Vec<Copied>,
    /// Bytes of the installed tree.
    pub size: u64,
    /// Unix time of the install.
    pub installed_at: u64,
}

/// An installed app, as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub version: String,
    pub origin: Origin,
    pub size: u64,
    /// `current` points at an existing version with its program in it.
    pub present: bool,
    /// The installed icon (a path), if the bundle brought one.
    pub icon: Option<PathBuf>,
}

fn export_ok(rel: &str) -> bool {
    super::manifest::valid_rel_path(rel)
        && rel
            .split('/')
            .next()
            .is_some_and(|first| EXPORT_ROOTS.contains(&first))
        && rel.contains('/')
}

fn exe_ok(exe: &str) -> bool {
    exe.strip_prefix("bin/")
        .is_some_and(|n| !n.is_empty() && !n.contains('/') && super::manifest::valid_rel_path(n))
}

impl Record {
    fn check(&self, dir_name: &str) -> bool {
        self.schema == 1
            && self.id == dir_name
            && valid_app_id(&self.id)
            && Version::parse(&self.version).is_some()
            && self
                .previous
                .as_deref()
                .is_none_or(|p| Version::parse(p).is_some())
            && exe_ok(&self.exe)
            && self.copied.len() <= 256
            && self.copied.iter().all(|c| {
                export_ok(&c.to)
                    && c.sha256.len() == 64
                    && c.sha256
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            })
    }
}

/// `$XDG_DATA_HOME`, opened (a link there is followed: dotfile managers make
/// them), and made when `create` and missing.
fn open_data(dirs: &Dirs, create: bool) -> Result<Dir, Error> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dirs.data)
            .map_err(|e| io_err("make the data folder", &e))?;
    }
    Dir::open_following(&dirs.data).map_err(|e| io_err("open the data folder", &e))
}

/// `telamon-apps` below the data folder: never a link, this user's, and, when
/// `create`, made 0700 (and set back to 0700 if group or others could use it).
fn open_apps(data: &Dir, create: bool) -> Result<Dir, Error> {
    let bad = |e: std::io::Error| {
        if matches!(
            e.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR) | Some(libc::EACCES)
        ) || e.kind() == std::io::ErrorKind::PermissionDenied
        {
            err(
                "The Store's apps folder is a link, a file or someone else's. The Store won't use it.",
            )
        } else {
            io_err("open the apps folder", &e)
        }
    };
    if create {
        let apps = data.ensure_sub(APPS_DIR, 0o700, true).map_err(bad)?;
        apps.make_private().map_err(bad)?;
        Ok(apps)
    } else {
        data.sub_owned(APPS_DIR).map_err(bad)
    }
}

/// The folder of an export (`icons/hicolor/48x48/apps/x.png`) and the file
/// name in it. The first part is one of the export roots and may be a link
/// (a dotfile manager's); every part below it must be a real folder. With
/// `create` missing folders are made; without it `None` means a part is
/// missing.
fn export_parent<'a>(
    data: &Dir,
    rel: &'a str,
    create: bool,
) -> std::io::Result<Option<(Dir, &'a str)>> {
    let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidInput);
    let mut parts: Vec<&str> = rel.split('/').collect();
    let last = parts.pop().filter(|l| !l.is_empty()).ok_or_else(invalid)?;
    let mut it = parts.into_iter();
    let root = it
        .next()
        .filter(|r| EXPORT_ROOTS.contains(r))
        .ok_or_else(invalid)?;
    let mut cur = match data.sub_following(root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(None);
            }
            match data.create_dir(root, 0o755) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            data.sub_following(root)?
        }
        Err(e) => return Err(e),
    };
    for part in it {
        cur = if create {
            cur.ensure_sub(part, 0o755, false)?
        } else {
            match cur.sub(part) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            }
        };
    }
    Ok(Some((cur, last)))
}

/// Reads the record of `id` from the apps folder; `None` when there is none
/// or it is not ours (the app's folder must be a real folder of this user).
fn read_record_in(apps: &Dir, id: &str) -> Option<Record> {
    if !valid_app_id(id) {
        return None;
    }
    let dir = apps.sub_owned(id).ok()?;
    let bytes = dir.read_file(RECORD, MAX_RECORD).ok()??;
    let mut rec: Record = serde_json::from_slice(&bytes).ok()?;
    if !rec.check(id) {
        return None;
    }
    rec.origin.signer = rec
        .origin
        .signer
        .take()
        .filter(|s| super::sign::valid_key_id(s));
    rec.name = crate::text::clean(&rec.name, 80);
    rec.summary = crate::text::clean(&rec.summary, 300);
    Some(rec)
}

/// Reads the record of `id`; `None` when there is none or it is not ours.
pub fn read_record(dirs: &Dirs, id: &str) -> Option<Record> {
    let data = open_data(dirs, false).ok()?;
    let apps = open_apps(&data, false).ok()?;
    read_record_in(&apps, id)
}

fn write_record(app: &Dir, rec: &Record) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(rec).map_err(|_| err("Could not write the record."))?;
    app.write_atomic(RECORD, &bytes, 0o600)
        .map_err(|e| io_err("write the record", &e))
}

/// The installed apps, sorted by name.
pub fn list(dirs: &Dirs) -> Vec<Installed> {
    let Ok(data) = open_data(dirs, false) else {
        return Vec::new();
    };
    let Ok(apps) = open_apps(&data, false) else {
        return Vec::new();
    };
    let Ok(names) = apps.list_text() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for id in names.iter().take(MAX_LISTED * 2) {
        let Some(rec) = read_record_in(&apps, id) else {
            continue;
        };
        let present = program_present(&apps, &rec);
        let icon = icon_of(dirs, &data, &rec);
        out.push(Installed {
            id: rec.id,
            name: rec.name,
            summary: rec.summary,
            version: rec.version,
            origin: rec.origin,
            size: rec.size,
            present,
            icon,
        });
        if out.len() >= MAX_LISTED {
            break;
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    out
}

/// `current` is a link to a version folder that holds the program, found
/// without leaving the app's folder.
fn program_present(apps: &Dir, rec: &Record) -> bool {
    let check = || -> Option<bool> {
        let app = apps.sub_owned(&rec.id).ok()?;
        if app.stat("current").ok()?.kind != Kind::Link {
            return Some(false);
        }
        let version = app.read_link("current").ok()?;
        Version::parse(&version)?;
        let bin = app.sub(&version).ok()?.sub("bin").ok()?;
        let name = rec.exe.strip_prefix("bin/")?;
        Some(bin.stat_following(name).ok()?.kind == Kind::File)
    };
    check().unwrap_or(false)
}

/// The icon the window shows for an installed app: the best of the copied
/// icons that still is what an icon must be. It is read through the open
/// folders here (a regular file of this user, of the size and the kind the
/// install checked: a PNG of a sane size or a plain SVG), because the path
/// goes to Qt, which decodes it.
fn icon_of(dirs: &Dirs, data: &Dir, rec: &Record) -> Option<PathBuf> {
    use crate::appimage::meta::{IconKind, MAX_ICON, icon_kind};
    let mut icons: Vec<&Copied> = rec
        .copied
        .iter()
        .filter(|c| c.to.starts_with("icons/hicolor/"))
        .collect();
    icons.sort_by_key(|c| std::cmp::Reverse(icon_rank(&c.to)));
    icons.into_iter().find_map(|c| {
        let (dir, name) = export_parent(data, &c.to, false).ok()??;
        let bytes = dir.read_file(name, MAX_ICON).ok()??;
        let want = if name.ends_with(".png") {
            IconKind::Png
        } else {
            IconKind::Svg
        };
        (icon_kind(&bytes) == Some(want)).then(|| dirs.data.join(&c.to))
    })
}

/// Prefers the scalable icon, then the largest bitmap that is not too big.
fn icon_rank(to: &str) -> u32 {
    let dir = to.split('/').nth(2).unwrap_or("");
    if dir == "scalable" {
        return 10_000;
    }
    dir.split('x')
        .next()
        .and_then(|w| w.parse::<u32>().ok())
        .map_or(0, |w| if w <= 512 { w } else { 1 })
}

/// The lock that keeps two installs apart: `flock` on a file in the apps
/// folder, released when dropped (or the process ends). It also holds the
/// folders every step works in, opened once.
struct Lock {
    _file: fs::File,
    data: Dir,
    apps: Dir,
}

impl Lock {
    fn take(dirs: &Dirs) -> Result<Lock, Error> {
        let data = open_data(dirs, true)?;
        let apps = open_apps(&data, true)?;
        let f = apps
            .open_or_create_private(".lock")
            .map_err(|e| io_err("lock the apps folder", &e))?;
        // SAFETY: flock on a file descriptor this struct owns.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io_err(
                "lock the apps folder",
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Lock {
            _file: f,
            data,
            apps,
        })
    }
}

static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn unique(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{prefix}{}-{n}", std::process::id())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What an install is told besides the file.
pub struct Options<'a> {
    /// The ID the caller expects (the catalog's); the bundle must say the same.
    pub expect_id: Option<&'a str>,
    /// The release's manifest, when the bundle came from a release.
    pub outer: Option<&'a Manifest>,
    pub origin: Origin,
    pub host: &'a Host,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The version it replaced, if any.
    pub replaced: Option<String>,
    /// The key ID the release was verified with (none for a local file).
    pub signer: Option<String>,
}

/// Puts back what an install changed, in the reverse order, when it is
/// dropped without [`Undo::keep`]: so it happens when the install fails, and
/// also when it panics. Every step works through the open folders, and puts
/// back only what is still what the install wrote.
struct Undo<'a> {
    data: &'a Dir,
    apps: &'a Dir,
    /// Copied files that were not there before, with the SHA-256 written.
    created: Vec<(String, String)>,
    /// Copied files that were, with their old content and the SHA-256 written.
    replaced: Vec<(String, Vec<u8>, String)>,
    /// The app's folder and the new version folder in it.
    app: Option<Dir>,
    version: Option<String>,
    /// `current` before it was switched: the old target, if the link existed.
    current: Option<Option<String>>,
    /// On a first install, the app's own folder (removed if empty).
    app_created: Option<String>,
    armed: bool,
}

impl<'a> Undo<'a> {
    fn new(data: &'a Dir, apps: &'a Dir) -> Undo<'a> {
        Undo {
            data,
            apps,
            created: Vec::new(),
            replaced: Vec::new(),
            app: None,
            version: None,
            current: None,
            app_created: None,
            armed: true,
        }
    }

    /// The install went through: nothing is put back.
    fn keep(&mut self) {
        self.armed = false;
    }

    fn run(&mut self) {
        if let (Some(app), Some(old)) = (&self.app, self.current.take()) {
            match old {
                Some(target) => {
                    let tmp = unique(".current.");
                    if app.symlink(&target, &tmp).is_ok()
                        && app.rename(&tmp, app, "current").is_err()
                    {
                        let _ = app.unlink(&tmp);
                    }
                }
                None => {
                    let _ = app.unlink("current");
                }
            }
        }
        for (rel, old, new_sha) in std::mem::take(&mut self.replaced).into_iter().rev() {
            if let Ok(Some((dir, name))) = export_parent(self.data, &rel, false) {
                let _ = dir.replace_checked(name, &old, 0o644, Expect::Sha(&new_sha), MAX_COPY);
            }
        }
        for (rel, sha) in std::mem::take(&mut self.created).into_iter().rev() {
            let _ = remove_export(self.data, &rel, &sha);
        }
        if let (Some(app), Some(version)) = (&self.app, self.version.take()) {
            let _ = app.remove_all(&version);
        }
        if let Some(id) = self.app_created.take() {
            let _ = self.apps.rmdir(&id);
        }
    }
}

impl Drop for Undo<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.run();
        }
    }
}

/// Removes an export if it still has the content the Store wrote, then the
/// folders that made it empty, from the inside out, never the export root or
/// the folder below it (`icons/hicolor`, `dbus-1/services` stay).
fn remove_export(data: &Dir, rel: &str, sha: &str) -> Result<bool, String> {
    let Some((dir, name)) = export_parent(data, rel, false).map_err(|_| "couldn't be checked")?
    else {
        return Ok(false);
    };
    // Temporary files an interrupted write or removal of this one left.
    dir.sweep_temps(name);
    match dir.stat_opt(name) {
        Ok(None) => return Ok(false),
        Ok(Some(m)) if m.kind != Kind::File => return Err("is not a plain file any more".into()),
        Ok(Some(_)) => {}
        Err(_) => return Err("couldn't be checked".into()),
    }
    match dir.remove_checked(name, sha) {
        Ok(()) => {}
        Err(RemoveError::Refused(Refused::Gone)) => return Ok(false),
        Err(RemoveError::Refused(Refused::Changed)) => {
            return Err("was changed since the Store wrote it".into());
        }
        Err(RemoveError::Io(_)) => return Err("couldn't be removed".into()),
    }
    // Empty folders below the second level.
    let parts: Vec<&str> = rel.split('/').collect();
    let folders = &parts[..parts.len() - 1];
    for depth in (3..=folders.len()).rev() {
        let upto = folders[..depth - 1].join("/");
        let probe = format!("{upto}/{}", folders[depth - 1]);
        let Ok(Some((parent, name))) = export_parent(data, &probe, false) else {
            break;
        };
        if parent.rmdir(name).is_err() {
            break;
        }
    }
    Ok(true)
}

/// Installs the bundle at `archive` (a `.tar.zst` the caller has already
/// checked against the release's size and SHA-256, or the user's own file).
/// Everything about the tree is checked here: see `archive::unpack`.
pub fn install_bundle(dirs: &Dirs, archive: &Path, opts: &Options<'_>) -> Result<Done, Error> {
    // A release (it comes with its outer manifest) is installed only on a
    // verified signature: the caller must name the key that verified it.
    if opts.outer.is_some() && opts.origin.signer.is_none() {
        return Err(err(
            "A release is installed only with a verified signature. Nothing was installed.",
        ));
    }
    let lock = Lock::take(dirs)?;
    // Leftovers of an install that was cut short; the lock is held, so none
    // is in use.
    sweep(&lock.apps);
    // The id is not known until the archive is read: unpack under the apps
    // folder (same filesystem as the final place), then move.
    let (stage_name, staging) = make_staging(&lock.apps)?;
    let result = {
        // Removes whatever is left of the staging folder (the move took it on
        // success), also when the install panics.
        let _cleanup = StagingCleanup {
            apps: &lock.apps,
            name: &stage_name,
        };
        install_from(dirs, &lock, archive, opts, &staging, &stage_name)
    };
    sweep(&lock.apps);
    result
}

struct StagingCleanup<'a> {
    apps: &'a Dir,
    name: &'a str,
}

impl Drop for StagingCleanup<'_> {
    fn drop(&mut self) {
        let _ = self.apps.remove_all(self.name);
    }
}

/// A new private folder for unpacking, opened. A name someone planted is not
/// used, another is tried.
fn make_staging(apps: &Dir) -> Result<(String, Dir), Error> {
    for _ in 0..100 {
        let name = unique(".staging-");
        match apps.create_dir(&name, 0o700) {
            Ok(()) => {
                let dir = apps
                    .sub_owned(&name)
                    .map_err(|e| io_err("make a folder", &e))?;
                return Ok((name, dir));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_err("make a folder", &e)),
        }
    }
    Err(err("Could not make a folder to unpack the bundle in."))
}

/// Removes staging folders an earlier, interrupted install left. The lock is
/// held by the caller, so none of them is in use.
fn sweep(apps: &Dir) {
    let Ok(names) = apps.list_text() else {
        return;
    };
    for name in names {
        if name.starts_with(".staging-") {
            let _ = apps.remove_all(&name);
        }
    }
}

fn install_from(
    dirs: &Dirs,
    lock: &Lock,
    archive_path: &Path,
    opts: &Options<'_>,
    staging: &Dir,
    stage_name: &str,
) -> Result<Done, Error> {
    let (data, apps) = (&lock.data, &lock.apps);
    hook("staged")?;
    let inner = archive::unpack_into(archive_path, staging, opts.outer)?;
    if let Some(expect) = opts.expect_id
        && inner.id != expect
    {
        return Err(err(
            "The bundle is for a different app than the one asked for.",
        ));
    }
    inner.compatible(opts.host)?;
    let id = inner.id.clone();
    let old = read_record_in(apps, &id);
    if let Some(old) = &old
        && old.version == inner.version
        && apps
            .sub_owned(&id)
            .and_then(|a| a.stat(&old.version))
            .is_ok_and(|m| m.kind == Kind::Dir)
    {
        return Err(err(format!(
            "{} {} is already installed.",
            inner.name, inner.version
        )));
    }
    // A release never goes over a newer version: a validly signed old release
    // served as the latest must not downgrade the app. (A file the user opens
    // themselves may: they are shown what it replaces.)
    if opts.outer.is_some()
        && let Some(old) = &old
        && let (Some(have), Some(new)) =
            (Version::parse(&old.version), Version::parse(&inner.version))
        && new < have
    {
        return Err(err(format!(
            "{} {} is older than the version you have ({}). Nothing was installed.",
            inner.name, inner.version, old.version
        )));
    }
    if old.is_none()
        && apps
            .stat_opt(&id)
            .map_err(|e| io_err("check a folder", &e))?
            .is_some()
    {
        // A folder the Store did not make for this app (a link is one).
        let leftover = apps
            .sub(&id)
            .and_then(|d| d.list())
            .map(|names| {
                names
                    .iter()
                    .any(|n| !n.to_string_lossy().starts_with(".staging-"))
            })
            .unwrap_or(true);
        if leftover {
            return Err(err(
                "A folder for this app already exists that the Store did not make. Remove it first.",
            ));
        }
    }
    let prefix = dirs.app(&id).join("current");
    let plan = desktop::plan(staging, &inner, &prefix)?;
    // The data folder itself may be listed among the system's (a session that
    // names it in `XDG_DATA_DIRS`): the Store's own copies are not "provided
    // by the system".
    let system: Vec<PathBuf> = dirs
        .system
        .iter()
        .filter(|p| !data.is_same_dir(p))
        .cloned()
        .collect();
    desktop::check_system(&plan, &id, &system)?;

    // Copied files: not over anything that is not this app's own, and not
    // over one the user changed since the Store wrote it. (Checked again,
    // on the very file replaced, when it is written.)
    let recorded = |to: &str| {
        old.as_ref()
            .and_then(|o| o.copied.iter().find(|c| c.to == to))
    };
    for e in &plan.exports {
        let shown = dirs.data.join(&e.to);
        let there = match export_parent(data, &e.to, false) {
            Ok(Some((dir, name))) => dir
                .stat_opt(name)
                .map(|m| m.map(|m| (dir, name, m)))
                .map_err(|er| io_err("check an existing file", &er))?,
            Ok(None) => None,
            Err(er) => {
                return Err(match er.kind() {
                    std::io::ErrorKind::InvalidInput => {
                        err("A folder for a menu or icon file is a link or a file.")
                    }
                    _ if matches!(er.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)) => {
                        err(format!(
                            "A folder on the way to {} is a link or a file. The Store won't write through it.",
                            shown.display()
                        ))
                    }
                    _ => io_err("check an existing file", &er),
                });
            }
        };
        let Some((dir, name, md)) = there else {
            continue;
        };
        if md.kind != Kind::File {
            return Err(err(format!(
                "{} is already there and the Store didn't put it there. Remove it first.",
                shown.display()
            )));
        }
        match recorded(&e.to) {
            Some(c) => {
                let same = dir.sha256(name).is_ok_and(|(h, _)| h == c.sha256);
                if !same {
                    return Err(err(format!(
                        "{} was changed since the Store wrote it. Put it back or remove it, then try again.",
                        shown.display()
                    )));
                }
            }
            None => {
                return Err(err(format!(
                    "{} is already there and the Store didn't put it there. Remove it first.",
                    shown.display()
                )));
            }
        }
    }

    hook("planned")?;

    // Commit. From here on a failure, or a panic, undoes what was done.
    let mut undo = Undo::new(data, apps);
    undo.app_created = old.is_none().then(|| id.clone());
    let app = apps
        .ensure_sub(&id, 0o700, true)
        .map_err(|e| io_err("make a folder", &e))?;
    undo.app = Some(app.try_clone().map_err(|e| io_err("make a folder", &e))?);
    match apps.rename_noreplace(stage_name, &app, &inner.version) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // A leftover of the same version that is not the installed one.
            if old.as_ref().is_some_and(|o| o.version == inner.version) {
                return Err(err("That version is already installed."));
            }
            app.remove_all(&inner.version)
                .map_err(|e| io_err("clear an old folder", &e))?;
            apps.rename_noreplace(stage_name, &app, &inner.version)
                .map_err(|e| io_err("move the app into place", &e))?;
        }
        Err(e) => return Err(io_err("move the app into place", &e)),
    }
    undo.version = Some(inner.version.clone());

    for e in &plan.exports {
        let (dir, name) = export_parent(data, &e.to, true)
            .map_err(|er| io_err("write a menu or icon file", &er))?
            .ok_or_else(|| err("Could not make a folder for a menu or icon file."))?;
        let new_sha = hex(&sha2::Sha256::digest(&e.bytes));
        let write = |expect| dir.replace_checked(name, &e.bytes, 0o644, expect, MAX_COPY);
        let result = match recorded(&e.to) {
            Some(c) => match write(Expect::Sha(&c.sha256)) {
                // The earlier copy is gone: a plain new file.
                Err(ReplaceError::Refused(Refused::Gone)) => write(Expect::Absent),
                other => other,
            },
            None => write(Expect::Absent),
        };
        match result {
            Ok(Some(old_bytes)) => undo.replaced.push((e.to.clone(), old_bytes, new_sha)),
            Ok(None) => undo.created.push((e.to.clone(), new_sha)),
            Err(ReplaceError::Refused(_)) => {
                return Err(err(format!(
                    "{} changed while the Store was installing. Nothing was changed; try again.",
                    dirs.data.join(&e.to).display()
                )));
            }
            Err(ReplaceError::Stranded) => {
                return Err(err(format!(
                    "{} was changed while the Store was installing, and could not be put back where it was. Its content is in a hidden file next to it.",
                    dirs.data.join(&e.to).display()
                )));
            }
            Err(ReplaceError::Displaced(kept)) => {
                return Err(err(format!(
                    "{} was changed while the Store was installing; the file written meanwhile was kept as {kept} next to it. Nothing else was changed.",
                    dirs.data.join(&e.to).display()
                )));
            }
            Err(ReplaceError::Io(er)) => return Err(io_err("write a menu or icon file", &er)),
        }
    }
    hook("exports")?;
    // Switch.
    let old_target = app.read_link("current").ok();
    let tmp = unique(".current.");
    app.symlink(&inner.version, &tmp)
        .map_err(|e| io_err("switch versions", &e))?;
    if let Err(e) = app.rename(&tmp, &app, "current") {
        let _ = app.unlink(&tmp);
        return Err(io_err("switch versions", &e));
    }
    undo.current = Some(old_target);
    hook("switch")?;
    let size = inner
        .files
        .iter()
        .fold(0u64, |sum, f| sum.saturating_add(f.size));
    let rec = Record {
        schema: 1,
        id: id.clone(),
        name: inner.name.clone(),
        summary: inner.summary.clone(),
        version: inner.version.clone(),
        previous: old.as_ref().map(|o| o.version.clone()),
        origin: opts.origin.clone(),
        exe: plan.exe.clone(),
        copied: plan
            .exports
            .iter()
            .map(|e| Copied {
                to: e.to.clone(),
                sha256: hex(&sha2::Sha256::digest(&e.bytes)),
            })
            .collect(),
        size,
        installed_at: now(),
    };
    hook("record")?;
    write_record(&app, &rec)?;
    undo.keep();

    // Tidy: nothing here can undo the install, and none of it is fatal.
    tidy(lock, &app, &rec, old.as_ref());
    Ok(Done {
        id,
        name: inner.name,
        version: inner.version,
        replaced: old.map(|o| o.version),
        signer: opts.origin.signer.clone(),
    })
}

/// After a successful install: copies the old version had and the new one has
/// not are removed (if still as the Store wrote them), and so is every version
/// folder other than the new one and the one before it.
fn tidy(lock: &Lock, app: &Dir, rec: &Record, old: Option<&Record>) {
    if let Some(old) = old {
        for c in &old.copied {
            if !rec.copied.iter().any(|n| n.to == c.to) {
                let _ = remove_export(&lock.data, &c.to, &c.sha256);
            }
        }
    }
    // What a killed write left: the record's and the link's temporary names,
    // and those of the copied files (only an exact match, only the Store's).
    app.sweep_temps(RECORD);
    app.sweep_link_temps(".current.");
    for c in &rec.copied {
        if let Ok(Some((dir, name))) = export_parent(&lock.data, &c.to, false) {
            dir.sweep_temps(name);
        }
    }
    let keep = [Some(rec.version.as_str()), rec.previous.as_deref()];
    let Ok(names) = app.list_text() else {
        return;
    };
    for name in names {
        if Version::parse(&name).is_some()
            && !keep.contains(&Some(name.as_str()))
            && app.stat(&name).is_ok_and(|m| m.kind == Kind::Dir)
        {
            let _ = app.remove_all(&name);
        }
    }
}

/// What an uninstall did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub name: String,
    /// Files left alone, with the reason.
    pub left: Vec<(PathBuf, String)>,
}

/// Removes an app the Store installed: its copied files (each only if still
/// as written) and its folder. The record decides; nothing else is removed.
pub fn uninstall(dirs: &Dirs, id: &str) -> Result<Removed, Error> {
    let lock = Lock::take(dirs)?;
    let rec = read_record_in(&lock.apps, id)
        .ok_or_else(|| err("That app wasn't installed by the Store."))?;
    let mut left = Vec::new();
    for c in &rec.copied {
        if let Err(why) = remove_export(&lock.data, &c.to, &c.sha256) {
            left.push((dirs.data.join(&c.to), why));
        }
    }
    // The app's folder, the record last: a removal that stops half way leaves
    // an app that is still listed (and can be removed again), not one that is
    // hidden and cannot be touched.
    remove_app_folder(&lock.apps, id)?;
    Ok(Removed {
        name: rec.name,
        left,
    })
}

/// Removes `telamon-apps/<id>`: everything in it but the record, then the
/// record, then the folder.
fn remove_app_folder(apps: &Dir, id: &str) -> Result<(), Error> {
    let bad = |what: &str, e: std::io::Error| io_err(what, &e);
    let app = apps
        .sub_owned(id)
        .map_err(|e| bad("open the app's folder", e))?;
    let names = app.list().map_err(|e| bad("remove the app's folder", e))?;
    for name in names {
        if name.to_str() == Some(RECORD) {
            continue;
        }
        let Some(name) = name.to_str() else {
            // A name that is not text cannot be named by `Dir`; the whole
            // folder is removed below, after the record.
            continue;
        };
        app.remove_all(name)
            .map_err(|e| bad("remove the app's folder", e))?;
    }
    hook("contents")?;
    app.unlink(RECORD)
        .map_err(|e| bad("remove the record", e))?;
    drop(app);
    apps.remove_all(id)
        .map_err(|e| bad("remove the app's folder", e))
}

/// Starts an installed app. The program is `current/<exe>` of the record; its
/// way there is checked through the open folders (real folders of this user,
/// `current` a link to a version, the program a file that can run). The
/// program is then started by that path, as the menu entry does; it gets no
/// descriptor of the Store's (all are marked close-on-exec) and an
/// activation token only if `token` is a valid one.
pub fn launch(dirs: &Dirs, id: &str, token: Option<&str>) -> Result<(), Error> {
    let gone = || err("The app's program is gone. Install it again.");
    let data = open_data(dirs, false).map_err(|_| gone())?;
    let apps = open_apps(&data, false).map_err(|_| gone())?;
    let rec =
        read_record_in(&apps, id).ok_or_else(|| err("That app wasn't installed by the Store."))?;
    let md = (|| {
        let app = apps.sub_owned(id).ok()?;
        let version = app.read_link("current").ok()?;
        Version::parse(&version)?;
        let bin = app.sub(&version).ok()?.sub("bin").ok()?;
        bin.stat_following(rec.exe.strip_prefix("bin/")?).ok()
    })()
    .ok_or_else(gone)?;
    if md.kind != Kind::File || md.mode & 0o100 == 0 {
        return Err(err("The app's program is not a program any more."));
    }
    let exe = dirs.app(id).join("current").join(&rec.exe);
    let mut cmd = Command::new(&exe);
    cmd.current_dir(&dirs.home);
    // SAFETY: the closure only makes one system call, which is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            // Anything above stderr that is still open in the Store (Qt,
            // libraries) is closed for the program, whatever flags it was
            // opened with. CLOSE_RANGE_CLOEXEC (Linux 5.11) only sets the
            // flag, so the pipe `spawn` reports a failed exec through stays
            // usable. Older kernels: ignored, the descriptors the Store opens
            // itself are close-on-exec already.
            libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 4u32);
            Ok(())
        });
    }
    let token = token.and_then(crate::flatpak::valid_activation_token);
    crate::appimage::install::run_detached(cmd, token).map_err(|e| err(e.0))
}

/// Writes `bytes` to a new file, for tests and the CLI.
pub fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}
