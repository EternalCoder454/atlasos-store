//! Flatpak operations through libflatpak: what is installed, what a remote
//! offers, and the lock that keeps one operation at a time. These names move
//! to atlas-framework-flatpak unchanged (Framework roadmap item 40).
//!
//! # Rules for everything here
//!
//! - **Every function that talks to libflatpak or the disk blocks.** Call it
//!   from a worker thread, never from the GUI thread.
//! - **Reads never ask for privilege.** [`open`] sets libflatpak's
//!   `no_interaction` flag, which suppresses polkit and sign-in prompts, so a
//!   read cannot start one. Only the functions that change an installation
//!   (install, uninstall, add a source, update AppStream) use
//!   `open_for_change`, which allows interaction: they take an
//!   [`OperationLock`] as proof the caller holds the locks, and run after the
//!   user confirmed in the Store's own dialog (for the system installation the
//!   prompt is polkit's).
//! - **Everything libflatpak returns is untrusted text** (it comes from remote
//!   metadata): IDs are checked with [`crate::text::valid_id`], the rest is
//!   cleaned and capped, and a ref that fails is skipped and reported.
//! - **Tests rely on libflatpak itself** honouring `FLATPAK_USER_DIR` and
//!   `FLATPAK_SYSTEM_DIR`: this module never reads those variables, so a test
//!   points them at a scratch installation before calling anything here.

mod installed;
pub mod lock;
mod remote;
pub mod sources;
mod supervise;
#[cfg(test)]
mod testenv;
pub mod transaction;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use libflatpak::gio;
use libflatpak::prelude::*;

pub use installed::{
    InstalledOutcome, InstalledRef, ListError, ListErrorKind, METADATA_MAX, RefKind,
    list_installed, list_installed_all, list_unused,
};
pub use lock::{LockError, LockName, OperationLock};
pub use remote::{RemoteRefInfo, remote_ref_info};
pub use sources::{
    RefResolution, RefSource, RemoteProposal, SweepOutcome, add_ref_remote, add_remote,
    remove_remote, resolve_ref_source, sweep_pending_remotes, update_appstream,
};
pub use transaction::{
    DataResult, InstallPlan, Installed, OpKind, PlanRemote, PlannedOp, Progress, Uninstalled,
    install, plan_install, plan_install_ref, uninstall, uninstall_unused,
};

/// Cancels a running operation from another thread. Clones share one state.
/// libflatpak gets a `gio::Cancellable` that is cancelled with the token, so a
/// blocked call returns soon after [`cancel`](CancelToken::cancel).
#[derive(Clone, Debug)]
pub struct CancelToken {
    inner: Arc<TokenInner>,
}

#[derive(Debug)]
struct TokenInner {
    flag: AtomicBool,
    timed_out: AtomicBool,
    // Send + Sync in gio-rs, and cancelling is thread-safe in GIO.
    cancellable: gio::Cancellable,
}

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken {
            inner: Arc::new(TokenInner {
                flag: AtomicBool::new(false),
                timed_out: AtomicBool::new(false),
                cancellable: gio::Cancellable::new(),
            }),
        }
    }

    /// Cancels the token, and with it every libflatpak call that uses it.
    /// Safe to call more than once and from any thread.
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::SeqCst);
        self.inner.cancellable.cancel();
    }

    /// Cancels the token because nothing moved for too long (a watchdog):
    /// the operation then ends with [`Error::TimedOut`], not
    /// [`Error::Cancelled`]. A user's earlier cancel keeps its meaning.
    pub fn cancel_timed_out(&self) {
        if !self.inner.flag.swap(true, Ordering::SeqCst) {
            self.inner.timed_out.store(true, Ordering::SeqCst);
        }
        self.inner.cancellable.cancel();
    }

    /// Whether a watchdog cancelled the token.
    pub fn is_timed_out(&self) -> bool {
        self.inner.timed_out.load(Ordering::SeqCst)
    }

    /// Cancels the token as timed out after `limit`, unless the guard is
    /// dropped first: for a single blocking call that is not supervised (the
    /// calls in this module supervise themselves). If the watchdog thread
    /// cannot be started the failure is logged and there is no limit.
    pub fn watchdog(&self, limit: std::time::Duration) -> Watchdog {
        let (stop, rx) = std::sync::mpsc::channel::<()>();
        let token = self.clone();
        let handle = std::thread::Builder::new()
            .name("atlas-flatpak-watchdog".into())
            .spawn(move || {
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(limit) {
                    token.cancel_timed_out();
                }
            })
            .map_err(|e| {
                log::error!("could not start the watchdog thread (no time limit): {e}");
            })
            .ok();
        Watchdog {
            stop: Some(stop),
            handle,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::SeqCst)
    }

    /// The `gio::Cancellable` to hand to libflatpak calls.
    pub fn cancellable(&self) -> &gio::Cancellable {
        &self.inner.cancellable
    }

    /// `Err(Error::Cancelled)` when cancelled.
    pub fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Stops its timer when dropped; see [`CancelToken::watchdog`].
#[derive(Debug)]
pub struct Watchdog {
    stop: Option<std::sync::mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        CancelToken::new()
    }
}

/// Which installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The default system-wide installation.
    System,
    /// The user's installation.
    User,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Scope::System => "system",
            Scope::User => "user",
        }
    }
}

/// What differs between a confirmed plan and what libflatpak would do now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanChange {
    /// In the plan, not in the new run.
    pub missing: Vec<String>,
    /// In the new run, not in the plan.
    pub unexpected: Vec<String>,
    /// The plan's source changed or went away (in plain words).
    pub source: Option<String>,
}

impl fmt::Display for PlanChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if let Some(s) = &self.source {
            parts.push(s.clone());
        }
        if !self.missing.is_empty() {
            parts.push(format!("no longer included: {}", self.missing.join(", ")));
        }
        if !self.unexpected.is_empty() {
            parts.push(format!("newly included: {}", self.unexpected.join(", ")));
        }
        f.write_str(&parts.join("; "))
    }
}

/// Why a Flatpak operation failed, in plain words. libflatpak's text in it is
/// cleaned and capped, and URLs lose their user name and password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The caller's [`CancelToken`] was cancelled.
    Cancelled,
    /// libflatpak failed; `action` says what was being done.
    Flatpak {
        action: &'static str,
        message: String,
    },
    /// Input or a value from libflatpak did not pass its check.
    Invalid(String),
    /// A value is over its size cap.
    TooLarge(&'static str),
    /// A file could not be read.
    Io {
        action: &'static str,
        message: String,
    },
    /// The binding or libflatpak lacks what this needs.
    Unsupported(&'static str),
    /// What libflatpak would do now differs from what the user confirmed:
    /// plan again and ask again.
    PlanChanged(PlanChange),
    /// The install needs a runtime remote (a `RuntimeRepo`) that has not been
    /// confirmed; the URL is shown for a confirmation of its own.
    NeedsRuntimeRepo(String),
    /// A remote with the same URL is already added, under this name.
    RemoteExists(String),
    /// A remote with this name is already added (with another URL); it is
    /// never overwritten.
    RemoteNameTaken(String),
    /// The ref is end-of-life and replaced by another; no automatic rebase.
    EndOfLifeRebase(String),
    /// The remote asks for a sign-in, which the Store does not do.
    SignInRefused,
    /// Nothing moved for too long and the operation was stopped.
    TimedOut,
    /// More bytes arrived than the plan announced (with a margin), so the
    /// download was stopped.
    SentTooMuch,
    /// Other apps or runtimes still use this (refs listed): remove them
    /// first, or use "remove unused".
    InUse(Vec<String>),
    /// Something that had to be checked first could not be (the installed
    /// apps could not be listed or read, say), so nothing was changed. The
    /// text says what, in plain words.
    CouldNotCheck(String),
    /// The app is running: close it first.
    AppRunning,
    /// A runtime the app needs is not available from its source.
    RuntimeNotFound,
    /// The ref changed since the list was loaded.
    Stale,
    /// The app as installed has other metadata than the plan showed the user
    /// (permissions included), so it was not kept: `rolled_back` says whether
    /// it was removed again. Its runtimes stay and the app's data was never
    /// touched.
    MetadataMismatch { rolled_back: bool },
    /// The operation stopped after these refs were already done (they stay
    /// done); `cause` says why it stopped.
    ///
    /// In the system scope a cancel or timeout only cancels the client's
    /// D-Bus call: `flatpak-system-helper` may keep deploying afterwards, so
    /// `completed` can be short and refs may appear later. Callers re-list
    /// the installation rather than trust `completed`. In the user scope
    /// everything runs in this process and `completed` is exact.
    Partial {
        completed: Vec<String>,
        cause: Box<Error>,
    },
}

impl Error {
    /// The error without a `Partial` wrapper.
    pub fn root(&self) -> &Error {
        match self {
            Error::Partial { cause, .. } => cause.root(),
            e => e,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => f.write_str("The operation was cancelled."),
            Error::Flatpak { action, message } => {
                write!(f, "Flatpak could not {action}: {message}")
            }
            Error::Invalid(what) => write!(f, "Not accepted: {what}."),
            Error::TooLarge(what) => write!(f, "The {what} is larger than the limit."),
            Error::Io { action, message } => write!(f, "Could not {action}: {message}"),
            Error::Unsupported(what) => write!(f, "{what} is not supported by this Flatpak."),
            Error::PlanChanged(c) => write!(
                f,
                "What would be installed has changed since you confirmed it ({c}). Please review it again."
            ),
            Error::NeedsRuntimeRepo(url) => write!(
                f,
                "This app needs its runtime from another source, {url}. Add that source first."
            ),
            Error::RemoteExists(name) => write!(f, "That source is already added as \"{name}\"."),
            Error::RemoteNameTaken(name) => write!(f, "A source named \"{name}\" already exists."),
            Error::EndOfLifeRebase(what) => {
                write!(
                    f,
                    "This app is no longer maintained and has been replaced ({what})."
                )
            }
            Error::SignInRefused => {
                f.write_str("The source asks you to sign in, which the Store does not support.")
            }
            Error::Stale => {
                f.write_str("This app changed since the list was loaded. Reload and try again.")
            }
            Error::MetadataMismatch { rolled_back: true } => f.write_str(
                "The installed app's details differ from what was shown before the install, \
                 so it was removed again. Please review it and try again.",
            ),
            Error::MetadataMismatch { rolled_back: false } => f.write_str(
                "The installed app's details differ from what was shown before the install, \
                 and it could not be removed again. Please remove it from the list of installed apps.",
            ),
            Error::TimedOut => {
                f.write_str("Nothing happened for too long, so the operation was stopped.")
            }
            Error::SentTooMuch => f.write_str("The source sent more than it announced."),
            Error::InUse(list) => write!(
                f,
                "Still in use by: {}. Remove those first, or use \"Remove Unused\" once nothing needs it.",
                list.join(", ")
            ),
            Error::CouldNotCheck(what) => {
                write!(
                    f,
                    "Could not check {what}, so nothing was changed. Try again."
                )
            }
            Error::AppRunning => f.write_str("The app is running. Close it first."),
            Error::RuntimeNotFound => {
                f.write_str("A runtime this app needs is not available from its source.")
            }
            Error::Partial { completed, cause } => write!(
                f,
                "{cause} Already done and kept: {}.",
                completed.join(", ")
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Turns a libflatpak error into ours. A GIO "cancelled" is
/// [`Error::Cancelled`], or [`Error::TimedOut`] when a watchdog cancelled the
/// token; any other error keeps its own mapping even after a late cancel. The
/// message is scrubbed, cleaned and capped.
pub(crate) fn from_glib(
    action: &'static str,
    e: &libflatpak::glib::Error,
    cancel: &CancelToken,
) -> Error {
    if e.matches(gio::IOErrorEnum::Cancelled) {
        if cancel.is_timed_out() {
            Error::TimedOut
        } else {
            Error::Cancelled
        }
    } else {
        Error::Flatpak {
            action,
            message: scrub(e.message()),
        }
    }
}

/// Opens the installation for `scope` to read it: `Installation::new_system`
/// (the default system installation) or `new_user`. libflatpak itself honours
/// `FLATPAK_USER_DIR` and `FLATPAK_SYSTEM_DIR`; this does not read them, and
/// tests rely on that to stay off real installations.
///
/// `set_no_interaction(true)` is set: libflatpak then never starts a polkit or
/// sign-in prompt, so a read cannot ask for privilege.
///
/// Blocking: run on a worker thread.
pub fn open(scope: Scope) -> Result<libflatpak::Installation, Error> {
    open_with(scope, true)
}

/// Opens the installation to change it, with interaction allowed (polkit for
/// the system installation). Only for operations the user already confirmed.
///
/// Blocking: run on a worker thread.
pub(crate) fn open_for_change(scope: Scope) -> Result<libflatpak::Installation, Error> {
    open_with(scope, false)
}

pub(crate) fn open_with(
    scope: Scope,
    no_interaction: bool,
) -> Result<libflatpak::Installation, Error> {
    let none: Option<&gio::Cancellable> = None;
    let inst = match scope {
        Scope::System => libflatpak::Installation::new_system(none),
        Scope::User => libflatpak::Installation::new_user(none),
    }
    .map_err(|e| Error::Flatpak {
        action: match scope {
            Scope::System => "open the system installation",
            Scope::User => "open the user installation",
        },
        message: scrub(e.message()),
    })?;
    inst.set_no_interaction(no_interaction);
    Ok(inst)
}

/// A libflatpak message made safe to show: in every word, each
/// `scheme://user:password@` is reduced to `scheme://`, a bare
/// `user:password@host` to `host`, and the value of a secret-looking query
/// parameter is dropped (see [`scrub_query`]). `Bearer <token>` loses the
/// token, and everything after an `Authorization:` on its line is dropped.
/// This happens before the text is capped, so a cut can't leave half a
/// secret; then it is cleaned and capped. Words end at whitespace and at
/// `, ; " ' ( )`.
pub(crate) fn scrub(msg: &str) -> String {
    scrub_with(msg, false)
}

/// [`scrub`] for a log line: a URL also loses its whole query string and
/// fragment, since a log is kept and shared and a query can carry anything.
pub(crate) fn scrub_log(msg: &str) -> String {
    scrub_with(msg, true)
}

/// Where a secret is split from the words around it, across the words of a
/// message.
#[derive(Default)]
struct Masking {
    /// The last word was `Bearer`: the next one is a token.
    bearer: bool,
    /// An `Authorization:` was seen on this line: the rest of it is a secret.
    line: bool,
}

impl Masking {
    fn word(&mut self, w: &str, drop_query: bool) -> String {
        if w.is_empty() {
            return String::new();
        }
        if self.line || self.bearer {
            self.bearer = false;
            return "***".into();
        }
        let lower = w.to_ascii_lowercase();
        if lower == "bearer" {
            self.bearer = true;
        } else if let Some(after) = lower.strip_prefix("authorization")
            && after.starts_with([':', '='])
        {
            self.line = true;
            if after.len() > 1 {
                // A value glued to the name.
                return format!("{}***", &w[.."authorization".len() + 1]);
            }
        }
        let w = if drop_query { drop_url_query(w) } else { w };
        scrub_word(w)
    }
}

/// A word that is a URL, without its query string and fragment.
fn drop_url_query(w: &str) -> &str {
    match w.find("://") {
        Some(i) => w[i..].find(['?', '#']).map_or(w, |q| &w[..i + q]),
        None => w,
    }
}

fn scrub_with(msg: &str, drop_query: bool) -> String {
    let mut out = String::with_capacity(msg.len().min(4096));
    let mut word = String::new();
    let mut mask = Masking::default();
    for c in msg.chars() {
        if c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '(' | ')') {
            out.push_str(&mask.word(&word, drop_query));
            word.clear();
            if matches!(c, '\n' | '\r') {
                mask.line = false;
            }
            out.push(c);
            if out.len() > 8192 {
                break;
            }
        } else {
            word.push(c);
        }
    }
    out.push_str(&mask.word(&word, drop_query));
    crate::text::clean(&out, 300)
}

/// Whether a query parameter's value is a secret: any name with `token`,
/// `password` or `secret` in it, an AWS signing parameter (`X-Amz-*`), or one
/// of the usual key and signature names.
fn secret_key(key: &str) -> bool {
    const NAMES: [&str; 8] = [
        "apikey",
        "api_key",
        "key",
        "auth",
        "sig",
        "signature",
        "access_token",
        "credential",
    ];
    let k = key.to_ascii_lowercase();
    k.contains("token")
        || k.contains("password")
        || k.contains("secret")
        || k.starts_with("x-amz-")
        || NAMES.contains(&k.as_str())
}

/// Drops the value of secret-looking `name=value` pairs (see [`secret_key`]),
/// in a query or anywhere else in the word: the value runs to the next `&` or
/// `#`.
fn scrub_query(w: &str) -> String {
    let b = w.as_bytes();
    let mut out = String::with_capacity(w.len());
    let mut at = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'=' {
            i += 1;
            continue;
        }
        // Every cut is next to an ASCII byte, so a char is never split.
        let start = b[..i]
            .iter()
            .rposition(|c| matches!(c, b'?' | b'&' | b'#' | b'/' | b':' | b'@'))
            .map_or(0, |p| p + 1)
            .max(at);
        let end = b[i + 1..]
            .iter()
            .position(|c| matches!(c, b'&' | b'#'))
            .map_or(b.len(), |p| i + 1 + p);
        if secret_key(&w[start..i]) {
            out.push_str(&w[at..=i]);
            out.push_str("***");
            at = end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&w[at..]);
    out
}

pub(crate) fn scrub_word(w: &str) -> String {
    const END: [char; 4] = ['/', '?', '#', '\\'];
    let w = scrub_query(w);
    let mut out = String::with_capacity(w.len());
    let mut rest = w.as_str();
    let mut had_scheme = false;
    while let Some(i) = rest.find("://") {
        had_scheme = true;
        let (head, r) = rest.split_at(i + 3);
        out.push_str(head);
        let end = r.find(END).unwrap_or(r.len());
        let auth = &r[..end];
        out.push_str(auth.rsplit_once('@').map_or(auth, |(_, h)| h));
        rest = &r[end..];
    }
    // user:password@host without a scheme (an email has no colon before @).
    while !had_scheme && let Some(at) = rest.find('@') {
        let before = &rest[..at];
        let start = before.rfind(END).map_or(0, |i| i + 1);
        if before[start..].contains(':') {
            out.push_str(&rest[..start]);
        } else {
            out.push_str(&rest[..=at]);
        }
        rest = &rest[at + 1..];
    }
    out.push_str(rest);
    out
}

/// A name that is safe to use as a remote name, arch or branch: `[A-Za-z0-9_.-]`
/// (plus `extra`), no leading `.`, `-` or `+`, and no `..` anywhere.
pub(crate) fn plain_word(s: &str, extra: &[u8]) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with(['.', '-', '+'])
        && !s.contains("..")
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-' || extra.contains(&b)
        })
}

pub(crate) fn valid_remote(s: &str) -> bool {
    plain_word(s, b"")
}

pub(crate) fn valid_arch(s: &str) -> bool {
    s.len() <= 32 && plain_word(s, b"")
}

pub(crate) fn valid_branch(s: &str) -> bool {
    plain_word(s, b"+")
}

pub(crate) fn valid_commit(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_shared_and_send_sync() {
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<CancelToken>();
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        assert!(b.check().is_ok());
        a.cancel();
        assert!(b.is_cancelled());
        assert!(b.cancellable().is_cancelled());
        assert_eq!(b.check(), Err(Error::Cancelled));
        a.cancel();
    }

    #[test]
    fn name_checks() {
        assert!(valid_remote("flathub"));
        assert!(!valid_remote("a b"));
        assert!(!valid_remote(""));
        assert!(valid_arch("x86_64"));
        assert!(!valid_arch("x/y"));
        assert!(valid_branch("stable"));
        assert!(valid_branch("23.08+1"));
        assert!(!valid_branch("a/b"));
        for bad in ["..", ".", "-x", "+x", "a..b", ".hidden"] {
            assert!(!valid_branch(bad), "{bad}");
            assert!(!valid_arch(bad), "{bad}");
            assert!(!valid_remote(bad), "{bad}");
        }
        assert!(valid_commit("abc123"));
        assert!(!valid_commit("xyz"));
        assert!(!valid_commit(&"a".repeat(65)));
    }
}

#[cfg(test)]
mod scrub_tests {
    use super::*;

    #[test]
    fn every_url_in_a_word_and_separators_and_tokens_are_handled() {
        let s = scrub("a(https://u:p@h1/x,https://v:q@h2/y)\"https://w:r@h3\"");
        assert!(
            !s.contains(":p@") && !s.contains(":q@") && !s.contains(":r@"),
            "{s}"
        );
        assert!(
            s.contains("h1") && s.contains("h2") && s.contains("h3"),
            "{s}"
        );
        let s = scrub("GET https://h/x?token=abc123&a=b and ?password=hunter2");
        assert!(
            !s.contains("abc123") && !s.contains("hunter2") && s.contains("a=b"),
            "{s}"
        );
        let s = scrub("x u:p@h1 and v:q@h2 mail me@example.org");
        assert!(
            !s.contains(":p@") && !s.contains(":q@") && s.contains("me@example.org"),
            "{s}"
        );
    }

    #[test]
    fn userinfo_is_removed_from_urls() {
        assert_eq!(
            scrub("fetch https://bob:pw@example.org/repo failed"),
            "fetch https://example.org/repo failed"
        );
        assert_eq!(
            scrub("a https://example.org/x@y b"),
            "a https://example.org/x@y b"
        );
        assert_eq!(scrub("http://u@h and ftp://v:w@g"), "http://h and ftp://g");
        assert_eq!(scrub("no url"), "no url");
        // ? # and \ end the authority.
        assert_eq!(scrub("https://a:b@h?x=y@z"), "https://h?x=y@z");
        assert_eq!(scrub("https://h?u=a:b@c"), "https://h?u=a:b@c");
        assert_eq!(scrub("https://h#f@x"), "https://h#f@x");
        assert_eq!(scrub("https://u:p@h\\x"), "https://h\\x");
        // Scheme-less.
        assert_eq!(
            scrub("login bob:secret@example.org failed"),
            "login example.org failed"
        );
        assert_eq!(scrub("mail bob@example.org"), "mail bob@example.org");
        // A secret near the cap is gone before the cut.
        let long = format!("{} https://u:SECRET@h/x", "a ".repeat(140));
        assert!(!scrub(&long).contains("SECRET"));
    }

    #[test]
    fn secret_query_values_bearer_tokens_and_authorization_are_dropped() {
        for k in [
            "apikey",
            "api_key",
            "key",
            "auth",
            "sig",
            "signature",
            "access_token",
            "credential",
            "X-Amz-Signature",
            "x-amz-credential",
            "X-Amz-Security-Token",
            "Token",
        ] {
            let s = scrub(&format!("GET https://h/x?a=b&{k}=SECRETVAL&c=d"));
            assert!(!s.contains("SECRETVAL"), "{k}: {s}");
            assert!(s.contains("a=b") && s.contains("c=d"), "{k}: {s}");
        }
        // A name that only contains "key" is not a secret.
        assert_eq!(scrub("https://h/x?monkey=1"), "https://h/x?monkey=1");
        // The last parameter, a fragment, and a value with an equals sign.
        assert!(!scrub("https://h/?sig=a=b").contains("a=b"));
        assert_eq!(scrub("https://h/?a=1&key=zz#f"), "https://h/?a=1&key=***#f");
        // Non-ASCII around a secret is not split.
        assert_eq!(scrub("é?key=ü&ö=1"), "é?key=***&ö=1");
        let s = scrub("sent Bearer abc.def.ghi to the host");
        assert_eq!(s, "sent Bearer *** to the host");
        let s = scrub("bearer\tABCDEF failed");
        assert!(!s.contains("ABCDEF"), "{s}");
        let s = scrub("headers: Authorization: Basic dXNlcjpwdw== and more\nnext line");
        // (The cleaning turns the line break into a space.)
        assert_eq!(s, "headers: Authorization: *** *** *** *** next line");
        let s = scrub("Authorization:Bearer-xyz ok");
        assert!(!s.contains("xyz") && !s.contains("ok"), "{s}");
        // The word alone is not a header.
        assert_eq!(scrub("authorization failed"), "authorization failed");
    }

    #[test]
    fn a_log_line_loses_the_whole_query_and_fragment_of_a_url() {
        assert_eq!(
            scrub_log("fetch https://u:p@h/x?a=b&c=d#frag failed"),
            "fetch https://h/x failed"
        );
        assert_eq!(scrub_log("(https://h/y?z=1)"), "(https://h/y)");
        // Not a URL: kept, apart from secrets.
        assert_eq!(scrub_log("what? yes#1"), "what? yes#1");
        assert_eq!(
            scrub_log("Bearer tok https://h/?k=v"),
            "Bearer *** https://h/"
        );
        // The display form keeps the harmless parameters.
        assert_eq!(scrub("https://h/x?a=b"), "https://h/x?a=b");
    }
}
