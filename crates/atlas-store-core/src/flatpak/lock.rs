//! The two locks one Flatpak operation holds, in `$XDG_RUNTIME_DIR` (private
//! to the user): first Atlas Updater's `atlas-updater-apps.lock`, so the
//! Updater and the Store never change installations together, then
//! `atlas-flatpak.lock`, so two Flatpak operations never overlap. The first
//! is taken the way Atlas Updater's `atlas-updater-base` does (same open
//! flags, mode 0600 and `flock`), so the two interoperate.
//!
//! Atlas Updater only ever takes `atlas-updater-apps.lock` (its
//! `atlas-updater-base/src/lock.rs`), so taking the Updater's lock first and ours
//! second cannot deadlock against it.
//!
//! A lock file must be a regular file owned by us, opened without following a
//! symlink; anything else is refused, never replaced. With no usable
//! `XDG_RUNTIME_DIR` there is no lock and an error: never `/tmp`.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::CancelToken;

/// Atlas Updater's lock for Flatpak work.
pub const UPDATER_LOCK_FILE: &str = "atlas-updater-apps.lock";
/// The Flatpak operations lock.
pub const FLATPAK_LOCK_FILE: &str = "atlas-flatpak.lock";

/// Longest wait between two tries in [`OperationLock::acquire`].
const MAX_BACKOFF: Duration = Duration::from_millis(250);
/// Cancel is looked at at least this often while waiting.
const CANCEL_SLICE: Duration = Duration::from_millis(25);

/// Which lock is meant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockName {
    /// `atlas-updater-apps.lock`: Atlas Updater is working on apps.
    Updater,
    /// `atlas-flatpak.lock`: another Flatpak operation is running.
    Flatpak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockError {
    /// This lock is held by someone else (and, from `acquire`, stayed so
    /// until the timeout).
    Busy(LockName),
    /// The caller's token was cancelled while waiting.
    Cancelled,
    /// `XDG_RUNTIME_DIR` is unset, not absolute, or not a private directory
    /// of ours.
    NoRuntimeDir(String),
    /// The lock file is not a regular file of ours (a symlink, a pipe,
    /// another user's file).
    Unsafe(&'static str),
    /// The lock file could not be opened or locked.
    Io(String),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Busy(LockName::Updater) => f.write_str(
                "Atlas Updater is working on apps right now. Try again when it is done.",
            ),
            LockError::Busy(LockName::Flatpak) => {
                f.write_str("Another Flatpak operation is running. Try again when it is done.")
            }
            LockError::Cancelled => f.write_str("The operation was cancelled."),
            LockError::NoRuntimeDir(why) => {
                write!(f, "There is no private runtime folder for the lock: {why}.")
            }
            LockError::Unsafe(why) => write!(f, "The lock file is not safe to use: {why}."),
            LockError::Io(e) => write!(f, "Could not take the lock: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

/// Both locks, held until dropped. The Flatpak lock is released first, then
/// the Updater's (fields drop in order); the process ending releases both.
#[derive(Debug)]
pub struct OperationLock {
    _flatpak: File,
    _updater: File,
}

impl OperationLock {
    /// Takes both locks without waiting: `Busy` says which one is held. The
    /// first is released again if the second is busy.
    ///
    /// Blocking only for the syscalls: run on a worker thread anyway.
    pub fn try_acquire() -> Result<OperationLock, LockError> {
        OperationLock::try_in(&runtime_dir()?)
    }

    /// [`try_acquire`](Self::try_acquire) with the locks in `dir`, which must
    /// be a private directory of ours.
    ///
    /// Test-only: production code uses `try_acquire`/`acquire`, which share
    /// the Updater's runtime dir.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn try_acquire_in(dir: &Path) -> Result<OperationLock, LockError> {
        OperationLock::try_in(dir)
    }

    fn try_in(dir: &Path) -> Result<OperationLock, LockError> {
        check_dir(dir)?;
        let updater = take(&dir.join(UPDATER_LOCK_FILE), LockName::Updater)?;
        let flatpak = take(&dir.join(FLATPAK_LOCK_FILE), LockName::Flatpak)?;
        Ok(OperationLock {
            _flatpak: flatpak,
            _updater: updater,
        })
    }

    /// Tries until both locks are taken, with a growing pause (up to 250 ms)
    /// between tries. `Busy` after `timeout`, `Cancelled` when `cancel` is
    /// cancelled (looked at every 25 ms or less).
    ///
    /// Blocking: run on a worker thread.
    pub fn acquire(timeout: Duration, cancel: &CancelToken) -> Result<OperationLock, LockError> {
        OperationLock::acquire_dir(&runtime_dir()?, timeout, cancel)
    }

    /// [`acquire`](Self::acquire) with the locks in `dir`.
    ///
    /// Test-only: production code uses `try_acquire`/`acquire`, which share
    /// the Updater's runtime dir.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn acquire_in(
        dir: &Path,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<OperationLock, LockError> {
        OperationLock::acquire_dir(dir, timeout, cancel)
    }

    fn acquire_dir(
        dir: &Path,
        timeout: Duration,
        cancel: &CancelToken,
    ) -> Result<OperationLock, LockError> {
        // An overflowing timeout means no deadline: wait until free or cancelled.
        let deadline = Instant::now().checked_add(timeout);
        let mut pause = Duration::from_millis(10);
        loop {
            if cancel.is_cancelled() {
                return Err(LockError::Cancelled);
            }
            match OperationLock::try_in(dir) {
                Err(LockError::Busy(which)) => {
                    let now = Instant::now();
                    let mut left = match deadline {
                        Some(d) if now >= d => return Err(LockError::Busy(which)),
                        Some(d) => pause.min(d - now),
                        None => pause,
                    };
                    while !left.is_zero() {
                        if cancel.is_cancelled() {
                            return Err(LockError::Cancelled);
                        }
                        let step = left.min(CANCEL_SLICE);
                        std::thread::sleep(step);
                        left -= step;
                    }
                    pause = (pause * 2).min(MAX_BACKOFF);
                }
                other => return other,
            }
        }
    }
}

fn euid() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { libc::geteuid() }
}

fn runtime_dir() -> Result<PathBuf, LockError> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| LockError::NoRuntimeDir("XDG_RUNTIME_DIR is not set".into()))?;
    if !dir.is_absolute() {
        return Err(LockError::NoRuntimeDir(
            "XDG_RUNTIME_DIR is not an absolute path".into(),
        ));
    }
    Ok(dir)
}

/// The directory must be a real directory of ours that others can't write to.
fn check_dir(dir: &Path) -> Result<(), LockError> {
    if !dir.is_absolute() {
        return Err(LockError::NoRuntimeDir("it is not an absolute path".into()));
    }
    let md = std::fs::metadata(dir).map_err(|e| LockError::NoRuntimeDir(io_kind(&e)))?;
    if !md.is_dir() {
        return Err(LockError::NoRuntimeDir("it is not a folder".into()));
    }
    if md.uid() != euid() {
        return Err(LockError::NoRuntimeDir("it belongs to another user".into()));
    }
    if md.mode() & 0o022 != 0 {
        return Err(LockError::NoRuntimeDir("others can write to it".into()));
    }
    Ok(())
}

/// A short, path-free description of an I/O error.
fn io_kind(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(_) => {
            // The OS text ("No such file or directory (os error 2)") without
            // the number.
            let s = e.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].to_string(),
                None => s,
            }
        }
        None => e.kind().to_string(),
    }
}

/// Opens `path` as Atlas Updater does, checks it, and takes `flock` without
/// waiting.
fn take(path: &Path, name: LockName) -> Result<File, LockError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) => LockError::Unsafe("it is a symbolic link"),
            _ => LockError::Io(io_kind(&e)),
        })?;
    let md = file.metadata().map_err(|e| LockError::Io(io_kind(&e)))?;
    if !md.is_file() {
        return Err(LockError::Unsafe("it is not a regular file"));
    }
    if md.uid() != euid() {
        return Err(LockError::Unsafe("it belongs to another user"));
    }
    loop {
        // SAFETY: flock on a file descriptor we own; no memory is passed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let e = io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) => return Err(LockError::Busy(name)),
            _ => return Err(LockError::Io(io_kind(&e))),
        }
    }
}
