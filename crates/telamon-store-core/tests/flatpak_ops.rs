//! The Flatpak operations layer. The lock and token tests need nothing. The
//! rest run against the local test remote from `scripts/test-remote.sh` and
//! only when `TELAMON_STORE_TEST_REMOTE` names a built test dir; they refuse to
//! run unless every Flatpak and XDG directory (HOME too) is under /work/, so a
//! real installation is never opened. The flatpak CLI is
//! test setup only: the module under test never shells out.

use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use telamon_store_core::flatpak::lock::{
    FLATPAK_LOCK_FILE, LEGACY_FLATPAK_LOCK_FILE, LEGACY_UPDATER_LOCK_FILE, UPDATER_LOCK_FILE,
};
use telamon_store_core::flatpak::{
    CancelToken, Error, InstalledRef, LockError, LockName, OperationLock, RefKind, Scope,
    list_installed, list_installed_all, list_unused, remote_ref_info,
};
use telamon_store_core::permissions::Permissions;

mod common;
use common::*;

// ---- locks (no remote needed) ----

fn lock_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("flatpak-lock-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Holds `flock` on a file the way Telamon Updater does.
fn updater_style_lock(path: &Path) -> std::fs::File {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    f
}

#[test]
fn lock_is_taken_and_released() {
    let d = lock_dir("basic");
    let first = OperationLock::try_acquire_in(&d).unwrap();
    assert_eq!(
        OperationLock::try_acquire_in(&d).unwrap_err(),
        LockError::Busy(LockName::Updater)
    );
    drop(first);
    let _again = OperationLock::try_acquire_in(&d).unwrap();
    let mode = std::fs::metadata(d.join(UPDATER_LOCK_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn busy_says_which_lock_and_holds_nothing_back() {
    let d = lock_dir("busy");
    {
        let _updater = updater_style_lock(&d.join(UPDATER_LOCK_FILE));
        assert_eq!(
            OperationLock::try_acquire_in(&d).unwrap_err(),
            LockError::Busy(LockName::Updater)
        );
    }
    let flatpak = updater_style_lock(&d.join(FLATPAK_LOCK_FILE));
    assert_eq!(
        OperationLock::try_acquire_in(&d).unwrap_err(),
        LockError::Busy(LockName::Flatpak)
    );
    // The Updater lock taken on the way was given back.
    drop(updater_style_lock(&d.join(UPDATER_LOCK_FILE)));
    drop(flatpak);
    assert!(OperationLock::try_acquire_in(&d).is_ok());
}

#[test]
fn the_old_lock_names_are_honoured_and_held_too() {
    // An Updater (or a Store) that still has its old name holds the old file.
    let d = lock_dir("legacy");
    {
        let _updater = updater_style_lock(&d.join(LEGACY_UPDATER_LOCK_FILE));
        assert_eq!(
            OperationLock::try_acquire_in(&d).unwrap_err(),
            LockError::Busy(LockName::Updater)
        );
    }
    {
        let _flatpak = updater_style_lock(&d.join(LEGACY_FLATPAK_LOCK_FILE));
        assert_eq!(
            OperationLock::try_acquire_in(&d).unwrap_err(),
            LockError::Busy(LockName::Flatpak)
        );
    }
    // Nothing was left held by the failed tries.
    let held = OperationLock::try_acquire_in(&d).unwrap();
    // The new Store holds both names of both locks, so an old Updater, which
    // only knows its own old name, is kept out as well.
    for name in [
        UPDATER_LOCK_FILE,
        LEGACY_UPDATER_LOCK_FILE,
        FLATPAK_LOCK_FILE,
        LEGACY_FLATPAK_LOCK_FILE,
    ] {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(d.join(name))
            .unwrap();
        let rc = unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&f),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        assert_eq!(rc, -1, "{name} is held");
    }
    drop(held);
    assert!(OperationLock::try_acquire_in(&d).is_ok());
}

#[test]
fn acquire_times_out_as_busy_and_gets_the_lock_when_freed() {
    let d = lock_dir("timeout");
    let held = updater_style_lock(&d.join(UPDATER_LOCK_FILE));
    let cancel = CancelToken::new();
    let t = Instant::now();
    let e = OperationLock::acquire_in(&d, Duration::from_millis(300), &cancel).unwrap_err();
    assert_eq!(e, LockError::Busy(LockName::Updater));
    assert!(t.elapsed() >= Duration::from_millis(300) && t.elapsed() < Duration::from_secs(3));

    let d2 = d.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(held);
        d2
    });
    let got = OperationLock::acquire_in(&d, Duration::from_secs(5), &cancel);
    releaser.join().unwrap();
    assert!(got.is_ok());
}

#[test]
fn acquire_stops_when_cancelled() {
    let d = lock_dir("cancel");
    let _held = updater_style_lock(&d.join(UPDATER_LOCK_FILE));
    let cancel = CancelToken::new();
    let c2 = cancel.clone();
    let t = Instant::now();
    let h = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c2.cancel();
    });
    let e = OperationLock::acquire_in(&d, Duration::from_secs(30), &cancel).unwrap_err();
    h.join().unwrap();
    assert_eq!(e, LockError::Cancelled);
    assert!(t.elapsed() < Duration::from_secs(3));
    // Already cancelled: no waiting at all, even when the lock is free.
    let free = lock_dir("cancel-free");
    assert_eq!(
        OperationLock::acquire_in(&free, Duration::from_secs(1), &cancel).unwrap_err(),
        LockError::Cancelled
    );
}

#[test]
fn a_symlink_lock_path_is_refused_and_not_followed() {
    for name in [UPDATER_LOCK_FILE, FLATPAK_LOCK_FILE] {
        let d = lock_dir(&format!("symlink-{name}"));
        let target = d.join("target");
        std::fs::write(&target, b"keep").unwrap();
        symlink(&target, d.join(name)).unwrap();
        let e = OperationLock::try_acquire_in(&d).unwrap_err();
        assert!(matches!(e, LockError::Unsafe(_)), "{e:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        // A refused second lock gives the first one back.
        if name == FLATPAK_LOCK_FILE {
            drop(updater_style_lock(&d.join(UPDATER_LOCK_FILE)));
        }
    }
}

#[test]
fn a_lock_path_that_is_not_a_regular_file_is_refused() {
    let d = lock_dir("fifo");
    let path = std::ffi::CString::new(d.join(UPDATER_LOCK_FILE).to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let e = OperationLock::try_acquire_in(&d).unwrap_err();
    assert!(matches!(e, LockError::Unsafe(_)), "{e:?}");

    let d = lock_dir("dir-as-lock");
    std::fs::create_dir(d.join(FLATPAK_LOCK_FILE)).unwrap();
    // Opening a directory read-write fails: refused either way.
    assert!(OperationLock::try_acquire_in(&d).is_err());
}

#[test]
fn a_lock_file_of_another_user_is_refused() {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("skipped: needs root to chown a lock file to another user");
        return;
    }
    let d = lock_dir("other-owner");
    let p = d.join(UPDATER_LOCK_FILE);
    std::fs::write(&p, b"").unwrap();
    let c = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::chown(c.as_ptr(), 12345, 12345) }, 0);
    let e = OperationLock::try_acquire_in(&d).unwrap_err();
    assert!(matches!(e, LockError::Unsafe(_)), "{e:?}");
}

#[test]
fn a_bad_lock_directory_is_an_error_never_tmp() {
    let d = lock_dir("baddir");
    let missing = d.join("missing");
    assert!(matches!(
        OperationLock::try_acquire_in(&missing),
        Err(LockError::NoRuntimeDir(_))
    ));
    assert!(matches!(
        OperationLock::try_acquire_in(Path::new("relative/dir")),
        Err(LockError::NoRuntimeDir(_))
    ));
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        OperationLock::try_acquire_in(&d),
        Err(LockError::NoRuntimeDir(_))
    ));
    let file = d.join("a-file");
    std::fs::write(&file, b"").unwrap();
    assert!(matches!(
        OperationLock::try_acquire_in(&file),
        Err(LockError::NoRuntimeDir(_))
    ));
}

#[test]
fn lock_errors_read_in_plain_words() {
    let s = LockError::Busy(LockName::Updater).to_string();
    assert!(s.contains("Telamon Updater"), "{s}");
    let s = LockError::Busy(LockName::Flatpak).to_string();
    assert!(s.contains("Flatpak operation"), "{s}");
}

// ---- token (no remote needed) ----

#[test]
fn a_cancelled_token_gives_cancelled_before_any_flatpak_call() {
    let c = CancelToken::new();
    c.cancel();
    assert_eq!(list_unused(Scope::User, &c).unwrap_err(), Error::Cancelled);
    assert_eq!(
        remote_ref_info(Scope::User, "test", "app/org.test.Hello/x86_64/stable", &c).unwrap_err(),
        Error::Cancelled
    );
    let out = list_installed(Scope::User, &c);
    assert!(out.refs.is_empty());
    assert!(out.cancelled && out.errors.is_empty(), "{:?}", out.errors);
    let all = list_installed_all(&c);
    assert!(all.cancelled && all.refs.is_empty());
}

#[test]
fn remote_ref_info_checks_its_input() {
    let c = CancelToken::new();
    for (remote, r) in [
        ("test", "app/org.test.Hello"),
        ("te st", "app/org.test.Hello/x86_64/stable"),
        ("test", "app/../x86_64/stable"),
        ("", "app/org.test.Hello/x86_64/stable"),
    ] {
        let e = remote_ref_info(Scope::User, remote, r, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
    }
}

// ---- against the local test remote ----

fn find<'a>(refs: &'a [InstalledRef], id: &str) -> &'a InstalledRef {
    refs.iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("{id} not listed: {refs:#?}"))
}

#[test]
fn lists_app_runtime_and_addon() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let out = list_installed(Scope::User, &c);
    assert!(out.errors.is_empty(), "{:?}", out.errors);

    let app = find(&out.refs, APP);
    assert_eq!(app.kind, RefKind::App);
    assert_eq!(app.scope, Scope::User);
    assert_eq!(app.origin, "test");
    assert_eq!(app.branch, "stable");
    assert_eq!(app.full_ref(), format!("app/{APP}/{}/stable", app.arch));
    assert!(app.is_current);
    assert!(app.installed_size > 0);
    assert_eq!(app.commit.len(), 64);
    assert_eq!(app.name, "Hello");
    assert_eq!(app.summary, "Says hello, for the Telamon Store's tests");
    assert_eq!(app.version, "1.0");
    assert_eq!(app.related_to, None);
    assert_eq!(app.eol, None);

    let rt = find(&out.refs, RUNTIME);
    assert_eq!(rt.kind, RefKind::Runtime);
    assert!(!rt.is_current);

    let addon = find(&out.refs, ADDON);
    assert_eq!(addon.kind, RefKind::Runtime);
    assert_eq!(addon.related_to.as_deref(), Some(APP));
    assert_eq!(rt.related_to, None);

    // Both scopes: the empty system installation adds nothing and hides nothing.
    let all = list_installed_all(&c);
    assert!(
        all.refs.iter().all(|r| r.scope == Scope::User),
        "{:?}",
        all.refs
    );
    assert_eq!(find(&all.refs, APP).commit, app.commit);
}

#[test]
fn metadata_feeds_permissions() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let out = list_installed(Scope::User, &c);
    let app = find(&out.refs, APP);
    let bytes = app.metadata(&c).unwrap();
    assert!(bytes.starts_with(b"[Application]"));
    let perms = Permissions::from_metadata(&bytes).unwrap();
    let codes: Vec<String> = perms
        .permissions()
        .iter()
        .map(|p| p.code().to_string())
        .collect();
    assert!(codes.iter().any(|c| c == "socket:wayland"), "{codes:?}");
    assert!(!codes.iter().any(|c| c == "share:network"), "{codes:?}");

    let gone = CancelToken::new();
    gone.cancel();
    assert_eq!(app.metadata(&gone).unwrap_err(), Error::Cancelled);
}

#[test]
fn remote_ref_info_reads_the_app() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let arch = libflatpak::default_arch().unwrap();
    let r = format!("app/{APP}/{arch}/stable");
    let info = remote_ref_info(Scope::User, "test", &r, &c).unwrap();
    assert_eq!(info.kind, RefKind::App);
    assert_eq!(info.id, APP);
    assert_eq!(info.remote, "test");
    assert_eq!(info.commit.len(), 64);
    assert!(info.installed_size > 0);
    assert!(info.download_size > 0);
    assert!(info.metadata.len() <= 1 << 20);
    let perms = Permissions::from_metadata(&info.metadata).unwrap();
    assert!(
        perms
            .permissions()
            .iter()
            .any(|p| p.code() == "socket:wayland")
    );

    let missing = format!("app/org.test.Missing/{arch}/stable");
    assert!(matches!(
        remote_ref_info(Scope::User, "test", &missing, &c).unwrap_err(),
        Error::Flatpak { .. }
    ));
    assert!(matches!(
        remote_ref_info(Scope::User, "nosuchremote", &r, &c).unwrap_err(),
        Error::Flatpak { .. }
    ));
    // Nothing was installed by asking.
    assert!(list_installed(Scope::User, &c).refs.is_empty());
}

#[test]
fn unused_runtime_is_listed_after_the_app_is_removed() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    assert!(list_unused(Scope::User, &c).unwrap().is_empty());
    must(&["uninstall", "-y", "--noninteractive", APP]);
    let unused = list_unused(Scope::User, &c).unwrap();
    let ids: Vec<&str> = unused.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&RUNTIME), "{ids:?}");
    assert!(!ids.contains(&APP));
    assert!(unused.iter().all(|r| r.scope == Scope::User));
}

#[test]
fn reads_never_interact_and_changes_may() {
    let Some((_dir, _g)) = remote() else { return };
    use libflatpak::prelude::*;
    assert!(
        telamon_store_core::flatpak::open(Scope::User)
            .unwrap()
            .is_no_interaction()
    );
}

/// Puts the test remote's repo back when dropped.
struct RepoAway(PathBuf, PathBuf);

impl RepoAway {
    fn new(dir: &Path) -> RepoAway {
        let (from, to) = (dir.join("repo"), dir.join("repo.gone"));
        std::fs::rename(&from, &to).expect("move the repo away");
        RepoAway(from, to)
    }
}

impl Drop for RepoAway {
    fn drop(&mut self) {
        let _ = std::fs::rename(&self.1, &self.0);
    }
}

#[test]
fn remote_ref_info_offline_is_an_error_not_a_hang() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    // Forget any cached summary, so the answer must come from the remote.
    let cache = PathBuf::from(std::env::var("FLATPAK_USER_DIR").unwrap()).join("repo/tmp/cache");
    let _ = std::fs::remove_dir_all(cache);
    let _away = RepoAway::new(&dir);
    let c = CancelToken::new();
    let _dog = c.watchdog(Duration::from_secs(30));
    let t = Instant::now();
    let r = format!("app/{APP}/{}/stable", libflatpak::default_arch().unwrap());
    let e = remote_ref_info(Scope::User, "test", &r, &c);
    assert!(t.elapsed() < Duration::from_secs(30), "it hung");
    match e {
        Err(Error::Flatpak { message, .. }) => {
            assert!(!message.is_empty() && !message.contains('\n'), "{message}")
        }
        other => panic!("{other:?}"),
    }
}
