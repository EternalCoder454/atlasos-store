//! Shared by the flatpak integration tests: the guard that keeps them inside
//! /work/, and set-up helpers that use the flatpak CLI (test setup only; the
//! module under test never shells out).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

use atlas_store_core::flatpak::{CancelToken, InstalledRef, Scope, list_installed};

pub const APP: &str = "org.test.Hello";
pub const ADDON: &str = "org.test.Hello.Plugin.Extra";
pub const RUNTIME: &str = "org.test.Platform";
pub const RTEXT: &str = "org.test.Platform.Ext";

static SERIAL: Mutex<()> = Mutex::new(());

pub fn under_work(var: &str) {
    let v = std::env::var(var).unwrap_or_else(|_| panic!("refusing to run: {var} is not set"));
    let p = std::fs::canonicalize(&v)
        .unwrap_or_else(|_| panic!("refusing to run: {var}={v} does not exist"));
    assert!(
        p.starts_with("/work/"),
        "refusing to run: {var}={v} is not under /work/"
    );
}

pub fn remote() -> Option<(PathBuf, MutexGuard<'static, ()>)> {
    let Some(dir) = std::env::var_os("ATLAS_STORE_TEST_REMOTE") else {
        eprintln!(
            "skipped: ATLAS_STORE_TEST_REMOTE does not name a built test remote (scripts/test-remote.sh build)"
        );
        return None;
    };
    let dir = std::fs::canonicalize(&dir).expect("ATLAS_STORE_TEST_REMOTE does not exist");
    assert!(
        dir.starts_with("/work/"),
        "refusing to run: the test remote is not under /work/"
    );
    assert!(
        dir.join("key.gpg").is_file(),
        "{} is not a built test remote",
        dir.display()
    );
    for var in [
        "FLATPAK_USER_DIR",
        "FLATPAK_SYSTEM_DIR",
        "FLATPAK_TRIGGERSDIR",
        "FLATPAK_CONFIG_DIR",
        "FLATPAK_RUN_DIR",
        "HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        under_work(var);
    }
    Some((dir, SERIAL.lock().unwrap_or_else(|e| e.into_inner())))
}

pub fn flatpak(args: &[&str]) -> bool {
    Command::new("flatpak")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The same for the system installation (`FLATPAK_SYSTEM_DIR`, which as root
/// in the dev container libflatpak writes directly, without the helper).
pub fn flatpak_system(args: &[&str]) -> bool {
    Command::new("flatpak")
        .arg("--system")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn must_system(args: &[&str]) {
    assert!(flatpak_system(args), "flatpak --system {args:?} failed");
}

/// Empties the system installation and removes its `test` remote when
/// dropped, so a failed test leaves nothing behind for the next one.
pub struct SystemCleanup;

impl Drop for SystemCleanup {
    fn drop(&mut self) {
        flatpak_system(&["uninstall", "-y", "--noninteractive", "--all"]);
        clear_pins_system();
        flatpak_system(&["remote-delete", "--force", "test"]);
    }
}

/// Removes the user remote `name` when dropped.
pub struct RemoteCleanup(pub &'static str);

impl Drop for RemoteCleanup {
    fn drop(&mut self) {
        flatpak(&["remote-delete", "--force", self.0]);
    }
}

pub fn must(args: &[&str]) {
    assert!(flatpak(args), "flatpak {args:?} failed");
}

/// Pins outlive `uninstall --all` and keep runtimes from being "unused".
pub fn clear_pins() {
    clear_pins_in("--user");
}

/// The same for the system installation.
pub fn clear_pins_system() {
    clear_pins_in("--system");
}

fn clear_pins_in(scope: &str) {
    let out = Command::new("flatpak")
        .args([scope, "pin"])
        .stdin(Stdio::null())
        .output()
        .expect("flatpak runs");
    for p in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        Command::new("flatpak")
            .args([scope, "pin", "--remove", p])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .ok();
    }
}

pub fn reset(dir: &Path) {
    flatpak(&["uninstall", "-y", "--noninteractive", "--all"]);
    clear_pins();
    flatpak(&["remote-delete", "--force", "test"]);
    let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
    let url = format!("file://{}/repo/", dir.display());
    must(&["remote-add", &gpg, "test", &url]);
}

pub fn with_app(dir: &Path) {
    reset(dir);
    must(&["install", "-y", "--noninteractive", "test", &app_ref()]);
    must(&["install", "-y", "--noninteractive", "test", ADDON]);
}

pub fn app_ref() -> String {
    format!("app/{APP}/{}/stable", libflatpak::default_arch().unwrap())
}

pub fn runtime_ref() -> String {
    format!(
        "runtime/{RUNTIME}/{}/stable",
        libflatpak::default_arch().unwrap()
    )
}

pub fn installed() -> Vec<InstalledRef> {
    let out = list_installed(Scope::User, &CancelToken::new());
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    out.refs
}

pub fn ids(refs: &[InstalledRef]) -> Vec<&str> {
    refs.iter().map(|r| r.id.as_str()).collect()
}

/// Rebuilds the test remote at 1.0 when dropped (after `bump`).
pub struct Rebuild(pub PathBuf);

impl Drop for Rebuild {
    fn drop(&mut self) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/test-remote.sh");
        let ok = Command::new("bash")
            .arg(script)
            .arg("build")
            .arg(&self.0)
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            ok || std::thread::panicking(),
            "rebuilding the test remote failed"
        );
    }
}

pub fn bump(dir: &Path) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/test-remote.sh");
    let ok = Command::new("bash")
        .arg(script)
        .arg("bump")
        .arg(dir)
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "bump failed");
}
