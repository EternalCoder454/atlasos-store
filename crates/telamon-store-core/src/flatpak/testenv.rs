//! Test support for unit tests that run against the local test remote: the
//! same guard as `tests/flatpak_ops.rs` (skip unless `TELAMON_STORE_TEST_REMOTE`
//! names a built test dir; refuse unless every Flatpak and XDG directory is
//! under /work/).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn under_work(var: &str) {
    let v = std::env::var(var).unwrap_or_else(|_| panic!("refusing to run: {var} is not set"));
    let p = std::fs::canonicalize(&v)
        .unwrap_or_else(|_| panic!("refusing to run: {var}={v} does not exist"));
    assert!(
        p.starts_with("/work/"),
        "refusing to run: {var}={v} is not under /work/"
    );
}

pub(crate) fn guard() -> Option<(PathBuf, MutexGuard<'static, ()>)> {
    let Some(dir) = std::env::var_os("TELAMON_STORE_TEST_REMOTE") else {
        eprintln!("skipped: TELAMON_STORE_TEST_REMOTE does not name a built test remote");
        return None;
    };
    let dir = std::fs::canonicalize(&dir).expect("TELAMON_STORE_TEST_REMOTE does not exist");
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
        "HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "FLATPAK_RUN_DIR",
    ] {
        under_work(var);
    }
    Some((dir, SERIAL.lock().unwrap_or_else(|e| e.into_inner())))
}

pub(crate) fn flatpak(args: &[&str]) -> bool {
    Command::new("flatpak")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(crate) fn must(args: &[&str]) {
    assert!(flatpak(args), "flatpak {args:?} failed");
}

/// Pins outlive `uninstall --all` and keep runtimes from being "unused".
fn clear_pins() {
    let out = Command::new("flatpak")
        .args(["--user", "pin"])
        .stdin(Stdio::null())
        .output()
        .expect("flatpak runs");
    for p in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        flatpak(&["pin", "--remove", p]);
    }
}

/// An empty user installation, no remotes.
pub(crate) fn reset_empty() {
    flatpak(&["uninstall", "-y", "--noninteractive", "--all"]);
    clear_pins();
    let out = Command::new("flatpak")
        .args(["--user", "remotes", "--show-disabled", "--columns=name"])
        .stdin(Stdio::null())
        .output()
        .expect("flatpak runs");
    for r in String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        flatpak(&["remote-delete", "--force", r]);
    }
}

/// An empty user installation with the test remote `test`.
pub(crate) fn reset(dir: &std::path::Path) {
    reset_empty();
    let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
    let url = format!("file://{}/repo/", dir.display());
    must(&["remote-add", &gpg, "test", &url]);
}

/// The `GPGKey=` line of a file the test remote wrote.
pub(crate) fn gpg_line(dir: &std::path::Path, file: &str) -> String {
    let t = std::fs::read_to_string(dir.join(file)).unwrap();
    t.lines()
        .find(|l| l.starts_with("GPGKey="))
        .unwrap()
        .to_string()
}

/// A fresh scratch folder under the (guard-checked) XDG_CACHE_HOME, or None
/// (skip) when that is not under /work/. Never /tmp.
pub(crate) fn scratch(name: &str) -> Option<PathBuf> {
    let Some(base) = std::env::var_os("XDG_CACHE_HOME") else {
        eprintln!("skipped: XDG_CACHE_HOME is not set (scratch folders live under /work/)");
        return None;
    };
    let Ok(base) = std::fs::canonicalize(base) else {
        eprintln!("skipped: XDG_CACHE_HOME does not exist");
        return None;
    };
    if !base.starts_with("/work/") {
        eprintln!("skipped: XDG_CACHE_HOME is not under /work/");
        return None;
    }
    let d = base
        .join("flatpak-scratch")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch folder");
    Some(std::fs::canonicalize(d).expect("scratch folder"))
}

/// Drops the file-permission overrides (CAP_DAC_OVERRIDE and
/// CAP_DAC_READ_SEARCH) from the calling thread, so a root test sees real
/// EACCES. Capabilities are per thread: nothing else is affected. False when
/// it could not be done (the test then skips).
pub(crate) fn drop_dac_caps() -> bool {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    // SAFETY: euid has no arguments.
    if unsafe { libc::geteuid() } != 0 {
        return true;
    }
    let mut head = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let mut data = [Data::default(); 2];
    // SAFETY: capget/capset read and write the two structs above, which have
    // the kernel's v3 layout.
    unsafe {
        if libc::syscall(
            libc::SYS_capget,
            &mut head as *mut Header,
            data.as_mut_ptr(),
        ) != 0
        {
            return false;
        }
        data[0].effective &= !((1 << 1) | (1 << 2));
        data[0].permitted &= !((1 << 1) | (1 << 2));
        head.pid = 0;
        libc::syscall(libc::SYS_capset, &mut head as *mut Header, data.as_ptr()) == 0
    }
}
