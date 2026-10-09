//! Folders held open by file descriptor, for everything the Store does below
//! `$XDG_DATA_HOME/telamon-apps` (and for the copies it puts in the data
//! folder's `applications`, `icons`, ...).
//!
//! A path is walked by name every time it is used, so a process of the same
//! user with less privilege (a Flatpak app with access to the home folder) can
//! swap a folder for a symbolic link between a check and the use. A [`Dir`]
//! is a folder that was opened once; every operation names one entry of it
//! (`openat`, `mkdirat`, `unlinkat`, `renameat2`, `symlinkat`, `readlinkat`,
//! `fstatat`), never a path, and never follows a link in that last step. Going
//! down a level is [`Dir::sub`], which refuses a link (`O_NOFOLLOW`). The only
//! places that follow a link are the ones the caller asks for by name
//! ([`Dir::open_following`], [`Dir::sub_following`]): `$XDG_DATA_HOME` and the
//! export roots, which a dotfile manager may legitimately have made links.
//!
//! Names are single path components (no `/`, not `.` or `..`, no NUL).

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use sha2::{Digest, Sha256};

/// How deep [`Dir::remove_all`] goes before giving up.
const MAX_DEPTH: usize = 256;
// A tree the unpacker made is never deeper than the cap on its paths, so
// `remove_all` always gets to the bottom of it.
const _: () = assert!(super::manifest::MAX_PATH_PARTS * 4 < MAX_DEPTH);

fn errno() -> io::Error {
    io::Error::last_os_error()
}

fn name_c(name: &str) -> io::Result<CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a single file name",
        ));
    }
    CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn path_c(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// What a name in a folder is, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Link,
    Other,
}

/// `fstatat` of one entry, links not followed.
#[derive(Debug, Clone, Copy)]
pub struct Meta {
    dev: u64,
    ino: u64,
    pub kind: Kind,
    pub uid: u32,
    pub mode: u32,
    pub size: u64,
}

impl Meta {
    /// Which file this is (device and inode).
    fn ident(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }

    fn from_stat(st: &libc::stat) -> Meta {
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFREG => Kind::File,
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFLNK => Kind::Link,
            _ => Kind::Other,
        };
        Meta {
            dev: st.st_dev,
            ino: st.st_ino,
            kind,
            uid: st.st_uid,
            mode: st.st_mode & 0o7777,
            size: st.st_size.max(0) as u64,
        }
    }
}

/// What the file replaced by [`Dir::replace_checked`] or removed by
/// [`Dir::remove_checked`] has to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect<'a> {
    /// Nothing is there.
    Absent,
    /// A regular file with this lower-case hex SHA-256.
    Sha(&'a str),
}

/// Why a checked replace or remove did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Something is there that should not be, or is not what was expected.
    Changed,
    /// It is gone (a remove only).
    Gone,
}

/// An open folder.
#[derive(Debug)]
pub struct Dir {
    fd: OwnedFd,
}

/// Tests force the paths that depend on the file system.
#[cfg(feature = "test-hooks")]
pub mod test_hooks {
    use std::cell::{Cell, RefCell};
    /// What a test runs at a point of a swap.
    pub type Callback = Box<dyn Fn()>;
    thread_local! {
        /// Make `renameat2` with a flag fail as a file system without it does.
        pub static NO_RENAME_FLAGS: Cell<bool> = const { Cell::new(false) };
        /// Run between a swap and the check of what was swapped out.
        pub static ON_SWAP: RefCell<Option<Callback>> = const { RefCell::new(None) };
    }
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A hidden temporary name that is unique to this process and call.
pub fn temp_name(base: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let base: String = base.chars().take(100).collect();
    format!(".{base}.{}.{n}.tmp", std::process::id())
}

pub fn hex(bytes: &[u8]) -> String {
    super::archive::hex(bytes)
}

/// SHA-256 of what `r` yields, at most `max` bytes (more is `InvalidData`),
/// and the bytes themselves when `keep`.
fn hash_reader(r: &mut impl Read, max: u64, keep: bool) -> io::Result<(String, Vec<u8>)> {
    let mut h = Sha256::new();
    let mut kept = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok((hex(&h.finalize()), kept));
        }
        total += n as u64;
        if total > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too large"));
        }
        h.update(&buf[..n]);
        if keep {
            kept.extend_from_slice(&buf[..n]);
        }
    }
}

impl Dir {
    fn from_fd(fd: RawFd) -> Dir {
        // SAFETY: `fd` was just returned by open/openat and nobody else owns it.
        Dir {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        }
    }

    fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Opens `path`, following links in every part of it, including the last
    /// (`$XDG_DATA_HOME` may be a link).
    pub fn open_following(path: &Path) -> io::Result<Dir> {
        let c = path_c(path)?;
        // SAFETY: `c` is a NUL-terminated path that outlives the call.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(errno())
        } else {
            Ok(Dir::from_fd(fd))
        }
    }

    /// Opens `path`, following links in the parts above the last but not in
    /// the last one.
    pub fn open_last_nofollow(path: &Path) -> io::Result<Dir> {
        let c = path_c(path)?;
        // SAFETY: as in `open_following`.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(errno())
        } else {
            Ok(Dir::from_fd(fd))
        }
    }

    /// A second handle on the same folder.
    pub fn try_clone(&self) -> io::Result<Dir> {
        // SAFETY: duplicating a descriptor this struct owns.
        let fd = unsafe { libc::fcntl(self.raw(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            Err(errno())
        } else {
            Ok(Dir::from_fd(fd))
        }
    }

    fn open_at(&self, name: &CStr, flags: libc::c_int) -> io::Result<libc::c_int> {
        // SAFETY: `name` is NUL-terminated and outlives the call; the folder's
        // descriptor is open.
        let fd = unsafe { libc::openat(self.raw(), name.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd < 0 { Err(errno()) } else { Ok(fd) }
    }

    /// The folder `name` of this one; a link is refused.
    pub fn sub(&self, name: &str) -> io::Result<Dir> {
        let c = name_c(name)?;
        let fd = self.open_at(&c, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW)?;
        Ok(Dir::from_fd(fd))
    }

    /// Like [`Dir::sub`], and the folder must belong to this user.
    pub fn sub_owned(&self, name: &str) -> io::Result<Dir> {
        let d = self.sub(name)?;
        d.require_owner()?;
        Ok(d)
    }

    /// The folder `name`, following a link in that last step (an export root).
    pub fn sub_following(&self, name: &str) -> io::Result<Dir> {
        let c = name_c(name)?;
        let fd = self.open_at(&c, libc::O_RDONLY | libc::O_DIRECTORY)?;
        Ok(Dir::from_fd(fd))
    }

    fn fstat(&self) -> io::Result<libc::stat> {
        // SAFETY: a zeroed `stat` is a valid out-parameter for fstat.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is open and `st` is writable.
        if unsafe { libc::fstat(self.raw(), &mut st) } != 0 {
            return Err(errno());
        }
        Ok(st)
    }

    /// Whether `path` (links followed) is this very folder.
    pub fn is_same_dir(&self, path: &Path) -> bool {
        let (Ok(st), Ok(md)) = (self.fstat(), std::fs::metadata(path)) else {
            return false;
        };
        ident_of(&md) == (st.st_dev, st.st_ino)
    }

    /// Fails unless the folder belongs to this user.
    pub fn require_owner(&self) -> io::Result<()> {
        if self.fstat()?.st_uid != crate::appimage::fsutil::euid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "belongs to someone else",
            ));
        }
        Ok(())
    }

    /// Sets the folder back to 0700 when group or others could use it.
    pub fn make_private(&self) -> io::Result<()> {
        if self.fstat()?.st_mode & 0o077 != 0 {
            // SAFETY: fchmod on a descriptor this struct owns.
            if unsafe { libc::fchmod(self.raw(), 0o700) } != 0 {
                return Err(errno());
            }
        }
        Ok(())
    }

    /// `mkdirat`. `AlreadyExists` when something is there.
    pub fn create_dir(&self, name: &str, mode: u32) -> io::Result<()> {
        let c = name_c(name)?;
        // SAFETY: as in `open_at`.
        if unsafe { libc::mkdirat(self.raw(), c.as_ptr(), mode as libc::mode_t) } != 0 {
            return Err(errno());
        }
        Ok(())
    }

    /// The folder `name`, made with `mode` (exactly, whatever the umask says)
    /// when missing; a link is refused; `owned` also requires this user's.
    pub fn ensure_sub(&self, name: &str, mode: u32, owned: bool) -> io::Result<Dir> {
        let made = match self.create_dir(name, mode) {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e),
        };
        let d = self.sub(name)?;
        if made {
            // SAFETY: fchmod on a descriptor `d` owns.
            if unsafe { libc::fchmod(d.raw(), mode as libc::mode_t) } != 0 {
                return Err(errno());
            }
        } else if owned {
            d.require_owner()?;
        }
        Ok(d)
    }

    /// `fstatat` without following a link.
    pub fn stat(&self, name: &str) -> io::Result<Meta> {
        let c = name_c(name)?;
        // SAFETY: a zeroed `stat` is a valid out-parameter.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as in `open_at`; `st` is writable.
        let rc =
            unsafe { libc::fstatat(self.raw(), c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
        if rc != 0 {
            return Err(errno());
        }
        Ok(Meta::from_stat(&st))
    }

    /// `fstatat` that follows a link in the last step (a program that is a
    /// link to another in the same folder).
    pub fn stat_following(&self, name: &str) -> io::Result<Meta> {
        let c = name_c(name)?;
        // SAFETY: as in `stat`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as in `stat`.
        let rc = unsafe { libc::fstatat(self.raw(), c.as_ptr(), &mut st, 0) };
        if rc != 0 {
            return Err(errno());
        }
        Ok(Meta::from_stat(&st))
    }

    /// `None` when nothing is there.
    pub fn stat_opt(&self, name: &str) -> io::Result<Option<Meta>> {
        match self.stat(name) {
            Ok(m) => Ok(Some(m)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Opens a regular file for reading: a link, a FIFO or any other kind of
    /// file is refused (and never waited for).
    pub fn open_read(&self, name: &str) -> io::Result<File> {
        let c = name_c(name)?;
        let fd = self.open_at(
            &c,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        )?;
        // SAFETY: `fd` is a fresh descriptor nobody else owns.
        let f = unsafe { File::from_raw_fd(fd) };
        if !f.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular file",
            ));
        }
        Ok(f)
    }

    /// Reads a regular file of this user of at most `max` bytes; `None` when
    /// it does not exist. A link, another kind of file, another owner or too
    /// big is an error.
    pub fn read_file(&self, name: &str, max: u64) -> io::Result<Option<Vec<u8>>> {
        let f = match self.open_read(name) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let md = f.metadata()?;
        use std::os::unix::fs::MetadataExt;
        if md.uid() != crate::appimage::fsutil::euid() {
            return Err(io::Error::other("not a file of this user"));
        }
        if md.len() > max {
            return Err(io::Error::other("too large"));
        }
        let mut buf = Vec::with_capacity(md.len() as usize);
        f.take(max + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > max {
            return Err(io::Error::other("too large"));
        }
        Ok(Some(buf))
    }

    /// Walks `rel` (`/`-separated names) down from this folder without
    /// following a link, and reads the file at its end like [`Dir::read_file`].
    pub fn read_at(&self, rel: &str, max: u64) -> io::Result<Option<Vec<u8>>> {
        let (dir, last) = self.walk_to_parent(rel)?;
        match dir {
            Some(d) => d.read_file(last, max),
            None => self.read_file(last, max),
        }
    }

    /// The folder holding `rel`'s last part, reached without following a link
    /// (`None` is this folder itself), and that last part.
    pub fn walk_to_parent<'a>(&self, rel: &'a str) -> io::Result<(Option<Dir>, &'a str)> {
        let mut parts: Vec<&str> = rel.split('/').collect();
        let last = parts.pop().unwrap_or("");
        let mut cur: Option<Dir> = None;
        for p in parts {
            let next = cur.as_ref().unwrap_or(self).sub(p)?;
            cur = Some(next);
        }
        Ok((cur, last))
    }

    /// A new file, `O_EXCL` and never through a link.
    pub fn create_new(&self, name: &str, mode: u32) -> io::Result<File> {
        let c = name_c(name)?;
        // SAFETY: as in `open_at`; the mode argument is passed as the variadic
        // `mode_t` openat reads for O_CREAT.
        let fd = unsafe {
            libc::openat(
                self.raw(),
                c.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_NOFOLLOW
                    | libc::O_NOCTTY
                    | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: a fresh descriptor nobody else owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Opens `name` for writing, making it (0600) when missing, never through
    /// a link; it must be a regular file of this user. For the lock file.
    pub fn open_or_create_private(&self, name: &str) -> io::Result<File> {
        let c = name_c(name)?;
        // SAFETY: as in `create_new`.
        let fd = unsafe {
            libc::openat(
                self.raw(),
                c.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_NOCTTY
                    | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: a fresh descriptor nobody else owns.
        let f = unsafe { File::from_raw_fd(fd) };
        use std::os::unix::fs::MetadataExt;
        let md = f.metadata()?;
        if !md.is_file() || md.uid() != crate::appimage::fsutil::euid() {
            return Err(io::Error::other("not a regular file of this user"));
        }
        Ok(f)
    }

    /// Sets the mode of the regular file `name` (not through a link).
    pub fn chmod_file(&self, name: &str, mode: u32) -> io::Result<()> {
        let f = self.open_read(name)?;
        // SAFETY: fchmod on a descriptor `f` owns.
        if unsafe { libc::fchmod(f.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(errno());
        }
        Ok(())
    }

    fn rename2(&self, from: &str, to_dir: &Dir, to: &str, flags: libc::c_uint) -> io::Result<()> {
        let (f, t) = (name_c(from)?, name_c(to)?);
        #[cfg(feature = "test-hooks")]
        if flags != 0 && test_hooks::NO_RENAME_FLAGS.with(|h| h.get()) {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        // SAFETY: both are NUL-terminated names that outlive the call; both
        // descriptors are open.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.raw(),
                f.as_ptr(),
                to_dir.raw(),
                t.as_ptr(),
                flags,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let e = errno();
        if e.raw_os_error() == Some(libc::EEXIST) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        Err(e)
    }

    /// `renameat`: replaces what is at `to` (a link at `to` is replaced, not
    /// followed).
    pub fn rename(&self, from: &str, to_dir: &Dir, to: &str) -> io::Result<()> {
        self.rename2(from, to_dir, to, 0)
    }

    /// Like [`Dir::rename`] but `AlreadyExists` instead of replacing
    /// (`RENAME_NOREPLACE`). On a file system without that flag (NFS, some
    /// FUSE, vfat) a file is hard-linked into place and the old name unlinked,
    /// which is still atomic about "not replacing"; a folder, or a file system
    /// without hard links, is checked for the name and then renamed, and a name
    /// made in between is replaced (an empty folder or a file the Store is
    /// about to look at anyway): narrower, and only where the file system
    /// leaves no other way.
    pub fn rename_noreplace(&self, from: &str, to_dir: &Dir, to: &str) -> io::Result<()> {
        match self.rename2(from, to_dir, to, libc::RENAME_NOREPLACE) {
            Err(e) if unsupported(&e) => self.noreplace_fallback(from, to_dir, to),
            r => r,
        }
    }

    fn noreplace_fallback(&self, from: &str, to_dir: &Dir, to: &str) -> io::Result<()> {
        let exists = || -> io::Result<()> {
            if to_dir.stat_opt(to)?.is_some() {
                Err(io::ErrorKind::AlreadyExists.into())
            } else {
                Ok(())
            }
        };
        if self.stat(from)?.kind != Kind::Dir {
            let (f, t) = (name_c(from)?, name_c(to)?);
            // SAFETY: as in `rename2`.
            let rc = unsafe { libc::linkat(self.raw(), f.as_ptr(), to_dir.raw(), t.as_ptr(), 0) };
            if rc == 0 {
                return self.unlink(from);
            }
            let e = errno();
            match e.raw_os_error() {
                Some(libc::EEXIST) => return Err(io::ErrorKind::AlreadyExists.into()),
                Some(libc::EPERM)
                | Some(libc::EOPNOTSUPP)
                | Some(libc::EMLINK)
                | Some(libc::ENOSYS)
                | Some(libc::EINVAL) => {}
                _ => return Err(e),
            }
        }
        exists()?;
        self.rename(from, to_dir, to)
    }

    /// Swaps two entries atomically (`RENAME_EXCHANGE`). `Unsupported` when the
    /// file system cannot.
    fn exchange(&self, a: &str, b: &str) -> io::Result<()> {
        match self.rename2(a, self, b, libc::RENAME_EXCHANGE) {
            Err(e) if unsupported(&e) => Err(io::ErrorKind::Unsupported.into()),
            r => r,
        }
    }

    /// `symlinkat`.
    pub fn symlink(&self, target: &str, name: &str) -> io::Result<()> {
        let n = name_c(name)?;
        let t = CString::new(target).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: both strings are NUL-terminated and outlive the call.
        if unsafe { libc::symlinkat(t.as_ptr(), self.raw(), n.as_ptr()) } != 0 {
            return Err(errno());
        }
        Ok(())
    }

    /// `readlinkat`, as text.
    pub fn read_link(&self, name: &str) -> io::Result<String> {
        let c = name_c(name)?;
        let mut buf = vec![0u8; 4097];
        // SAFETY: the buffer is writable for its whole length.
        let n = unsafe {
            libc::readlinkat(
                self.raw(),
                c.as_ptr(),
                buf.as_mut_ptr().cast::<libc::c_char>(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(errno());
        }
        buf.truncate(n as usize);
        if buf.len() > 4096 {
            return Err(io::Error::other("a link target that is too long"));
        }
        String::from_utf8(buf).map_err(|_| io::Error::other("a link target that is not text"))
    }

    /// `unlinkat` of a file or a link (never a folder).
    pub fn unlink(&self, name: &str) -> io::Result<()> {
        let c = name_c(name)?;
        // SAFETY: as in `open_at`.
        if unsafe { libc::unlinkat(self.raw(), c.as_ptr(), 0) } != 0 {
            return Err(errno());
        }
        Ok(())
    }

    /// `unlinkat` of an empty folder.
    pub fn rmdir(&self, name: &str) -> io::Result<()> {
        let c = name_c(name)?;
        // SAFETY: as in `open_at`.
        if unsafe { libc::unlinkat(self.raw(), c.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(errno());
        }
        Ok(())
    }

    /// The names in this folder.
    pub fn list(&self) -> io::Result<Vec<OsString>> {
        // A new open file description of the same folder, so the position of
        // this handle is not shared with a listing in progress elsewhere.
        let dot = CString::new(".").expect("no NUL");
        let fd = self.open_at(&dot, libc::O_RDONLY | libc::O_DIRECTORY)?;
        // SAFETY: `fd` is open and ownership passes to the DIR on success.
        let dp = unsafe { libc::fdopendir(fd) };
        if dp.is_null() {
            let e = errno();
            // SAFETY: fdopendir failed, so `fd` is still ours to close.
            unsafe { libc::close(fd) };
            return Err(e);
        }
        let mut out = Vec::new();
        loop {
            // SAFETY: errno is per thread; reset so the end can be told from an error.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: `dp` is an open DIR.
            let ent = unsafe { libc::readdir(dp) };
            if ent.is_null() {
                let e = errno();
                // SAFETY: closing the DIR opened above.
                unsafe { libc::closedir(dp) };
                return if e.raw_os_error().unwrap_or(0) == 0 {
                    Ok(out)
                } else {
                    Err(e)
                };
            }
            // SAFETY: readdir returned a valid entry whose name is NUL-terminated.
            let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                out.push(OsStr::from_bytes(name).to_os_string());
            }
        }
    }

    /// The names in this folder that are text (an app ID, a version).
    pub fn list_text(&self) -> io::Result<Vec<String>> {
        Ok(self
            .list()?
            .into_iter()
            .filter_map(|n| n.into_string().ok())
            .collect())
    }

    /// Removes `name` and everything below it. A link anywhere is removed, not
    /// followed: every level is opened `O_NOFOLLOW` from the one above, so a
    /// folder swapped for a link in the middle of the walk is unlinked, and
    /// what it pointed at is not touched.
    pub fn remove_all(&self, name: &str) -> io::Result<()> {
        let c = name_c(name)?;
        remove_entry(self, &c, 0)
    }

    /// Writes `bytes` to a new file `name` through a temporary name, synced,
    /// then `renameat` over `name` (a link at `name` is replaced, not
    /// followed).
    pub fn write_atomic(&self, name: &str, bytes: &[u8], mode: u32) -> io::Result<()> {
        let tmp = temp_name(name);
        let result = (|| {
            let mut f = self.create_new(&tmp, mode)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            drop(f);
            self.rename(&tmp, self, name)
        })();
        if result.is_err() {
            let _ = self.unlink(&tmp);
        }
        result
    }

    /// Puts `bytes` at `name` only if what is there is `expect`, and says what
    /// was there. The check is of the very object that is replaced: the new
    /// file is made under a temporary name and swapped in with
    /// `RENAME_EXCHANGE` (or `RENAME_NOREPLACE` for an empty place), so the old
    /// file ends up under the temporary name where it is read and hashed
    /// through the descriptor; if it is not what was expected the swap is
    /// undone. On a file system without those flags the file is read first and
    /// then renamed into place, which leaves a short window (narrower). If a
    /// writer puts a file at `name` while the swap is undone, that file is
    /// kept as `<name>.orig-<pid>` and [`ReplaceError::Displaced`] says so;
    /// nothing is unlinked that the Store did not make. Returns the old
    /// content (at most `max_old` bytes).
    pub fn replace_checked(
        &self,
        name: &str,
        bytes: &[u8],
        mode: u32,
        expect: Expect<'_>,
        max_old: u64,
    ) -> Result<Option<Vec<u8>>, ReplaceError> {
        let tmp = temp_name(name);
        let io = ReplaceError::Io;
        let mut f = self.create_new(&tmp, mode).map_err(io)?;
        let ours = f.metadata().map(|m| ident_of(&m));
        let wrote = f.write_all(bytes).and_then(|()| f.sync_all());
        drop(f);
        let ours = match (wrote, ours) {
            (Ok(()), Ok(ours)) => ours,
            (Err(e), _) | (_, Err(e)) => {
                let _ = self.unlink(&tmp);
                return Err(ReplaceError::Io(e));
            }
        };
        let outcome = match expect {
            Expect::Absent => match self.rename_noreplace(&tmp, self, name) {
                Ok(()) => Ok(None),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    Err(ReplaceError::Refused(Refused::Changed))
                }
                Err(e) => Err(ReplaceError::Io(e)),
            },
            Expect::Sha(want) => self.swap_and_verify(&tmp, name, want, max_old, ours),
        };
        // These two leave the temporary name alone: someone's file is there.
        if outcome.is_err()
            && !matches!(
                outcome,
                Err(ReplaceError::Stranded) | Err(ReplaceError::Displaced(_))
            )
        {
            let _ = self.unlink(&tmp);
        }
        outcome
    }

    fn swap_and_verify(
        &self,
        tmp: &str,
        name: &str,
        want: &str,
        max_old: u64,
        ours: (u64, u64),
    ) -> Result<Option<Vec<u8>>, ReplaceError> {
        match self.exchange(tmp, name) {
            Ok(()) => {
                // `tmp` is what was at `name`.
                let verdict = self
                    .open_read(tmp)
                    .and_then(|mut f| hash_reader(&mut f, max_old, true));
                let reason = match verdict {
                    Ok((sha, old)) if sha == want => {
                        let _ = self.unlink(tmp);
                        return Ok(Some(old));
                    }
                    Ok(_) => None,
                    Err(e) if is_not_ours(&e) => None,
                    Err(e) => Some(e),
                };
                #[cfg(feature = "test-hooks")]
                test_hooks::ON_SWAP.with(|h| {
                    if let Some(f) = h.borrow().as_ref() {
                        f()
                    }
                });
                // Not what was expected: put it back.
                if self.exchange(tmp, name).is_err() {
                    return Err(ReplaceError::Stranded);
                }
                // What is at `tmp` now should be the file made here. If a
                // writer put its own file at `name` meanwhile, that is what
                // came back: keep it where it can be seen.
                match self.stat(tmp) {
                    Ok(m) if m.ident() == ours => {
                        let _ = self.unlink(tmp);
                    }
                    Ok(_) => return Err(ReplaceError::Displaced(self.keep_visible(tmp, name)?)),
                    Err(_) => {}
                }
                match reason {
                    Some(e) => Err(ReplaceError::Io(e)),
                    None => Err(ReplaceError::Refused(Refused::Changed)),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(ReplaceError::Refused(Refused::Gone))
            }
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                // No exchange on this file system: check, then replace.
                let (sha, old) = match self
                    .open_read(name)
                    .and_then(|mut f| hash_reader(&mut f, max_old, true))
                {
                    Ok(v) => v,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(ReplaceError::Refused(Refused::Gone));
                    }
                    Err(e) if is_not_ours(&e) => {
                        return Err(ReplaceError::Refused(Refused::Changed));
                    }
                    Err(e) => return Err(ReplaceError::Io(e)),
                };
                if sha != want {
                    return Err(ReplaceError::Refused(Refused::Changed));
                }
                self.rename(tmp, self, name).map_err(ReplaceError::Io)?;
                Ok(Some(old))
            }
            Err(e) => Err(ReplaceError::Io(e)),
        }
    }

    /// Gives the entry `tmp` a name that shows (`<name>.orig-<pid>`, then
    /// `-2`...) and returns it.
    fn keep_visible(&self, tmp: &str, name: &str) -> Result<String, ReplaceError> {
        for n in 0..100 {
            let shown = if n == 0 {
                format!("{name}.orig-{}", std::process::id())
            } else {
                format!("{name}.orig-{}-{n}", std::process::id())
            };
            match self.rename_noreplace(tmp, self, &shown) {
                Ok(()) => return Ok(shown),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => break,
            }
        }
        Err(ReplaceError::Stranded)
    }

    /// Removes the regular file `name` only if it has the SHA-256 `want`. The
    /// file is first moved to a temporary name (so nothing can be swapped in
    /// between), read and hashed there through the descriptor, and unlinked;
    /// if it is not what was expected it is moved back, or, if something took
    /// its place meanwhile, kept as `<name>.orig-<pid>`. An error of the file
    /// system is [`RemoveError::Io`], not "changed".
    pub fn remove_checked(&self, name: &str, want: &str) -> Result<(), RemoveError> {
        let tmp = temp_name(name);
        match self.rename_noreplace(name, self, &tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(RemoveError::Refused(Refused::Gone));
            }
            Err(e) => return Err(RemoveError::Io(e)),
        }
        let verdict = self
            .open_read(&tmp)
            .and_then(|mut f| hash_reader(&mut f, u64::MAX, false));
        match verdict {
            Ok((sha, _)) if sha == want => {
                let _ = self.unlink(&tmp);
                Ok(())
            }
            other => {
                let reason = match other {
                    Err(e) if !is_not_ours(&e) => Some(e),
                    _ => None,
                };
                match self.rename_noreplace(&tmp, self, name) {
                    Ok(()) => {}
                    Err(_) => {
                        let _ = self.keep_visible(&tmp, name);
                    }
                }
                match reason {
                    Some(e) => Err(RemoveError::Io(e)),
                    None => Err(RemoveError::Refused(Refused::Changed)),
                }
            }
        }
    }

    /// Removes the temporary files an interrupted [`Dir::write_atomic`],
    /// [`Dir::replace_checked`] or [`Dir::remove_checked`] left for `base`:
    /// exactly `.<base>.<digits>.<digits>.tmp`, and only a regular file of
    /// this user. (The caller holds the install lock, so none is in use.)
    pub fn sweep_temps(&self, base: &str) {
        let prefix = format!(".{base}.");
        let Ok(names) = self.list_text() else { return };
        for name in names {
            let Some(mid) = name
                .strip_prefix(&prefix)
                .and_then(|r| r.strip_suffix(".tmp"))
            else {
                continue;
            };
            let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            if mid
                .split_once('.')
                .is_some_and(|(a, b)| digits(a) && digits(b))
                && self
                    .stat(&name)
                    .is_ok_and(|m| m.kind == Kind::File && m.uid == crate::appimage::fsutil::euid())
            {
                let _ = self.unlink(&name);
            }
        }
    }

    /// Removes the links an interrupted switch of a link left:
    /// `<prefix><digits>-<digits>`, only links of this user.
    pub fn sweep_link_temps(&self, prefix: &str) {
        let Ok(names) = self.list_text() else { return };
        for name in names {
            let Some(rest) = name.strip_prefix(prefix) else {
                continue;
            };
            let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            if rest
                .split_once('-')
                .is_some_and(|(a, b)| digits(a) && digits(b))
                && self
                    .stat(&name)
                    .is_ok_and(|m| m.kind == Kind::Link && m.uid == crate::appimage::fsutil::euid())
            {
                let _ = self.unlink(&name);
            }
        }
    }

    /// SHA-256 and size of the regular file `name`.
    pub fn sha256(&self, name: &str) -> io::Result<(String, u64)> {
        let mut f = self.open_read(name)?;
        let size = f.metadata()?.len();
        let (sha, _) = hash_reader(&mut f, u64::MAX, false)?;
        Ok((sha, size))
    }
}

/// Why [`Dir::remove_checked`] did not remove.
#[derive(Debug)]
pub enum RemoveError {
    Refused(Refused),
    /// The file system failed (not "the file is something else").
    Io(io::Error),
}

/// Whether an error means "this is not what the Store wrote" (a link, a
/// folder, a FIFO, a file that is too big) and not a failure of the file
/// system.
fn is_not_ours(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::InvalidData
        || matches!(
            e.raw_os_error(),
            Some(libc::ELOOP | libc::ENXIO | libc::EISDIR | libc::ENOTDIR | libc::ENOENT)
        )
}

/// `renameat2` flags the file system does not know.
fn unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
    )
}

fn ident_of(m: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

/// Why [`Dir::replace_checked`] did not replace.
#[derive(Debug)]
pub enum ReplaceError {
    Refused(Refused),
    Io(io::Error),
    /// A writer put a file at the name while a swap was undone; it was kept
    /// under this visible name next to it.
    Displaced(String),
    /// A swap could not be undone: the file that was in the place is under a
    /// hidden temporary name next to it, and is left there.
    Stranded,
}

fn remove_entry(parent: &Dir, name: &CStr, depth: usize) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::other("folders nested too deeply"));
    }
    // Files and links first: `unlinkat` never follows.
    // SAFETY: `name` is NUL-terminated and outlives the call.
    if unsafe { libc::unlinkat(parent.raw(), name.as_ptr(), 0) } == 0 {
        return Ok(());
    }
    let e = errno();
    match e.raw_os_error() {
        Some(libc::ENOENT) => return Ok(()),
        Some(libc::EISDIR) | Some(libc::EPERM) => {}
        _ => return Err(e),
    }
    // A folder: open it without following (it may have just become a link,
    // in which case the next round unlinks the link).
    let fd = match parent.open_at(name, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW) {
        Ok(fd) => fd,
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTDIR) | Some(libc::ELOOP)) => {
            // SAFETY: as above.
            if unsafe { libc::unlinkat(parent.raw(), name.as_ptr(), 0) } == 0 {
                return Ok(());
            }
            return Err(errno());
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let dir = Dir::from_fd(fd);
    for child in dir.list()? {
        let c = CString::new(child.into_vec())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        remove_entry(&dir, &c, depth + 1)?;
    }
    // SAFETY: as above.
    if unsafe { libc::unlinkat(parent.raw(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        let e = errno();
        if e.raw_os_error() != Some(libc::ENOENT) {
            return Err(e);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("telamon-dirfd-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_link_is_not_followed_below_the_root() {
        let root = scratch("nofollow");
        let outside = scratch("nofollow-outside");
        std::fs::write(outside.join("secret"), b"s").unwrap();
        symlink(&outside, root.join("l")).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        let d = Dir::open_following(&root).unwrap();
        assert!(d.sub("l").is_err());
        assert!(d.sub("real").is_ok());
        // The following variant is for the export roots only.
        assert!(d.sub_following("l").is_ok());
        // Neither a read nor a create goes through a link.
        symlink(outside.join("secret"), root.join("f")).unwrap();
        assert!(d.read_file("f", 100).is_err());
        assert!(d.create_new("f", 0o600).is_err());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"s");
        // Names are single components.
        for bad in ["", ".", "..", "a/b", "/etc"] {
            assert!(d.stat(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn remove_all_unlinks_links_and_never_follows_them() {
        let root = scratch("rm");
        let outside = scratch("rm-outside");
        std::fs::write(outside.join("keep"), b"k").unwrap();
        std::fs::create_dir_all(root.join("t/a/b")).unwrap();
        std::fs::write(root.join("t/a/b/f"), b"x").unwrap();
        symlink(&outside, root.join("t/a/link")).unwrap();
        symlink(&outside, root.join("t/top")).unwrap();
        let d = Dir::open_following(&root).unwrap();
        d.remove_all("t").unwrap();
        assert!(!root.join("t").exists());
        assert_eq!(std::fs::read(outside.join("keep")).unwrap(), b"k");
        // A top-level link is removed as a link.
        symlink(&outside, root.join("direct")).unwrap();
        d.remove_all("direct").unwrap();
        assert!(!root.join("direct").exists() && outside.join("keep").exists());
        // Missing is fine.
        d.remove_all("nothing").unwrap();
    }

    #[test]
    fn replace_checked_replaces_only_what_it_expects() {
        let root = scratch("replace");
        let d = Dir::open_following(&root).unwrap();
        // Absent: made; present: refused.
        assert!(
            d.replace_checked("f", b"one", 0o644, Expect::Absent, 100)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            d.replace_checked("f", b"two", 0o644, Expect::Absent, 100),
            Err(ReplaceError::Refused(Refused::Changed))
        ));
        let one = hex(&Sha256::digest(b"one"));
        assert_eq!(
            d.replace_checked("f", b"two", 0o644, Expect::Sha(&one), 100)
                .unwrap()
                .as_deref(),
            Some(&b"one"[..])
        );
        assert_eq!(std::fs::read(root.join("f")).unwrap(), b"two");
        // A changed file is put back as it was, no temporary is left.
        assert!(matches!(
            d.replace_checked("f", b"three", 0o644, Expect::Sha(&one), 100),
            Err(ReplaceError::Refused(Refused::Changed))
        ));
        assert_eq!(std::fs::read(root.join("f")).unwrap(), b"two");
        assert!(matches!(
            d.replace_checked("g", b"x", 0o644, Expect::Sha(&one), 100),
            Err(ReplaceError::Refused(Refused::Gone))
        ));
        // A link in the place is not a file: refused, and left alone.
        symlink("f", root.join("l")).unwrap();
        assert!(
            d.replace_checked("l", b"x", 0o644, Expect::Sha(&one), 100)
                .is_err()
        );
        assert!(
            std::fs::symlink_metadata(root.join("l"))
                .unwrap()
                .is_symlink()
        );
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
    }

    fn refused(r: Result<(), RemoveError>) -> Option<Refused> {
        match r {
            Ok(()) => None,
            Err(RemoveError::Refused(r)) => Some(r),
            Err(RemoveError::Io(e)) => panic!("an I/O error: {e}"),
        }
    }

    #[test]
    fn remove_checked_removes_only_the_expected_file() {
        let root = scratch("rmchecked");
        let d = Dir::open_following(&root).unwrap();
        std::fs::write(root.join("f"), b"mine").unwrap();
        let sha = hex(&Sha256::digest(b"mine"));
        assert_eq!(
            refused(d.remove_checked("f", &"0".repeat(64))),
            Some(Refused::Changed)
        );
        assert_eq!(std::fs::read(root.join("f")).unwrap(), b"mine");
        d.remove_checked("f", &sha).unwrap();
        assert_eq!(refused(d.remove_checked("f", &sha)), Some(Refused::Gone));
        symlink("/etc/hostname", root.join("l")).unwrap();
        assert_eq!(refused(d.remove_checked("l", &sha)), Some(Refused::Changed));
        assert!(
            std::fs::symlink_metadata(root.join("l"))
                .unwrap()
                .is_symlink()
        );
        // A folder and a FIFO are "something else", not an error of the system.
        std::fs::create_dir(root.join("dir")).unwrap();
        assert_eq!(
            refused(d.remove_checked("dir", &sha)),
            Some(Refused::Changed)
        );
        assert!(root.join("dir").is_dir());
        let fifo = std::ffi::CString::new(root.join("fifo").as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(
            refused(d.remove_checked("fifo", &sha)),
            Some(Refused::Changed)
        );
        assert!(root.join("fifo").exists());
    }

    #[test]
    fn a_failure_of_the_file_system_is_not_called_a_change() {
        for (e, ours) in [
            (io::Error::from_raw_os_error(libc::EIO), false),
            (io::Error::from_raw_os_error(libc::EACCES), false),
            (io::Error::from_raw_os_error(libc::ENOSPC), false),
            (io::Error::from_raw_os_error(libc::ELOOP), true),
            (io::Error::from_raw_os_error(libc::ENXIO), true),
            (io::Error::from_raw_os_error(libc::EISDIR), true),
            (
                io::Error::new(io::ErrorKind::InvalidData, "too large"),
                true,
            ),
        ] {
            assert_eq!(is_not_ours(&e), ours, "{e}");
        }
        // Not a regular file is `InvalidData`, which is "something else".
        let root = scratch("classify");
        let d = Dir::open_following(&root).unwrap();
        std::fs::create_dir(root.join("dir")).unwrap();
        assert!(is_not_ours(&d.open_read("dir").unwrap_err()));
    }

    #[test]
    fn without_renameat2_flags_the_same_things_happen_as_far_as_the_system_allows() {
        test_hooks::NO_RENAME_FLAGS.with(|h| h.set(true));
        let root = scratch("noflags");
        let d = Dir::open_following(&root).unwrap();
        let sha = |b: &[u8]| hex(&Sha256::digest(b));
        // NOREPLACE: a file is linked into place, a folder is renamed, neither over something.
        std::fs::write(root.join("a"), b"a").unwrap();
        std::fs::write(root.join("b"), b"b").unwrap();
        assert_eq!(
            d.rename_noreplace("a", &d, "b").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(root.join("a")).unwrap(), b"a");
        assert_eq!(std::fs::read(root.join("b")).unwrap(), b"b");
        d.rename_noreplace("a", &d, "c").unwrap();
        assert!(!root.join("a").exists());
        assert_eq!(std::fs::read(root.join("c")).unwrap(), b"a");
        std::fs::create_dir(root.join("d1")).unwrap();
        std::fs::create_dir(root.join("d2")).unwrap();
        assert_eq!(
            d.rename_noreplace("d1", &d, "d2").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        d.rename_noreplace("d1", &d, "d3").unwrap();
        assert!(root.join("d3").is_dir() && !root.join("d1").exists());
        // A link is moved as a link.
        symlink("c", root.join("l1")).unwrap();
        d.rename_noreplace("l1", &d, "l2").unwrap();
        assert_eq!(std::fs::read_link(root.join("l2")).unwrap(), Path::new("c"));
        // replace_checked: absent, then exchange-less swap with a check.
        assert!(
            d.replace_checked("n", b"one", 0o644, Expect::Absent, 100)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            d.replace_checked("n", b"x", 0o644, Expect::Absent, 100),
            Err(ReplaceError::Refused(Refused::Changed))
        ));
        assert_eq!(
            d.replace_checked("n", b"two", 0o644, Expect::Sha(&sha(b"one")), 100)
                .unwrap()
                .as_deref(),
            Some(&b"one"[..])
        );
        assert!(matches!(
            d.replace_checked("n", b"three", 0o644, Expect::Sha(&sha(b"one")), 100),
            Err(ReplaceError::Refused(Refused::Changed))
        ));
        assert!(matches!(
            d.replace_checked("missing", b"x", 0o644, Expect::Sha(&sha(b"one")), 100),
            Err(ReplaceError::Refused(Refused::Gone))
        ));
        symlink("c", root.join("lk")).unwrap();
        assert!(
            d.replace_checked("lk", b"x", 0o644, Expect::Sha(&sha(b"a")), 100)
                .is_err()
        );
        assert!(
            std::fs::symlink_metadata(root.join("lk"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(std::fs::read(root.join("n")).unwrap(), b"two");
        // remove_checked (moves aside, then NOREPLACE back if needed).
        assert_eq!(
            refused(d.remove_checked("n", &"0".repeat(64))),
            Some(Refused::Changed)
        );
        assert_eq!(std::fs::read(root.join("n")).unwrap(), b"two");
        d.remove_checked("n", &sha(b"two")).unwrap();
        assert!(!root.join("n").exists());
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
        test_hooks::NO_RENAME_FLAGS.with(|h| h.set(false));
    }

    #[test]
    fn a_file_a_writer_puts_there_while_a_swap_is_undone_is_kept_in_sight() {
        let root = scratch("displaced");
        let d = Dir::open_following(&root).unwrap();
        std::fs::write(root.join("f.desktop"), b"user edit").unwrap();
        let r = root.clone();
        test_hooks::ON_SWAP.with(|h| {
            *h.borrow_mut() = Some(Box::new(move || {
                // An editor saves its file over the name, as editors do.
                std::fs::write(r.join(".editor"), b"editor save").unwrap();
                std::fs::rename(r.join(".editor"), r.join("f.desktop")).unwrap();
            }))
        });
        let want = hex(&Sha256::digest(b"what the Store wrote"));
        let r = d.replace_checked("f.desktop", b"new", 0o644, Expect::Sha(&want), 100);
        test_hooks::ON_SWAP.with(|h| *h.borrow_mut() = None);
        let Err(ReplaceError::Displaced(kept)) = r else {
            panic!("{r:?}");
        };
        assert_eq!(kept, format!("f.desktop.orig-{}", std::process::id()));
        // The editor's file is there to see; the file the Store moved aside is back.
        assert_eq!(std::fs::read(root.join(&kept)).unwrap(), b"editor save");
        assert_eq!(std::fs::read(root.join("f.desktop")).unwrap(), b"user edit");
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
        // Without a writer the Store's own file is unlinked, as before.
        std::fs::remove_file(root.join(&kept)).unwrap();
        assert!(matches!(
            d.replace_checked("f.desktop", b"new", 0o644, Expect::Sha(&want), 100),
            Err(ReplaceError::Refused(Refused::Changed))
        ));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn only_exact_temporary_names_of_this_user_are_swept() {
        let root = scratch("sweep");
        let d = Dir::open_following(&root).unwrap();
        for ok in [".x.desktop.123.4.tmp", ".x.desktop.1.0.tmp"] {
            std::fs::write(root.join(ok), b"left").unwrap();
        }
        let keep_files = [
            ".x.desktop.123.tmp",
            ".x.desktop.a.4.tmp",
            ".x.desktop.1.2.3.tmp",
            ".x.desktop.1.2.tmp.bak",
            ".y.desktop.1.2.tmp",
            "x.desktop.1.2.tmp",
            ".x.desktop..2.tmp",
            "x.desktop.orig-1",
        ];
        for k in keep_files {
            std::fs::write(root.join(k), b"keep").unwrap();
        }
        // A link and a folder with the exact pattern are not files.
        symlink("x", root.join(".x.desktop.5.6.tmp")).unwrap();
        std::fs::create_dir(root.join(".x.desktop.7.8.tmp")).unwrap();
        d.sweep_temps("x.desktop");
        assert!(!root.join(".x.desktop.123.4.tmp").exists());
        assert!(!root.join(".x.desktop.1.0.tmp").exists());
        for k in keep_files {
            assert!(root.join(k).exists(), "{k}");
        }
        assert!(std::fs::symlink_metadata(root.join(".x.desktop.5.6.tmp")).is_ok());
        assert!(root.join(".x.desktop.7.8.tmp").is_dir());
        // Links of an interrupted switch.
        symlink("1.0", root.join(".current.12-3")).unwrap();
        symlink("1.0", root.join(".current.x-3")).unwrap();
        std::fs::write(root.join(".current.9-9"), b"file").unwrap();
        d.sweep_link_temps(".current.");
        assert!(std::fs::symlink_metadata(root.join(".current.12-3")).is_err());
        assert!(std::fs::symlink_metadata(root.join(".current.x-3")).is_ok());
        assert!(root.join(".current.9-9").exists());
    }

    #[test]
    fn listing_and_renames() {
        let root = scratch("list");
        let d = Dir::open_following(&root).unwrap();
        d.create_dir("a", 0o700).unwrap();
        d.create_dir("b", 0o700).unwrap();
        let mut names = d.list_text().unwrap();
        names.sort();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(
            d.create_dir("a", 0o700).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let a = d.sub("a").unwrap();
        d.rename_noreplace("b", &a, "b").unwrap();
        assert!(root.join("a/b").is_dir());
        d.create_dir("c", 0o700).unwrap();
        assert_eq!(
            d.rename_noreplace("c", &d, "a").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        // Listing twice gives the same names.
        assert_eq!(d.list().unwrap().len(), 2);
        assert_eq!(d.list().unwrap().len(), 2);
    }
}
