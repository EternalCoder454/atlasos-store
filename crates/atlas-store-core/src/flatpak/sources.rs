//! Sources (remotes): adding one from a `.flatpakrepo` or for a `.flatpakref`
//! the user confirmed, removing one the Store added, and refreshing a
//! remote's AppStream.
//!
//! **A `.flatpakref` never reaches libflatpak.** libflatpak adds the file's
//! remote while it plans, even if the install is then abandoned, so planning
//! from the file would change the installation before the user agreed. The
//! Store parses the file itself and works in three steps:
//! [`resolve_ref_source`] (pure: looks at the remotes, changes nothing) says
//! either "use this existing remote" or proposes a new one
//! ([`RemoteProposal`], shown in an "Add Source" confirmation);
//! [`add_ref_remote`] builds that remote from the proposal's own fields; then
//! the normal [`plan_install`](super::plan_install) runs against it. A source
//! the user declines at the install step can be taken back with
//! [`remove_remote`]. Existing remotes are never modified.
//!
//! # What a caller does after each outcome
//!
//! 1. [`resolve_ref_source`] returns [`RefSource::Existing`] (go straight to
//!    [`plan_install`](super::plan_install) with that remote) or
//!    [`RefSource::New`] (a [`RemoteProposal`]).
//! 2. For a proposal the UI shows "Add Source". Only on the user's
//!    confirmation: [`add_ref_remote`], then `plan_install`, then the usual
//!    install confirmation. The remote is now recorded in a journal of
//!    "added but not yet used" remotes.
//! 3. [`Error::RemoteExists`] or [`Error::RemoteNameTaken`] from
//!    `add_ref_remote`: the remotes changed since step 1. Call
//!    `resolve_ref_source` again and show its answer; never retry the old
//!    proposal.
//! 4. [`Error::NeedsRuntimeRepo`] from planning: the file names a runtime
//!    source. Show a separate confirmation for it (a `.flatpakrepo` through
//!    [`add_remote`]). If the user declines, [`remove_remote`] the source added
//!    in step 2 (with its name, URL and main ref from the proposal).
//! 5. The user declines the plan, or the install fails: [`remove_remote`] the
//!    source added in step 2. [`Error::InUse`] there means the install had
//!    already put something in from it (a partial install): keep the remote.
//!    After a successful install call `journal_clear` (the remote is in use
//!    and no longer pending).
//! 6. At startup, with the operation lock held and before anything else adds
//!    a source, call [`sweep_pending_remotes`]: it removes the remotes a crash
//!    or a lost process left behind (still what the Store added: same name,
//!    URL and main ref, not a well-known name, nothing installed from them)
//!    and forgets the ones that are in use, gone or not the Store's. It skips
//!    entries a Store that is still running wrote.
//!
//! Everything that changes takes `&OperationLock` as proof that the caller
//! holds the locks, and blocks: run it on a worker thread. System scope needs
//! libflatpak's system helper and polkit; it is not covered by tests here.

use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use libflatpak::glib::translate::ToGlibPtr;
use libflatpak::prelude::*;

use super::lock::OperationLock;
use super::supervise::{Msg, Phase, run_supervised};
use super::transaction::explain;
use super::transaction::{RemoteCfg, check_ref, norm_url, remote_configs};
use super::{CancelToken, Error, Scope};
use crate::flatpakref::{
    FlatpakRef, FlatpakRepo, GpgKey, suggested_remote_name, valid_remote_name,
};
use crate::launch::https_url;
use crate::text;

/// Names that belong to well-known sources: a source from a file never gets
/// one, unless its URL is that source's own.
const RESERVED: [(&str, &[&str]); 8] = [
    (
        "flathub",
        &["https://dl.flathub.org/repo", "https://flathub.org/repo"],
    ),
    (
        "flathub-beta",
        &[
            "https://dl.flathub.org/beta-repo",
            "https://flathub.org/beta-repo",
        ],
    ),
    ("fedora", &[]),
    ("fedora-testing", &[]),
    ("kde", &[]),
    ("kdeapps", &[]),
    ("gnome", &[]),
    ("gnome-nightly", &[]),
];

/// Whether `name` is a reserved remote name that `url` has no right to.
pub(crate) fn reserved_name(name: &str, url: &str) -> bool {
    let url = norm_url(url);
    RESERVED
        .iter()
        .any(|(n, urls)| n.eq_ignore_ascii_case(name) && !urls.contains(&url.as_str()))
}

/// The most "-N" numbers tried to find a free remote name.
const NAME_TRIES: u32 = 99;

/// The remote a `.flatpakref` would add, for the "Add Source" confirmation.
/// Built only from the parsed file ([`RemoteProposal::from_ref`]); the fields
/// are checked again by [`add_ref_remote`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProposal {
    pub name: String,
    /// Normalized, https.
    pub url: String,
    /// The signing key; `None` means the remote would be unsigned (the dialog
    /// must warn).
    pub key: Option<GpgKey>,
    pub title: Option<String>,
    /// As flatpak's own origin remotes: the remote is not listed in the
    /// catalog.
    pub noenumerate: bool,
    pub default_branch: Option<String>,
    /// The ref the file installs.
    pub main_ref: String,
    pub prio: i32,
}

impl RemoteProposal {
    /// The remote flatpak itself would create for `r` (an origin remote:
    /// noenumerate, priority 0, signature checked when there is a key, the
    /// file's ref as main ref), under `name`.
    pub fn from_ref(r: &FlatpakRef, name: String) -> Result<RemoteProposal, Error> {
        Ok(RemoteProposal {
            name,
            url: norm_url(&r.url),
            key: r.key.clone(),
            title: r.title.clone(),
            noenumerate: true,
            default_branch: r.branch.clone(),
            main_ref: main_ref_of(r)?,
            prio: 0,
        })
    }

    /// The key's fingerprint, to show.
    pub fn fingerprint(&self) -> Option<&str> {
        self.key.as_ref().map(GpgKey::fingerprint)
    }
}

/// Where a `.flatpakref`'s app comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefSource {
    /// A remote with the file's URL is already added and enabled: use it as
    /// it is. The file's key and settings are ignored.
    Existing { remote: String },
    /// Add this remote first, after the user confirms it.
    New(RemoteProposal),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefResolution {
    /// `app/ID/arch/branch` (or `runtime/...`) the file installs.
    pub ref_: String,
    pub source: RefSource,
}

/// The ref a `.flatpakref` installs: its name, the default architecture and
/// its branch (`master` when it has none, as flatpak does).
pub(crate) fn main_ref_of(r: &FlatpakRef) -> Result<String, Error> {
    let arch = libflatpak::default_arch()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let s = format!(
        "{}/{}/{}/{}",
        if r.is_runtime { "runtime" } else { "app" },
        r.name,
        arch,
        r.branch.as_deref().unwrap_or("master")
    );
    check_ref(&s)?;
    Ok(s)
}

/// A name no remote has (compared without case, which a case-insensitive
/// file system would not tell apart), by numbering as flatpak does.
fn free_name(base: &str, remotes: &[RemoteCfg]) -> Result<String, Error> {
    let taken = |n: &str| remotes.iter().any(|r| r.name.eq_ignore_ascii_case(n));
    if !taken(base) {
        return Ok(base.to_string());
    }
    (2..=NAME_TRIES)
        .map(|i| format!("{base}-{i}"))
        .find(|n| valid_remote_name(n) && !taken(n))
        .ok_or_else(|| Error::RemoteNameTaken(base.to_string()))
}

/// Decides where the app of `r` comes from, without changing anything: an
/// enabled remote with the same (normalized) URL is used as it is, else a
/// [`RemoteProposal`] for a new one. Disabled remotes are ignored.
///
/// Blocking (reads the remote list): run on a worker thread.
pub fn resolve_ref_source(
    scope: Scope,
    r: &FlatpakRef,
    cancel: &CancelToken,
) -> Result<RefResolution, Error> {
    cancel.check()?;
    let ref_ = main_ref_of(r)?;
    let remotes = remote_configs(scope, cancel)?;
    let url = norm_url(&r.url);
    if let Some(e) = remotes.iter().find(|c| !c.disabled && c.url == url) {
        return Ok(RefResolution {
            ref_,
            source: RefSource::Existing {
                remote: e.name.clone(),
            },
        });
    }
    let mut base = suggested_remote_name(r);
    if reserved_name(&base, &r.url) {
        base = format!("{}-origin", r.name);
    }
    let name = free_name(&base, &remotes)?;
    Ok(RefResolution {
        ref_,
        source: RefSource::New(RemoteProposal::from_ref(r, name)?),
    })
}

/// Whether `name` is a well-known source's name, whatever its URL: such a
/// remote is never removed by the Store.
fn reserved_any(name: &str) -> bool {
    RESERVED.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// The remote libflatpak is given for a proposal: built from the proposal's
/// own fields only.
fn build_remote(p: &RemoteProposal) -> libflatpak::Remote {
    let remote = libflatpak::Remote::new(&p.name);
    remote.set_url(&p.url);
    match &p.key {
        Some(k) => {
            remote.set_gpg_key(&libflatpak::glib::Bytes::from(k.bytes()));
            remote.set_gpg_verify(true);
        }
        None => remote.set_gpg_verify(false),
    }
    if let Some(t) = &p.title {
        remote.set_title(t);
    }
    remote.set_noenumerate(p.noenumerate);
    // The main ref marks the remote as the Store's own: removal checks it.
    remote.set_main_ref(&p.main_ref);
    if let Some(b) = &p.default_branch {
        remote.set_default_branch(b);
    }
    remote.set_prio(p.prio);
    remote
}

/// Whether a failed add certainly left nothing behind. Cancelled, timed out
/// or an unknown error may have happened after libflatpak wrote the remote,
/// so the journal entry stays and the sweep decides later.
fn certainly_not_added(e: &Error) -> bool {
    matches!(
        e,
        Error::RemoteNameTaken(_) | Error::RemoteExists(_) | Error::Invalid(_)
    )
}

/// Adds the remote of a confirmed [`RemoteProposal`]. libflatpak gets a
/// remote built from the proposal's own fields (URL, key, title, noenumerate,
/// main ref, default branch, priority); without a key the signature check is
/// off. Refused: a name already taken without regard to case
/// ([`Error::RemoteNameTaken`]) and an enabled remote with the same URL
/// ([`Error::RemoteExists`]).
///
/// Blocking: run on a worker thread.
pub fn add_ref_remote(
    scope: Scope,
    proposal: &RemoteProposal,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    if https_url(&proposal.url).as_deref() != Some(proposal.url.as_str()) {
        return Err(Error::Invalid(
            "the source's URL is not an https address".into(),
        ));
    }
    add_ref_remote_in(scope, proposal, lock, cancel)
}

/// The libflatpak-facing part (tests call it with a `file://` remote, which
/// the URL policy refuses).
pub(crate) fn add_ref_remote_in(
    scope: Scope,
    p: &RemoteProposal,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    cancel.check()?;
    if !valid_remote_name(&p.name) {
        return Err(Error::Invalid("the source name is not valid".into()));
    }
    check_ref(&p.main_ref)?;
    if reserved_name(&p.name, &p.url) {
        return Err(Error::Invalid("that source name is reserved".into()));
    }
    if p.default_branch
        .as_deref()
        .is_some_and(|b| !super::valid_branch(b))
    {
        return Err(Error::Invalid("the source's branch is not valid".into()));
    }
    if p.title
        .as_deref()
        .is_some_and(|t| t.chars().count() > 200 || t.chars().any(char::is_control))
    {
        return Err(Error::Invalid("the source's title is not valid".into()));
    }
    if p.prio != 0 || !p.noenumerate {
        return Err(Error::Invalid(
            "the source's settings are not the usual ones".into(),
        ));
    }
    if p.url.is_empty() || p.url.len() > 500 || p.url.chars().any(char::is_control) {
        return Err(Error::Invalid("the source's URL is not valid".into()));
    }
    let remotes = remote_configs(scope, cancel)?;
    if remotes.iter().any(|r| r.name.eq_ignore_ascii_case(&p.name)) {
        return Err(Error::RemoteNameTaken(p.name.clone()));
    }
    if let Some(r) = remotes
        .iter()
        .find(|r| !r.disabled && r.url == norm_url(&p.url))
    {
        return Err(Error::RemoteExists(r.name.clone()));
    }
    let remote = build_remote(p);
    // Recorded before it is added: a crash between the two leaves a journal
    // entry for a remote that is not there (the sweep forgets it), never a
    // remote nobody knows about. If the journal can't be written, nothing is
    // added.
    journal_add(scope, &p.name, &p.url, &p.main_ref, lock, cancel)?;
    // (error, whether libflatpak was never asked)
    let added = match super::open_for_change(scope) {
        Err(e) => Err((e, true)),
        Ok(inst) => inst
            .add_remote(&remote, false, Some(cancel.cancellable()))
            .map_err(|e| (super::from_glib("add the source", &e, cancel), false)),
    };
    match added {
        Ok(()) => Ok(()),
        Err((e, untried)) => {
            if untried || certainly_not_added(&e) {
                if let Err(je) = journal_clear(scope, &p.name, &p.url) {
                    log::warn!("could not forget the source that was not added: {je}");
                }
            } else {
                log::warn!(
                    "adding the source {} ended with an error ({e}); it may have been \
                     added, so it stays on the list and the next start cleans it up",
                    p.name
                );
            }
            Err(e)
        }
    }
}

/// Removes a remote the Store added, to take back a source the user declined
/// at the install step. Only when it is what [`add_ref_remote`] makes: not a
/// well-known name, not listed in the catalog, priority 0, `main_ref` as its
/// main ref, and `name` and `url` both matching it as it is now, with nothing
/// installed from it ([`Error::InUse`] lists what is); libflatpak's own check
/// (no force) is the last guard. The remote's entry in the pending journal is
/// cleared when it is gone.
///
/// In the system installation this may ask for authentication (polkit): the
/// user is at the dialog that declined the source.
///
/// Blocking: run on a worker thread.
pub fn remove_remote(
    scope: Scope,
    name: &str,
    url: &str,
    main_ref: &str,
    _lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    match remove_checked(scope, name, url, main_ref, true, cancel)? {
        Removal::Removed => Ok(()),
        Removal::Gone => Err(Error::Invalid("that source is not added".into())),
        Removal::Changed => Err(Error::Invalid(
            "that source is not the one that was added".into(),
        )),
        Removal::NotOurs => Err(Error::Invalid(
            "that source was not added by the Store, or it was changed since".into(),
        )),
    }
}

/// What [`remove_checked`] found.
#[derive(Debug, PartialEq, Eq)]
enum Removal {
    Removed,
    /// No remote of that name.
    Gone,
    /// The remote of that name has another URL.
    Changed,
    /// Not the remote [`add_ref_remote`] makes (a well-known name, listed in
    /// the catalog, another priority or another main ref). Kept.
    NotOurs,
}

fn remove_checked(
    scope: Scope,
    name: &str,
    url: &str,
    main_ref: &str,
    interactive: bool,
    cancel: &CancelToken,
) -> Result<Removal, Error> {
    cancel.check()?;
    if !valid_remote_name(name) {
        return Err(Error::Invalid("the source name is not valid".into()));
    }
    check_ref(main_ref)?;
    if reserved_any(name) {
        return Ok(Removal::NotOurs);
    }
    let remotes = remote_configs(scope, cancel)?;
    let forget = |why: &str| {
        if let Err(e) = journal_clear(scope, name, url) {
            log::warn!("could not forget the source ({why}): {e}");
        }
    };
    let Some(cfg) = remotes.iter().find(|r| r.name == name) else {
        forget("it is not added");
        return Ok(Removal::Gone);
    };
    if cfg.url != norm_url(url) {
        forget("its address changed");
        return Ok(Removal::Changed);
    }
    let rm = super::open(scope)?
        .remote_by_name(name, Some(cancel.cancellable()))
        .map_err(|e| super::from_glib("read the source", &e, cancel))?;
    if !rm.is_noenumerate() || rm.prio() != 0 || rm.main_ref().as_deref() != Some(main_ref) {
        return Ok(Removal::NotOurs);
    }
    let all = super::list_installed_all(cancel);
    if all.cancelled {
        return Err(Error::Cancelled);
    }
    // A failed add-on link says nothing about where a ref came from, so it
    // must not block every removal. Anything else (an unreadable
    // installation, a skipped ref of any kind, or "N more" hiding such a
    // ref) might be something installed from this remote: not knowing is not
    // permission.
    if let Some(e) = all
        .errors
        .iter()
        .find(|e| e.kind != super::installed::ListErrorKind::ExtensionLink)
    {
        log::warn!(
            "could not list what is installed from {name}: {}",
            e.message
        );
        return Err(Error::CouldNotCheck(
            "what is installed from the source".into(),
        ));
    }
    let users: Vec<String> = all
        .refs
        .iter()
        .filter(|r| r.scope == scope && r.origin == name)
        .map(|r| r.full_ref())
        .collect();
    if !users.is_empty() {
        return Err(Error::InUse(users));
    }
    let inst = super::open_with(scope, !interactive)?;
    inst.remove_remote(name, Some(cancel.cancellable()))
        .map_err(|e| super::from_glib("remove the source", &e, cancel))?;
    forget("it was removed");
    Ok(Removal::Removed)
}

/// What [`sweep_pending_remotes`] did, by remote name.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepOutcome {
    /// Removed: added by the Store, never used.
    pub removed: Vec<String>,
    /// Kept and forgotten by the journal: something is installed from it, or
    /// it is not the remote the Store added (a well-known name, listed in the
    /// catalog, another main ref).
    pub kept: Vec<String>,
    /// Gone or changed since it was journaled. Forgotten by the journal.
    pub stale: Vec<String>,
    /// Could not be checked or removed, with the reason. Still in the journal
    /// for the next start, unless it is in `evicted`.
    pub failed: Vec<(String, Error)>,
    /// Given up on after failing at 3 starts and forgotten by the journal.
    /// The remote may still be added: the user can remove it by hand.
    pub evicted: Vec<String>,
    /// The journal itself could not be read or written, or it was damaged
    /// (then the damaged copy is `pending-remotes.bad` and the lines that
    /// were fine are kept).
    pub journal_error: Option<Error>,
    /// The token was cancelled; the rest stay in the journal.
    pub cancelled: bool,
}

/// Removes the remotes [`add_ref_remote`] journaled and that were never used:
/// each one that is still what the Store added (the checks of
/// [`remove_remote`]) and has nothing installed from it. A remote with refs
/// installed, or one that is not the Store's, is kept; either way its entry is
/// cleared. Never prompts for authentication. An entry whose writing Store is
/// still running as another process is left alone. A removal that fails is
/// retried at the next starts and given up after 3.
///
/// Call it once at startup, from the only running Store (see the module
/// docs). Blocking: run on a worker thread.
pub fn sweep_pending_remotes(
    scope: Scope,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> SweepOutcome {
    sweep_inner(scope, lock, cancel, true)
}

/// `own`: whether entries this very process wrote are swept too (true at
/// startup, false when the journal is full in the middle of a flow).
fn sweep_inner(
    scope: Scope,
    _lock: &OperationLock,
    cancel: &CancelToken,
    own: bool,
) -> SweepOutcome {
    let mut out = SweepOutcome::default();
    let entries = match journal_entries() {
        Ok((e, notice)) => {
            out.journal_error = notice;
            e
        }
        Err(e) => {
            log::warn!("could not read the added sources list: {e}");
            out.journal_error = Some(e);
            return out;
        }
    };
    for e in entries.into_iter().filter(|e| e.scope == scope) {
        if e.writer_alive() && !(own && e.is_own()) {
            log::info!(
                "the source {} belongs to a Store that is still running; left alone",
                e.name
            );
            continue;
        }
        let forget = |out: &mut SweepOutcome| {
            if let Err(je) = journal_clear(scope, &e.name, &e.url) {
                log::warn!("could not forget a source: {je}");
                out.journal_error.get_or_insert(je);
            }
        };
        match remove_checked(scope, &e.name, &e.url, &e.main_ref, false, cancel) {
            Ok(Removal::Removed) => out.removed.push(e.name),
            Ok(Removal::Gone | Removal::Changed) => out.stale.push(e.name),
            Ok(Removal::NotOurs) => {
                forget(&mut out);
                out.kept.push(e.name);
            }
            Err(Error::InUse(_)) => {
                forget(&mut out);
                out.kept.push(e.name);
            }
            Err(Error::Cancelled | Error::TimedOut) => {
                out.cancelled = true;
                break;
            }
            Err(err) => {
                log::warn!("could not sweep the source {}: {err}", e.name);
                match journal_note_failure(&e) {
                    Ok(true) => {
                        log::warn!("giving up on the source {}", e.name);
                        out.evicted.push(e.name.clone());
                    }
                    Ok(false) => {}
                    Err(je) => {
                        out.journal_error.get_or_insert(je);
                    }
                }
                out.failed.push((e.name, err));
            }
        }
    }
    out
}

// The pending-remotes journal: `<state>/atlas-store/pending-remotes`, one
// `scope<TAB>name<TAB>url<TAB>main-ref<TAB>pid<TAB>start<TAB>fails` line per
// remote the Store added and has not yet seen used (user scope only: a
// system sweep could never prompt for the password). `pid` and `start` (field
// 22 of /proc/<pid>/stat) name the Store that wrote it. Names, URLs and refs
// are checked before they get here, so they hold no tab or newline. All file
// access goes through the open folder, never through a path.

/// Largest journal read (64 KiB) and most entries kept.
const JOURNAL_MAX_BYTES: usize = 64 * 1024;
const JOURNAL_MAX_ENTRIES: usize = 64;
const JOURNAL_FILE: &str = "pending-remotes";
/// Where a damaged journal is kept.
const JOURNAL_BAD: &str = "pending-remotes.bad";
/// Failed sweeps after which an entry is dropped.
const JOURNAL_MAX_FAILS: u32 = 3;

/// Serializes read-modify-write cycles inside the process.
static JOURNAL: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    scope: Scope,
    name: String,
    url: String,
    main_ref: String,
    /// The Store that wrote it.
    pid: u32,
    start: u64,
    fails: u32,
}

/// This process's pid and start time (0 when /proc can't be read).
fn own_identity() -> (u32, u64) {
    static ID: OnceLock<(u32, u64)> = OnceLock::new();
    *ID.get_or_init(|| {
        let pid = std::process::id();
        (pid, proc_start(pid).unwrap_or(0))
    })
}

/// Field 22 of `/proc/<pid>/stat`: when the process started, in ticks since
/// boot. With the pid it tells one process from a later one that reused the
/// number.
fn proc_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The name (field 2) is in parentheses and may hold spaces: count from
    // the last ")". The rest starts at field 3.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

impl Entry {
    fn same(&self, o: &Entry) -> bool {
        self.scope == o.scope
            && self.name == o.name
            && self.url == o.url
            && self.main_ref == o.main_ref
    }

    fn is_own(&self) -> bool {
        (self.pid, self.start) == own_identity()
    }

    /// Whether the Store that wrote it is still running.
    fn writer_alive(&self) -> bool {
        self.is_own() || (self.start != 0 && proc_start(self.pid) == Some(self.start))
    }

    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.scope.label(),
            self.name,
            self.url,
            self.main_ref,
            self.pid,
            self.start,
            self.fails
        )
    }
}

fn journal_io(what: &str, e: &std::io::Error) -> Error {
    Error::Io {
        action: "keep the list of added sources",
        message: match e.raw_os_error() {
            Some(libc::ELOOP) => format!("{what} is a symbolic link"),
            _ => format!("{what}: {}", e.kind()),
        },
    }
}

fn journal_refused(msg: &str) -> Error {
    Error::Io {
        action: "keep the list of added sources",
        message: msg.to_string(),
    }
}

/// `$XDG_STATE_HOME/atlas-store`, else `$HOME/.local/state/atlas-store`.
fn journal_dir_path() -> Result<PathBuf, Error> {
    let abs = |v: Option<std::ffi::OsString>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    let base = abs(std::env::var_os("XDG_STATE_HOME"))
        .or_else(|| abs(std::env::var_os("HOME")).map(|h| h.join(".local/state")))
        .ok_or_else(|| journal_refused("there is no state folder"))?;
    Ok(base.join("atlas-store"))
}

/// Opens the journal folder (not through a symlink), making it (0700) when
/// `create`. It must be a folder of ours; a folder open to others is closed
/// again. `None` when it does not exist and `create` is false.
fn journal_dir(create: bool) -> Result<Option<File>, Error> {
    let dir = journal_dir_path()?;
    if create {
        if let Some(parent) = dir.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|e| journal_io("the state folder", &e))?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(journal_io("the folder", &e)),
        }
    }
    let f = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(&dir)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(e) => return Err(journal_io("the folder", &e)),
    };
    let md = f.metadata().map_err(|e| journal_io("the folder", &e))?;
    // SAFETY: geteuid has no arguments and cannot fail.
    if md.uid() != unsafe { libc::geteuid() } {
        return Err(journal_refused("the folder belongs to someone else"));
    }
    if md.mode() & 0o077 != 0 {
        f.set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| journal_io("the folder", &e))?;
    }
    Ok(Some(f))
}

fn c_name(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|_| std::io::ErrorKind::InvalidInput.into())
}

/// `openat(2)` relative to the open folder; the result is closed on drop.
fn open_at(dir: &File, name: &str, flags: c_int, mode: libc::mode_t) -> std::io::Result<File> {
    let c = c_name(name)?;
    loop {
        // SAFETY: `dir` is an open descriptor for the call, `c` is a valid
        // NUL-terminated string that outlives it, and the mode is passed as
        // the unsigned int openat reads.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c.as_ptr(),
                flags | libc::O_CLOEXEC,
                mode as c_uint,
            )
        };
        if fd >= 0 {
            // SAFETY: `fd` is a new descriptor that nothing else owns.
            return Ok(unsafe { File::from_raw_fd(fd) });
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// `renameat(2)` inside the open folder.
fn rename_at(dir: &File, from: &str, to: &str) -> std::io::Result<()> {
    let (f, t) = (c_name(from)?, c_name(to)?);
    // SAFETY: `dir` is open for the call and both names are valid
    // NUL-terminated strings that outlive it.
    let r = unsafe { libc::renameat(dir.as_raw_fd(), f.as_ptr(), dir.as_raw_fd(), t.as_ptr()) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// `unlinkat(2)` of a file inside the open folder.
fn unlink_at(dir: &File, name: &str) -> std::io::Result<()> {
    let c = c_name(name)?;
    // SAFETY: `dir` is open for the call and `c` is a valid NUL-terminated
    // string that outlives it.
    let r = unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn parse_line(line: &[u8]) -> Option<Entry> {
    let line = std::str::from_utf8(line).ok()?;
    let mut it = line.split('\t');
    let (
        Some(scope),
        Some(name),
        Some(url),
        Some(main_ref),
        Some(pid),
        Some(start),
        Some(fails),
        None,
    ) = (
        it.next(),
        it.next(),
        it.next(),
        it.next(),
        it.next(),
        it.next(),
        it.next(),
        it.next(),
    )
    else {
        return None;
    };
    // Only user entries are ever written.
    if scope != "user"
        || !valid_remote_name(name)
        || url.is_empty()
        || url.len() > 500
        || url.chars().any(char::is_control)
        || check_ref(main_ref).is_err()
    {
        return None;
    }
    Some(Entry {
        scope: Scope::User,
        name: name.to_string(),
        url: url.to_string(),
        main_ref: main_ref.to_string(),
        pid: pid.parse().ok()?,
        start: start.parse().ok()?,
        fails: fails.parse::<u32>().ok()?.min(JOURNAL_MAX_FAILS),
    })
}

/// The entries that are fine, and whether anything was not (too big, too many
/// entries, a line not understood). A missing file is empty. A symlink, a file
/// that is not a regular file of ours, or a failed read is an error.
fn read_entries(dir: &File) -> Result<(Vec<Entry>, bool), Error> {
    let file = match open_at(
        dir,
        JOURNAL_FILE,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
        0,
    ) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), false)),
        Err(e) => return Err(journal_io("the list", &e)),
    };
    let md = file.metadata().map_err(|e| journal_io("the list", &e))?;
    // SAFETY: geteuid has no arguments and cannot fail.
    if !md.is_file() || md.uid() != unsafe { libc::geteuid() } {
        return Err(journal_refused(
            "the list is not a regular file of this user",
        ));
    }
    let mut buf = Vec::new();
    file.take(JOURNAL_MAX_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| journal_io("the list", &e))?;
    let mut damaged = false;
    if buf.len() > JOURNAL_MAX_BYTES {
        damaged = true;
        buf.truncate(JOURNAL_MAX_BYTES);
        // Drop the line the cut went through.
        buf.truncate(buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1));
    }
    let mut out = Vec::new();
    for line in buf.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        match parse_line(line) {
            Some(e) if out.len() < JOURNAL_MAX_ENTRIES => out.push(e),
            _ => damaged = true,
        }
    }
    Ok((out, damaged))
}

/// [`read_entries`], and a damaged list is renamed to `pending-remotes.bad`
/// (never overwritten in place) with the good lines written back. The notice
/// says so, for the caller to report.
fn load(dir: &File) -> Result<(Vec<Entry>, Option<Error>), Error> {
    let (entries, damaged) = read_entries(dir)?;
    if !damaged {
        return Ok((entries, None));
    }
    log::warn!("the list of added sources is damaged; kept as {JOURNAL_BAD}");
    rename_at(dir, JOURNAL_FILE, JOURNAL_BAD).map_err(|e| journal_io("the damaged list", &e))?;
    write_entries(dir, &entries)?;
    Ok((
        entries,
        Some(journal_refused(&format!(
            "the list of added sources was damaged; the damaged copy was kept as {JOURNAL_BAD}"
        ))),
    ))
}

/// A name no other file in the folder has, for the temporary file.
fn temp_name(n: u32) -> String {
    use std::hash::{BuildHasher, Hasher};
    // RandomState is seeded randomly: an unpredictable suffix.
    let r = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    format!("{JOURNAL_FILE}.{}.{n}.{r:016x}.tmp", std::process::id())
}

/// Removes temporary files left by a Store that died mid-write: those whose
/// writer pid no longer exists. A live writer's file is left alone.
fn remove_dead_temps(dir: &File) {
    let Ok(names) = std::fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd())) else {
        return;
    };
    let prefix = format!("{JOURNAL_FILE}.");
    for name in names
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
    {
        let Some(pid) = name
            .strip_prefix(&prefix)
            .filter(|rest| rest.ends_with(".tmp"))
            .and_then(|rest| rest.split('.').next())
            .and_then(|p| p.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id()
            && !std::path::Path::new(&format!("/proc/{pid}")).exists()
            && let Err(e) = unlink_at(dir, &name)
        {
            log::warn!("could not remove the leftover {name}: {e}");
        }
    }
}

/// Writes the entries atomically: temporary file (0600, a name made fresh,
/// never through a link), fsync, rename, fsync of the folder. No entries: the
/// file goes.
fn write_entries(dir: &File, entries: &[Entry]) -> Result<(), Error> {
    if entries.is_empty() {
        return match unlink_at(dir, JOURNAL_FILE) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(journal_io("the list", &e)),
        };
    }
    remove_dead_temps(dir);
    let text: String = entries.iter().map(Entry::line).collect();
    let mut tries = 0;
    let (tmp, mut f) = loop {
        let tmp = temp_name(tries);
        match open_at(
            dir,
            &tmp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        ) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 5 => tries += 1,
            Err(e) => return Err(journal_io("the temporary list", &e)),
        }
    };
    let res = (|| -> std::io::Result<()> {
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        rename_at(dir, &tmp, JOURNAL_FILE)?;
        dir.sync_all()
    })();
    if let Err(e) = res {
        let _ = unlink_at(dir, &tmp);
        return Err(journal_io("the list", &e));
    }
    Ok(())
}

/// Reads, changes and writes the journal under the process lock. `create`:
/// make the folder if needed (no for a pure clear).
fn update_journal(
    create: bool,
    f: impl FnOnce(&mut Vec<Entry>) -> Result<(), Error>,
) -> Result<(), Error> {
    let _g = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = journal_dir(create)? else {
        return Ok(());
    };
    let (mut entries, notice) = load(&dir)?;
    if let Some(n) = notice {
        log::warn!("{n}");
    }
    let before = entries.clone();
    f(&mut entries)?;
    if entries == before {
        return Ok(());
    }
    write_entries(&dir, &entries)
}

/// The entries, and the notice when a damaged list was set aside.
fn journal_entries() -> Result<(Vec<Entry>, Option<Error>), Error> {
    let _g = JOURNAL.lock().unwrap_or_else(|e| e.into_inner());
    match journal_dir(false)? {
        Some(dir) => load(&dir),
        None => Ok((Vec::new(), None)),
    }
}

/// Records a remote about to be added (user scope only). When the journal is
/// full, entries nobody needs are swept first (not those of a Store that is
/// still running, nor this process's own); if it is still full the answer is
/// a plain error.
fn journal_add(
    scope: Scope,
    name: &str,
    url: &str,
    main_ref: &str,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    if scope != Scope::User {
        return Ok(());
    }
    let (pid, start) = own_identity();
    let entry = Entry {
        scope,
        name: name.to_string(),
        url: norm_url(url),
        main_ref: main_ref.to_string(),
        pid,
        start,
        fails: 0,
    };
    let (known, _) = journal_entries()?;
    if known.len() >= JOURNAL_MAX_ENTRIES && !known.iter().any(|e| e.same(&entry)) {
        let out = sweep_inner(scope, lock, cancel, false);
        log::info!("the list of added sources was full; swept: {out:?}");
        cancel.check()?;
    }
    update_journal(true, |v| {
        v.retain(|e| !e.same(&entry));
        if v.len() >= JOURNAL_MAX_ENTRIES {
            return Err(Error::Invalid(
                "too many added sources are waiting to be used; remove some of them first".into(),
            ));
        }
        v.push(entry);
        Ok(())
    })
}

/// Counts a failed sweep of `e`; at the limit the entry is dropped (true).
fn journal_note_failure(e: &Entry) -> Result<bool, Error> {
    let mut evicted = false;
    update_journal(false, |v| {
        if let Some(i) = v.iter().position(|x| x.same(e)) {
            v[i].fails += 1;
            if v[i].fails >= JOURNAL_MAX_FAILS {
                v.remove(i);
                evicted = true;
            }
        }
        Ok(())
    })?;
    Ok(evicted)
}

/// Forgets the pending entry for this remote: call it after a successful
/// install from it (it is then in use, not pending). Matching is by scope,
/// name and URL. Nothing to forget is fine. Needs no operation lock of its
/// own, but call it while the install's lock is held.
pub(crate) fn journal_clear(scope: Scope, name: &str, url: &str) -> Result<(), Error> {
    let url = norm_url(url);
    update_journal(false, |v| {
        v.retain(|e| !(e.scope == scope && e.name == name && norm_url(&e.url) == url));
        Ok(())
    })
}

/// Adds `repo` as a remote called `name`. libflatpak gets
/// [`FlatpakRepo::to_bytes`], never the original file. Refused: a name that
/// is already a remote ([`Error::RemoteNameTaken`]) and a URL that is already
/// a remote under any name ([`Error::RemoteExists`]); nothing is modified or
/// overwritten.
///
/// Blocking: run on a worker thread.
pub fn add_remote(
    scope: Scope,
    repo: &FlatpakRepo,
    name: &str,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    if https_url(&repo.url).is_none() {
        return Err(Error::Invalid(
            "the source's URL is not an https address".into(),
        ));
    }
    let bytes = repo.to_bytes().map_err(|e| Error::Invalid(e.to_string()))?;
    add_remote_bytes(scope, &norm_url(&repo.url), &bytes, name, lock, cancel)
}

/// The libflatpak-facing part of [`add_remote`] (tests call it with a
/// `file://` remote, which the parser refuses).
pub(crate) fn add_remote_bytes(
    scope: Scope,
    url: &str,
    bytes: &[u8],
    name: &str,
    _lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    cancel.check()?;
    if !valid_remote_name(name) {
        return Err(Error::Invalid("the source name is not valid".into()));
    }
    if reserved_name(name, url) {
        return Err(Error::Invalid("that source name is reserved".into()));
    }
    let remotes = remote_configs(scope, cancel)?;
    if let Some(r) = remotes.iter().find(|r| r.url == url) {
        return Err(Error::RemoteExists(text::clean(&r.name, 64)));
    }
    if remotes.iter().any(|r| r.name.eq_ignore_ascii_case(name)) {
        return Err(Error::RemoteNameTaken(name.to_string()));
    }
    let remote = libflatpak::Remote::from_file(name, &libflatpak::glib::Bytes::from(bytes))
        .map_err(|e| super::from_glib("read the source's description", &e, cancel))?;
    let inst = super::open_for_change(scope)?;
    inst.add_remote(&remote, false, Some(cancel.cancellable()))
        .map_err(|e| super::from_glib("add the source", &e, cancel))
}

/// libflatpak's progress callback of the catalog update: only signals that
/// something moved. It must not panic or block.
unsafe extern "C" fn beat(_status: *const c_char, _percent: c_uint, _est: i32, data: *mut c_void) {
    // SAFETY: `data` is the `Sender<Msg>` that `update_appstream` keeps alive
    // for the whole call.
    let tx = unsafe { &*(data as *const std::sync::mpsc::Sender<Msg>) };
    let _ = tx.send(Msg::Beat);
}

/// Refreshes `remote`'s AppStream catalog for the default architecture
/// (`flatpak_installation_update_appstream_full_sync`, which the safe
/// bindings lack: one unsafe call, nothing shelled out). A refresh that makes
/// no progress is stopped ([`Error::TimedOut`]): up to 300 s before the first
/// sign of progress, then 120 s without any. In the system installation with
/// `interactive` set there is no limit while a person may be at the password
/// prompt (cancelling still works).
///
/// `interactive` is true only when the user asked for this refresh in the
/// Store: only then may libflatpak ask for authentication (polkit). Anything
/// else must pass false.
///
/// Blocking (a network request, which `cancel` stops): run on a worker thread.
pub fn update_appstream(
    scope: Scope,
    remote: &str,
    interactive: bool,
    _lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    cancel.check()?;
    if !super::valid_remote(remote) {
        return Err(Error::Invalid("the remote name is not valid".into()));
    }
    let arch = libflatpak::default_arch()
        .map(|s| s.to_string())
        .unwrap_or_default();
    let (Ok(remote_c), Ok(arch_c)) = (CString::new(remote), CString::new(arch)) else {
        return Err(Error::Invalid("a name contains a null byte".into()));
    };
    let (res, _) = run_supervised(cancel, &mut |_| {}, |tx| {
        let inst = super::open_with(scope, !interactive)?;
        if interactive && scope == Scope::System {
            // A person may be at the polkit prompt: a silence is not a stall.
            let _ = tx.send(Msg::Phase(Phase::Quiet));
        }
        let mut changed: libflatpak::glib::ffi::gboolean = 0;
        let mut error: *mut libflatpak::glib::ffi::GError = std::ptr::null_mut();
        // SAFETY: all pointers are valid for the call: the installation and
        // the cancellable are kept alive by their owners, the strings by their
        // CStrings, `tx` by this frame (the callback only borrows it);
        // `changed` and `error` are our locals. On failure `error` is a new
        // GError we take over.
        let ok = unsafe {
            libflatpak::ffi::flatpak_installation_update_appstream_full_sync(
                inst.to_glib_none().0,
                remote_c.as_ptr(),
                arch_c.as_ptr(),
                Some(beat),
                &tx as *const _ as *mut c_void,
                &mut changed,
                cancel.cancellable().to_glib_none().0,
                &mut error,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        if error.is_null() {
            return Err(Error::Flatpak {
                action: "update the catalog",
                message: "no reason was given".into(),
            });
        }
        // SAFETY: `error` is a GError we own (set by the failed call).
        let e: libflatpak::glib::Error =
            unsafe { libflatpak::glib::translate::from_glib_full(error) };
        Err(super::from_glib("update the catalog", &e, cancel))
    })?;
    res.map_err(|e| explain(e, cancel))
}

#[cfg(test)]
mod tests {
    use super::super::lock::OperationLock;
    use super::super::testenv::*;
    use super::super::transaction::plan_install_ref;
    use super::super::transaction::{install, plan_install};
    use super::*;
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn names() -> Vec<String> {
        super::super::open(Scope::User)
            .unwrap()
            .list_remotes(None::<&libflatpak::gio::Cancellable>)
            .unwrap()
            .iter()
            .filter_map(|r| r.name().map(|n| n.to_string()))
            .collect()
    }

    fn repo_bytes(dir: &Path, url: &str) -> Vec<u8> {
        format!(
            "[Flatpak Repo]\nUrl={url}\nTitle=Added\n{}\n",
            gpg_line(dir, "test.flatpakrepo")
        )
        .into_bytes()
    }

    fn key_of(dir: &Path) -> GpgKey {
        GpgKey::from_bytes(std::fs::read(dir.join("key.gpg")).unwrap()).unwrap()
    }

    /// The test remote as a `.flatpakref` would describe it.
    fn file_ref(dir: &Path, key: Option<GpgKey>) -> FlatpakRef {
        FlatpakRef {
            name: "org.test.Hello".into(),
            branch: Some("stable".into()),
            url: format!("file://{}/repo/", dir.display()),
            title: Some("Hello".into()),
            comment: None,
            description: None,
            icon: None,
            homepage: None,
            is_runtime: false,
            key,
            runtime_repo: None,
            suggest_remote_name: "hello-origin".into(),
            collection_id: None,
            deploy_collection_id: None,
        }
    }

    fn cfg(name: &str) -> Option<RemoteCfg> {
        remote_configs(Scope::User, &CancelToken::new())
            .unwrap()
            .into_iter()
            .find(|r| r.name == name)
    }

    #[test]
    fn a_proposal_is_pure_and_mirrors_flatpaks_origin_remote() {
        let Some((dir, _g)) = guard() else { return };
        let r = file_ref(&dir, Some(key_of(&dir)));
        let p = RemoteProposal::from_ref(&r, "hello-origin".into()).unwrap();
        assert_eq!(p.name, "hello-origin");
        assert_eq!(p.url, norm_url(&r.url));
        assert!(p.noenumerate && p.prio == 0);
        assert_eq!(p.default_branch.as_deref(), Some("stable"));
        assert!(p.main_ref.starts_with("app/org.test.Hello/") && p.main_ref.ends_with("/stable"));
        assert_eq!(p.fingerprint(), r.key.as_ref().map(GpgKey::fingerprint));
        assert_eq!(p.title.as_deref(), Some("Hello"));
        let mut bad = r.clone();
        bad.name = "../x".into();
        assert!(RemoteProposal::from_ref(&bad, "x".into()).is_err());
    }

    #[test]
    fn the_new_remote_flow_adds_only_after_confirmation_and_can_be_taken_back() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let r = file_ref(&dir, Some(key_of(&dir)));

        // Resolving changes nothing.
        let res = resolve_ref_source(Scope::User, &r, &c).unwrap();
        assert!(names().is_empty());
        let RefSource::New(p) = res.source else {
            panic!("{res:?}")
        };
        assert_eq!(p.name, "hello-origin");

        // Confirmed: added, as flatpak's own origin remote.
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        assert_eq!(names(), vec!["hello-origin".to_string()]);
        let inst = super::super::open(Scope::User).unwrap();
        let rm = inst
            .remote_by_name("hello-origin", None::<&libflatpak::gio::Cancellable>)
            .unwrap();
        assert!(rm.is_gpg_verify() && rm.is_noenumerate() && rm.prio() == 0 && !rm.is_disabled());
        assert_eq!(rm.title().as_deref(), Some("Hello"));
        assert_eq!(rm.main_ref().as_deref(), Some(p.main_ref.as_str()));

        // The same URL is now used as it is, and the name is not taken twice.
        let again = resolve_ref_source(Scope::User, &r, &c).unwrap();
        assert_eq!(
            again.source,
            RefSource::Existing {
                remote: "hello-origin".into()
            }
        );
        assert_eq!(
            add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap_err(),
            Error::RemoteNameTaken("hello-origin".into())
        );
        let mut other = p.clone();
        other.name = "HELLO-ORIGIN".into();
        assert_eq!(
            add_ref_remote_in(Scope::User, &other, &lock, &c).unwrap_err(),
            Error::RemoteNameTaken("HELLO-ORIGIN".into())
        );
        other.name = "another".into();
        assert_eq!(
            add_ref_remote_in(Scope::User, &other, &lock, &c).unwrap_err(),
            Error::RemoteExists("hello-origin".into())
        );

        // The runtime is not in an origin remote's reach: planning stops
        // there, and a file's RuntimeRepo is only named, never followed.
        assert_eq!(
            plan_install(Scope::User, "hello-origin", &res.ref_, &c).unwrap_err(),
            Error::RuntimeNotFound
        );
        let mut with_repo = r.clone();
        with_repo.runtime_repo = Some("https://example.org/runtime.flatpakrepo".into());
        assert_eq!(
            plan_install_ref(Scope::User, "hello-origin", &with_repo, &c).unwrap_err(),
            Error::NeedsRuntimeRepo("https://example.org/runtime.flatpakrepo".into())
        );
        assert_eq!(names(), vec!["hello-origin".to_string()]);
        // With the runtime installed from another source, the plan works.
        let Some(tmp) = scratch("rt-link") else {
            return;
        };
        let link = tmp.join("repolink");
        std::os::unix::fs::symlink(dir.join("repo"), &link).unwrap();
        let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
        must(&[
            "remote-add",
            &gpg,
            "rt",
            &format!("file://{}/", link.display()),
        ]);
        must(&[
            "install",
            "-y",
            "--noninteractive",
            "rt",
            "org.test.Platform",
        ]);
        let plan = plan_install(Scope::User, "hello-origin", &res.ref_, &c).unwrap();
        assert!(plan.gpg_verified && plan.remote == "hello-origin");
        install(&plan, &lock, &c, |_| {}).unwrap();

        // In use: cannot be removed, and nothing changed.
        let e =
            remove_remote(Scope::User, "hello-origin", &p.url, &p.main_ref, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::InUse(ref v) if !v.is_empty()), "{e:?}");
        assert!(names().contains(&"hello-origin".to_string()));

        // A mismatched URL or name is refused.
        let e = remove_remote(
            Scope::User,
            "hello-origin",
            "https://other.example/repo",
            &p.main_ref,
            &lock,
            &c,
        )
        .unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        let e =
            remove_remote(Scope::User, "Hello-Origin", &p.url, &p.main_ref, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        let e = remove_remote(Scope::User, "../x", &p.url, &p.main_ref, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(names().contains(&"hello-origin".to_string()));

        // (flatpak itself drops an origin remote with the last ref from it.)
        flatpak(&["uninstall", "-y", "--noninteractive", "--all"]);
        must(&["remote-delete", "--force", "rt"]);
        std::fs::remove_dir_all(&tmp).unwrap();
        reset_empty();

        // The exact name and URL (a trailing slash does not matter) go.
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        remove_remote(
            Scope::User,
            "hello-origin",
            &format!("{}/", p.url),
            &p.main_ref,
            &lock,
            &c,
        )
        .unwrap();
        assert!(names().is_empty());

        // Declined: the proposal was never added, so there is nothing to undo.
        let res = resolve_ref_source(Scope::User, &r, &c).unwrap();
        assert!(matches!(res.source, RefSource::New(_)));
        assert!(names().is_empty());
    }

    fn journal_file() -> PathBuf {
        journal_dir_path().unwrap().join(JOURNAL_FILE)
    }

    fn journal_text() -> String {
        std::fs::read_to_string(journal_file()).unwrap_or_default()
    }

    fn proposal(dir: &Path) -> RemoteProposal {
        let r = file_ref(dir, Some(key_of(dir)));
        let RefSource::New(p) = resolve_ref_source(Scope::User, &r, &CancelToken::new())
            .unwrap()
            .source
        else {
            panic!()
        };
        p
    }

    #[test]
    fn an_unused_added_remote_is_journaled_and_swept() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let _ = std::fs::remove_file(journal_file());
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let p = proposal(&dir);
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        let text = journal_text();
        let (pid, start) = own_identity();
        assert_eq!(
            text,
            format!(
                "user\t{}\t{}\t{}\t{pid}\t{start}\t0\n",
                p.name,
                norm_url(&p.url),
                p.main_ref
            )
        );
        assert!(start != 0);
        use std::os::unix::fs::MetadataExt;
        let md = std::fs::metadata(journal_file()).unwrap();
        assert_eq!(md.mode() & 0o777, 0o600);
        let dmd = std::fs::metadata(journal_dir_path().unwrap()).unwrap();
        assert_eq!(dmd.mode() & 0o777, 0o700);
        // A refused add leaves no entry behind.
        assert!(add_ref_remote_in(Scope::User, &p, &lock, &c).is_err());
        assert_eq!(journal_text(), text);

        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.removed, vec![p.name.clone()], "{out:?}");
        assert!(out.kept.is_empty() && out.failed.is_empty() && out.journal_error.is_none());
        assert!(names().is_empty());
        assert!(!journal_file().exists());
        // Nothing left to do.
        assert_eq!(
            sweep_pending_remotes(Scope::User, &lock, &c),
            SweepOutcome::default()
        );

        // remove_remote clears its own entry.
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        remove_remote(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();
        assert!(!journal_file().exists() && names().is_empty());

        // A journaled remote that is already gone is forgotten.
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        must(&["remote-delete", "--force", &p.name]);
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.stale, vec![p.name.clone()], "{out:?}");
        assert!(!journal_file().exists());

        // One whose URL changed is not removed, and is forgotten.
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        must(&["remote-modify", "--url=file:///nonexistent/other", &p.name]);
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.stale, vec![p.name.clone()], "{out:?}");
        assert!(names().contains(&p.name));
        assert!(!journal_file().exists());
        reset_empty();
    }

    #[test]
    fn a_remote_in_use_is_kept_and_forgotten_by_the_sweep() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let _ = std::fs::remove_file(journal_file());
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let p = proposal(&dir);
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();

        let Some(tmp) = scratch("sweep-rt") else {
            return;
        };
        let link = tmp.join("repolink");
        std::os::unix::fs::symlink(dir.join("repo"), &link).unwrap();
        let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
        must(&[
            "remote-add",
            &gpg,
            "rt",
            &format!("file://{}/", link.display()),
        ]);
        must(&[
            "install",
            "-y",
            "--noninteractive",
            "rt",
            "org.test.Platform",
        ]);
        let plan = plan_install(Scope::User, &p.name, &p.main_ref, &c).unwrap();
        assert!(journal_text().contains(&p.name));
        install(&plan, &lock, &c, |_| {}).unwrap();
        // The install took the source off the pending list.
        assert!(!journal_file().exists(), "{}", journal_text());
        assert_eq!(
            sweep_pending_remotes(Scope::User, &lock, &c),
            SweepOutcome::default()
        );
        // Journaled again (a crash between the install and the clear): the
        // sweep sees it is in use and keeps it.
        journal_add(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.kept, vec![p.name.clone()], "{out:?}");
        assert!(out.removed.is_empty() && out.failed.is_empty());
        assert!(names().contains(&p.name));
        assert!(!journal_file().exists());

        // journal_clear for the install path: a no-op when nothing is there.
        journal_clear(Scope::User, &p.name, &p.url).unwrap();
        flatpak(&["uninstall", "-y", "--noninteractive", "--all"]);
        std::fs::remove_dir_all(&tmp).unwrap();
        reset_empty();
    }

    #[test]
    fn a_damaged_journal_is_ignored_and_a_symlinked_one_refused() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let p = proposal(&dir);
        let jf = journal_file();
        let jd = journal_dir_path().unwrap();
        std::fs::create_dir_all(&jd).unwrap();
        std::fs::set_permissions(&jd, PermissionsExt::from_mode(0o700)).unwrap();

        // Damaged: garbage, too large, too many entries, bad lines. The
        // damaged copy is kept, never overwritten, and the sweep says so.
        let bad_file = jd.join(JOURNAL_BAD);
        let dead = Entry {
            scope: Scope::User,
            name: "r".into(),
            url: "https://x.example/r".into(),
            main_ref: p.main_ref.clone(),
            pid: u32::MAX,
            start: 1,
            fails: 0,
        };
        let many: String = (0..=JOURNAL_MAX_ENTRIES)
            .map(|i| {
                Entry {
                    name: format!("r{i}"),
                    ..dead.clone()
                }
                .line()
            })
            .collect();
        let cases: Vec<Vec<u8>> = vec![
            b"\xff\xfe garbage".to_vec(),
            vec![b'a'; JOURNAL_MAX_BYTES + 1],
            many.into_bytes(),
            b"user\t../x\thttps://x.example/r\n".to_vec(),
            b"nowhere\tx\thttps://x.example/r\t\t1\t1\t0\n".to_vec(),
            b"system\tx\thttps://x.example/r\tapp/a.b/x86_64/m\t1\t1\t0\n".to_vec(),
            b"user\tx\n".to_vec(),
        ];
        for bad in &cases {
            let _ = std::fs::remove_file(&bad_file);
            std::fs::write(&jf, bad).unwrap();
            let out = sweep_pending_remotes(Scope::User, &lock, &c);
            assert!(out.journal_error.is_some(), "{out:?}");
            assert_eq!(std::fs::read(&bad_file).unwrap(), *bad);
            assert!(out.failed.is_empty() && out.removed.is_empty());
        }
        // The good lines of a damaged list are kept.
        let _ = std::fs::remove_file(&bad_file);
        let good = Entry {
            pid: std::process::id(),
            start: own_identity().1,
            ..dead.clone()
        };
        std::fs::write(&jf, format!("{}garbage\n", good.line())).unwrap();
        let (v, notice) = journal_entries().unwrap();
        assert_eq!(v, vec![good.clone()]);
        assert!(notice.is_some() && bad_file.exists());
        assert_eq!(journal_text(), good.line());
        std::fs::remove_file(&jf).unwrap();
        // Adding rewrites it cleanly.
        let _ = std::fs::remove_file(&bad_file);
        std::fs::write(&jf, b"\xff garbage").unwrap();
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        assert_eq!(journal_entries().unwrap().0.len(), 1);
        assert!(journal_text().starts_with(&format!("user\t{}\t", p.name)));
        assert_eq!(std::fs::read(&bad_file).unwrap(), b"\xff garbage");
        std::fs::remove_file(&bad_file).unwrap();
        remove_remote(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();

        // Symlinked journal: refused, nothing added, the target untouched.
        let target = jd.join("elsewhere");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &jf).unwrap();
        let e = add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "{e:?}");
        assert!(names().is_empty());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert!(
            matches!(out.journal_error, Some(Error::Io { .. })),
            "{out:?}"
        );
        assert!(journal_clear(Scope::User, &p.name, &p.url).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        std::fs::remove_file(&jf).unwrap();
        std::fs::remove_file(&target).unwrap();

        // A symlinked folder is refused too.
        let real = jd.with_file_name("atlas-store-real");
        std::fs::rename(&jd, &real).unwrap();
        std::os::unix::fs::symlink(&real, &jd).unwrap();
        let e = add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Io { .. }), "{e:?}");
        assert!(names().is_empty());
        std::fs::remove_file(&jd).unwrap();
        std::fs::rename(&real, &jd).unwrap();
    }

    /// A remote added by hand through libflatpak (so any name can be used).
    fn raw_add(p: &RemoteProposal) {
        super::super::open_for_change(Scope::User)
            .unwrap()
            .add_remote(
                &build_remote(p),
                false,
                None::<&libflatpak::gio::Cancellable>,
            )
            .unwrap();
    }

    #[test]
    fn the_sweep_only_removes_what_the_store_made() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let _ = std::fs::remove_file(journal_file());
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let p = proposal(&dir);

        // A journaled well-known name, even with the Store's settings and its
        // own URL, is kept.
        for name in ["flathub", "FlatHub"] {
            let mut f = p.clone();
            f.name = name.into();
            raw_add(&f);
            journal_add(Scope::User, name, &f.url, &f.main_ref, &lock, &c).unwrap();
            let out = sweep_pending_remotes(Scope::User, &lock, &c);
            assert_eq!(out.kept, vec![name.to_string()], "{out:?}");
            assert!(out.removed.is_empty() && out.failed.is_empty());
            assert!(names().contains(&name.to_string()));
            assert!(!journal_file().exists());
            let e = remove_remote(Scope::User, name, &f.url, &f.main_ref, &lock, &c).unwrap_err();
            assert!(matches!(e, Error::Invalid(_)), "{e:?}");
            reset_empty();
        }

        // An enumerable remote is kept.
        let en = libflatpak::Remote::new(&p.name);
        en.set_url(&p.url);
        en.set_gpg_verify(false);
        en.set_noenumerate(false);
        en.set_main_ref(&p.main_ref);
        super::super::open_for_change(Scope::User)
            .unwrap()
            .add_remote(&en, false, None::<&libflatpak::gio::Cancellable>)
            .unwrap();
        journal_add(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.kept, vec![p.name.clone()], "{out:?}");
        assert!(names().contains(&p.name) && !journal_file().exists());
        reset_empty();

        // A wrong main ref is kept, by the sweep and by remove_remote.
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        let other = "app/org.other.App/x86_64/stable";
        journal_clear(Scope::User, &p.name, &p.url).unwrap();
        journal_add(Scope::User, &p.name, &p.url, other, &lock, &c).unwrap();
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.kept, vec![p.name.clone()], "{out:?}");
        assert!(names().contains(&p.name));
        let e = remove_remote(Scope::User, &p.name, &p.url, other, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(names().contains(&p.name));
        // A priority other than 0 is kept too.
        must(&["remote-modify", "--prio=5", &p.name]);
        let e = remove_remote(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        must(&["remote-modify", "--prio=0", &p.name]);
        remove_remote(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();
        assert!(names().is_empty());
        reset_empty();
    }

    #[test]
    fn a_running_stores_entry_is_left_alone_and_failures_are_counted() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let _ = std::fs::remove_file(journal_file());
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let p = proposal(&dir);
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        let mine = journal_entries().unwrap().0.remove(0);

        // Written by another running process (pid 1, its real start time).
        let init = Entry {
            pid: 1,
            start: proc_start(1).expect("pid 1"),
            ..mine.clone()
        };
        std::fs::write(journal_file(), init.line()).unwrap();
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out, SweepOutcome::default());
        assert!(names().contains(&p.name));
        assert_eq!(journal_text(), init.line());
        // The same pid with another start time is a reused number: swept.
        let reused = Entry {
            start: init.start + 1,
            ..init.clone()
        };
        std::fs::write(journal_file(), reused.line()).unwrap();
        let out = sweep_pending_remotes(Scope::User, &lock, &c);
        assert_eq!(out.removed, vec![p.name.clone()], "{out:?}");
        assert!(!journal_file().exists());

        // Failures are counted, and the entry goes at the third.
        std::fs::write(journal_file(), mine.line()).unwrap();
        assert!(!journal_note_failure(&mine).unwrap());
        assert!(journal_text().ends_with("\t1\n"), "{}", journal_text());
        assert!(!journal_note_failure(&mine).unwrap());
        assert!(journal_note_failure(&mine).unwrap());
        assert!(!journal_file().exists());
        // Nothing to count is fine.
        assert!(!journal_note_failure(&mine).unwrap());
        reset_empty();
    }

    #[test]
    fn a_full_journal_sweeps_first_and_then_refuses_plainly() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let p = proposal(&dir);
        let fill = |pid: u32, start: u64| -> String {
            (0..JOURNAL_MAX_ENTRIES)
                .map(|i| {
                    Entry {
                        scope: Scope::User,
                        name: format!("old{i}"),
                        url: format!("https://x{i}.example/r"),
                        main_ref: p.main_ref.clone(),
                        pid,
                        start,
                        fails: 0,
                    }
                    .line()
                })
                .collect()
        };
        // Of dead writers, for remotes that are gone: swept, then added.
        std::fs::write(journal_file(), fill(u32::MAX, 1)).unwrap();
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        assert_eq!(journal_entries().unwrap().0.len(), 1);
        remove_remote(Scope::User, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();

        // Of a running Store: a plain error, and nothing is added.
        std::fs::write(journal_file(), fill(1, proc_start(1).unwrap())).unwrap();
        let e = add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        assert!(names().is_empty());
        assert_eq!(journal_entries().unwrap().0.len(), JOURNAL_MAX_ENTRIES);
        std::fs::remove_file(journal_file()).unwrap();

        // The system scope is never journaled.
        journal_add(Scope::System, &p.name, &p.url, &p.main_ref, &lock, &c).unwrap();
        assert!(!journal_file().exists());
    }

    #[test]
    fn only_a_certain_failure_forgets_the_entry() {
        for e in [
            Error::RemoteNameTaken("x".into()),
            Error::RemoteExists("x".into()),
            Error::Invalid("x".into()),
        ] {
            assert!(certainly_not_added(&e), "{e:?}");
        }
        for e in [
            Error::Cancelled,
            Error::TimedOut,
            Error::Flatpak {
                action: "x",
                message: "y".into(),
            },
        ] {
            assert!(!certainly_not_added(&e), "{e:?}");
        }
        // And the appstream mapping: a cancel that arrives as another error.
        let c = CancelToken::new();
        let other = || Error::Flatpak {
            action: "x",
            message: "y".into(),
        };
        assert!(matches!(explain(other(), &c), Error::Flatpak { .. }));
        c.cancel();
        assert_eq!(explain(other(), &c), Error::Cancelled);
        let t = CancelToken::new();
        t.cancel_timed_out();
        assert_eq!(explain(other(), &t), Error::TimedOut);
    }

    #[test]
    fn an_unsigned_proposal_adds_an_unverified_remote() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let r = file_ref(&dir, None);
        let RefSource::New(p) = resolve_ref_source(Scope::User, &r, &c).unwrap().source else {
            panic!()
        };
        assert!(p.key.is_none() && p.fingerprint().is_none());
        add_ref_remote_in(Scope::User, &p, &lock, &c).unwrap();
        assert!(!cfg("hello-origin").unwrap().gpg);
        // The https gate on the public entry point.
        let e = add_ref_remote(Scope::User, &p, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
    }

    #[test]
    fn reserved_names_are_not_used_for_files() {
        assert!(reserved_name("flathub", "https://evil.example/repo"));
        assert!(reserved_name("FlatHub", "https://evil.example/repo"));
        assert!(!reserved_name("flathub", "https://dl.flathub.org/repo/"));
        assert!(reserved_name("kde", "https://dl.flathub.org/repo"));
        assert!(!reserved_name("hello-origin", "https://x.example/r"));
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let mut r = file_ref(&dir, None);
        r.suggest_remote_name = "flathub".into();
        let RefSource::New(p) = resolve_ref_source(Scope::User, &r, &c).unwrap().source else {
            panic!()
        };
        assert_eq!(p.name, "org.test.Hello-origin");
        let mut bad = p.clone();
        bad.name = "gnome".into();
        let e = add_ref_remote_in(Scope::User, &bad, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");
        let mut odd = p.clone();
        odd.prio = 5;
        assert!(add_ref_remote_in(Scope::User, &odd, &lock, &c).is_err());
        odd = p.clone();
        odd.default_branch = Some("../x".into());
        assert!(add_ref_remote_in(Scope::User, &odd, &lock, &c).is_err());
        assert!(names().is_empty());
    }

    #[test]
    fn a_taken_name_is_numbered_and_disabled_remotes_are_ignored() {
        let Some((dir, _g)) = guard() else { return };
        reset(&dir);
        let c = CancelToken::new();
        // Another URL under the name the file suggests (case differs).
        let mut r = file_ref(&dir, Some(key_of(&dir)));
        r.url = format!("file://{}/other/", dir.display());
        r.suggest_remote_name = "TEST".into();
        let RefSource::New(p) = resolve_ref_source(Scope::User, &r, &c).unwrap().source else {
            panic!()
        };
        assert_eq!(p.name, "TEST-2");
        // A disabled remote with the file's URL is not used.
        must(&["remote-modify", "--disable", "test"]);
        let r = file_ref(&dir, Some(key_of(&dir)));
        assert!(matches!(
            resolve_ref_source(Scope::User, &r, &c).unwrap().source,
            RefSource::New(_)
        ));
    }

    #[test]
    fn a_remote_with_the_same_url_is_used_as_is_and_the_files_key_never_reaches_libflatpak() {
        let Some((dir, _g)) = guard() else { return };
        reset(&dir);
        let c = CancelToken::new();
        let before = cfg("test").unwrap();
        let Some(tmp) = scratch("second-key") else {
            return;
        };
        let gpg = tmp.join("gpg");
        std::fs::create_dir_all(&gpg).unwrap();
        std::fs::set_permissions(&gpg, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let ok = Command::new("gpg")
            .args(["--homedir"])
            .arg(&gpg)
            .args([
                "--batch",
                "--passphrase",
                "",
                "--quick-gen-key",
                "Other <o@atlas.invalid>",
                "ed25519",
                "sign",
                "never",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let out = Command::new("gpg")
            .args(["--homedir"])
            .arg(&gpg)
            .args(["--export"])
            .output()
            .unwrap();
        let other = GpgKey::from_bytes(out.stdout).unwrap();
        assert_ne!(other.fingerprint(), key_of(&dir).fingerprint());

        let r = file_ref(&dir, Some(other));
        let res = resolve_ref_source(Scope::User, &r, &c).unwrap();
        assert_eq!(
            res.source,
            RefSource::Existing {
                remote: "test".into()
            }
        );
        // Planning from the existing remote installs from it, verified with
        // the remote's own key, and the remote stays as it was.
        let plan = plan_install(Scope::User, "test", &res.ref_, &c).unwrap();
        assert!(plan.gpg_verified);
        assert_eq!(cfg("test").unwrap(), before);
        assert_eq!(names(), vec!["test".to_string()]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn adds_a_remote_but_never_a_duplicate_or_an_overwrite() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let url = format!("file://{}/repo/", dir.display());
        let bytes = repo_bytes(&dir, &url);
        let n = norm_url(&url);

        add_remote_bytes(Scope::User, &n, &bytes, "added", &lock, &c).unwrap();
        assert_eq!(names(), vec!["added".to_string()]);

        // Same URL, any name.
        let e = add_remote_bytes(Scope::User, &n, &bytes, "other", &lock, &c).unwrap_err();
        assert_eq!(e, Error::RemoteExists("added".into()));
        let e = add_remote_bytes(Scope::User, &n, &bytes, "added", &lock, &c).unwrap_err();
        assert_eq!(e, Error::RemoteExists("added".into()));
        // Same name, other URL: refused, and the remote is untouched.
        let url2 = format!("file://{}/other/", dir.display());
        let e = add_remote_bytes(
            Scope::User,
            &norm_url(&url2),
            &repo_bytes(&dir, &url2),
            "added",
            &lock,
            &c,
        )
        .unwrap_err();
        assert_eq!(e, Error::RemoteNameTaken("added".into()));
        let e = add_remote_bytes(
            Scope::User,
            &norm_url(&url2),
            &repo_bytes(&dir, &url2),
            "ADDED",
            &lock,
            &c,
        )
        .unwrap_err();
        assert_eq!(e, Error::RemoteNameTaken("ADDED".into()));
        assert_eq!(names(), vec!["added".to_string()]);
        assert_eq!(cfg("added").unwrap().url, n);

        // Bad names, and a cancelled token.
        for bad in ["", "a b", "-x", ".x", "a/b"] {
            let e = add_remote_bytes(Scope::User, "file:///z", &bytes, bad, &lock, &c).unwrap_err();
            assert!(matches!(e, Error::Invalid(_)), "{bad}: {e:?}");
        }
        let gone = CancelToken::new();
        gone.cancel();
        assert_eq!(
            add_remote_bytes(Scope::User, "file:///z", &bytes, "zzz", &lock, &gone).unwrap_err(),
            Error::Cancelled
        );
    }

    #[test]
    fn updates_a_remotes_appstream_and_can_be_cancelled() {
        let Some((dir, _g)) = guard() else { return };
        reset(&dir);
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        update_appstream(Scope::User, "test", false, &lock, &c).unwrap();
        let arch = libflatpak::default_arch().unwrap();
        let p = Path::new(&std::env::var("FLATPAK_USER_DIR").unwrap())
            .join(format!("appstream/test/{arch}/active/appstream.xml.gz"));
        assert!(p.is_file(), "{}", p.display());
        let e = update_appstream(Scope::User, "nosuchremote", false, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Flatpak { .. }), "{e:?}");
        let e = update_appstream(Scope::User, "../x", false, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{e:?}");

        // Cancelled before: nothing runs. Cancelled during: Cancelled, or Ok
        // when the (local) refresh was already done.
        let gone = CancelToken::new();
        gone.cancel();
        assert_eq!(
            update_appstream(Scope::User, "test", false, &lock, &gone).unwrap_err(),
            Error::Cancelled
        );
        for ms in [0, 1, 5] {
            let c = CancelToken::new();
            let c2 = c.clone();
            let h = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(ms));
                c2.cancel();
            });
            let r = update_appstream(Scope::User, "test", false, &lock, &c);
            h.join().unwrap();
            assert!(matches!(r, Ok(()) | Err(Error::Cancelled)), "{r:?}");
        }
    }

    #[test]
    fn a_dead_writers_temporary_file_goes_and_a_live_ones_stays() {
        let Some(dir) = scratch("dead-temps") else {
            return;
        };
        // Above the kernel's pid limit (2^22): never a live process.
        let dead = format!("{JOURNAL_FILE}.999999999.0.0000000000000000.tmp");
        let live = format!(
            "{JOURNAL_FILE}.{}.0.0000000000000000.tmp",
            std::process::id()
        );
        let other = "unrelated.999999999.tmp";
        for n in [dead.as_str(), live.as_str(), other] {
            std::fs::write(dir.join(n), "x").unwrap();
        }
        remove_dead_temps(&File::open(&dir).unwrap());
        assert!(!dir.join(&dead).exists());
        assert!(dir.join(&live).exists());
        assert!(dir.join(other).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
