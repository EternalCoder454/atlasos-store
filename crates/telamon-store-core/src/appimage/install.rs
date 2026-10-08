//! Installing an AppImage for the user, and taking it away again.
//!
//! Install copies (never moves) the file to `~/Applications/<Name>.AppImage`,
//! puts its icon in the user's hicolor theme and writes a desktop entry in
//! `$XDG_DATA_HOME/applications` that carries the Store's marker, so the Store
//! lists it under Installed. Uninstall removes exactly those three things and
//! only after checking that they are what the marker says they are: nothing
//! outside `~/Applications` and the XDG data folders is ever removed, and no
//! link is followed.
//!
//! Nothing here runs the AppImage except [`launch`], which the user asks for
//! with Open.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::fsutil;
use super::inspect::Inspection;
use super::meta::IconKind;
use crate::keyfile::{KeyFile, Limits as KeyLimits};
use crate::text;

/// The folder AppImages are installed to, under the user's home.
pub const APPLICATIONS: &str = "Applications";
/// Prefix of the desktop entries the Store writes.
pub const ID_PREFIX: &str = "appimage-";
const MARKER: &str = "X-Telamon-AppImage";
const MARKER_PATH: &str = "X-Telamon-AppImage-Path";
const MARKER_ICON: &str = "X-Telamon-AppImage-Icon";
const MARKER_VERSION: &str = "X-Telamon-AppImage-Version";
/// Most numbered names tried when a name is taken.
const MAX_NUMBERED: u32 = 99;
/// Most desktop entries looked at when listing.
const MAX_LISTED: usize = 500;
/// Largest desktop entry read.
const MAX_ENTRY: u64 = 64 * 1024;
/// Standard icon sizes of the hicolor theme.
const SIZES: [u32; 13] = [16, 22, 24, 32, 36, 48, 64, 72, 96, 128, 192, 256, 512];
/// Where the FUSE 2 library lives; type 2 AppImages mount themselves with it.
const FUSE_LIBS: [&str; 5] = [
    "/usr/lib64/libfuse.so.2",
    "/usr/lib/libfuse.so.2",
    "/lib64/libfuse.so.2",
    "/usr/lib/x86_64-linux-gnu/libfuse.so.2",
    "/usr/lib/aarch64-linux-gnu/libfuse.so.2",
];

/// Where things go. [`Dirs::from_env`] is the user's; tests make their own.
#[derive(Debug, Clone)]
pub struct Dirs {
    /// `~/Applications`
    pub applications: PathBuf,
    /// `$XDG_DATA_HOME`
    pub data: PathBuf,
    /// Places `libfuse.so.2` may be.
    pub fuse_libs: Vec<PathBuf>,
}

impl Dirs {
    pub fn from_env() -> Option<Dirs> {
        Some(Dirs {
            applications: fsutil::home()?.join(APPLICATIONS),
            data: fsutil::data_home()?,
            fuse_libs: FUSE_LIBS.iter().map(PathBuf::from).collect(),
        })
    }

    fn entries(&self) -> PathBuf {
        self.data.join("applications")
    }

    fn icons(&self) -> PathBuf {
        self.data.join("icons")
    }

    /// Whether type 2 AppImages can mount themselves here.
    pub fn fuse_available(&self) -> bool {
        self.fuse_libs.iter().any(|p| p.exists())
    }
}

/// Why an install or uninstall did not happen, in words for the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallError(pub String);

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn err(s: impl Into<String>) -> InstallError {
    InstallError(s.into())
}

fn io_err(what: &str, e: &io::Error) -> InstallError {
    err(format!("{what}: {}", e.kind()))
}

/// A file name part from untrusted text: ASCII letters and digits, `.`, `_`
/// and `-`; spaces become `-`; nothing else survives; no leading dot.
pub fn safe_part(s: &str, max: usize) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let c = if c == ' ' { '-' } else { c };
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            if out.len() >= max {
                break;
            }
            out.push(c);
        }
    }
    // Runs of dots become one, however long (so the result is its own
    // `safe_part`: install and the listing both compare names this way).
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    out.trim_matches(|c| c == '.' || c == '-' || c == '_')
        .to_string()
}

/// What an install will do, decided before the user is asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Where the AppImage is copied to.
    pub target: PathBuf,
    /// The desktop entry's ID (`appimage-...`, without `.desktop`).
    pub id: String,
    /// An earlier install of this app by the Store is replaced.
    pub replaces: bool,
    /// The target is not the plain name because that was taken.
    pub renamed: bool,
}

/// An installed AppImage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub version: String,
    pub path: PathBuf,
    /// The icon file, when the entry has one.
    pub icon: Option<PathBuf>,
    /// The AppImage file exists.
    pub present: bool,
    pub size: u64,
}

/// A desktop entry of ours, read back and checked.
struct Record {
    id: String,
    name: String,
    version: String,
    path: PathBuf,
    icon_rel: Option<String>,
}

fn id_ok(id: &str) -> bool {
    id.strip_prefix(ID_PREFIX).is_some_and(|rest| {
        (1..=100).contains(&rest.len())
            && rest.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
            && !rest.starts_with(['.', '-', '_'])
    })
}

/// `hicolor/<size>x<size>/apps/<id>.png` or `hicolor/scalable/apps/<id>.svg`.
fn icon_rel_ok(rel: &str, id: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    let [theme, dir, apps, file] = parts[..] else {
        return false;
    };
    if theme != "hicolor" || apps != "apps" {
        return false;
    }
    let dir_ok = dir == "scalable" || SIZES.iter().any(|s| dir == format!("{s}x{s}"));
    let file_ok = file == format!("{id}.png") || file == format!("{id}.svg");
    dir_ok && file_ok
}

fn key_limits() -> KeyLimits {
    KeyLimits {
        max_bytes: MAX_ENTRY as usize,
        max_lines: 500,
        max_groups: 8,
        max_keys: 200,
        max_value: 8192,
    }
}

/// Reads and checks the entry `id` of ours; `None` when it is not one.
fn read_record(dirs: &Dirs, id: &str) -> Option<Record> {
    if !id_ok(id) {
        return None;
    }
    let path = dirs.entries().join(format!("{id}.desktop"));
    let bytes = fsutil::read_private(&path, MAX_ENTRY).ok()??;
    let kf = KeyFile::parse(&bytes, &key_limits()).ok()?;
    let g = "Desktop Entry";
    if kf.string(g, MARKER).ok()??.as_str() != "true" {
        return None;
    }
    let file = PathBuf::from(kf.string(g, MARKER_PATH).ok()??);
    // Only a plain file directly in ~/Applications.
    if file.parent() != Some(dirs.applications.as_path())
        || file.file_name().and_then(|n| n.to_str()).is_none_or(|n| {
            !n.ends_with(".AppImage") || n != safe_part(n, 120) || n.starts_with('.')
        })
    {
        return None;
    }
    let icon_rel = kf
        .string(g, MARKER_ICON)
        .ok()
        .flatten()
        .filter(|r| icon_rel_ok(r, id));
    Some(Record {
        id: id.to_string(),
        name: text::clean(&kf.string(g, "Name").ok()??, super::meta::MAX_NAME),
        version: text::clean(
            &kf.string(g, MARKER_VERSION)
                .ok()
                .flatten()
                .unwrap_or_default(),
            100,
        ),
        path: file,
        icon_rel,
    })
}

/// The base of the entry ID for an app: its AppStream ID, else its name.
fn id_base(insp: &Inspection) -> String {
    let src = if insp.app_id.is_empty() {
        &insp.name
    } else {
        &insp.app_id
    };
    let mut s = String::new();
    for c in src.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-') {
            s.push(c);
        } else if c == ' ' {
            s.push('-');
        }
    }
    let s: String = s.trim_matches(['.', '-', '_']).chars().take(60).collect();
    if s.is_empty() { "app".to_string() } else { s }
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

/// Decides the names for an install: an earlier install of the same app by
/// the Store is replaced; otherwise a free name, numbered when needed.
pub fn plan(dirs: &Dirs, insp: &Inspection) -> Result<Plan, InstallError> {
    let base = id_base(insp);
    let file_base = {
        let b = safe_part(&insp.name, 60);
        if b.is_empty() { "App".to_string() } else { b }
    };
    for n in 1..=MAX_NUMBERED {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let id = format!("{ID_PREFIX}{base}{suffix}");
        // The Store's own earlier install of this app, under whichever
        // number it got, is the one to replace.
        if let Some(r) = read_record(dirs, &id) {
            return Ok(Plan {
                target: r.path,
                id,
                replaces: true,
                renamed: n > 1,
            });
        }
        let target = dirs
            .applications
            .join(format!("{file_base}{suffix}.AppImage"));
        if exists(&dirs.entries().join(format!("{id}.desktop"))) || exists(&target) {
            continue;
        }
        return Ok(Plan {
            target,
            id,
            replaces: false,
            renamed: n > 1,
        });
    }
    Err(err("There are too many apps with this name already."))
}

// ---- the desktop entry ----

/// A value of the key file: backslashes doubled, control characters gone,
/// leading space kept (`\s`).
pub fn escape_value(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            ' ' if i == 0 => out.push_str("\\s"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// One argument of an `Exec` line as the Desktop Entry spec wants it:
/// `%` as `%%`, and in double quotes with `"`, `` ` ``, `$` and `\` escaped
/// when it holds a reserved character. (The key-file escaping of the whole
/// line comes after.)
pub fn exec_arg(arg: &str) -> String {
    let arg = arg.replace('%', "%%");
    let reserved = |c: char| " \t\n\"'\\><~|&;$*?#()`".contains(c);
    if arg.is_empty() || arg.chars().any(reserved) {
        let mut q = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                q.push('\\');
            }
            q.push(c);
        }
        q.push('"');
        q
    } else {
        arg
    }
}

/// The `Exec` value (before key-file escaping) for running `path`.
pub fn exec_line(path: &Path, extract_and_run: bool) -> String {
    let p = exec_arg(&path.to_string_lossy());
    if extract_and_run {
        format!("env APPIMAGE_EXTRACT_AND_RUN=1 {p}")
    } else {
        p
    }
}

/// Splits an `Exec` line the way a launcher does (the inverse of
/// [`exec_line`]): quoting, `\` escapes inside quotes and `%%`. Field codes
/// (`%f`, `%U`...) are returned as they are. Used by the tests and by nothing
/// that runs anything.
pub fn split_exec(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => cur.push(chars.next()?),
            '%' if chars.peek() == Some(&'%') => {
                chars.next();
                cur.push('%');
                started = true;
            }
            ' ' | '\t' if !quoted => {
                if started {
                    out.push(std::mem::take(&mut cur));
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
        out.push(cur);
    }
    Some(out)
}

/// The text of the desktop entry for an install.
pub fn entry_text(plan: &Plan, insp: &Inspection, exec: &str, icon_rel: Option<&str>) -> String {
    let mut s = String::from("[Desktop Entry]\nType=Application\nVersion=1.5\n");
    s.push_str(&format!("Name={}\n", escape_value(&insp.name)));
    if !insp.summary.is_empty() {
        s.push_str(&format!("Comment={}\n", escape_value(&insp.summary)));
    }
    s.push_str(&format!("Exec={}\n", escape_value(exec)));
    s.push_str(&format!(
        "Icon={}\n",
        if icon_rel.is_some() {
            plan.id.as_str()
        } else {
            "application-x-executable"
        }
    ));
    s.push_str("Terminal=false\nStartupNotify=true\n");
    s.push_str(&format!("{MARKER}=true\n"));
    s.push_str(&format!(
        "{MARKER_PATH}={}\n",
        escape_value(&plan.target.to_string_lossy())
    ));
    if let Some(rel) = icon_rel {
        s.push_str(&format!("{MARKER_ICON}={rel}\n"));
    }
    if !insp.version.is_empty() {
        s.push_str(&format!(
            "{MARKER_VERSION}={}\n",
            escape_value(&insp.version)
        ));
    }
    s
}

// ---- install ----

fn applications_dir(dirs: &Dirs) -> Result<(), InstallError> {
    match fs::symlink_metadata(&dirs.applications) {
        Ok(md) if md.is_dir() && md.uid() == fsutil::euid() => Ok(()),
        Ok(_) => Err(err(
            "The Applications folder in your home is a link, or isn't yours, so nothing is put there.",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dirs.applications.parent() {
                fs::create_dir_all(parent).map_err(|e| io_err("make the folder", &e))?;
            }
            fs::DirBuilder::new()
                .mode(0o755)
                .create(&dirs.applications)
                .map_err(|e| io_err("make the Applications folder", &e))
        }
        Err(e) => Err(io_err("look at the Applications folder", &e)),
    }
}

/// Copies `source` to a temporary file next to `target`, hashing as it goes.
/// Returns the temporary path and the hash.
fn copy_checked(
    source: &Path,
    target: &Path,
    expect: &Inspection,
) -> Result<PathBuf, InstallError> {
    let mut src = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(source)
        .map_err(|e| io_err("open the file", &e))?;
    let md = src.metadata().map_err(|e| io_err("read the file", &e))?;
    if !md.is_file() || md.len() != expect.size {
        return Err(err(
            "The file changed since it was looked at. Open it again.",
        ));
    }
    let tmp = fsutil::temp_sibling(target);
    let result = (|| -> Result<(), InstallError> {
        let mut dst = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
            .map_err(|e| io_err("make the copy", &e))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut total = 0u64;
        loop {
            let n = src
                .read(&mut buf)
                .map_err(|e| io_err("read the file", &e))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > expect.size {
                return Err(err(
                    "The file changed since it was looked at. Open it again.",
                ));
            }
            hash.update(&buf[..n]);
            dst.write_all(&buf[..n])
                .map_err(|e| io_err("write the copy", &e))?;
        }
        if total != expect.size || super::inspect::hex(&hash.finalize()) != expect.sha256 {
            return Err(err(
                "The file changed since it was looked at. Open it again.",
            ));
        }
        dst.sync_all().map_err(|e| io_err("write the copy", &e))?;
        dst.set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(|e| io_err("make the copy runnable", &e))?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(tmp),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The hicolor folder and file extension for an icon.
fn icon_place(insp: &Inspection) -> Option<(String, IconKind)> {
    let icon = insp.icon.as_ref()?;
    match icon.kind {
        IconKind::Svg => Some(("scalable".to_string(), IconKind::Svg)),
        IconKind::Png => {
            let (w, h) = super::meta::png_size(&icon.bytes)?;
            let side = w.max(h);
            let pick = SIZES
                .iter()
                .copied()
                .min_by_key(|s| s.abs_diff(side))
                .unwrap_or(256);
            Some((format!("{pick}x{pick}"), IconKind::Png))
        }
    }
}

/// Installs. `insp` is what the user was shown; the file must still be
/// exactly that (size and SHA-256 are checked while copying). Returns what
/// was installed.
pub fn install(
    dirs: &Dirs,
    source: &Path,
    insp: &Inspection,
    plan: &Plan,
) -> Result<Installed, InstallError> {
    if !id_ok(&plan.id) || plan.target.parent() != Some(dirs.applications.as_path()) {
        return Err(err("That isn't a place the Store installs to."));
    }
    if insp.sha256.is_empty() || insp.name.is_empty() {
        return Err(err(
            "There is nothing to install: the file was not looked at.",
        ));
    }
    let old = if plan.replaces {
        // The plan was made when the dialog opened: it must still be the
        // Store's own install, at the same place.
        let r = read_record(dirs, &plan.id).filter(|r| r.path == plan.target);
        Some(r.ok_or_else(|| {
            err("The installed copy changed since you were asked. Open the file again.")
        })?)
    } else {
        None
    };
    applications_dir(dirs)?;
    let exec = exec_line(&plan.target, !dirs.fuse_available());
    let tmp = copy_checked(source, &plan.target, insp)?;

    let mut created: Vec<PathBuf> = vec![tmp.clone()];
    let rollback = |created: &[PathBuf]| {
        for p in created {
            let _ = fs::remove_file(p);
        }
    };
    // The AppImage: a new name, or our own earlier copy.
    let placed = if plan.replaces {
        fs::rename(&tmp, &plan.target)
    } else {
        fsutil::rename_noreplace(&tmp, &plan.target)
    };
    match placed {
        Ok(()) => {
            created.clear();
            if !plan.replaces {
                created.push(plan.target.clone());
            }
        }
        Err(e) => {
            rollback(&created);
            return Err(if e.kind() == io::ErrorKind::AlreadyExists {
                err("A file with this name appeared in Applications. Try again.")
            } else {
                io_err("put the app in Applications", &e)
            });
        }
    }

    // The icon.
    let mut icon_rel = None;
    if let (Some(icon), Some((dir, kind))) = (insp.icon.as_ref(), icon_place(insp)) {
        let rel = format!("hicolor/{dir}/apps/{}.{}", plan.id, kind.ext());
        let path = dirs.icons().join(&rel);
        let made = path
            .parent()
            .map(fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| fsutil::write_atomic(&path, &icon.bytes, 0o644));
        match made {
            Ok(()) => {
                if !plan.replaces {
                    created.push(path);
                }
                icon_rel = Some(rel);
            }
            Err(e) => log::warn!("could not save the icon: {e}"),
        }
    }

    // The desktop entry, last: the app shows up only when all is in place.
    let entry = dirs.entries().join(format!("{}.desktop", plan.id));
    let text = entry_text(plan, insp, &exec, icon_rel.as_deref());
    // A menu entry that is not ours, appeared meanwhile, is never replaced.
    if !plan.replaces && exists(&entry) {
        rollback(&created);
        return Err(err("A menu entry with this name appeared. Try again."));
    }
    let wrote = fs::create_dir_all(dirs.entries())
        .and_then(|()| fsutil::write_atomic(&entry, text.as_bytes(), 0o644));
    if let Err(e) = wrote {
        if !plan.replaces {
            rollback(&created);
        }
        return Err(io_err("write the menu entry", &e));
    }
    // The earlier install's icon, when this one has another (a different
    // size or kind, or none): it was the Store's own, and is not recorded now.
    if let Some(old_rel) = old.as_ref().and_then(|r| r.icon_rel.as_ref())
        && icon_rel.as_ref() != Some(old_rel)
    {
        let (mut removed, mut left) = (Vec::new(), Vec::new());
        remove_regular(&dirs.icons().join(old_rel), &mut removed, &mut left);
    }
    Ok(Installed {
        id: plan.id.clone(),
        name: insp.name.clone(),
        version: insp.version.clone(),
        path: plan.target.clone(),
        icon: icon_rel.map(|r| dirs.icons().join(r)),
        present: true,
        size: insp.size,
    })
}

// ---- list, uninstall, open ----

/// The AppImages the Store installed.
pub fn list(dirs: &Dirs) -> Vec<Installed> {
    let Ok(rd) = fs::read_dir(dirs.entries()) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".desktop").map(str::to_string))
        .filter(|n| n.starts_with(ID_PREFIX))
        .take(MAX_LISTED)
        .collect();
    ids.sort();
    ids.iter()
        .filter_map(|id| read_record(dirs, id))
        .map(|r| {
            let md = fs::symlink_metadata(&r.path).ok();
            let present = md.as_ref().is_some_and(|m| m.is_file());
            Installed {
                icon: r.icon_rel.as_ref().map(|rel| dirs.icons().join(rel)),
                size: md.filter(|m| m.is_file()).map_or(0, |m| m.len()),
                id: r.id,
                name: r.name,
                version: r.version,
                path: r.path,
                present,
            }
        })
        .collect()
}

/// What an uninstall did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub name: String,
    /// Files removed.
    pub removed: Vec<PathBuf>,
    /// Things left alone, with why (not a regular file, for one).
    pub left: Vec<(PathBuf, &'static str)>,
}

fn remove_regular(
    path: &Path,
    removed: &mut Vec<PathBuf>,
    left: &mut Vec<(PathBuf, &'static str)>,
) {
    match fs::symlink_metadata(path) {
        Ok(md) if md.is_file() && md.uid() == fsutil::euid() => match fs::remove_file(path) {
            Ok(()) => removed.push(path.to_path_buf()),
            Err(_) => left.push((path.to_path_buf(), "could not be removed")),
        },
        Ok(_) => left.push((path.to_path_buf(), "is not a plain file of yours")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => left.push((path.to_path_buf(), "could not be looked at")),
    }
}

/// Removes the install `id`: the AppImage, its icon and its menu entry, the
/// entry last. Refuses an entry that is not the Store's own.
pub fn uninstall(dirs: &Dirs, id: &str) -> Result<Removed, InstallError> {
    let r = read_record(dirs, id).ok_or_else(|| err("That app wasn't installed by the Store."))?;
    let mut removed = Vec::new();
    let mut left = Vec::new();
    remove_regular(&r.path, &mut removed, &mut left);
    if let Some(rel) = &r.icon_rel {
        remove_regular(&dirs.icons().join(rel), &mut removed, &mut left);
    }
    // The menu entry goes whatever happened to the app's file (a foreign
    // file or a link put in its place is left alone and reported).
    let entry = dirs.entries().join(format!("{id}.desktop"));
    remove_regular(&entry, &mut removed, &mut left);
    Ok(Removed {
        name: r.name,
        removed,
        left,
    })
}

/// The command and environment to open an installed AppImage: the recorded
/// path, with `APPIMAGE_EXTRACT_AND_RUN=1` when FUSE 2 is missing.
fn open_command(dirs: &Dirs, id: &str) -> Result<Command, InstallError> {
    let r = read_record(dirs, id).ok_or_else(|| err("That app wasn't installed by the Store."))?;
    let md = fs::symlink_metadata(&r.path)
        .map_err(|_| err("The app's file is gone. Remove it and install it again."))?;
    if !md.is_file() || md.mode() & 0o100 == 0 {
        return Err(err("The app's file is not a program any more."));
    }
    let mut cmd = Command::new(&r.path);
    if !dirs.fuse_available() {
        cmd.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    }
    if let Some(home) = fsutil::home() {
        cmd.current_dir(home);
    }
    Ok(cmd)
}

/// Starts an installed AppImage and returns once it has started; it is not
/// waited for. Own process group, no input, output dropped, the activation
/// token (checked by the caller) in its environment. An error is reported only
/// when it stops with a failure in the first moments.
pub fn launch(dirs: &Dirs, id: &str, token: Option<&str>) -> Result<(), InstallError> {
    run_detached(open_command(dirs, id)?, token)
}

/// Starts `cmd` as [`launch`] does: own process group, no input, output
/// dropped, the activation token (checked by the caller) or none in its
/// environment, reaped in the background; an error only when it stops with a
/// failure in the first moments. Native Telamon apps open the same way.
pub fn run_detached(mut cmd: Command, token: Option<&str>) -> Result<(), InstallError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    match token {
        Some(t) => {
            cmd.env("XDG_ACTIVATION_TOKEN", t)
                .env("DESKTOP_STARTUP_ID", t);
        }
        None => {
            cmd.env_remove("XDG_ACTIVATION_TOKEN")
                .env_remove("DESKTOP_STARTUP_ID");
        }
    }
    let mut child = cmd.spawn().map_err(|e| io_err("start the app", &e))?;
    let start = Instant::now();
    let early = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < Duration::from_millis(1500) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => break None,
        }
    };
    match early {
        Some(status) if !status.success() => Err(err(format!(
            "The app stopped right after it started ({status})."
        ))),
        Some(_) => Ok(()),
        None => {
            let _ = std::thread::Builder::new()
                .name("telamon-store-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
            Ok(())
        }
    }
}

/// Opens a file for reading only to hand its `File` to callers that need one
/// (kept here so the module's users need not know the flags).
pub fn open_source(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
}
