//! Changing an installation: plan an install, install exactly what was
//! planned, uninstall, and remove unused runtimes.
//!
//! **What runs is what the user confirmed.** A plan is a dry run that lists
//! every operation libflatpak would do (ref, commit, remote, sizes) and the
//! app's metadata; it changes nothing. [`install`] first checks that every
//! source of the plan (the app's and the runtimes') is still as planned (same
//! URL, same signing, enabled), then
//! runs a fresh transaction and compares its operations with the plan in
//! libflatpak's `ready` step, before anything is downloaded: any difference
//! stops it with [`Error::PlanChanged`], which says what differs, and the UI
//! plans and asks again.
//!
//! Every function that changes an installation takes `&OperationLock` as proof
//! that the caller holds the Updater's and the Flatpak locks, a
//! [`CancelToken`] and a progress callback, and blocks: run it on a worker
//! thread. The transaction runs on a thread of its own inside the call (the
//! libflatpak objects never leave it) and its progress comes back to the
//! calling thread, which calls `progress`; a panic there cancels the
//! transaction and is re-raised once it has stopped. A transaction that makes
//! no progress is cancelled and ends as [`Error::TimedOut`] (300 s before the
//! first sign of life, 120 s without a byte while downloading; deploying, an
//! uninstall and a polkit or sign-in prompt are only reported to `progress`
//! as `not_responding` after 120 s, and in the user scope, where no prompt is
//! possible, cancelled after 30 min: see `supervise`). A cancelled run that
//! does not return is waited for, never abandoned, since the caller's lock
//! must stay held while libflatpak may still be writing. The dry run of
//! [`plan_install`] is supervised the same way. A run stops with
//! [`Error::SentTooMuch`] if more bytes arrive than the plan announced (plus a
//! quarter and 64 MiB; the sizes come from the same remote that sends the
//! bytes, so this is defence in depth and no bound against a hostile remote).
//! Before any authentication prompt (`ready-pre-auth`)
//! and again at `ready`, the operations are compared with the plan.
//!
//! **Partial state.** libflatpak commits one operation at a time. When an
//! install is cancelled, fails or times out after some operations finished
//! (a runtime, say), those stay installed and the error is
//! [`Error::Partial`] naming them (also logged at info); an operation that
//! was running is rolled back by libflatpak. Nothing is half-installed.
//!
//! System scope needs libflatpak's system helper and polkit, which the dev
//! container cannot run: it is covered by the same code but not by tests.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use libflatpak::glib::{self, prelude::*, translate::ToGlibPtr};
use libflatpak::prelude::*;

use super::installed::list_unused_raw;
use super::lock::OperationLock;
use super::supervise::{Limits, Msg, OpPhase, Phase, run_supervised, run_supervised_with};
use super::{
    CancelToken, Error, InstalledRef, ListError, ListErrorKind, PlanChange, RefKind, Scope,
};
use crate::launch::https_url;
use crate::text;

/// Largest metadata kept per operation (1 MiB).
const METADATA_MAX: usize = 1 << 20;
/// Progress is passed on at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// Slack on top of a plan's download size before a run is stopped for
/// transferring too much (defence in depth only: see `Mode::byte_cap`).
const BYTE_SLACK: u64 = 64 << 20;
/// Deepest folder the app-data removal goes into.
const MAX_DEPTH: u32 = 256;
/// Most refs named in a plan difference.
const DIFF_MAX: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Install,
    Update,
    Uninstall,
}

impl OpKind {
    fn word(self) -> &'static str {
        match self {
            OpKind::Install => "install",
            OpKind::Update => "update",
            OpKind::Uninstall => "uninstall",
        }
    }
}

/// One thing libflatpak will do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedOp {
    pub kind: OpKind,
    /// `app/ID/arch/branch` or `runtime/ID/arch/branch`.
    pub ref_: String,
    pub commit: String,
    pub remote: String,
    pub download_size: u64,
    pub installed_size: u64,
    /// The remote verifies its commits with a GPG key. Set by the plan (the
    /// operations a run reads back leave it false, and it is not compared):
    /// the dialog marks a runtime from an unsigned source.
    pub signed: bool,
}

/// A remote as it was when the plan was made: [`install`] stops with
/// [`Error::PlanChanged`] if it is not like this any more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRemote {
    pub name: String,
    /// Normalized.
    pub url: String,
    pub gpg_verified: bool,
    pub disabled: bool,
}

/// What an install would do, to show the user and to hold [`install`] to.
#[derive(Clone, Debug)]
pub struct InstallPlan {
    pub scope: Scope,
    /// The requested ref.
    pub ref_: String,
    /// The remote the ref comes from.
    pub remote: String,
    /// Normalized.
    pub remote_url: String,
    /// Whether the remote's commits are verified with a GPG key.
    pub gpg_verified: bool,
    /// Every distinct remote an operation comes from (the app's own and the
    /// ones libflatpak chose for runtimes), sorted by name. All of them are
    /// checked again by [`install`].
    pub remotes: Vec<PlanRemote>,
    pub ops: Vec<PlannedOp>,
    pub download_total: u64,
    pub installed_total: u64,
    /// The requested ref's `metadata`, for `Permissions::from_metadata`.
    pub metadata: Vec<u8>,
    /// Runtimes (and extensions) that will be installed besides the app, each
    /// with the remote libflatpak chose for it (which may not be `remote`).
    pub new_runtimes: Vec<PlannedOp>,
}

/// What an install did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// Every ref installed or updated.
    pub refs: Vec<String>,
    /// Non-fatal problems libflatpak reported, and any unplanned change to
    /// the sources during the run.
    pub warnings: Vec<String>,
}

/// What happened to an app's data folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataResult {
    /// Deleting the data was not asked for.
    NotRequested,
    /// There was no data.
    NoData,
    Deleted,
    /// Kept on purpose, with the reason (another branch is still installed).
    Kept(String),
    /// The app is uninstalled but its data could not be deleted.
    Failed(String),
}

/// What an uninstall did: the uninstall itself and the data are separate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uninstalled {
    /// Every ref removed (an app's add-ons go with it).
    pub removed: Vec<String>,
    pub data: DataResult,
    pub warnings: Vec<String>,
}

/// Progress of the running operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    /// 1-based index of the running operation.
    pub op: usize,
    pub ops: usize,
    pub ref_: String,
    pub kind: OpKind,
    pub percent: u8,
    /// Bytes transferred by the running operation so far. Always 0 in the
    /// `Done` report (status "Done", percent 100) that ends each operation,
    /// and for an uninstall.
    pub bytes: u64,
    pub status: String,
    /// The operation has not answered for a long time (a deploy or a prompt
    /// that is taking very long, or a cancel that has not taken effect). The
    /// fields besides `status` are those of the last report; the wait goes on
    /// and the lock stays held while libflatpak may still be writing.
    ///
    /// For whoever shows this:
    /// - **Cancel must stay enabled** while this is set: a notice never means
    ///   the operation cannot be stopped, and a person waiting on a password
    ///   prompt or a stuck deploy must be able to cancel.
    /// - A notice can come before any real report, with `op` 0, `ops` 0 and an
    ///   empty `ref_` (the call was silent from the start); show it without
    ///   the operation's name then.
    /// - In the user scope a quiet phase is cancelled by the watchdog after 30
    ///   minutes (the run ends as `TimedOut`); the system scope only ever
    ///   gets notices.
    pub not_responding: bool,
}

impl Progress {
    /// A "not responding" notice, carrying the last report if there was one.
    pub(crate) fn not_responding(last: Option<&Progress>) -> Progress {
        let mut p = last.cloned().unwrap_or(Progress {
            op: 0,
            ops: 0,
            ref_: String::new(),
            kind: OpKind::Install,
            percent: 0,
            bytes: 0,
            status: String::new(),
            not_responding: false,
        });
        p.status = "Not responding".into();
        p.not_responding = true;
        p
    }
}

// ---- refs and URLs ----

/// Splits `app/ID/arch/branch` or `runtime/ID/arch/branch`, checking each part.
pub(crate) fn check_ref(s: &str) -> Result<(RefKind, &str, &str, &str), Error> {
    let bad =
        || Error::Invalid("the ref is not app/ID/arch/branch or runtime/ID/arch/branch".into());
    let mut p = s.split('/');
    let (Some(kind), Some(id), Some(arch), Some(branch), None) =
        (p.next(), p.next(), p.next(), p.next(), p.next())
    else {
        return Err(bad());
    };
    let kind = match kind {
        "app" => RefKind::App,
        "runtime" => RefKind::Runtime,
        _ => return Err(bad()),
    };
    if !text::valid_id(id) || !super::valid_arch(arch) || !super::valid_branch(branch) {
        return Err(bad());
    }
    Ok((kind, id, arch, branch))
}

/// A URL in the form used to compare remotes: through the launch policy when
/// it passes, else as given, without a trailing slash.
pub(crate) fn norm_url(s: &str) -> String {
    let u = https_url(s).unwrap_or_else(|| super::scrub_word(s));
    u.trim_end_matches('/').to_string()
}

// ---- sources ----

/// A remote's settings that matter to a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteCfg {
    pub name: String,
    pub url: String,
    pub gpg: bool,
    pub disabled: bool,
}

pub(crate) fn remote_configs(scope: Scope, cancel: &CancelToken) -> Result<Vec<RemoteCfg>, Error> {
    let inst = super::open(scope)?;
    let mut v: Vec<RemoteCfg> = inst
        .list_remotes(Some(cancel.cancellable()))
        .map_err(|e| super::from_glib("list the sources", &e, cancel))?
        .iter()
        .map(|r| RemoteCfg {
            name: text::clean(r.name().as_deref().unwrap_or_default(), 255),
            url: norm_url(&text::clean(r.url().as_deref().unwrap_or_default(), 500)),
            gpg: r.is_gpg_verify(),
            disabled: r.is_disabled(),
        })
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

fn changed(source: String) -> Error {
    Error::PlanChanged(PlanChange {
        source: Some(source),
        ..PlanChange::default()
    })
}

impl InstallPlan {
    /// The remotes to check: the recorded ones, and the app's own as the plan
    /// states it if a hand-made plan left it out.
    fn sources(&self) -> Vec<PlanRemote> {
        let mut v = self.remotes.clone();
        if !v.iter().any(|r| r.name == self.remote) {
            v.push(PlanRemote {
                name: self.remote.clone(),
                url: self.remote_url.clone(),
                gpg_verified: self.gpg_verified,
                disabled: false,
            });
        }
        v
    }
}

/// Every remote of the plan (the app's and the runtimes') must still exist as
/// planned: same address, same signing, still enabled.
fn check_plan_source(plan: &InstallPlan, remotes: &[RemoteCfg]) -> Result<(), Error> {
    for want in plan.sources() {
        let Some(r) = remotes.iter().find(|r| r.name == want.name) else {
            return Err(changed(format!("the source \"{}\" is gone", want.name)));
        };
        if r.url != want.url {
            return Err(changed(format!(
                "the source \"{}\" now has another address",
                r.name
            )));
        }
        if r.gpg != want.gpg_verified {
            return Err(changed(format!(
                "the signature check of \"{}\" changed",
                r.name
            )));
        }
        if r.disabled || r.disabled != want.disabled {
            return Err(changed(format!("the source \"{}\" is disabled", r.name)));
        }
    }
    Ok(())
}

// ---- observing a transaction ----

struct Observed {
    op: PlannedOp,
    metadata: Vec<u8>,
    /// Refs of the operations this one is related to (an add-on's app).
    related: Vec<String>,
}

fn op_kind(t: libflatpak::TransactionOperationType) -> Option<OpKind> {
    match t {
        libflatpak::TransactionOperationType::Install => Some(OpKind::Install),
        libflatpak::TransactionOperationType::Update => Some(OpKind::Update),
        libflatpak::TransactionOperationType::Uninstall => Some(OpKind::Uninstall),
        _ => None,
    }
}

/// The refs of the operations `op` is related to. The binding's
/// `related_to_ops` dereferences the array unchecked, but libflatpak returns
/// NULL when there are none (which aborts the process), so this reads the
/// `GPtrArray` itself.
fn related_refs(op: &libflatpak::TransactionOperation) -> Vec<String> {
    // SAFETY: the call returns a GPtrArray owned by `op` (transfer none) or
    // NULL; its `len` entries are TransactionOperation pointers, each of
    // which is only borrowed (from_glib_borrow) while `op` is alive.
    unsafe {
        let arr =
            libflatpak::ffi::flatpak_transaction_operation_get_related_to_ops(op.to_glib_none().0);
        if arr.is_null() {
            return Vec::new();
        }
        let arr = &*arr;
        let mut out = Vec::new();
        for i in 0..arr.len as usize {
            let p = *arr.pdata.add(i) as *mut libflatpak::ffi::FlatpakTransactionOperation;
            if p.is_null() {
                continue;
            }
            let o: glib::translate::Borrowed<libflatpak::TransactionOperation> =
                glib::translate::from_glib_borrow(p);
            if let Some(r) = o.get_ref() {
                out.push(text::clean(&r, 255));
            }
        }
        out
    }
}

/// Checks and copies one operation from libflatpak (all of it untrusted).
fn observe(op: &libflatpak::TransactionOperation) -> Result<Observed, Error> {
    let bad = |what: &str| Error::Invalid(format!("libflatpak planned an operation with {what}"));
    let kind =
        op_kind(op.operation_type()).ok_or_else(|| bad("a kind the Store does not handle"))?;
    let ref_ = op.get_ref().map(|s| s.to_string()).unwrap_or_default();
    check_ref(&ref_).map_err(|_| bad("an invalid ref"))?;
    let remote = op.remote().map(|s| s.to_string()).unwrap_or_default();
    if !(remote.is_empty() && kind == OpKind::Uninstall) && !super::valid_remote(&remote) {
        return Err(bad("an invalid remote name"));
    }
    let commit = op.commit().map(|s| s.to_string()).unwrap_or_default();
    if !(commit.is_empty() && kind == OpKind::Uninstall) && !super::valid_commit(&commit) {
        return Err(bad("an invalid commit"));
    }
    let metadata = match op.metadata() {
        Some(k) => {
            let data = k.to_data();
            if data.len() > METADATA_MAX {
                return Err(Error::TooLarge("operation metadata"));
            }
            data.as_bytes().to_vec()
        }
        None => Vec::new(),
    };
    let related = related_refs(op);
    Ok(Observed {
        op: PlannedOp {
            kind,
            ref_,
            commit,
            remote,
            download_size: op.download_size(),
            installed_size: op.installed_size(),
            signed: false,
        },
        metadata,
        related,
    })
}

type OpKey = (String, u8, String, String);

fn key(o: &PlannedOp) -> OpKey {
    let rank = match o.kind {
        OpKind::Install => 0,
        OpKind::Update => 1,
        OpKind::Uninstall => 2,
    };
    (o.ref_.clone(), rank, o.commit.clone(), o.remote.clone())
}

fn shown(o: &PlannedOp) -> String {
    format!(
        "{} {} ({}, from {})",
        o.kind.word(),
        o.ref_,
        o.commit.chars().take(12).collect::<String>(),
        o.remote
    )
}

/// Same operations: refs, kinds, commits and remotes (sizes are derived).
fn same_ops(a: &[PlannedOp], b: &[PlannedOp]) -> bool {
    let mut x: Vec<OpKey> = a.iter().map(key).collect();
    let mut y: Vec<OpKey> = b.iter().map(key).collect();
    x.sort();
    y.sort();
    x == y
}

/// What differs between the plan and what libflatpak would do now.
fn diff_ops(plan: &[PlannedOp], now: &[PlannedOp]) -> PlanChange {
    let pk: HashSet<OpKey> = plan.iter().map(key).collect();
    let nk: HashSet<OpKey> = now.iter().map(key).collect();
    let list = |ops: &[PlannedOp], other: &HashSet<OpKey>| -> Vec<String> {
        let mut v: Vec<String> = ops
            .iter()
            .filter(|o| !other.contains(&key(o)))
            .map(shown)
            .collect();
        v.sort();
        v.truncate(DIFF_MAX);
        v
    };
    PlanChange {
        missing: list(plan, &nk),
        unexpected: list(now, &pk),
        source: None,
    }
}

// ---- driving a transaction ----

enum Job {
    Install { remote: String, ref_: String },
    Uninstall(Vec<String>),
}

enum Mode {
    /// Collect the operations at `ready`, then abort.
    Dry,
    /// Run only if the operations at `ready` are exactly these, and the
    /// requested ref comes from `remote`.
    Install {
        ops: Vec<PlannedOp>,
        main_ref: String,
        remote: String,
    },
    /// Run only if every operation is an uninstall and the ref is among them.
    UninstallMain(String),
    /// Run only if every operation is an uninstall of one of these refs or
    /// related to one (an extension of a wanted runtime).
    UninstallSet(Vec<String>),
}

impl Mode {
    fn copy(&self) -> Mode {
        match self {
            Mode::Dry => Mode::Dry,
            Mode::Install {
                ops,
                main_ref,
                remote,
            } => Mode::Install {
                ops: ops.clone(),
                main_ref: main_ref.clone(),
                remote: remote.clone(),
            },
            Mode::UninstallMain(r) => Mode::UninstallMain(r.clone()),
            Mode::UninstallSet(r) => Mode::UninstallSet(r.clone()),
        }
    }

    /// The most bytes a run of this mode may transfer: what the plan announced
    /// plus a quarter and 64 MiB of slack (HTTP framing, a retried object).
    /// Only an install downloads.
    ///
    /// This is defence in depth, not a bound against a hostile remote: the
    /// announced sizes come from the same remote that sends the bytes, so a
    /// remote that lies can announce a large size and send it. It catches a
    /// bug, a broken mirror or a changed summary; the stall limits, the user's
    /// confirmation of the sizes and the disk's own free space are the other
    /// layers.
    fn byte_cap(&self) -> u64 {
        let Mode::Install { ops, .. } = self else {
            return u64::MAX;
        };
        let total = ops
            .iter()
            .fold(0u64, |a, o| a.saturating_add(o.download_size));
        total.saturating_add(total / 4).saturating_add(BYTE_SLACK)
    }
}

/// Hooks that let the integration tests make a run take a branch that is
/// otherwise a matter of timing (a cancel at the right moment, a source that
/// sends too much). Nothing sets them outside tests; each setter returns a
/// guard that puts everything back when dropped. They are process-wide, so a
/// test using one must hold the tests' serial lock.
///
/// Only built for tests (`cfg(test)`, or the `test-hooks` feature that the
/// integration tests turn on through a dev-dependency): a release build has
/// the inert stand-in below, so nothing outside the crate can reach a hook.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    static EXTRA_BYTES: AtomicU64 = AtomicU64::new(0);
    static CANCEL_IN_READY: AtomicBool = AtomicBool::new(false);
    static CANCEL_AFTER_DONE: AtomicUsize = AtomicUsize::new(0);

    /// Puts every hook back when dropped.
    #[must_use]
    pub struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            EXTRA_BYTES.store(0, Ordering::SeqCst);
            CANCEL_IN_READY.store(false, Ordering::SeqCst);
            CANCEL_AFTER_DONE.store(0, Ordering::SeqCst);
        }
    }

    /// Makes every progress report of an install count `n` bytes more than
    /// libflatpak says, as a source that sends too much would (a local test
    /// remote reports no bytes at all).
    pub fn extra_bytes(n: u64) -> Guard {
        EXTRA_BYTES.store(n, Ordering::SeqCst);
        Guard
    }

    /// Cancels the token inside libflatpak's `ready` step, after the plan was
    /// accepted, so the run is cancelled before its first operation.
    pub fn cancel_in_ready() -> Guard {
        CANCEL_IN_READY.store(true, Ordering::SeqCst);
        Guard
    }

    /// Cancels the token inside the handler of the `n`th finished operation
    /// (n >= 1), so the run stops after exactly `n` operations: a
    /// deterministic partial install.
    pub fn cancel_after_done(n: usize) -> Guard {
        CANCEL_AFTER_DONE.store(n, Ordering::SeqCst);
        Guard
    }

    pub(super) fn extra_bytes_set() -> u64 {
        EXTRA_BYTES.load(Ordering::SeqCst)
    }

    pub(super) fn cancel_in_ready_set() -> bool {
        CANCEL_IN_READY.load(Ordering::SeqCst)
    }

    pub(super) fn cancel_after_done_set() -> usize {
        CANCEL_AFTER_DONE.load(Ordering::SeqCst)
    }
}

/// The release build's hooks: never set.
#[cfg(not(any(test, feature = "test-hooks")))]
mod test_hooks {
    pub(super) fn extra_bytes_set() -> u64 {
        0
    }

    pub(super) fn cancel_in_ready_set() -> bool {
        false
    }

    pub(super) fn cancel_after_done_set() -> usize {
        0
    }
}

#[derive(Default)]
struct St {
    observed: Vec<Observed>,
    ready_seen: bool,
    err: Option<Error>,
    needs_repo: Option<String>,
    rebase: Option<String>,
    sign_in: bool,
    /// Fatal operation errors, all of them.
    op_errors: Vec<String>,
    /// Non-fatal ones, which the run went on after.
    warnings: Vec<String>,
    done: Vec<String>,
    ops_total: usize,
    op_index: usize,
    /// Bytes of the operations before the running one, and of the running one.
    finished_bytes: u64,
    cur_bytes: u64,
    /// The run was stopped for transferring more than the cap.
    over_cap: bool,
}

struct Drive<'a> {
    scope: Scope,
    job: &'a Job,
    mode: &'a Mode,
    /// The app's own remote: the tie-break when libflatpak asks where a
    /// dependency should come from.
    prefer: Option<&'a str>,
    cancel: &'a CancelToken,
    tx: Option<Sender<Msg>>,
}

/// An error, with what had finished before it.
struct Failed {
    err: Error,
    done: Vec<String>,
}

impl From<Error> for Failed {
    fn from(err: Error) -> Failed {
        Failed {
            err,
            done: Vec::new(),
        }
    }
}

fn prep(e: &glib::Error, cancel: &CancelToken) -> Error {
    super::from_glib("prepare the operation", e, cancel)
}

/// Signal handlers are called from C: a panic there would abort the process.
/// Runs `f` and returns `default` if it panics.
fn safe<T>(default: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        log::error!("a libflatpak signal handler panicked; carrying on safely");
        default
    })
}

fn with_st(st: &Rc<RefCell<St>>, f: impl FnOnce(&mut St)) {
    match st.try_borrow_mut() {
        Ok(mut s) => f(&mut s),
        Err(_) => log::error!("the transaction state was busy in a signal handler"),
    }
}

/// The bits of a flags `GValue` (the error details), whatever the binding
/// hands over.
fn flag_bits(v: Option<&glib::Value>) -> u32 {
    let Some(v) = v else { return 0 };
    if v.type_().is_a(glib::Type::FLAGS) {
        // SAFETY: the value holds a flags type, checked just above.
        return unsafe { glib::gobject_ffi::g_value_get_flags(v.to_glib_none().0) };
    }
    v.get::<i32>().map(|x| x as u32).unwrap_or(0)
}

fn send(tx: &Option<Sender<Msg>>, m: Msg) {
    if let Some(tx) = tx {
        let _ = tx.send(m);
    }
}

/// What a remote is worth as the source of a dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteRank {
    name: String,
    prio: i32,
    gpg: bool,
    enumerable: bool,
}

fn remote_ranks(inst: &libflatpak::Installation, cancel: &CancelToken) -> Vec<RemoteRank> {
    match inst.list_remotes(Some(cancel.cancellable())) {
        Ok(list) => list
            .iter()
            .map(|r| RemoteRank {
                name: r.name().map(|s| s.to_string()).unwrap_or_default(),
                prio: r.prio(),
                gpg: r.is_gpg_verify(),
                enumerable: !r.is_noenumerate(),
            })
            .collect(),
        Err(e) => {
            log::warn!(
                "could not read the sources to choose a runtime source: {}",
                super::scrub_log(e.message())
            );
            Vec::new()
        }
    }
}

/// Which of `candidates` (the remotes that have a runtime the app needs) the
/// runtime comes from, as an index into them, or `None` for none. The best
/// remote wins: signature-checked first, then listed in the catalog (not
/// "noenumerate"), then by flatpak priority (higher first), then the app's
/// own remote, then the order libflatpak gave. A remote that is not a known
/// source is never chosen.
fn choose_remote(candidates: &[String], ranks: &[RemoteRank], own: Option<&str>) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let r = ranks.iter().find(|r| &r.name == name)?;
            Some((
                (
                    r.gpg,
                    r.enumerable,
                    r.prio,
                    own == Some(name.as_str()),
                    std::cmp::Reverse(i),
                ),
                i,
            ))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, i)| i)
}

/// Checks the operations libflatpak lists against the mode, and keeps them.
/// True when the run may go on. Called from `ready-pre-auth` (before any
/// authentication prompt) and again from `ready`.
fn check_ops(st: &Rc<RefCell<St>>, mode: &Mode, t: &libflatpak::Transaction) -> bool {
    let obs: Result<Vec<Observed>, Error> = t.operations().iter().map(observe).collect();
    let mut go = false;
    with_st(st, |s| {
        s.ready_seen = true;
        let obs = match obs {
            Ok(o) => o,
            Err(e) => {
                s.err = Some(e);
                return;
            }
        };
        s.ops_total = obs.len();
        let ops: Vec<PlannedOp> = obs.iter().map(|o| o.op.clone()).collect();
        if let Some(r) = s.rebase.clone() {
            s.err = Some(Error::EndOfLifeRebase(r));
            s.observed = obs;
            return;
        }
        match mode {
            Mode::Dry => {}
            Mode::Install {
                ops: plan,
                main_ref,
                remote,
            } => {
                if !same_ops(plan, &ops) {
                    s.err = Some(Error::PlanChanged(diff_ops(plan, &ops)));
                } else if ops
                    .iter()
                    .any(|o| &o.ref_ == main_ref && &o.remote != remote)
                {
                    s.err = Some(changed("the app now comes from another source".into()));
                } else {
                    go = true;
                }
            }
            Mode::UninstallMain(r) => {
                if ops.iter().all(|o| o.kind == OpKind::Uninstall)
                    && ops.iter().any(|o| &o.ref_ == r)
                {
                    go = true;
                } else {
                    s.err = Some(Error::PlanChanged(diff_ops(
                        &[PlannedOp {
                            kind: OpKind::Uninstall,
                            ref_: r.clone(),
                            commit: String::new(),
                            remote: String::new(),
                            download_size: 0,
                            installed_size: 0,
                            signed: false,
                        }],
                        &ops,
                    )));
                }
            }
            Mode::UninstallSet(refs) => {
                let unexpected: Vec<String> = obs
                    .iter()
                    .filter(|o| {
                        !(o.op.kind == OpKind::Uninstall
                            && (refs.contains(&o.op.ref_)
                                || o.related.iter().any(|r| refs.contains(r))))
                    })
                    .map(|o| shown(&o.op))
                    .take(DIFF_MAX)
                    .collect();
                if unexpected.is_empty() {
                    go = true;
                } else {
                    s.err = Some(Error::PlanChanged(PlanChange {
                        unexpected,
                        ..PlanChange::default()
                    }));
                }
            }
        }
        s.observed = obs;
    });
    go
}

/// Keeps one operation error: a fatal one stops the run, a non-fatal one
/// (libflatpak's own flag, as the CLI treats it) is kept as a warning and the
/// run goes on. Every message is logged with its ref. Returns whether the run
/// should go on.
fn record_op_error(st: &Rc<RefCell<St>>, ref_: &str, msg: &str, non_fatal: bool) -> bool {
    let line = format!("{ref_}: {msg}");
    // The log line loses URL queries; the text shown keeps what is harmless.
    let logged = super::scrub_log(&line);
    if non_fatal {
        log::warn!("flatpak: {logged}");
        with_st(st, |s| s.warnings.push(line));
    } else {
        log::error!("flatpak: {logged}");
        with_st(st, |s| s.op_errors.push(line));
    }
    non_fatal
}

/// Builds, checks and runs one transaction on this thread.
fn drive(d: &Drive) -> Result<St, Failed> {
    d.cancel.check()?;
    let dry = matches!(d.mode, Mode::Dry);
    let inst = if dry {
        super::open(d.scope)?
    } else {
        super::open_for_change(d.scope)?
    };
    let t = libflatpak::Transaction::for_installation(&inst, Some(d.cancel.cancellable()))
        .map_err(|e| prep(&e, d.cancel))?;
    // A dry run never asks for privilege; a real run may (polkit, after the
    // user's confirmation).
    t.set_no_interaction(dry);
    let st = Rc::new(RefCell::new(St::default()));
    let cap = d.mode.byte_cap();
    let hook_cancel_in_ready = test_hooks::cancel_in_ready_set();
    let hook_cancel_after = test_hooks::cancel_after_done_set();
    let hook_extra_bytes = test_hooks::extra_bytes_set();

    // ready-pre-auth: the same check as `ready`, before libflatpak asks for
    // any authentication, so a plan that changed never costs a polkit prompt.
    // (From here until `ready` a person may be typing a password: a quiet
    // phase, which the watchdog never cancels.)
    if !dry {
        let st = st.clone();
        let mode = d.mode.copy();
        let tx = d.tx.clone();
        t.connect_ready_pre_auth(move |t| {
            safe(false, || {
                send(&tx, Msg::Phase(Phase::Quiet));
                check_ops(&st, &mode, t)
            })
        });
    }

    // ready: collect, compare, go or stop.
    {
        let st = st.clone();
        let mode = d.mode.copy();
        let tx = d.tx.clone();
        let cancel = d.cancel.clone();
        t.connect_ready(move |t| {
            safe(false, || {
                send(&tx, Msg::Beat);
                let go = check_ops(&st, &mode, t);
                send(&tx, Msg::Phase(Phase::Downloading));
                if go && hook_cancel_in_ready {
                    cancel.cancel();
                }
                go
            })
        });
    }

    // A new remote is never added by a transaction: a runtime remote needs
    // its own confirmation.
    {
        let st = st.clone();
        t.connect_add_new_remote(move |_, _reason, _from, _name, url| {
            safe(false, || {
                with_st(&st, |s| s.needs_repo = Some(url.to_string()));
                false
            })
        });
    }

    // Sign-in flows are not supported (and waiting for one is not a stall).
    {
        let st1 = st.clone();
        let tx1 = d.tx.clone();
        t.connect_webflow_start(move |_, _, _, _, _| {
            safe(false, || {
                send(&tx1, Msg::Phase(Phase::Quiet));
                with_st(&st1, |s| s.sign_in = true);
                false
            })
        });
        let st2 = st.clone();
        let tx2 = d.tx.clone();
        t.connect_basic_auth_start(move |_, _, _, _, _| {
            safe(false, || {
                send(&tx2, Msg::Phase(Phase::Quiet));
                with_st(&st2, |s| s.sign_in = true);
                false
            })
        });
    }

    // Which remote a dependency comes from: the best one (see
    // `choose_remote`), the app's own only as the tie-break.
    {
        let prefer = d.prefer.map(str::to_string);
        let ranks = remote_ranks(&inst, d.cancel);
        t.connect_local("choose-remote-for-ref", false, move |args| {
            safe(Some((-1i32).to_value()), || {
                let remotes: Vec<String> =
                    args.get(3).and_then(|v| v.get().ok()).unwrap_or_default();
                let idx = choose_remote(&remotes, &ranks, prefer.as_deref())
                    .map_or(-1, |i| i32::try_from(i).unwrap_or(-1));
                Some(idx.to_value())
            })
        });
    }

    // No automatic rebase of an end-of-life ref.
    {
        let st = st.clone();
        t.connect_local("end-of-lifed-with-rebase", false, move |args| {
            safe(Some(false.to_value()), || {
                let get = |i: usize| -> String {
                    args.get(i)
                        .and_then(|v| v.get::<Option<String>>().ok().flatten())
                        .map(|s| text::clean(&s, 120))
                        .unwrap_or_default()
                };
                let what = format!("{} to {}", get(2), get(4));
                with_st(&st, |s| s.rebase = Some(what));
                Some(false.to_value())
            })
        });
    }

    // Errors (see `record_op_error`).
    {
        let st = st.clone();
        t.connect_local("operation-error", false, move |args| {
            safe(Some(false.to_value()), || {
                let ref_ = args
                    .get(1)
                    .and_then(|v| v.get::<libflatpak::TransactionOperation>().ok())
                    .and_then(|o| o.get_ref())
                    .map(|r| text::clean(&r, 255))
                    .unwrap_or_default();
                let msg = args
                    .get(2)
                    .and_then(|v| v.get::<glib::Error>().ok())
                    .map(|e| super::scrub(e.message()))
                    .unwrap_or_else(|| "no reason was given".into());
                let non_fatal = flag_bits(args.get(3)) & 1 != 0;
                Some(record_op_error(&st, &ref_, &msg, non_fatal).to_value())
            })
        });
    }

    // Progress.
    {
        let st2 = st.clone();
        let tx = d.tx.clone();
        let cancel = d.cancel.clone();
        t.connect_new_operation(move |_, op, progress| {
            safe((), || {
                let kind = op_kind(op.operation_type()).unwrap_or(OpKind::Install);
                // An uninstall is quiet from the start (see `OpPhase`).
                let tracker = RefCell::new(OpPhase::new(kind == OpKind::Uninstall));
                send(&tx, Msg::Phase(tracker.borrow().phase()));
                let (mut index, mut total) = (0, 0);
                with_st(&st2, |s| {
                    s.finished_bytes = s.finished_bytes.saturating_add(s.cur_bytes);
                    s.cur_bytes = 0;
                    s.op_index += 1;
                    index = s.op_index;
                    total = s.ops_total;
                });
                let ref_ = text::clean(
                    &op.get_ref().map(|s| s.to_string()).unwrap_or_default(),
                    255,
                );
                progress.set_update_frequency(PROGRESS_EVERY.as_millis() as u32);
                let tx = tx.clone();
                let st = st2.clone();
                let cancel = cancel.clone();
                let last: Cell<Option<Instant>> = Cell::new(None);
                // What the watchdog was last told, so it only hears of a
                // change: bytes (or percent) that moved are a beat, and the
                // phase comes from `OpPhase` (a finished download is quiet,
                // a deploy; bytes that grow after that are a download again).
                let last_moved: Cell<(u64, u8)> = Cell::new((u64::MAX, 0));
                progress.connect_changed(move |p| {
                    safe((), || {
                        let percent = p.progress().clamp(0, 100) as u8;
                        let bytes = p.bytes_transferred().saturating_add(hook_extra_bytes);
                        let mut over = false;
                        with_st(&st, |s| {
                            s.cur_bytes = bytes;
                            if s.finished_bytes.saturating_add(bytes) > cap {
                                s.over_cap = true;
                                over = true;
                            }
                        });
                        if over {
                            log::error!(
                                "the source sent more than the plan announced ({bytes} bytes in this operation): stopping"
                            );
                            cancel.cancel();
                            return;
                        }
                        let changed = match tracker.try_borrow_mut() {
                            Ok(mut t) => t.report(percent, bytes),
                            Err(_) => None,
                        };
                        if let Some(want) = changed {
                            send(&tx, Msg::Phase(want));
                        } else if last_moved.replace((bytes, percent)) != (bytes, percent) {
                            send(&tx, Msg::Beat);
                        }
                        last_moved.set((bytes, percent));
                        let now = Instant::now();
                        if let Some(l) = last.get()
                            && now.duration_since(l) < PROGRESS_EVERY
                            && percent < 100
                        {
                            return;
                        }
                        last.set(Some(now));
                        send(
                            &tx,
                            Msg::Progress(Progress {
                                op: index,
                                ops: total,
                                ref_: ref_.clone(),
                                kind,
                                percent,
                                bytes,
                                status: p
                                    .status()
                                    .map(|s| text::clean(&s, 120))
                                    .unwrap_or_default(),
                                not_responding: false,
                            }),
                        );
                    })
                });
            })
        });
        let st3 = st.clone();
        let tx = d.tx.clone();
        let cancel = d.cancel.clone();
        t.connect_local("operation-done", false, move |args| {
            safe(None, || {
                send(&tx, Msg::Beat);
                let op = args
                    .get(1)
                    .and_then(|v| v.get::<libflatpak::TransactionOperation>().ok());
                let seen = op.as_ref().map(observe);
                match seen {
                    Some(Ok(o)) => {
                        let (mut index, mut total, mut finished) = (0, 0, 0);
                        with_st(&st3, |s| {
                            s.done.push(o.op.ref_.clone());
                            index = s.op_index;
                            total = s.ops_total;
                            finished = s.done.len();
                        });
                        send(
                            &tx,
                            Msg::Progress(Progress {
                                op: index,
                                ops: total,
                                ref_: o.op.ref_,
                                kind: o.op.kind,
                                percent: 100,
                                bytes: 0,
                                status: "Done".into(),
                                not_responding: false,
                            }),
                        );
                        if hook_cancel_after > 0 && finished >= hook_cancel_after {
                            cancel.cancel();
                        }
                    }
                    other => {
                        let why = match other {
                            Some(Err(e)) => e.to_string(),
                            _ => "libflatpak gave no details".to_string(),
                        };
                        log::warn!("could not read a finished operation: {why}");
                        with_st(&st3, |s| {
                            s.warnings
                                .push("A finished step could not be read back.".to_string())
                        });
                    }
                }
                None
            })
        });
    }

    match d.job {
        Job::Install { remote, ref_ } => t.add_install(remote, ref_, &[]),
        Job::Uninstall(refs) => refs.iter().try_for_each(|r| t.add_uninstall(r)),
    }
    .map_err(|e| prep(&e, d.cancel))?;

    let res = t.run(Some(d.cancel.cancellable()));
    let s = std::mem::take(&mut *st.borrow_mut());
    finish_run(res, s, dry, d.cancel)
}

/// Turns how `Transaction::run` ended, and what the handlers saw, into the
/// result: what the handlers found explains a libflatpak error better than
/// libflatpak's own words.
fn finish_run(
    res: Result<(), glib::Error>,
    mut s: St,
    dry: bool,
    cancel: &CancelToken,
) -> Result<St, Failed> {
    let fail = |err: Error, done: Vec<String>| Err(Failed { err, done });
    match res {
        Ok(()) => match s.err.take() {
            Some(e) => fail(e, s.done),
            None if dry => fail(
                Error::Invalid("there is nothing to install".into()),
                Vec::new(),
            ),
            None => {
                if s.over_cap {
                    s.warnings.push(Error::SentTooMuch.to_string());
                }
                Ok(s)
            }
        },
        Err(e) => {
            let done = std::mem::take(&mut s.done);
            if let Some(err) = s.err.take() {
                return fail(err, done);
            }
            if s.over_cap {
                return fail(Error::SentTooMuch, done);
            }
            if e.matches(libflatpak::gio::IOErrorEnum::Cancelled) {
                let err = if cancel.is_timed_out() {
                    Error::TimedOut
                } else {
                    Error::Cancelled
                };
                return fail(err, done);
            }
            if let Some(url) = s.needs_repo.take() {
                return fail(
                    Error::NeedsRuntimeRepo(
                        https_url(&url)
                            .unwrap_or_else(|| "the app's own runtime source".to_string()),
                    ),
                    done,
                );
            }
            if s.sign_in {
                return fail(Error::SignInRefused, done);
            }
            if let Some(r) = s.rebase.take() {
                return fail(Error::EndOfLifeRebase(r), done);
            }
            if dry && s.ready_seen && e.matches(libflatpak::Error::Aborted) {
                s.done = done;
                return Ok(s);
            }
            if !s.op_errors.is_empty() {
                return fail(
                    Error::Flatpak {
                        action: "run the operation",
                        message: text::clean(&s.op_errors.join("; "), 600),
                    },
                    done,
                );
            }
            if e.matches(libflatpak::Error::RuntimeNotFound) {
                return fail(Error::RuntimeNotFound, done);
            }
            fail(super::from_glib("run the operation", &e, cancel), done)
        }
    }
}

/// A token that was cancelled or timed out explains whatever error libflatpak
/// gave back as a result of it (a download that was stopped for sending too
/// much keeps its own words).
pub(super) fn explain(err: Error, cancel: &CancelToken) -> Error {
    match err {
        Error::Cancelled | Error::TimedOut | Error::SentTooMuch => err,
        _ if cancel.is_timed_out() => Error::TimedOut,
        _ if cancel.is_cancelled() => Error::Cancelled,
        other => other,
    }
}

/// Runs a changing transaction supervised (see [`super::supervise`]); an
/// error after finished operations becomes [`Error::Partial`].
fn run_job(
    scope: Scope,
    job: Job,
    mode: Mode,
    prefer: Option<&str>,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Progress),
) -> Result<St, Error> {
    let (res, sup) = run_supervised_with(cancel, &Limits::for_scope(scope), progress, |tx| {
        drive(&Drive {
            scope,
            job: &job,
            mode: &mode,
            prefer,
            cancel,
            tx: Some(tx),
        })
    })?;
    if let Some(p) = sup.panic {
        let done = match &res {
            Ok(s) => s.done.join(", "),
            Err(f) => f.done.join(", "),
        };
        log::error!("the progress callback panicked; refs already done and kept: [{done}]");
        resume_unwind(p);
    }
    res.map_err(|f| {
        let f = Failed {
            err: explain(f.err, cancel),
            done: f.done,
        };
        if f.done.is_empty() {
            f.err
        } else {
            log::info!("stopped after finishing: {}", f.done.join(", "));
            Error::Partial {
                completed: f.done,
                cause: Box::new(f.err),
            }
        }
    })
}

// ---- plans ----

fn build_plan(
    scope: Scope,
    ref_: &str,
    remote: String,
    remote_url: String,
    gpg_verified: bool,
    cfgs: &[RemoteCfg],
    observed: Vec<Observed>,
) -> Result<InstallPlan, Error> {
    let main = observed
        .iter()
        .find(|o| o.op.ref_ == ref_ && o.op.kind != OpKind::Uninstall)
        .ok_or_else(|| Error::Invalid("the plan does not install the requested ref".into()))?;
    let metadata = main.metadata.clone();
    let mut ops: Vec<PlannedOp> = observed.iter().map(|o| o.op.clone()).collect();
    // Every remote an operation comes from, as it is now, so that `install`
    // can hold all of them to it (a runtime's remote is as trusted as the
    // app's).
    let mut remotes: Vec<PlanRemote> = Vec::new();
    for o in &mut ops {
        if o.remote.is_empty() {
            continue;
        }
        let cfg = cfgs.iter().find(|c| c.name == o.remote).ok_or_else(|| {
            Error::Invalid(format!(
                "{} would come from a source that is not added",
                o.ref_
            ))
        })?;
        o.signed = cfg.gpg;
        if !remotes.iter().any(|r| r.name == cfg.name) {
            remotes.push(PlanRemote {
                name: cfg.name.clone(),
                url: cfg.url.clone(),
                gpg_verified: cfg.gpg,
                disabled: cfg.disabled,
            });
        }
    }
    remotes.sort_by(|a, b| a.name.cmp(&b.name));
    let new_runtimes = ops
        .iter()
        .filter(|o| o.kind == OpKind::Install && o.ref_.starts_with("runtime/") && o.ref_ != ref_)
        .cloned()
        .collect();
    let download_total = ops
        .iter()
        .fold(0u64, |a, o| a.saturating_add(o.download_size));
    let installed_total = ops
        .iter()
        .fold(0u64, |a, o| a.saturating_add(o.installed_size));
    Ok(InstallPlan {
        scope,
        ref_: ref_.to_string(),
        remote,
        remote_url,
        gpg_verified,
        remotes,
        ops,
        download_total,
        installed_total,
        metadata,
        new_runtimes,
    })
}

/// A dry run of installing `ref_` (`app/ID/arch/branch`) from the installed,
/// enabled remote `remote`: what would be installed, with sizes and the app's
/// metadata. It changes nothing (it may fetch the remote's catalog).
///
/// Blocking: run on a worker thread.
pub fn plan_install(
    scope: Scope,
    remote: &str,
    ref_: &str,
    cancel: &CancelToken,
) -> Result<InstallPlan, Error> {
    plan_install_inner(scope, remote, ref_, None, cancel)
}

/// [`plan_install`] for the ref of a `.flatpakref` whose remote is already
/// added (see `resolve_ref_source`). The file's `RuntimeRepo` is never
/// followed: when the plan fails for want of a runtime, the error is
/// [`Error::NeedsRuntimeRepo`] with that URL (when it is a valid https
/// address) for a confirmation of its own.
///
/// Blocking: run on a worker thread.
pub fn plan_install_ref(
    scope: Scope,
    remote: &str,
    r: &crate::flatpakref::FlatpakRef,
    cancel: &CancelToken,
) -> Result<InstallPlan, Error> {
    let ref_ = super::sources::main_ref_of(r)?;
    plan_install_inner(scope, remote, &ref_, r.runtime_repo.as_deref(), cancel)
}

fn plan_install_inner(
    scope: Scope,
    remote: &str,
    ref_: &str,
    runtime_repo: Option<&str>,
    cancel: &CancelToken,
) -> Result<InstallPlan, Error> {
    cancel.check()?;
    if !super::valid_remote(remote) {
        return Err(Error::Invalid("the remote name is not valid".into()));
    }
    check_ref(ref_)?;
    let cfgs = remote_configs(scope, cancel)?;
    let cfg = cfgs
        .iter()
        .find(|r| r.name == remote)
        .ok_or_else(|| Error::Invalid("that source is not added".into()))?;
    if cfg.disabled {
        return Err(Error::Invalid("the source is disabled".into()));
    }
    let job = Job::Install {
        remote: remote.to_string(),
        ref_: ref_.to_string(),
    };
    let (res, _timed_out) = run_supervised(cancel, &mut |_| {}, |tx| {
        drive(&Drive {
            scope,
            job: &job,
            mode: &Mode::Dry,
            prefer: Some(remote),
            cancel,
            tx: Some(tx),
        })
    })?;
    let st = res.map_err(|f| match (explain(f.err, cancel), runtime_repo) {
        (Error::RuntimeNotFound, Some(url)) => Error::NeedsRuntimeRepo(
            https_url(url).unwrap_or_else(|| "the app's own runtime source".to_string()),
        ),
        (e, _) => e,
    })?;
    build_plan(
        scope,
        ref_,
        remote.to_string(),
        cfg.url.clone(),
        cfg.gpg,
        &cfgs,
        st.observed,
    )
}

// ---- running ----

/// Installs exactly what `plan` lists. The plan's source is checked again
/// (same URL and signing, still enabled), then a fresh transaction is
/// compared with the plan before anything is downloaded; any difference is
/// [`Error::PlanChanged`], saying what differs, and the UI plans again. A
/// runtime remote (`RuntimeRepo`) is never added
/// ([`Error::NeedsRuntimeRepo`]), an end-of-life rebase is reported
/// ([`Error::EndOfLifeRebase`]) and a sign-in is refused. The sources are
/// compared before and after; an unplanned change is logged and reported in
/// `warnings`. For the system scope polkit asks the user.
///
/// Partial state: libflatpak commits one operation at a time. If the install
/// is cancelled, times out or fails after some finished, those stay and the
/// error is [`Error::Partial`] listing them.
///
/// System scope: a cancel or timeout only cancels this client's D-Bus call.
/// `flatpak-system-helper` may keep deploying afterwards, so `Partial`'s
/// `completed` list can be short and more refs may appear later. Re-list the
/// installation rather than trust it. The user scope is exact.
///
/// After a successful install the source's entry in the pending-sources
/// journal is cleared (it is in use now); a failure to do that is logged and
/// never fails the install.
///
/// Blocking: run on a worker thread; `progress` is called on that thread, at
/// most about ten times a second.
pub fn install(
    plan: &InstallPlan,
    _lock: &OperationLock,
    cancel: &CancelToken,
    mut progress: impl FnMut(Progress),
) -> Result<Installed, Error> {
    cancel.check()?;
    let before = remote_configs(plan.scope, cancel)?;
    check_plan_source(plan, &before)?;
    let res = run_job(
        plan.scope,
        Job::Install {
            remote: plan.remote.clone(),
            ref_: plan.ref_.clone(),
        },
        Mode::Install {
            ops: plan.ops.clone(),
            main_ref: plan.ref_.clone(),
            remote: plan.remote.clone(),
        },
        Some(&plan.remote),
        cancel,
        &mut progress,
    );
    let mut note = None;
    match remote_configs(plan.scope, &CancelToken::new()) {
        Ok(after) if after != before => {
            log::error!(
                "{}",
                super::scrub_log(&format!(
                    "the sources changed during the install: {before:?} then {after:?}"
                ))
            );
            note = Some("The list of sources changed during the install.".to_string());
        }
        Ok(_) => {}
        Err(e) => log::warn!("could not re-check the sources after the install: {e}"),
    }
    let st = res?;
    let mut warnings = st.warnings;
    warnings.extend(note);
    if !st.done.contains(&plan.ref_) {
        log::error!(
            "the install finished without reporting {} as done (done: {})",
            plan.ref_,
            st.done.join(", ")
        );
        warnings.push("The app was not reported as installed. Check the list of apps.".into());
    }
    match check_deployed_metadata(plan) {
        Deployed::Same => {}
        Deployed::Unchecked(why) => warnings.push(why),
        Deployed::Differs => {
            // What is installed is not what the user confirmed (permissions
            // included): do not keep it.
            let rolled_back = roll_back(plan);
            return Err(Error::MetadataMismatch { rolled_back });
        }
    }
    // The source is in use now, so it is no longer pending.
    if let Err(e) = super::sources::journal_clear(plan.scope, &plan.remote, &plan.remote_url) {
        log::warn!("could not forget the pending source {}: {e}", plan.remote);
    }
    Ok(Installed {
        refs: st.done,
        warnings,
    })
}

/// A key file in the one form GLib writes, so two spellings of the same
/// metadata compare equal (the plan's copy came out of a `GKeyFile`, the
/// deployed one is the file as published).
fn normalized_metadata(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let kf = glib::KeyFile::new();
    kf.load_from_data(text, glib::KeyFileFlags::NONE).ok()?;
    Some(kf.to_data().to_string())
}

/// How the deployed metadata compares with the plan's.
enum Deployed {
    Same,
    /// It could not be compared (the text is a warning for the user).
    Unchecked(String),
    /// Definitely different.
    Differs,
}

/// Removes the app that was just installed (its data was never touched, and
/// its runtimes stay) after its metadata did not match the plan. Under the
/// caller's lock, on a token of its own: a cancel that came late must not
/// leave the unconfirmed app in place. Returns whether it worked.
fn roll_back(plan: &InstallPlan) -> bool {
    let cancel = CancelToken::new();
    let res = run_job(
        plan.scope,
        Job::Uninstall(vec![plan.ref_.clone()]),
        Mode::UninstallMain(plan.ref_.clone()),
        None,
        &cancel,
        &mut |_| {},
    );
    match res {
        Ok(st) => {
            log::error!(
                "the deployed metadata of {} differs from the plan's: removed it again ({})",
                plan.ref_,
                st.done.join(", ")
            );
            true
        }
        Err(e) => {
            log::error!(
                "the deployed metadata of {} differs from the plan's and removing it again failed: {e}",
                plan.ref_
            );
            false
        }
    }
}

/// Compares the metadata of the app as deployed with what the plan showed the
/// user.
fn check_deployed_metadata(plan: &InstallPlan) -> Deployed {
    let unchecked = |s: &str| Deployed::Unchecked(s.to_string());
    let fresh = CancelToken::new();
    let out = super::list_installed(plan.scope, &fresh);
    let Some(r) = out.refs.iter().find(|r| r.full_ref() == plan.ref_) else {
        log::warn!("the installed app {} was not found to check it", plan.ref_);
        return unchecked("The installed app could not be found to check it.");
    };
    let deployed = match r.metadata(&fresh) {
        Ok(m) => m,
        Err(e) => {
            log::warn!(
                "could not read the installed metadata of {}: {e}",
                plan.ref_
            );
            return unchecked("The installed app's details could not be checked.");
        }
    };
    match (
        normalized_metadata(&plan.metadata),
        normalized_metadata(&deployed),
    ) {
        (Some(a), Some(b)) if a == b => Deployed::Same,
        (Some(_), Some(_)) => Deployed::Differs,
        _ => unchecked("The installed app's details could not be checked."),
    }
}

/// What an installed ref's metadata says about what it depends on.
pub(super) struct Dep {
    pub(super) full_ref: String,
    /// `[ExtensionOf] ref=`.
    pub(super) ext_of: Option<String>,
    /// The `id/arch/branch` of its runtime and sdk.
    pub(super) uses: Vec<String>,
}

/// A failure to read the installed refs, as the error callers show.
fn unreadable(e: Error) -> Error {
    match e {
        Error::Cancelled | Error::TimedOut => e,
        other => {
            log::warn!("could not read an installed ref: {other}");
            Error::CouldNotCheck("what the installed apps need".into())
        }
    }
}

/// Whether a problem in listing the installed refs makes the dependency scan
/// unusable (see [`scan_deps`]).
fn fatal_list_error(e: &ListError) -> bool {
    match e.kind {
        ListErrorKind::ScopeUnreadable => true,
        ListErrorKind::BadRef => e.ref_kind != Some(RefKind::App),
        ListErrorKind::ExtensionLink | ListErrorKind::TooMany => false,
    }
}

/// Reads the dependencies of every installed app and runtime in every
/// installation. Not knowing is not permission, so a check fails
/// ([`Error::CouldNotCheck`]) when an installation cannot be read, when a
/// runtime (or a ref of unknown kind) was skipped as bad, or when a ref's
/// metadata cannot be read. A skipped bad *app* cannot be what a runtime is
/// needed by in a way that matters here, and a failed add-on link or the
/// "N more problems" note says nothing about what uses what, so those are
/// only logged.
pub(super) fn scan_deps(cancel: &CancelToken) -> Result<Vec<Dep>, Error> {
    let all = super::list_installed_all(cancel);
    if all.cancelled {
        return Err(Error::Cancelled);
    }
    for e in &all.errors {
        let fatal = fatal_list_error(e);
        let line = super::scrub_log(&e.message);
        if fatal {
            log::warn!("could not list the installed apps: {line}");
            return Err(Error::CouldNotCheck("what is installed".into()));
        }
        log::info!("ignored while checking what is installed: {line}");
    }
    let limits = crate::keyfile::Limits::default();
    let mut deps = Vec::with_capacity(all.refs.len());
    for r in &all.refs {
        let md = r.metadata(cancel).map_err(unreadable)?;
        let kf = crate::keyfile::KeyFile::parse(&md, &limits).map_err(|_| {
            log::warn!("the metadata of {} is not readable", r.full_ref());
            Error::CouldNotCheck("what the installed apps need".into())
        })?;
        let uses = ["Application", "Runtime"]
            .iter()
            .flat_map(|g| ["runtime", "sdk"].iter().map(move |k| (*g, *k)))
            .filter_map(|(g, k)| kf.raw(g, k).map(str::to_string))
            .collect();
        deps.push(Dep {
            full_ref: r.full_ref(),
            ext_of: kf.raw("ExtensionOf", "ref").map(str::to_string),
            uses,
        });
    }
    Ok(deps)
}

/// Splits libflatpak's unused list into what is really unused and what an
/// installed ref (in either installation) still uses, as
/// `(ref, its users)`. A runtime only the dropped ones kept alive is dropped
/// too, until nothing changes, so the first list is what `uninstall_unused`
/// accepts as it stands.
pub(super) fn split_unused(
    refs: Vec<InstalledRef>,
    deps: &[Dep],
) -> (Vec<InstalledRef>, Vec<(String, Vec<String>)>) {
    let mut keep = refs;
    let mut used: Vec<(String, Vec<String>)> = Vec::new();
    loop {
        let set: Vec<String> = keep.iter().map(|r| r.full_ref()).collect();
        let before = keep.len();
        keep.retain(|r| {
            if r.kind != RefKind::Runtime {
                return true;
            }
            let full = r.full_ref();
            match runtime_users(deps, &full, &set) {
                Ok(users) if users.is_empty() => true,
                Ok(users) => {
                    log::info!("{full} is used by {users:?}: not unused");
                    used.push((full, users));
                    false
                }
                Err(e) => {
                    log::warn!("could not check who uses {full}: {e}");
                    false
                }
            }
        });
        if keep.len() == before {
            return (keep, used);
        }
    }
}

/// The installed parent of `target` when `target` is an extension (its
/// metadata has `[ExtensionOf] ref=` and that ref is installed). The value
/// comes from a file on disk, so it must be a valid ref and match an
/// installed one exactly to count.
fn extension_parent(deps: &[Dep], target: &str) -> Option<String> {
    let me = deps.iter().find(|d| d.full_ref == target)?;
    let parent = me.ext_of.as_deref()?;
    check_ref(parent).ok()?;
    deps.iter()
        .any(|d| d.full_ref == parent)
        .then(|| parent.to_string())
}

/// Which installed refs use the runtime `target`, ignoring `skip` (refs that
/// go together with it) and its own extensions, which go with it.
fn runtime_users(deps: &[Dep], target: &str, skip: &[String]) -> Result<Vec<String>, Error> {
    let (_, id, arch, branch) = check_ref(target)?;
    let want = format!("{id}/{arch}/{branch}");
    Ok(deps
        .iter()
        .filter(|d| d.full_ref != target && !skip.contains(&d.full_ref))
        .filter(|d| d.ext_of.as_deref() != Some(target))
        .filter(|d| d.uses.contains(&want))
        .map(|d| d.full_ref.clone())
        .collect())
}

fn app_running(id: &str) -> bool {
    libflatpak::Instance::all()
        .iter()
        .any(|i| i.is_running() && i.app().as_deref() == Some(id))
}

/// `$HOME`, canonicalized (a symlinked home is fine).
fn home_dir() -> Result<PathBuf, Error> {
    let h = std::env::var_os("HOME").ok_or_else(|| Error::Invalid("HOME is not set".into()))?;
    std::fs::canonicalize(PathBuf::from(h))
        .map_err(|_| Error::Invalid("the home folder is not readable".into()))
}

/// Uninstalls `ref_`, with whatever libflatpak removes along with it (an
/// app's add-ons); anything else in the transaction stops it with
/// [`Error::PlanChanged`]. A runtime that an installed app or runtime in
/// either installation still uses is refused ([`Error::InUse`]; "remove
/// unused" is the way to remove runtimes).
///
/// With `delete_data` (apps only) the app must not be running
/// ([`Error::AppRunning`]); after the uninstall, `$HOME/.var/app/<id>` is
/// deleted for the current user only, and only when no ref with that ID
/// remains in either installation (another branch shares the folder). The
/// outcome says what happened to the data apart from the uninstall, which
/// has succeeded either way. Without `delete_data` the data is kept. The
/// "still installed" check covers the default system installation, the user
/// installation and every other system installation; if it cannot be made
/// the data is kept.
///
/// A runtime that is an extension of an installed runtime is refused
/// ([`Error::InUse`] naming the parent): use "remove unused". If the
/// installed refs cannot be read for the check, the error is
/// [`Error::CouldNotCheck`].
///
/// System scope: a cancel or timeout only cancels this client's D-Bus call;
/// `flatpak-system-helper` may keep removing afterwards. After
/// [`Error::Partial`] or a cancel, re-list the installation rather than trust
/// `completed`. The user scope is exact.
///
/// Blocking: run on a worker thread.
pub fn uninstall(
    scope: Scope,
    ref_: &str,
    delete_data: bool,
    _lock: &OperationLock,
    cancel: &CancelToken,
    mut progress: impl FnMut(Progress),
) -> Result<Uninstalled, Error> {
    cancel.check()?;
    let (kind, id, _, _) = check_ref(ref_)?;
    let id = id.to_string();
    if kind == RefKind::Runtime {
        let deps = scan_deps(cancel)?;
        // An extension goes with its parent: removing it alone would break
        // the parent, so "remove unused" is the way once nothing needs it.
        if let Some(parent) = extension_parent(&deps, ref_) {
            return Err(Error::InUse(vec![parent]));
        }
        let users = runtime_users(&deps, ref_, &[])?;
        if !users.is_empty() {
            return Err(Error::InUse(users));
        }
    }
    let wants_data = delete_data && kind == RefKind::App;
    if wants_data && app_running(&id) {
        return Err(Error::AppRunning);
    }
    let st = run_job(
        scope,
        Job::Uninstall(vec![ref_.to_string()]),
        Mode::UninstallMain(ref_.to_string()),
        None,
        cancel,
        &mut progress,
    )?;
    let mut warnings = st.warnings;
    let data = if !wants_data {
        DataResult::NotRequested
    } else {
        remove_data_after(&id, cancel, &mut warnings)
    };
    Ok(Uninstalled {
        removed: st.done,
        data,
        warnings,
    })
}

fn remove_data_after(id: &str, cancel: &CancelToken, warnings: &mut Vec<String>) -> DataResult {
    // The data folder is shared by every installation (system, user and the
    // other system ones), so it goes only when the app is installed in none.
    // A fresh token: the uninstall is done, and this decision must not be
    // skipped by a cancel that came late. Not knowing keeps the data.
    let _ = cancel;
    match super::installed::find_app_everywhere(id, &CancelToken::new()) {
        Ok(found) if found.is_empty() => {}
        Ok(found) => {
            let places: Vec<&str> = found.iter().map(|f| f.installation.as_str()).collect();
            log::info!("keeping the data of {id}: still installed in {places:?}");
            return DataResult::Kept(
                "the app is still installed in another version or installation and uses the same data"
                    .into(),
            );
        }
        Err(e) => {
            log::warn!("could not check where {id} is installed: {e}");
            return DataResult::Kept(
                "the installed apps could not be checked, so the data was kept".into(),
            );
        }
    }
    let home = match home_dir() {
        Ok(h) => h,
        Err(e) => return DataResult::Failed(e.to_string()),
    };
    // It may have been started while the uninstall ran.
    if app_running(id) {
        return DataResult::Kept("the app is running, so its data was kept".into());
    }
    let mut sweep = Vec::new();
    let res = delete_app_data_with(&home, id, &mut sweep);
    warnings.extend(sweep);
    match res {
        Ok(true) => DataResult::Deleted,
        Ok(false) => DataResult::NoData,
        Err(e) => {
            log::warn!("could not delete the data of {id}: {e}");
            DataResult::Failed(e.to_string())
        }
    }
}

/// Removes the unused runtimes the user confirmed: `expected` must be exactly
/// what [`super::list_unused`] returns now (as `runtime/...` refs, see
/// `InstalledRef::full_ref`; that list already leaves out every runtime that
/// an installed ref in either installation uses), else
/// [`Error::PlanChanged`] says what differs. A wanted runtime that an
/// installed ref outside the set uses is [`Error::InUse`] (naming the users),
/// both when the list is made and again just before the transaction, under
/// the caller's lock, with nothing removed: that is not a plan that can be
/// asked again, so the UI stops. Extensions libflatpak removes along with a
/// wanted runtime are accepted; any other operation stops the run. The same
/// system-scope cancel caveat as [`uninstall`] applies.
///
/// Blocking: run on a worker thread.
pub fn uninstall_unused(
    scope: Scope,
    expected: &[String],
    _lock: &OperationLock,
    cancel: &CancelToken,
    mut progress: impl FnMut(Progress),
) -> Result<Uninstalled, Error> {
    cancel.check()?;
    for r in expected {
        check_ref(r)?;
    }
    let mut want = expected.to_vec();
    want.sort();
    want.dedup();
    let raw = list_unused_raw(scope, cancel)?;
    let deps = scan_deps(cancel)?;
    let (kept, used) = split_unused(raw, &deps);
    let mut users: Vec<String> = used
        .into_iter()
        .filter(|(r, _)| want.contains(r))
        .flat_map(|(_, u)| u)
        .collect();
    if !users.is_empty() {
        users.sort();
        users.dedup();
        return Err(Error::InUse(users));
    }
    let mut now: Vec<String> = kept.iter().map(|r| r.full_ref()).collect();
    now.sort();
    if now != want {
        let missing = want.iter().filter(|r| !now.contains(r)).cloned().collect();
        let unexpected = now.iter().filter(|r| !want.contains(r)).cloned().collect();
        return Err(Error::PlanChanged(PlanChange {
            missing,
            unexpected,
            source: None,
        }));
    }
    if want.is_empty() {
        return Ok(Uninstalled {
            removed: Vec::new(),
            data: DataResult::NotRequested,
            warnings: Vec::new(),
        });
    }
    // Under the caller's lock, just before the transaction: a runtime that
    // something started needing since the list was made is not removed. The
    // user confirmed this exact list, so it is not shortened silently: the
    // call stops with `InUse` and nothing is removed.
    let deps = scan_deps(cancel)?;
    let mut users = Vec::new();
    for r in want.iter().filter(|r| r.starts_with("runtime/")) {
        let u = runtime_users(&deps, r, &want)?;
        if !u.is_empty() {
            log::warn!("{r} is in use by {u:?} now, so it is not removed");
            users.extend(u);
        }
    }
    if !users.is_empty() {
        users.sort();
        users.dedup();
        return Err(Error::InUse(users));
    }
    let st = run_job(
        scope,
        Job::Uninstall(want.clone()),
        Mode::UninstallSet(want),
        None,
        cancel,
        &mut progress,
    )?;
    Ok(Uninstalled {
        removed: st.done,
        data: DataResult::NotRequested,
        warnings: st.warnings,
    })
}

// ---- app data ----

fn io_err(e: std::io::Error) -> Error {
    Error::Io {
        action: "delete the app's data",
        message: match e.raw_os_error() {
            Some(libc::EBUSY) => "a mounted folder is in the way".to_string(),
            Some(libc::EXDEV) => {
                "it holds a folder on another mount or drive, which was left alone".to_string()
            }
            _ => e.kind().to_string(),
        },
    }
}

fn cstr(s: &str) -> Result<CString, Error> {
    CString::new(s).map_err(|_| Error::Invalid("a name contains a null byte".into()))
}

fn open_dir_at(parent: RawFd, name: &CStr) -> std::io::Result<OwnedFd> {
    // SAFETY: openat with a valid C string; the fd, if any, is owned at once.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: fd is a new, open descriptor nobody else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn fstat(fd: RawFd) -> std::io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes a stat into the buffer on success.
    if unsafe { libc::fstat(fd, st.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized by the successful call.
    Ok(unsafe { st.assume_init() })
}

fn fstatat_nofollow(parent: RawFd, name: &CStr) -> std::io::Result<libc::stat> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: as above, with a valid C string.
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            st.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: initialized by the successful call.
    Ok(unsafe { st.assume_init() })
}

fn unlinkat(parent: RawFd, name: &CStr, flags: libc::c_int) -> std::io::Result<()> {
    // SAFETY: unlinkat with a valid C string.
    if unsafe { libc::unlinkat(parent, name.as_ptr(), flags) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Renames within one directory, failing (EEXIST) rather than replacing.
fn renameat_noreplace(parent: RawFd, from: &CStr, to: &CStr) -> std::io::Result<()> {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    // SAFETY: renameat2 with valid C strings in one directory.
    if unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent,
            from.as_ptr(),
            parent,
            to.as_ptr(),
            RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// The entry names of the open directory `fd` (not `.` and `..`).
fn entry_names(fd: RawFd) -> std::io::Result<Vec<CString>> {
    // SAFETY: dup of a valid fd; fdopendir takes over the copy.
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: dup is a valid directory fd; on failure we close it ourselves.
    let dir = unsafe { libc::fdopendir(dup) };
    if dir.is_null() {
        let e = std::io::Error::last_os_error();
        // SAFETY: dup is still ours when fdopendir failed.
        unsafe { libc::close(dup) };
        return Err(e);
    }
    let mut names = Vec::new();
    let mut result = Ok(());
    loop {
        // SAFETY: errno is thread-local; set to 0 to tell the end from an error.
        unsafe { *libc::__errno_location() = 0 };
        // SAFETY: dir is a valid DIR* until closedir.
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(0) {
                result = Err(e);
            }
            break;
        }
        // SAFETY: d_name is a NUL-terminated name inside the dirent.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    // SAFETY: closes dir and the fd it owns.
    unsafe { libc::closedir(dir) };
    result.map(|()| names)
}

/// Removes the directory `name` under `parent` and all it holds, never
/// following a symlink (a link is removed, not what it points to) and never
/// entering a folder on another device (EXDEV).
fn remove_tree(parent: RawFd, name: &CStr, dev: libc::dev_t, depth: u32) -> std::io::Result<()> {
    if depth >= MAX_DEPTH {
        return Err(std::io::Error::other("folders are nested too deep"));
    }
    let fd = open_dir_at(parent, name)?;
    if fstat(fd.as_raw_fd())?.st_dev != dev {
        return Err(std::io::Error::from_raw_os_error(libc::EXDEV));
    }
    for n in entry_names(fd.as_raw_fd())? {
        let st = match fstatat_nofollow(fd.as_raw_fd(), &n) {
            Ok(st) => st,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
            Err(e) => return Err(e),
        };
        if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
            remove_tree(fd.as_raw_fd(), &n, dev, depth + 1)?;
        } else {
            unlinkat(fd.as_raw_fd(), &n, 0)?;
        }
    }
    drop(fd);
    unlinkat(parent, name, libc::AT_REMOVEDIR)
}

/// Counts leftover names made by this process.
static LEFTOVERS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A name no other delete uses: `.<id>.<pid>.<n>.deleting`.
fn leftover_name(id: &str) -> String {
    let n = LEFTOVERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".{id}.{}.{n}.deleting", std::process::id())
}

/// A leftover of an earlier delete: `.<id>.deleting` or
/// `.<id>.<pid>.<n>.deleting`.
fn is_leftover(name: &[u8]) -> bool {
    let Some(mid) = name
        .strip_prefix(b".")
        .and_then(|n| n.strip_suffix(b".deleting"))
        .and_then(|m| std::str::from_utf8(m).ok())
    else {
        return false;
    };
    if text::valid_id(mid) {
        return true;
    }
    let mut p = mid.rsplitn(3, '.');
    match (p.next(), p.next(), p.next()) {
        (Some(n), Some(pid), Some(id)) => {
            let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            digits(n) && digits(pid) && text::valid_id(id)
        }
        _ => false,
    }
}

/// [`delete_app_data_with`] when the caller does not need the sweep's failures.
#[cfg(test)]
pub(crate) fn delete_app_data(home: &Path, id: &str) -> Result<bool, Error> {
    delete_app_data_with(home, id, &mut Vec::new())
}

/// Removes `<home>/.var/app/<id>` (true if there was something). `home` must
/// be a folder of ours. The folder is first renamed, atomically and without
/// replacing anything, to a unique `.<id>.<pid>.<n>.deleting` in the same
/// directory and then deleted, so a crash leaves either the data or a
/// leftover that the next call sweeps away (a leftover that is not a folder is
/// unlinked). A symlink in the way is removed, not followed; nothing on
/// another device is touched. What the sweep could not remove is added to
/// `sweep_failures` in plain words.
pub(crate) fn delete_app_data_with(
    home: &Path,
    id: &str,
    sweep_failures: &mut Vec<String>,
) -> Result<bool, Error> {
    if !text::valid_id(id) || !home.is_absolute() {
        return Err(Error::Invalid(
            "the app ID or the home folder is not valid".into(),
        ));
    }
    let home = std::fs::canonicalize(home)
        .map_err(|_| Error::Invalid("the home folder is not readable".into()))?;
    let home_c = cstr(
        home.to_str()
            .ok_or_else(|| Error::Invalid("HOME is not valid text".into()))?,
    )?;
    let root = open_dir_at(libc::AT_FDCWD, &home_c).map_err(io_err)?;
    // SAFETY: geteuid cannot fail.
    if fstat(root.as_raw_fd()).map_err(io_err)?.st_uid != unsafe { libc::geteuid() } {
        return Err(Error::Invalid(
            "the home folder belongs to another user".into(),
        ));
    }
    let mut dir = root;
    for part in [".var", "app"] {
        match open_dir_at(dir.as_raw_fd(), &cstr(part)?) {
            Ok(next) => dir = next,
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
            Err(e) => return Err(io_err(e)),
        }
    }
    let dev = fstat(dir.as_raw_fd()).map_err(io_err)?.st_dev;
    // Leftovers of earlier deletes.
    match entry_names(dir.as_raw_fd()) {
        Ok(names) => {
            for n in names.into_iter().filter(|n| is_leftover(n.to_bytes())) {
                let res = match fstatat_nofollow(dir.as_raw_fd(), &n) {
                    Ok(st) if st.st_mode & libc::S_IFMT == libc::S_IFDIR => {
                        remove_tree(dir.as_raw_fd(), &n, dev, 0)
                    }
                    Ok(_) => unlinkat(dir.as_raw_fd(), &n, 0),
                    Err(e) => Err(e),
                };
                if let Err(e) = res {
                    log::warn!("could not sweep a leftover app data folder: {e}");
                    let why = match io_err(e) {
                        Error::Io { message, .. } => message,
                        other => other.to_string(),
                    };
                    sweep_failures.push(format!(
                        "A leftover of an earlier delete in the app data folder could not be removed ({why})."
                    ));
                }
            }
        }
        Err(e) => {
            log::warn!("could not look for leftover app data folders: {e}");
            sweep_failures.push("Leftovers of earlier deletes could not be looked for.".into());
        }
    }
    let name = cstr(id)?;
    let st = match fstatat_nofollow(dir.as_raw_fd(), &name) {
        Ok(st) => st,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
        Err(e) => return Err(io_err(e)),
    };
    if st.st_dev != dev {
        return Err(io_err(std::io::Error::from_raw_os_error(libc::EXDEV)));
    }
    let tmp = cstr(&leftover_name(id))?;
    renameat_noreplace(dir.as_raw_fd(), &name, &tmp).map_err(io_err)?;
    if st.st_mode & libc::S_IFMT == libc::S_IFDIR {
        remove_tree(dir.as_raw_fd(), &tmp, dev, 0).map_err(io_err)?;
    } else {
        unlinkat(dir.as_raw_fd(), &tmp, 0).map_err(io_err)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_checked() {
        assert!(check_ref("app/org.test.Hello/x86_64/stable").is_ok());
        assert!(check_ref("runtime/org.test.Platform/x86_64/23.08").is_ok());
        for bad in [
            "app/org.test.Hello/x86_64",
            "app/org.test.Hello/x86_64/stable/x",
            "file/org.test.Hello/x86_64/stable",
            "app/../x86_64/stable",
            "app/org.test.Hello/../stable",
            "app/org.test.Hello/x86_64/..",
            "app/org.test.Hello/x86_64/-x",
            "app/org.test.Hello/./stable",
        ] {
            assert!(check_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn urls_compare_without_a_trailing_slash() {
        assert_eq!(norm_url("file:///a/b/"), "file:///a/b");
        assert_eq!(
            norm_url("https://Example.org/repo/"),
            norm_url("https://example.org/repo")
        );
    }

    fn op(r: &str, c: &str) -> PlannedOp {
        PlannedOp {
            kind: OpKind::Install,
            ref_: r.into(),
            commit: c.into(),
            remote: "t".into(),
            download_size: 1,
            installed_size: 2,
            signed: true,
        }
    }

    #[test]
    fn plans_compare_by_ref_kind_commit_remote_and_say_what_differs() {
        let a = vec![op("app/a.b/x/s", "aa"), op("runtime/a.c/x/s", "bb")];
        let mut b = vec![a[1].clone(), a[0].clone()];
        b[0].download_size = 99;
        assert!(same_ops(&a, &b));
        assert_eq!(diff_ops(&a, &b), PlanChange::default());
        b[1].commit = "cc".into();
        assert!(!same_ops(&a, &b));
        let d = diff_ops(&a, &b);
        assert_eq!(d.missing.len(), 1);
        assert!(d.missing[0].contains("app/a.b/x/s") && d.missing[0].contains("aa"));
        assert_eq!(d.unexpected.len(), 1);
        assert!(d.unexpected[0].contains("cc"));
        b.push(op("runtime/new.One/x/s", "dd"));
        assert!(
            diff_ops(&a, &b)
                .unexpected
                .iter()
                .any(|u| u.contains("new.One"))
        );
        let msg = Error::PlanChanged(diff_ops(&a, &b)).to_string();
        assert!(
            msg.contains("newly included") && msg.contains("no longer included"),
            "{msg}"
        );
    }

    #[test]
    fn the_plans_source_must_still_be_as_planned() {
        let plan = InstallPlan {
            scope: Scope::User,
            ref_: "app/a.b/x/s".into(),
            remote: "r".into(),
            remote_url: "https://example.org/repo".into(),
            gpg_verified: true,
            remotes: vec![],
            ops: vec![],
            download_total: 0,
            installed_total: 0,
            metadata: vec![],
            new_runtimes: vec![],
        };
        let cfg = |url: &str, gpg: bool, disabled: bool| RemoteCfg {
            name: "r".into(),
            url: url.into(),
            gpg,
            disabled,
        };
        assert!(check_plan_source(&plan, &[cfg("https://example.org/repo", true, false)]).is_ok());
        for bad in [
            vec![],
            vec![cfg("https://evil.example/repo", true, false)],
            vec![cfg("https://example.org/repo", false, false)],
            vec![cfg("https://example.org/repo", true, true)],
        ] {
            let e = check_plan_source(&plan, &bad).unwrap_err();
            assert!(
                matches!(&e, Error::PlanChanged(c) if c.source.is_some()),
                "{e:?}"
            );
        }
    }

    #[test]
    fn every_remote_of_the_plan_is_held_to_it() {
        let snap = |name: &str, url: &str, gpg: bool| PlanRemote {
            name: name.into(),
            url: url.into(),
            gpg_verified: gpg,
            disabled: false,
        };
        let plan = InstallPlan {
            scope: Scope::User,
            ref_: "app/a.b/x/s".into(),
            remote: "r".into(),
            remote_url: "https://example.org/repo".into(),
            gpg_verified: true,
            remotes: vec![
                snap("r", "https://example.org/repo", true),
                snap("rt", "https://runtimes.example/repo", false),
            ],
            ops: vec![],
            download_total: 0,
            installed_total: 0,
            metadata: vec![],
            new_runtimes: vec![],
        };
        let cfg = |name: &str, url: &str, gpg: bool, disabled: bool| RemoteCfg {
            name: name.into(),
            url: url.into(),
            gpg,
            disabled,
        };
        let good = vec![
            cfg("r", "https://example.org/repo", true, false),
            cfg("rt", "https://runtimes.example/repo", false, false),
        ];
        assert!(check_plan_source(&plan, &good).is_ok());
        let says = |bad: Vec<RemoteCfg>| match check_plan_source(&plan, &bad).unwrap_err() {
            Error::PlanChanged(c) => c.source.unwrap(),
            e => panic!("{e:?}"),
        };
        // The runtime remote's address changed, it is gone, it is signed now,
        // or it is disabled.
        let mut moved = good.clone();
        moved[1].url = "https://evil.example/repo".into();
        assert!(says(moved).contains("\"rt\" now has another address"));
        assert!(says(vec![good[0].clone()]).contains("\"rt\" is gone"));
        let mut signed = good.clone();
        signed[1].gpg = true;
        assert!(says(signed).contains("signature check of \"rt\""));
        let mut off = good.clone();
        off[1].disabled = true;
        assert!(says(off).contains("\"rt\" is disabled"));
        // A plan made by hand without the snapshot still holds the app's
        // own remote to what it states.
        let bare = InstallPlan {
            remotes: vec![],
            ..plan.clone()
        };
        assert!(check_plan_source(&bare, &good).is_ok());
        assert!(check_plan_source(&bare, &[]).is_err());
    }

    #[test]
    fn only_a_unreadable_installation_or_a_bad_runtime_stops_the_dependency_scan() {
        let e = |kind, ref_kind| super::super::ListError {
            scope: Scope::User,
            kind,
            message: "x".into(),
            ref_kind,
        };
        assert!(fatal_list_error(&e(ListErrorKind::ScopeUnreadable, None)));
        assert!(fatal_list_error(&e(
            ListErrorKind::BadRef,
            Some(RefKind::Runtime)
        )));
        // The kind could not even be told: it may be a runtime.
        assert!(fatal_list_error(&e(ListErrorKind::BadRef, None)));
        assert!(!fatal_list_error(&e(
            ListErrorKind::BadRef,
            Some(RefKind::App)
        )));
        assert!(!fatal_list_error(&e(ListErrorKind::ExtensionLink, None)));
        assert!(!fatal_list_error(&e(ListErrorKind::TooMany, None)));
    }

    #[test]
    fn a_metadata_mismatch_says_in_plain_words_whether_it_was_undone() {
        let yes = Error::MetadataMismatch { rolled_back: true }.to_string();
        let no = Error::MetadataMismatch { rolled_back: false }.to_string();
        assert!(yes.contains("removed again") && !yes.contains("could not be removed"));
        assert!(no.contains("could not be removed again"));
    }

    fn dep(r: &str, ext_of: Option<&str>, uses: &[&str]) -> Dep {
        Dep {
            full_ref: r.into(),
            ext_of: ext_of.map(Into::into),
            uses: uses.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn extensions_and_users_are_told_apart() {
        let plat = "runtime/org.t.Platform/x86_64/1";
        let deps = vec![
            dep(plat, None, &[]),
            dep("runtime/org.t.Platform.Ext/x86_64/1", Some(plat), &[]),
            dep(
                "runtime/org.t.Lone.Ext/x86_64/1",
                Some("runtime/org.t.Gone/x86_64/1"),
                &[],
            ),
            dep("runtime/org.t.Evil/x86_64/1", Some("../../etc"), &[]),
            dep(
                "app/org.t.App/x86_64/stable",
                None,
                &["org.t.Platform/x86_64/1"],
            ),
            dep(
                "runtime/org.t.Sdk/x86_64/1",
                None,
                &["org.t.Platform/x86_64/1"],
            ),
        ];
        assert_eq!(
            extension_parent(&deps, "runtime/org.t.Platform.Ext/x86_64/1").as_deref(),
            Some(plat)
        );
        // A parent that is not installed, a bad ref and a plain runtime: no parent.
        for t in [
            "runtime/org.t.Lone.Ext/x86_64/1",
            "runtime/org.t.Evil/x86_64/1",
            plat,
            "runtime/org.t.Missing/x86_64/1",
        ] {
            assert_eq!(extension_parent(&deps, t), None, "{t}");
        }
        // The extension doesn't count as a user; the app and the sdk do, and
        // a ref in `skip` goes together with the target.
        let users = runtime_users(&deps, plat, &[]).unwrap();
        assert_eq!(
            users,
            vec!["app/org.t.App/x86_64/stable", "runtime/org.t.Sdk/x86_64/1"]
        );
        let users = runtime_users(&deps, plat, &["runtime/org.t.Sdk/x86_64/1".into()]).unwrap();
        assert_eq!(users, vec!["app/org.t.App/x86_64/stable"]);
        assert!(runtime_users(&deps, "nonsense", &[]).is_err());
    }

    #[test]
    fn leftover_names_are_recognized() {
        assert!(is_leftover(b".org.test.Hello.deleting"));
        assert!(!is_leftover(b"org.test.Hello.deleting"));
        assert!(!is_leftover(b".deleting"));
        assert!(!is_leftover(b"..deleting"));
        assert!(!is_leftover(b".nodots.deleting"));
        assert!(is_leftover(b".org.test.Hello.123.4.deleting"));
        assert!(is_leftover(leftover_name("org.test.Hello").as_bytes()));
        assert_ne!(
            leftover_name("org.test.Hello"),
            leftover_name("org.test.Hello")
        );
    }
}

#[cfg(test)]
mod data_tests {
    use super::*;
    use crate::flatpak::testenv::{drop_dac_caps, scratch};
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn app_data_goes_but_not_what_a_link_points_at() {
        let Some(home) = scratch("tree") else { return };
        let outside = home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"keep").unwrap();
        let data = home.join(".var/app/org.test.Hello");
        std::fs::create_dir_all(data.join("config/deep/er")).unwrap();
        std::fs::write(data.join("config/deep/er/f"), b"x").unwrap();
        std::fs::write(data.join("top"), b"x").unwrap();
        symlink(&outside, data.join("dirlink")).unwrap();
        symlink(outside.join("secret"), data.join("filelink")).unwrap();
        symlink("/nonexistent", data.join("dangling")).unwrap();
        std::fs::create_dir_all(home.join(".var/app/org.test.Other")).unwrap();
        // A leftover of an earlier, interrupted delete is swept.
        let left = home.join(".var/app/.org.test.Old.deleting");
        std::fs::create_dir_all(left.join("x")).unwrap();

        assert!(delete_app_data(&home, "org.test.Hello").unwrap());
        assert!(!data.exists());
        assert!(!left.exists());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"keep");
        assert!(home.join(".var/app/org.test.Other").is_dir());
        assert!(!home.join(".var/app/.org.test.Hello.deleting").exists());
        // Nothing there any more: not an error.
        assert!(!delete_app_data(&home, "org.test.Hello").unwrap());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_symlinked_home_works_and_a_symlink_in_the_way_is_removed() {
        let Some(home) = scratch("link") else { return };
        let outside = home.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"keep").unwrap();
        std::fs::create_dir_all(home.join("real/.var/app")).unwrap();
        symlink(&outside, home.join("real/.var/app/org.test.Hello")).unwrap();
        symlink(home.join("real"), home.join("homelink")).unwrap();
        assert!(delete_app_data(&home.join("homelink"), "org.test.Hello").unwrap());
        assert!(!home.join("real/.var/app/org.test.Hello").exists());
        assert_eq!(std::fs::read(outside.join("secret")).unwrap(), b"keep");

        // `.var` itself a link: nothing is followed or deleted.
        let other = home.join("other");
        std::fs::create_dir_all(other.join("app/org.test.Hello")).unwrap();
        std::fs::create_dir_all(home.join("h2")).unwrap();
        symlink(&other, home.join("h2/.var")).unwrap();
        assert!(delete_app_data(&home.join("h2"), "org.test.Hello").is_err());
        assert!(other.join("app/org.test.Hello").is_dir());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn bad_ids_and_homes_are_refused_and_depth_is_limited() {
        let Some(home) = scratch("bad") else { return };
        for id in ["..", "a/b", "org.test.Hello/../x", "nodots", ""] {
            assert!(delete_app_data(&home, id).is_err(), "{id}");
        }
        assert!(delete_app_data(Path::new("relative"), "org.test.Hello").is_err());
        let mut deep = home.join(".var/app/org.test.Deep");
        for _ in 0..300 {
            deep.push("d");
        }
        std::fs::create_dir_all(&deep).unwrap();
        assert!(delete_app_data(&home, "org.test.Deep").is_err());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_folder_nobody_may_open_is_an_error_in_plain_words() {
        let Some(home) = scratch("eacces") else {
            return;
        };
        let locked = home.join(".var/app/org.test.Hello/locked");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("f"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o0)).unwrap();
        // A thread of its own loses root's permission overrides, so nothing
        // else in the test process is affected.
        let h2 = home.clone();
        let dropped = std::thread::spawn(move || {
            if !drop_dac_caps() {
                return false;
            }
            let e = delete_app_data(&h2, "org.test.Hello").unwrap_err();
            match &e {
                Error::Io { message, .. } => assert!(message.contains("permission"), "{message}"),
                other => panic!("{other:?}"),
            }
            true
        })
        .join()
        .unwrap();
        if !dropped {
            eprintln!("skipped: could not drop CAP_DAC_OVERRIDE for a thread");
        }
        // Nothing was lost: the data is under its leftover name, and the
        // next delete (with permission) finishes the job.
        let app_dir = home.join(".var/app");
        let left: Vec<_> = std::fs::read_dir(&app_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".deleting"))
            .collect();
        if dropped {
            assert_eq!(left.len(), 1, "{left:?}");
            std::fs::set_permissions(
                left[0].join("locked"),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            let mut sweep = Vec::new();
            assert!(!delete_app_data_with(&home, "org.test.Hello", &mut sweep).unwrap());
            assert!(sweep.is_empty(), "{sweep:?}");
            assert!(!left[0].exists());
        } else {
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn leftover_names_never_collide_and_a_file_leftover_is_unlinked() {
        let Some(home) = scratch("leftover") else {
            return;
        };
        let app = home.join(".var/app");
        std::fs::create_dir_all(app.join("org.test.Hello")).unwrap();
        // A plain file under a leftover name is unlinked, not left to rot.
        std::fs::write(app.join(".org.test.Old.9.9.deleting"), b"x").unwrap();
        let mut sweep = Vec::new();
        assert!(delete_app_data_with(&home, "org.test.Hello", &mut sweep).unwrap());
        assert!(sweep.is_empty(), "{sweep:?}");
        assert!(!app.join(".org.test.Old.9.9.deleting").exists());
        // The rename never replaces: a taken name is an error and keeps both.
        let d = std::fs::File::open(&app).unwrap();
        std::fs::create_dir(app.join("a")).unwrap();
        std::fs::create_dir(app.join("b")).unwrap();
        let e = renameat_noreplace(
            std::os::fd::AsRawFd::as_raw_fd(&d),
            &cstr("a").unwrap(),
            &cstr("b").unwrap(),
        )
        .unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::EEXIST));
        assert!(app.join("a").is_dir() && app.join("b").is_dir());
        std::fs::remove_dir_all(&home).unwrap();
    }
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use crate::flatpak::supervise::BEATS;
    use crate::flatpak::testenv;
    use crate::flatpak::{OperationLock, update_appstream};

    fn rank(name: &str, prio: i32, gpg: bool, enumerable: bool) -> RemoteRank {
        RemoteRank {
            name: name.into(),
            prio,
            gpg,
            enumerable,
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_runtime_comes_from_the_best_remote_and_the_apps_own_only_breaks_ties() {
        let ranks = [
            rank("own", 0, true, true),
            rank("high", 10, true, true),
            rank("unsigned", 99, false, true),
            rank("hidden", 99, true, false),
            rank("twin", 0, true, true),
        ];
        let c = names(&["own", "high", "unsigned", "hidden", "twin"]);
        // Priority beats being the app's own remote.
        assert_eq!(choose_remote(&c, &ranks, Some("own")), Some(1));
        // Signed and listed beat a higher priority without them.
        assert_eq!(
            choose_remote(&names(&["unsigned", "hidden", "own"]), &ranks, None),
            Some(2)
        );
        assert_eq!(
            choose_remote(&names(&["unsigned", "hidden"]), &ranks, None),
            Some(1)
        );
        // A tie goes to the app's own remote, else to the first.
        let tie = names(&["twin", "own"]);
        assert_eq!(choose_remote(&tie, &ranks, Some("own")), Some(1));
        assert_eq!(choose_remote(&tie, &ranks, None), Some(0));
        // A remote that is not a known source is never chosen.
        assert_eq!(
            choose_remote(&names(&["ghost"]), &ranks, Some("ghost")),
            None
        );
        assert_eq!(choose_remote(&[], &ranks, None), None);
        assert_eq!(choose_remote(&c, &[], Some("own")), None);
    }

    #[test]
    fn a_non_fatal_operation_error_is_a_warning_and_the_run_goes_on() {
        let st = Rc::new(RefCell::new(St::default()));
        assert!(record_op_error(
            &st,
            "runtime/a.b/x/s",
            "no extension",
            true
        ));
        assert!(!record_op_error(&st, "app/a.b/x/s", "disk full", false));
        let s = st.borrow();
        assert_eq!(
            s.warnings,
            vec!["runtime/a.b/x/s: no extension".to_string()]
        );
        assert_eq!(s.op_errors, vec!["app/a.b/x/s: disk full".to_string()]);
    }

    fn glib_err(code: libflatpak::Error) -> glib::Error {
        glib::Error::new(code, "x")
    }

    #[test]
    fn how_a_run_ended_becomes_the_error_in_plain_words() {
        let c = CancelToken::new();
        let done = vec!["runtime/a.b/x/s".to_string()];
        let run = |res: Result<(), glib::Error>, f: &dyn Fn(&mut St)| {
            let mut s = St {
                done: done.clone(),
                ready_seen: true,
                ..St::default()
            };
            f(&mut s);
            finish_run(res, s, false, &c).map(|_| ()).map_err(|f| f.err)
        };
        // A runtime that needs another remote.
        let e = run(Err(glib_err(libflatpak::Error::RuntimeNotFound)), &|s| {
            s.needs_repo = Some("https://runtimes.example.org/repo".into())
        })
        .unwrap_err();
        assert_eq!(
            e,
            Error::NeedsRuntimeRepo("https://runtimes.example.org/repo".into())
        );
        // A source that sent too much, even though the run reports a cancel.
        let cancelled = glib::Error::new(libflatpak::gio::IOErrorEnum::Cancelled, "x");
        assert_eq!(
            run(Err(cancelled), &|s| s.over_cap = true).unwrap_err(),
            Error::SentTooMuch
        );
        // A plan that changed wins over libflatpak's "aborted".
        let e = run(Err(glib_err(libflatpak::Error::Aborted)), &|s| {
            s.err = Some(Error::PlanChanged(PlanChange::default()))
        })
        .unwrap_err();
        assert!(matches!(e, Error::PlanChanged(_)));
        assert_eq!(
            run(Err(glib_err(libflatpak::Error::Aborted)), &|s| s.sign_in =
                true)
            .unwrap_err(),
            Error::SignInRefused
        );
        assert_eq!(
            run(Err(glib_err(libflatpak::Error::RuntimeNotFound)), &|_| {}).unwrap_err(),
            Error::RuntimeNotFound
        );
        // An operation that failed: its message, and what finished is kept.
        let e = run(Err(glib_err(libflatpak::Error::Aborted)), &|s| {
            s.op_errors.push("app/a.b/x/s: disk full".into())
        })
        .unwrap_err();
        assert!(matches!(&e, Error::Flatpak { message, .. } if message.contains("disk full")));
        // A run that went through, over the cap, only warns.
        let s = St {
            over_cap: true,
            ..St::default()
        };
        let ok = finish_run(Ok(()), s, false, &c).ok().unwrap();
        assert_eq!(ok.warnings, vec!["The source sent more than it announced."]);
        assert_eq!(
            Error::SentTooMuch.to_string(),
            "The source sent more than it announced."
        );
    }

    #[test]
    fn the_byte_cap_is_a_quarter_and_64_mib_over_the_plan() {
        let op = |n| PlannedOp {
            kind: OpKind::Install,
            ref_: "app/a.b/x/s".into(),
            commit: "ab".into(),
            remote: "r".into(),
            download_size: n,
            installed_size: 0,
            signed: false,
        };
        let mode = |ops| Mode::Install {
            ops,
            main_ref: "app/a.b/x/s".into(),
            remote: "r".into(),
        };
        assert_eq!(mode(vec![op(0)]).byte_cap(), 64 << 20);
        assert_eq!(mode(vec![op(400), op(400)]).byte_cap(), 1000 + (64 << 20));
        assert_eq!(mode(vec![op(u64::MAX), op(5)]).byte_cap(), u64::MAX);
        assert_eq!(Mode::Dry.byte_cap(), u64::MAX);
    }

    #[test]
    fn metadata_compares_by_content_not_by_spelling() {
        let a = b"[Application]\nname=a.b\n# a comment\n\n[Context]\nshared=ipc;\n";
        let b = b"[Application]\nname=a.b\n[Context]\nshared=ipc;\n";
        let c = b"[Application]\nname=a.b\n[Context]\nshared=ipc;network;\n";
        assert_eq!(normalized_metadata(a), normalized_metadata(b));
        assert_ne!(normalized_metadata(b), normalized_metadata(c));
        assert!(normalized_metadata(b"\xff\xfe").is_none());
        assert!(normalized_metadata(b"no group\n").is_none());
    }

    #[test]
    fn the_progress_notice_keeps_the_last_report() {
        let n = Progress::not_responding(None);
        assert!(n.not_responding && n.status == "Not responding" && n.op == 0);
        let mut last = n.clone();
        last.not_responding = false;
        last.ref_ = "app/a.b/x/s".into();
        last.percent = 40;
        let n = Progress::not_responding(Some(&last));
        assert!(n.not_responding && n.percent == 40 && n.ref_ == "app/a.b/x/s");
    }

    #[test]
    fn real_operations_deliver_beats() {
        let Some((dir, _g)) = testenv::guard() else {
            return;
        };
        testenv::reset(&dir);
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        // The AppStream refresh reports through the supervisor.
        BEATS.with(|b| b.set(0));
        update_appstream(Scope::User, "test", false, &lock, &c).unwrap();
        let appstream = BEATS.with(|b| b.get());
        assert!(appstream > 0, "update_appstream sent no beat");
        // So does an install, and the dry run before it ends supervised.
        let r = format!(
            "app/org.test.Hello/{}/stable",
            libflatpak::default_arch().unwrap()
        );
        let plan = plan_install(Scope::User, "test", &r, &c).unwrap();
        BEATS.with(|b| b.set(0));
        let mut reports = Vec::new();
        install(&plan, &lock, &c, |p| reports.push(p)).unwrap();
        let beats = BEATS.with(|b| b.get());
        assert!(
            beats >= plan.ops.len(),
            "{beats} beats for {} ops",
            plan.ops.len()
        );
        assert!(reports.iter().all(|p| !p.not_responding));
    }
}
