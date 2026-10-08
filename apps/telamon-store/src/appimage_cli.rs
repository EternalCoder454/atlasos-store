//! The two options `telamon-store` handles before Qt starts (`cpp/main.cpp`
//! calls `telamon_store_early` first thing): no window, no single-instance
//! service, and the process ends when it is done.
//!
//! - `--appimage-inspect <file>`: the helper that looks inside an AppImage
//!   under resource limits and prints what it found (see
//!   `telamon_store_core::appimage::helper`). Started by the Store itself.
//! - `--appimage-check <folder or file>`: what the `telamon-store-appimage`
//!   systemd user path unit runs when the Downloads folder changes. Finds
//!   AppImages that just arrived, tells the user once with a notification
//!   (Install, Not Now, Show in Store), starts the Store when asked, and
//!   exits. Nothing stays running; the wait for an answer is 60 s at most.

use std::ffi::{CStr, c_char, c_int};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use telamon_store_core::appimage::check::{
    self, Answer, Config, Launcher, Notice, Notifier, Reply,
};
use telamon_store_core::appimage::helper;
use telamon_store_core::appimage::inspect::{InspectError, Inspection};
use telamon_store_core::appimage::state::SeenState;
use telamon_store_core::flatpak::valid_activation_token;
use telamon_store_core::launch::internal_path;
use telamon_updater_core::notify::{DEFAULT_ACTION, Note, Notifier as DesktopNotifier, escape};
use zbus::message::Type as MessageType;

/// How long the notification waits for an answer.
const ANSWER_WAIT: Duration = Duration::from_secs(60);
/// The whole check ends after this, whatever it is doing (SIGALRM).
const CHECK_DEADLINE_SECONDS: u32 = 5 * 60;
/// The notification event in `telamon-store.notifyrc`.
const EVENT: &str = "appimageFound";

/// Called first by `main`. `*handled` is true when the arguments were one of
/// the two options above (the return value is then the exit code); false
/// means a normal launch.
///
/// # Safety
/// `argv` must point to `argc` valid NUL-terminated strings (as `main`'s
/// own do), and `handled` to a writable `c_int`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn telamon_store_early(
    argc: c_int,
    argv: *const *const c_char,
    handled: *mut c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { *handled = 0 };
    if argc < 2 || argv.is_null() {
        return 0;
    }
    let args: Vec<String> = (1..argc as isize)
        // SAFETY: `argv[i]` is valid for i < argc by the contract above.
        .map(|i| {
            unsafe { CStr::from_ptr(*argv.offset(i)) }
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    match early(&args) {
        Some(code) => {
            // SAFETY: the caller's contract.
            unsafe { *handled = 1 };
            code
        }
        None => 0,
    }
}

/// `Some(exit code)` for the two options, `None` for anything else.
pub fn early(args: &[String]) -> Option<i32> {
    if let Some(path) = internal_path("--appimage-inspect", args) {
        return Some(match path {
            Ok(p) => helper::child_main(&p),
            Err(reason) => {
                eprintln!("telamon-store: --appimage-inspect: {reason}");
                2
            }
        });
    }
    if let Some(path) = internal_path("--appimage-check", args) {
        return Some(match path {
            Ok(p) => check_main(&p),
            Err(reason) => {
                eprintln!("telamon-store: --appimage-check: {reason}");
                2
            }
        });
    }
    None
}

fn check_main(target: &Path) -> i32 {
    // No core files, and an end to everything after a few minutes.
    helper::limit_core();
    // SAFETY: alarm has no memory effects; SIGALRM's default action ends the process.
    unsafe {
        libc::alarm(CHECK_DEADLINE_SECONDS);
    }
    let Some(state_path) = SeenState::default_path() else {
        eprintln!("telamon-store: there is no state folder to keep the list of announced files in");
        return 1;
    };
    let mut state = match SeenState::load(&state_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("telamon-store: not checking: {e}");
            return 1;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("telamon-store: {e}");
            return 1;
        }
    };
    let report = check::run(
        target,
        &Config::default(),
        &mut state,
        &HelperInspector { exe: exe.clone() },
        &mut DbusNotifier::new(),
        &mut SystemdLauncher { exe },
    );
    for e in &report.errors {
        eprintln!("telamon-store: {e}");
    }
    eprintln!(
        "telamon-store: {} announced, {} opened, {} skipped",
        report.notified.len(),
        report.launched.len(),
        report.skipped
    );
    0
}

/// Looks inside a file through the helper process.
struct HelperInspector {
    exe: PathBuf,
}

impl check::Inspector for HelperInspector {
    fn inspect(&self, path: &Path) -> Result<Inspection, InspectError> {
        helper::run(&self.exe, path, helper::TIMEOUT)
    }
}

/// Sends the notification over D-Bus and waits for the user's answer.
struct DbusNotifier {
    notifier: DesktopNotifier,
}

impl DbusNotifier {
    fn new() -> DbusNotifier {
        DbusNotifier {
            notifier: DesktopNotifier::new(&crate::telamon_framework_ui_app_info()),
        }
    }
}

impl Notifier for DbusNotifier {
    fn notify(&mut self, n: &Notice) -> Result<Reply, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(send_and_wait(&self.notifier, n, ANSWER_WAIT))
    }
}

/// The note for a notice: the body is markup to the server, so it is escaped;
/// the title is plain text by the specification.
fn note_for(n: &Notice) -> Note {
    let mut note = Note::new(EVENT, n.title.clone(), escape(&n.body));
    note.actions = vec![
        ("install", "Install".to_string()),
        ("later", "Not Now".to_string()),
        ("show", "Show in Store".to_string()),
        (DEFAULT_ACTION, "Show in Store".to_string()),
    ];
    note
}

fn answer_for(key: &str) -> Answer {
    match key {
        "install" => Answer::Install,
        "show" | DEFAULT_ACTION => Answer::ShowInStore,
        "later" => Answer::NotNow,
        _ => Answer::None,
    }
}

async fn send_and_wait(
    notifier: &DesktopNotifier,
    n: &Notice,
    wait: Duration,
) -> Result<Reply, String> {
    let none = Reply {
        answer: Answer::None,
        token: None,
    };
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| format!("no session bus: {e}"))?;
    // Listen before sending, so a fast click is not missed.
    let rule = zbus::MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface("org.freedesktop.Notifications")
        .map_err(|e| e.to_string())?
        .path("/org/freedesktop/Notifications")
        .map_err(|e| e.to_string())?
        .build();
    let mut signals = zbus::MessageStream::for_match_rule(rule, &conn, Some(16))
        .await
        .map_err(|e| e.to_string())?;
    let sent = match notifier.send(&conn, &note_for(n)).await {
        Ok(Some(s)) => s,
        // The user turned this notification off.
        Ok(None) => return Ok(none),
        Err(e) => return Err(e.to_string()),
    };
    let deadline = Instant::now() + wait;
    let mut token = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(next) = tokio::time::timeout(left, signals.next()).await else {
            return Ok(Reply {
                answer: Answer::None,
                token,
            });
        };
        let Some(Ok(msg)) = next else {
            return Ok(Reply {
                answer: Answer::None,
                token,
            });
        };
        let header = msg.header();
        if sent
            .server
            .as_deref()
            .is_some_and(|s| header.sender().map(|h| h.as_str()) != Some(s))
        {
            continue;
        }
        let Some(member) = header.member().map(|m| m.as_str().to_string()) else {
            continue;
        };
        let body = msg.body();
        match member.as_str() {
            "ActivationToken" => {
                if let Ok((id, t)) = body.deserialize::<(u32, String)>()
                    && id == sent.id
                {
                    token = valid_activation_token(&t).map(str::to_string);
                }
            }
            "ActionInvoked" => {
                if let Ok((id, key)) = body.deserialize::<(u32, String)>()
                    && id == sent.id
                {
                    return Ok(Reply {
                        answer: answer_for(&key),
                        token,
                    });
                }
            }
            "NotificationClosed" => {
                if let Ok((id, _reason)) = body.deserialize::<(u32, u32)>()
                    && id == sent.id
                {
                    return Ok(Reply {
                        answer: Answer::None,
                        token,
                    });
                }
            }
            _ => {}
        }
    }
}

/// Starts the Store outside this process's own cgroup: the check is a
/// oneshot service and systemd ends everything in it when it exits.
struct SystemdLauncher {
    exe: PathBuf,
}

impl Launcher for SystemdLauncher {
    fn open_install(&mut self, path: &Path, token: Option<&str>) -> Result<(), String> {
        let mut r = [0u8; 4];
        // SAFETY: fills the buffer; a failure only makes the unit name less random.
        let _ = unsafe { libc::getrandom(r.as_mut_ptr().cast(), r.len(), 0) };
        let unit = format!("telamon-store-appimage-{:08x}", u32::from_le_bytes(r));
        let mut cmd = Command::new("systemd-run");
        cmd.args([
            "--user",
            "--quiet",
            "--collect",
            "--no-block",
            "--unit",
            &unit,
            "--description=Telamon Store",
        ]);
        if let Some(t) = token.and_then(valid_activation_token) {
            cmd.arg(format!("--setenv=XDG_ACTIVATION_TOKEN={t}"))
                .arg(format!("--setenv=DESKTOP_STARTUP_ID={t}"));
        }
        for var in [
            "WAYLAND_DISPLAY",
            "DISPLAY",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_TYPE",
        ] {
            if std::env::var_os(var).is_some() {
                cmd.arg(format!("--setenv={var}"));
            }
        }
        // `--` first: the path is never read as an option. The Store reads
        // the path again with the launch rules.
        cmd.arg("--")
            .arg(&self.exe)
            .arg("--appimage-install")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0);
        let out = cmd
            .output()
            .map_err(|e| format!("systemd-run could not start ({})", e.kind()))?;
        if out.status.success() {
            Ok(())
        } else {
            let why = telamon_store_core::text::clean(&String::from_utf8_lossy(&out.stderr), 200);
            Err(format!("systemd-run failed ({}): {why}", out.status))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_launches_are_left_alone() {
        for args in [
            vec![],
            vec!["--app".to_string(), "org.x.Y".to_string()],
            vec!["/home/u/a.AppImage".to_string()],
        ] {
            assert_eq!(early(&args), None, "{args:?}");
        }
    }

    #[test]
    fn a_bad_path_ends_with_an_error_and_no_window() {
        assert_eq!(
            early(&["--appimage-check".to_string(), "relative".to_string()]),
            Some(2)
        );
        assert_eq!(early(&["--appimage-inspect".to_string()]), Some(2));
        assert_eq!(early(&["--appimage-check=/a/../b".to_string()]), Some(2));
    }

    #[test]
    fn the_note_escapes_the_body_and_offers_the_three_answers() {
        let n = Notice {
            title: "Install A & B?".into(),
            body: "<b>x</b> is an AppImage.".into(),
            path: PathBuf::from("/d/x.AppImage"),
        };
        let note = note_for(&n);
        assert_eq!(note.event, "appimageFound");
        assert_eq!(note.title, "Install A & B?");
        assert_eq!(note.text, "&lt;b&gt;x&lt;/b&gt; is an AppImage.");
        let keys: Vec<&str> = note.actions.iter().map(|a| a.0).collect();
        assert_eq!(keys, ["install", "later", "show", "default"]);
        assert_eq!(answer_for("install"), Answer::Install);
        assert_eq!(answer_for("show"), Answer::ShowInStore);
        assert_eq!(answer_for("default"), Answer::ShowInStore);
        assert_eq!(answer_for("later"), Answer::NotNow);
        assert_eq!(answer_for("whatever"), Answer::None);
    }
}
