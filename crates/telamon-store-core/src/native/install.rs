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

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::archive::{self, hex};
use super::desktop;
use super::manifest::{Host, Manifest};
use super::version::Version;
use super::{APPS_DIR, Error, err, io_err, valid_app_id};
use crate::appimage::fsutil;

const RECORD: &str = "install.json";
const MAX_RECORD: u64 = 256 * 1024;
const MAX_LISTED: usize = 200;
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
    use std::cell::Cell;
    thread_local! {
        /// `"exports"`, `"switch"` or `"record"`: fail after that step.
        pub static FAIL_AFTER: Cell<Option<&'static str>> = const { Cell::new(None) };
    }
}

#[cfg(feature = "test-hooks")]
fn hook(step: &str) -> Result<(), Error> {
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
    /// The system's data folders (`XDG_DATA_DIRS`, Flatpak's exports): an app
    /// whose menu entry or D-Bus name is already there is not installed, so a
    /// bundle cannot replace an app that is not its own.
    pub system: Vec<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Option<Dirs> {
        let home = fsutil::home()?;
        let mut system: Vec<PathBuf> = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into())
            .split(':')
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .collect();
        system.push("/var/lib/flatpak/exports/share".into());
        system.push(home.join(".local/share/flatpak/exports/share"));
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
}

impl Origin {
    pub fn release(repo: &str, tag: &str) -> Origin {
        Origin {
            kind: "release".into(),
            repo: Some(repo.into()),
            tag: Some(tag.into()),
        }
    }

    pub fn local() -> Origin {
        Origin {
            kind: "local".into(),
            repo: None,
            tag: None,
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

/// Reads the record of `id`; `None` when there is none or it is not ours.
pub fn read_record(dirs: &Dirs, id: &str) -> Option<Record> {
    if !valid_app_id(id) {
        return None;
    }
    let dir = dirs.app(id);
    match fs::symlink_metadata(&dir) {
        Ok(md) if md.is_dir() && md.uid() == fsutil::euid() => {}
        _ => return None,
    }
    let bytes = fsutil::read_private(&dir.join(RECORD), MAX_RECORD).ok()??;
    let mut rec: Record = serde_json::from_slice(&bytes).ok()?;
    if !rec.check(id) {
        return None;
    }
    rec.name = crate::text::clean(&rec.name, 80);
    rec.summary = crate::text::clean(&rec.summary, 300);
    Some(rec)
}

fn write_record(dirs: &Dirs, rec: &Record) -> Result<(), Error> {
    let bytes = serde_json::to_vec_pretty(rec).map_err(|_| err("Could not write the record."))?;
    fsutil::write_atomic(&dirs.app(&rec.id).join(RECORD), &bytes, 0o644)
        .map_err(|e| io_err("write the record", &e))
}

/// The installed apps, sorted by name.
pub fn list(dirs: &Dirs) -> Vec<Installed> {
    let Ok(rd) = fs::read_dir(dirs.apps()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten().take(MAX_LISTED * 2) {
        let name = e.file_name();
        let Some(id) = name.to_str() else { continue };
        let Some(rec) = read_record(dirs, id) else {
            continue;
        };
        let exe = dirs.app(id).join("current").join(&rec.exe);
        let present = fs::symlink_metadata(dirs.app(id).join("current"))
            .is_ok_and(|m| m.file_type().is_symlink())
            && fs::metadata(&exe).is_ok_and(|m| m.is_file());
        let icon = rec
            .copied
            .iter()
            .filter(|c| c.to.starts_with("icons/hicolor/"))
            .max_by_key(|c| icon_rank(&c.to))
            .map(|c| dirs.data.join(&c.to));
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
/// folder, released when dropped (or the process ends).
struct Lock(#[allow(dead_code)] fs::File);

impl Lock {
    fn take(dirs: &Dirs) -> Result<Lock, Error> {
        ensure_dir(&dirs.apps())?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dirs.apps().join(".lock"))
            .map_err(|e| io_err("lock the apps folder", &e))?;
        // SAFETY: flock on a file descriptor this struct owns.
        if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&f), libc::LOCK_EX) } != 0 {
            return Err(io_err(
                "lock the apps folder",
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(Lock(f))
    }
}

fn ensure_dir(dir: &Path) -> Result<(), Error> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(dir)
        .map_err(|e| io_err("make a folder", &e))?;
    let md = fs::metadata(dir).map_err(|e| io_err("check a folder", &e))?;
    if !md.is_dir() {
        return Err(err("A folder the Store needs is a file."));
    }
    Ok(())
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
}

/// Puts back what an install changed, in the reverse order, when it fails.
#[derive(Default)]
struct Undo {
    /// Copied files that were not there before.
    created: Vec<PathBuf>,
    /// Copied files that were, with their old content.
    replaced: Vec<(PathBuf, Vec<u8>)>,
    /// The new version folder.
    version_dir: Option<PathBuf>,
    /// `current` before it was switched: the old target, if the link existed.
    current: Option<(PathBuf, Option<PathBuf>)>,
    /// The user's data folder: folders made for the copies are removed again
    /// (when empty) below its second level, never above.
    data: PathBuf,
    /// On a first install, the app's own folder (removed if empty).
    app_dir: Option<PathBuf>,
}

impl Undo {
    fn run(self) {
        if let Some((link, old)) = self.current {
            match old {
                Some(target) => {
                    let tmp = link.with_file_name(unique(".current."));
                    if std::os::unix::fs::symlink(&target, &tmp).is_ok()
                        && fs::rename(&tmp, &link).is_err()
                    {
                        let _ = fs::remove_file(&tmp);
                    }
                }
                None => {
                    let _ = fs::remove_file(&link);
                }
            }
        }
        for (path, bytes) in self.replaced {
            let _ = fsutil::write_atomic(&path, &bytes, 0o644);
        }
        for path in self.created {
            let _ = fs::remove_file(&path);
            let mut dir = path.parent();
            while let Some(d) = dir {
                // data/<root>/<sub> and above stay.
                let depth = d
                    .strip_prefix(&self.data)
                    .map_or(0, |r| r.components().count());
                if depth <= 2 || fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }
        if let Some(dir) = self.version_dir {
            let _ = fs::remove_dir_all(dir);
        }
        if let Some(dir) = self.app_dir {
            let _ = fs::remove_dir(dir);
        }
    }
}

/// Installs the bundle at `archive` (a `.tar.zst` the caller has already
/// checked against the release's size and SHA-256, or the user's own file).
/// Everything about the tree is checked here: see `archive::unpack`.
pub fn install_bundle(dirs: &Dirs, archive: &Path, opts: &Options<'_>) -> Result<Done, Error> {
    let _lock = Lock::take(dirs)?;
    // The id is not known until the archive is read: unpack under the apps
    // folder (same filesystem as the final place), then move.
    let staging = dirs.apps().join(unique(".staging-"));
    fs::DirBuilder::new()
        .mode(0o755)
        .create(&staging)
        .map_err(|e| io_err("make a folder", &e))?;
    let result = install_from(dirs, archive, opts, &staging);
    // Whatever is left of the staging folder (the move took it on success).
    let _ = fs::remove_dir_all(&staging);
    sweep(dirs);
    result
}

/// Removes staging folders an earlier, interrupted install left. The lock is
/// held by the caller, so none of them is in use.
fn sweep(dirs: &Dirs) {
    let Ok(rd) = fs::read_dir(dirs.apps()) else {
        return;
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(".staging-") {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

fn install_from(
    dirs: &Dirs,
    archive_path: &Path,
    opts: &Options<'_>,
    staging: &Path,
) -> Result<Done, Error> {
    let inner = archive::unpack(archive_path, staging, opts.outer)?;
    if let Some(expect) = opts.expect_id
        && inner.id != expect
    {
        return Err(err(
            "The bundle is for a different app than the one asked for.",
        ));
    }
    inner.compatible(opts.host)?;
    let id = inner.id.clone();
    let app_dir = dirs.app(&id);
    let old = read_record(dirs, &id);
    if let Some(old) = &old
        && old.version == inner.version
        && app_dir.join(&old.version).is_dir()
    {
        return Err(err(format!(
            "{} {} is already installed.",
            inner.name, inner.version
        )));
    }
    if old.is_none() && fs::symlink_metadata(&app_dir).is_ok() {
        // A folder the Store did not make for this app.
        let leftover = fs::read_dir(&app_dir)
            .map(|mut rd| {
                rd.any(|e| {
                    e.is_ok_and(|e| !e.file_name().to_string_lossy().starts_with(".staging-"))
                })
            })
            .unwrap_or(true);
        if leftover {
            return Err(err(
                "A folder for this app already exists that the Store did not make. Remove it first.",
            ));
        }
    }
    let prefix = app_dir.join("current");
    let plan = desktop::plan(staging, &inner, &prefix)?;

    // Not an app that is already on this computer under that ID.
    for dir in &dirs.system {
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
    }

    // Copied files: not over anything that is not this app's own, and not
    // over one the user changed since the Store wrote it.
    let recorded = |to: &str| {
        old.as_ref()
            .and_then(|o| o.copied.iter().find(|c| c.to == to))
    };
    for e in &plan.exports {
        let target = dirs.data.join(&e.to);
        match fs::symlink_metadata(&target) {
            Ok(md) if md.is_file() => match recorded(&e.to) {
                Some(c) => {
                    let same = archive::sha256_file(&target).is_ok_and(|(h, _)| h == c.sha256);
                    if !same {
                        return Err(err(format!(
                            "{} was changed since the Store wrote it. Put it back or remove it, then try again.",
                            target.display()
                        )));
                    }
                }
                None => {
                    return Err(err(format!(
                        "{} is already there and the Store didn't put it there. Remove it first.",
                        target.display()
                    )));
                }
            },
            Ok(_) => {
                return Err(err(format!(
                    "{} is already there and the Store didn't put it there. Remove it first.",
                    target.display()
                )));
            }
            Err(ref er) if er.kind() == std::io::ErrorKind::NotFound => {}
            Err(er) => return Err(io_err("check an existing file", &er)),
        }
    }

    // Commit. From here on a failure undoes what was done.
    ensure_dir(&app_dir)?;
    let version_dir = app_dir.join(&inner.version);
    let mut undo = Undo {
        data: dirs.data.clone(),
        app_dir: old.is_none().then(|| app_dir.clone()),
        ..Undo::default()
    };
    match fsutil::rename_noreplace(staging, &version_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // A leftover of the same version that is not the installed one.
            if old.as_ref().is_some_and(|o| o.version == inner.version) {
                return Err(err("That version is already installed."));
            }
            fs::remove_dir_all(&version_dir).map_err(|e| io_err("clear an old folder", &e))?;
            fsutil::rename_noreplace(staging, &version_dir)
                .map_err(|e| io_err("move the app into place", &e))?;
        }
        Err(e) => return Err(io_err("move the app into place", &e)),
    }
    undo.version_dir = Some(version_dir.clone());

    let outcome = (|| -> Result<Record, Error> {
        for e in &plan.exports {
            let target = dirs.data.join(&e.to);
            if let Some(parent) = target.parent() {
                ensure_dir(parent)?;
            }
            match fsutil::read_private(&target, 1024 * 1024) {
                Ok(Some(bytes)) => undo.replaced.push((target.clone(), bytes)),
                Ok(None) => undo.created.push(target.clone()),
                Err(er) => return Err(io_err("read a file the Store installed", &er)),
            }
            fsutil::write_atomic(&target, &e.bytes, 0o644)
                .map_err(|er| io_err("write a menu or icon file", &er))?;
        }
        hook("exports")?;
        // Switch.
        let link = app_dir.join("current");
        let old_target = fs::read_link(&link).ok();
        let tmp = app_dir.join(unique(".current."));
        std::os::unix::fs::symlink(&inner.version, &tmp)
            .map_err(|e| io_err("switch versions", &e))?;
        if let Err(e) = fs::rename(&tmp, &link) {
            let _ = fs::remove_file(&tmp);
            return Err(io_err("switch versions", &e));
        }
        undo.current = Some((link, old_target));
        hook("switch")?;
        let size = inner.files.iter().map(|f| f.size).sum();
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
        write_record(dirs, &rec)?;
        Ok(rec)
    })();
    let rec = match outcome {
        Ok(r) => r,
        Err(e) => {
            undo.run();
            return Err(e);
        }
    };

    // Tidy: nothing here can undo the install, and none of it is fatal.
    tidy(dirs, &rec, old.as_ref());
    Ok(Done {
        id,
        name: inner.name,
        version: inner.version,
        replaced: old.map(|o| o.version),
    })
}

/// After a successful install: copies the old version had and the new one has
/// not are removed (if still as the Store wrote them), and so is every version
/// folder other than the new one and the one before it.
fn tidy(dirs: &Dirs, rec: &Record, old: Option<&Record>) {
    if let Some(old) = old {
        for c in &old.copied {
            if !rec.copied.iter().any(|n| n.to == c.to) {
                let _ = remove_if_ours(dirs, c);
            }
        }
    }
    let keep = [Some(rec.version.as_str()), rec.previous.as_deref()];
    let Ok(rd) = fs::read_dir(dirs.app(&rec.id)) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if Version::parse(name).is_some()
            && !keep.contains(&Some(name))
            && e.file_type().is_ok_and(|t| t.is_dir())
        {
            let _ = fs::remove_dir_all(e.path());
        }
    }
}

/// Removes a copied file when it is a regular file with the content the Store
/// wrote. `Ok(true)` removed, `Ok(false)` was already gone, `Err` left alone.
fn remove_if_ours(dirs: &Dirs, c: &Copied) -> Result<bool, String> {
    let path = dirs.data.join(&c.to);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err("couldn't be checked".into()),
        Ok(md) if !md.is_file() => return Err("is not a plain file any more".into()),
        Ok(_) => {}
    }
    let (sha, _) = archive::sha256_file(&path).map_err(|_| "couldn't be read".to_string())?;
    if sha != c.sha256 {
        return Err("was changed since the Store wrote it".into());
    }
    fs::remove_file(&path).map_err(|_| "couldn't be removed".to_string())?;
    Ok(true)
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
    let _lock = Lock::take(dirs)?;
    let rec =
        read_record(dirs, id).ok_or_else(|| err("That app wasn't installed by the Store."))?;
    let mut left = Vec::new();
    for c in &rec.copied {
        if let Err(why) = remove_if_ours(dirs, c) {
            left.push((dirs.data.join(&c.to), why));
        }
    }
    let dir = dirs.app(id);
    // The record goes first: an interrupted removal must not look installed.
    fs::remove_file(dir.join(RECORD)).map_err(|e| io_err("remove the record", &e))?;
    fs::remove_dir_all(&dir).map_err(|e| io_err("remove the app's folder", &e))?;
    Ok(Removed {
        name: rec.name,
        left,
    })
}

/// Starts an installed app. The program is `current/<exe>` of the record.
pub fn launch(dirs: &Dirs, id: &str, token: Option<&str>) -> Result<(), Error> {
    let rec =
        read_record(dirs, id).ok_or_else(|| err("That app wasn't installed by the Store."))?;
    let exe = dirs.app(id).join("current").join(&rec.exe);
    let md = fs::metadata(&exe).map_err(|_| err("The app's program is gone. Install it again."))?;
    if !md.is_file() || md.mode() & 0o100 == 0 {
        return Err(err("The app's program is not a program any more."));
    }
    let mut cmd = Command::new(&exe);
    cmd.current_dir(&dirs.home);
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
