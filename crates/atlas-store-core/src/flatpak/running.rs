//! Apps that are running: whether one is, how to close it, and how to start
//! one with the window system's activation token.
//!
//! Everything here blocks (it polls, sleeps and spawns): call it from a worker
//! thread, never from the GUI thread. Nothing here asks for privilege: signals
//! go to processes of the same user, and the app is started by the same
//! `flatpak run` the user would type.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use libflatpak::prelude::*;

use super::{CancelToken, Error, Scope};
use crate::text;

/// How long an app gets to leave after SIGTERM before SIGKILL.
const TERM_WAIT: Duration = Duration::from_secs(3);
/// How long it gets after SIGKILL before the close counts as failed.
const KILL_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(100);
/// How long a started app may take to fail before the open counts as done.
const EARLY_EXIT_WAIT: Duration = Duration::from_millis(600);
/// Longest activation token accepted. Real ones are a few dozen characters.
const TOKEN_MAX: usize = 256;

/// What the Store needs to know about one libflatpak instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Running {
    pub app: Option<String>,
    pub running: bool,
    /// The outermost sandbox process (bubblewrap's babysitter).
    pub pid: i32,
    /// The first process inside the sandbox, as the host sees it.
    pub child_pid: i32,
}

fn instances() -> Vec<Running> {
    libflatpak::Instance::all()
        .iter()
        .map(|i| Running {
            app: i.app().map(|a| a.to_string()),
            running: i.is_running(),
            pid: i.pid(),
            child_pid: i.child_pid(),
        })
        .collect()
}

/// Whether a running instance of exactly this app ID is in `list`.
pub(crate) fn is_running_in(list: &[Running], id: &str) -> bool {
    list.iter()
        .any(|r| r.running && r.app.as_deref() == Some(id))
}

/// The processes to signal to close `id`: the pid and child pid of each
/// running instance of exactly that ID. The info files live in the user's own
/// runtime directory, so a number is not trusted: 0 and 1 (and negative ones,
/// which would address whole process groups), and the Store itself, are left
/// out. Each pid is listed once.
pub(crate) fn pids_of(list: &[Running], id: &str, own: i32) -> Vec<i32> {
    let mut out: Vec<i32> = Vec::new();
    for r in list
        .iter()
        .filter(|r| r.running && r.app.as_deref() == Some(id))
    {
        for p in [r.pid, r.child_pid] {
            if p > 1 && p != own && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Whether an instance of `id` is running now.
pub(crate) fn app_running(id: &str) -> bool {
    is_running_in(&instances(), id)
}

fn signal(pid: i32, sig: libc::c_int) {
    // SAFETY: `kill` only takes two integers. `pids_of` kept the pid above 1,
    // so it names one process and never a group or init. A pid that has gone
    // away is an ESRCH error, which is fine.
    let _ = unsafe { libc::kill(pid, sig) };
}

/// Waits until no instance of `id` runs, up to `limit`. `Ok(false)` when it
/// still does.
fn wait_stopped(id: &str, limit: Duration, cancel: &CancelToken) -> Result<bool, Error> {
    let start = Instant::now();
    loop {
        cancel.check()?;
        if !app_running(id) {
            return Ok(true);
        }
        if start.elapsed() >= limit {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    }
}

/// Closes every running instance of the app `id`: SIGTERM, up to 3 seconds
/// for it to leave, then SIGKILL (to whatever still runs by then, read again,
/// so a pid that was reused meanwhile is not hit). The app gets no chance to
/// save: the caller asked the user first. Nothing running is `Ok`.
///
/// Blocking: run on a worker thread.
pub fn close_app(id: &str, cancel: &CancelToken) -> Result<(), Error> {
    if !text::valid_id(id) {
        return Err(Error::Invalid("the app ID is not valid".into()));
    }
    let own = i32::try_from(std::process::id()).unwrap_or(0);
    let first = pids_of(&instances(), id, own);
    if first.is_empty() && !app_running(id) {
        return Ok(());
    }
    for p in &first {
        signal(*p, libc::SIGTERM);
    }
    if wait_stopped(id, TERM_WAIT, cancel)? {
        return Ok(());
    }
    for p in pids_of(&instances(), id, own) {
        signal(p, libc::SIGKILL);
    }
    if wait_stopped(id, KILL_WAIT, cancel)? {
        return Ok(());
    }
    Err(Error::Flatpak {
        action: "close the app",
        message: "it did not stop".into(),
    })
}

/// An activation token as the window system hands it out, or `None` when it
/// is empty, too long or has anything but visible ASCII (the token goes into
/// a child's environment, so it must not carry a NUL or a line break).
pub fn valid_activation_token(token: &str) -> Option<&str> {
    (!token.is_empty() && token.len() <= TOKEN_MAX && token.bytes().all(|b| b.is_ascii_graphic()))
        .then_some(token)
}

/// The arguments of `flatpak run` for one app in one installation. Every part
/// is checked, so none can be read as an option.
pub(crate) fn run_args(
    scope: Scope,
    id: &str,
    arch: &str,
    branch: &str,
) -> Result<Vec<String>, Error> {
    if !text::valid_id(id) || !super::valid_arch(arch) || !super::valid_branch(branch) {
        return Err(Error::Invalid("the app's ref is not valid".into()));
    }
    let installation = match scope {
        Scope::System => "--system",
        Scope::User => "--user",
    };
    Ok(vec![
        "run".into(),
        installation.into(),
        format!("--arch={arch}"),
        format!("--branch={branch}"),
        id.into(),
    ])
}

/// Starts an installed app and returns once it has started; it is not waited
/// for. It runs as `flatpak run` in a process group of its own, so quitting
/// the Store does not end it, with no input and its output dropped.
///
/// libflatpak's own `launch` cannot be used: it takes no environment, and the
/// Wayland compositor only brings a new window forward when the process that
/// opens it carries an activation token (`XDG_ACTIVATION_TOKEN`, with
/// `DESKTOP_STARTUP_ID` for X11 apps). Without one the window can open behind
/// the Store, or unfocused. With `token` `None` nothing is added.
///
/// An app that stops with an error inside the first moments is reported; one
/// that is still running (or has handed over to a running instance and left
/// cleanly) is a success.
///
/// Blocking: run on a worker thread.
pub fn launch_app(
    scope: Scope,
    id: &str,
    arch: &str,
    branch: &str,
    token: Option<&str>,
    cancel: &CancelToken,
) -> Result<(), Error> {
    cancel.check()?;
    let args = run_args(scope, id, arch, branch)?;
    let mut cmd = Command::new("flatpak");
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(t) = token.and_then(valid_activation_token) {
        cmd.env("XDG_ACTIVATION_TOKEN", t)
            .env("DESKTOP_STARTUP_ID", t);
    } else {
        // Ours must not leak a stale one to the app.
        cmd.env_remove("XDG_ACTIVATION_TOKEN")
            .env_remove("DESKTOP_STARTUP_ID");
    }
    let mut child = cmd.spawn().map_err(|e| Error::Flatpak {
        action: "open the app",
        message: format!("`flatpak run` could not start ({})", e.kind()),
    })?;
    let start = Instant::now();
    let early = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < EARLY_EXIT_WAIT => std::thread::sleep(POLL / 4),
            _ => break None,
        }
    };
    match early {
        Some(status) if !status.success() => Err(Error::Flatpak {
            action: "open the app",
            message: format!("it stopped right after it started ({status})"),
        }),
        Some(_) => Ok(()),
        None => {
            // Still running: something has to collect its exit status when it
            // ends, or it stays a zombie until the Store quits. The thread
            // only waits.
            let reaper = std::thread::Builder::new()
                .name("atlas-store-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                });
            if let Err(e) = reaper {
                log::warn!("could not start the thread that collects the app's exit: {e}");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(app: &str, running: bool, pid: i32, child_pid: i32) -> Running {
        Running {
            app: Some(app.into()),
            running,
            pid,
            child_pid,
        }
    }

    #[test]
    fn only_running_instances_of_the_exact_id_are_chosen() {
        let list = vec![
            inst("org.kde.konsole", true, 500, 501),
            inst("org.kde.konsole.Extra", true, 600, 601),
            inst("org.kde.konsole", false, 700, 701),
            inst("org.kde.konsole", true, 800, 801),
            Running {
                app: None,
                running: true,
                pid: 900,
                child_pid: 901,
            },
        ];
        assert_eq!(
            pids_of(&list, "org.kde.konsole", 1234),
            vec![500, 501, 800, 801]
        );
        assert!(is_running_in(&list, "org.kde.konsole"));
        assert!(!is_running_in(&list, "org.kde"));
        assert!(pids_of(&list, "org.kde.dolphin", 1234).is_empty());
    }

    #[test]
    fn a_stopped_instance_does_not_count() {
        let list = vec![inst("org.kde.konsole", false, 700, 701)];
        assert!(!is_running_in(&list, "org.kde.konsole"));
        assert!(pids_of(&list, "org.kde.konsole", 1234).is_empty());
    }

    #[test]
    fn unsafe_pids_are_never_signalled() {
        let list = vec![
            inst("org.kde.konsole", true, 0, 1),
            inst("org.kde.konsole", true, -1, -4242),
            inst("org.kde.konsole", true, 1234, 77),
            inst("org.kde.konsole", true, 77, 77),
        ];
        assert_eq!(pids_of(&list, "org.kde.konsole", 1234), vec![77]);
    }

    #[test]
    fn tokens_are_visible_ascii_of_sane_length() {
        assert_eq!(valid_activation_token("abc123_-DEF"), Some("abc123_-DEF"));
        assert_eq!(valid_activation_token(""), None);
        assert_eq!(valid_activation_token("a b"), None);
        assert_eq!(valid_activation_token("a\nb"), None);
        assert_eq!(valid_activation_token("a\0b"), None);
        assert_eq!(valid_activation_token("\u{e9}"), None);
        assert!(valid_activation_token(&"a".repeat(TOKEN_MAX)).is_some());
        assert_eq!(valid_activation_token(&"a".repeat(TOKEN_MAX + 1)), None);
    }

    #[test]
    fn run_arguments_name_the_installation_and_the_ref() {
        let user = run_args(Scope::User, "org.kde.konsole", "x86_64", "stable").unwrap();
        assert_eq!(
            user,
            [
                "run",
                "--user",
                "--arch=x86_64",
                "--branch=stable",
                "org.kde.konsole"
            ]
        );
        let sys = run_args(Scope::System, "org.kde.konsole", "aarch64", "24.08").unwrap();
        assert_eq!(sys[1], "--system");
        assert_eq!(sys[2], "--arch=aarch64");
        assert_eq!(sys[3], "--branch=24.08");
    }

    #[test]
    fn run_arguments_refuse_anything_that_could_be_an_option() {
        for (id, arch, branch) in [
            ("--help", "x86_64", "stable"),
            ("org.kde.konsole", "--user", "stable"),
            ("org.kde.konsole", "x86_64", "--system"),
            ("org.kde.konsole", "x86_64", "stable extra"),
            ("org.kde.konsole", "", "stable"),
            ("konsole", "x86_64", "stable"),
        ] {
            assert!(
                run_args(Scope::User, id, arch, branch).is_err(),
                "{id} {arch} {branch}"
            );
        }
    }
}
