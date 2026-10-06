//! The Flatpak jobs the Store runs: plan and confirm an install, install,
//! remove, remove unused runtimes, open an app, and the list of what is
//! installed.
//!
//! Threading: `Jobs::start` runs ONE worker thread that takes one job at a
//! time from a channel. Every job that changes an installation (and the plan
//! before it) takes the shared `OperationLock` on that worker, showing
//! "Another update is running" while it waits. Reads (listing, opening an
//! app) take none: they change nothing. The GUI thread only sends jobs and
//! applies results queued back with `qt_thread().queue`. The installed list is
//! read again after every job, so a cancel or timeout in the system scope
//! (where `Partial.completed` can't be trusted) shows what is really there.
//!
//! One job at a time: `phase` is "idle" or the running job, and a request
//! made while it is not idle is refused. A plan waits for the user's answer
//! with the phase "idle" and the plan kept here; `confirmInstall` hands
//! exactly that plan to the core, which compares it with a fresh transaction.
//! All text the QML shows from here is plain; lists go to QML as JSON.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        /// "idle", "planning", "installing", "removing", "unused" or "opening".
        #[qproperty(QString, phase)]
        /// The app the running job is for.
        #[qproperty(QString, app_id, cxx_name = "appId")]
        /// 0 to 100 while a figure is known, else -1.
        #[qproperty(i32, percent)]
        /// One plain line about what the job is doing.
        #[qproperty(QString, status)]
        /// The job has been silent for long; Cancel must stay enabled.
        #[qproperty(bool, not_responding, cxx_name = "notResponding")]
        /// The app the error or result text is about ("" for none).
        #[qproperty(QString, message_app, cxx_name = "messageApp")]
        /// Why the last job failed, in plain words, or "".
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// What the last job did, in plain words, or "".
        #[qproperty(QString, result_text, cxx_name = "resultText")]
        /// The installed list was read at least once.
        #[qproperty(bool, installed_ready, cxx_name = "installedReady")]
        /// Goes up each time the installed list is replaced.
        #[qproperty(i32, installed_revision, cxx_name = "installedRevision")]
        /// JSON array of the installed apps.
        #[qproperty(QString, installed_json, cxx_name = "installedJson")]
        /// Problems reading the installed list, one per line.
        #[qproperty(QString, installed_error, cxx_name = "installedError")]
        /// JSON object: what the install dialog shows.
        #[qproperty(QString, plan_json, cxx_name = "planJson")]
        /// JSON array: the unused runtimes offered for removal.
        #[qproperty(QString, unused_json, cxx_name = "unusedJson")]
        #[namespace = "atlas_store"]
        type Jobs = super::JobsRust;

        /// A plan is ready for the user to confirm.
        #[qsignal]
        #[cxx_name = "planReady"]
        fn plan_ready(self: Pin<&mut Jobs>, app_id: QString);

        /// A removal with "Also Delete App Data" stopped because the app is
        /// running; the dialog offers to close it (`closeAndRemove`).
        #[qsignal]
        #[cxx_name = "removeBlocked"]
        fn remove_blocked(self: Pin<&mut Jobs>, app_id: QString, full_ref: QString);

        /// The unused runtimes were listed (`count` may be 0).
        #[qsignal]
        #[cxx_name = "unusedReady"]
        fn unused_ready(self: Pin<&mut Jobs>, count: i32);
    }

    unsafe extern "RustQt" {
        /// Starts the worker and reads what is installed. Once.
        #[qinvokable]
        fn start(self: Pin<&mut Jobs>);

        /// Reads the installed list again (on the worker).
        #[qinvokable]
        fn refresh(self: Pin<&mut Jobs>);

        /// Plans the install of a catalog app from its source.
        #[qinvokable]
        #[cxx_name = "planInstall"]
        fn plan_install(self: Pin<&mut Jobs>, app_id: &QString);

        /// Installs the plan the user confirmed.
        #[qinvokable]
        #[cxx_name = "confirmInstall"]
        fn confirm_install(self: Pin<&mut Jobs>);

        /// Stops the running job, or drops a plan nobody confirmed.
        #[qinvokable]
        fn cancel(self: Pin<&mut Jobs>);

        /// Removes an installed app, with its data only if asked.
        #[qinvokable]
        fn remove(self: Pin<&mut Jobs>, app_id: &QString, full_ref: &QString, delete_data: bool);

        /// Closes the running app (SIGTERM, then SIGKILL after 3 s), then
        /// removes it with its data. Only after the user confirmed that.
        #[qinvokable]
        #[cxx_name = "closeAndRemove"]
        fn close_and_remove(self: Pin<&mut Jobs>, app_id: &QString, full_ref: &QString);

        /// Lists the unused runtimes (signals `unusedReady`).
        #[qinvokable]
        #[cxx_name = "checkUnused"]
        fn check_unused(self: Pin<&mut Jobs>);

        /// Removes the unused runtimes last listed.
        #[qinvokable]
        #[cxx_name = "removeUnused"]
        fn remove_unused(self: Pin<&mut Jobs>);

        /// Opens an installed app. `token` is the activation token QML got
        /// from `ActivationToken` ("" for none).
        #[qinvokable]
        fn open(self: Pin<&mut Jobs>, app_id: &QString, token: &QString);

        #[qinvokable]
        #[cxx_name = "clearMessages"]
        fn clear_messages(self: Pin<&mut Jobs>);

        /// "user", "system", or "" when the app is not installed.
        #[qinvokable]
        #[cxx_name = "installedScope"]
        fn installed_scope(self: &Jobs, app_id: &QString) -> QString;

        /// JSON for an app's page, from the catalog and the installed list.
        #[qinvokable]
        #[cxx_name = "appInfo"]
        fn app_info(self: &Jobs, app_id: &QString) -> QString;
    }

    impl cxx_qt::Threading for Jobs {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn jobs_make_unique() -> UniquePtr<Jobs>;
    }
}

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use atlas_store_core::appstream::{Block, Span, UrlKind};
use atlas_store_core::catalog::is_free_license;
use atlas_store_core::flatpak::{
    CancelToken, Error, InstallPlan, InstalledRef, LockError, OperationLock, Progress, RefKind,
    Scope, close_app, install, launch_app, list_installed_all, list_unused, plan_install,
    sweep_pending_remotes, uninstall, uninstall_unused, valid_activation_token,
};
use atlas_store_core::launch::https_url;
use atlas_store_core::permissions::{Permissions, Risk};
use atlas_store_core::text::clean;
use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::{Value, json};

use crate::catalog::{ICON_SIZE, library, safe_icon};

/// How long a job waits for the Updater's lock before it gives up.
const LOCK_GIVE_UP: Duration = Duration::from_secs(600);
/// Descriptions longer than this many blocks are cut.
const MAX_BLOCKS: usize = 60;

/// An installed app, ready to show.
#[derive(Clone, Debug)]
struct Entry {
    id: String,
    name: String,
    summary: String,
    version: String,
    scope: Scope,
    size: u64,
    full_ref: String,
    arch: String,
    branch: String,
    /// A `file:` URL or "".
    icon: String,
}

/// An unused runtime offered for removal.
#[derive(Clone, Debug)]
struct Unused {
    scope: Scope,
    full_ref: String,
    id: String,
    branch: String,
    size: u64,
}

struct Listing {
    entries: Vec<Entry>,
    error: String,
}

enum Job {
    Startup,
    Refresh,
    Plan {
        name: String,
        scope: Scope,
        remote: String,
        ref_: String,
    },
    Install {
        plan: Box<InstallPlan>,
        name: String,
    },
    Remove {
        name: String,
        app_id: String,
        scope: Scope,
        ref_: String,
        delete_data: bool,
        /// Close the app's running instances first (the user said so).
        close_first: bool,
    },
    Unused,
    RemoveUnused(Vec<(Scope, Vec<String>)>),
    /// The app and the activation token QML got for it, if any.
    Open(Entry, Option<String>),
}

impl Job {
    /// Whether the job set the phase (so it must clear it).
    fn foreground(&self) -> bool {
        !matches!(self, Job::Startup | Job::Refresh)
    }
}

struct Msg {
    job: Job,
    cancel: CancelToken,
}

#[derive(Default)]
struct Outcome {
    error: Option<String>,
    result: Option<String>,
    plan: Option<(InstallPlan, String)>,
    unused: Option<Vec<Unused>>,
    /// The removal stopped because the app runs: the ref to offer closing it for.
    blocked_ref: Option<String>,
}

pub struct JobsRust {
    phase: QString,
    app_id: QString,
    percent: i32,
    status: QString,
    not_responding: bool,
    error_text: QString,
    message_app: QString,
    result_text: QString,
    installed_ready: bool,
    installed_revision: i32,
    installed_json: QString,
    installed_error: QString,
    plan_json: QString,
    unused_json: QString,
    worker: Option<mpsc::Sender<Msg>>,
    /// The token of the last background job (startup, refresh).
    bg_cancel: CancelToken,
    /// The token of the last foreground job (what Cancel stops).
    fg_cancel: CancelToken,
    handle: Option<std::thread::JoinHandle<()>>,
    installed: Vec<Entry>,
    plan: Option<InstallPlan>,
    plan_name: String,
    unused: Vec<Unused>,
}

impl Default for JobsRust {
    fn default() -> Self {
        JobsRust {
            phase: QString::from("idle"),
            app_id: QString::default(),
            percent: -1,
            status: QString::default(),
            not_responding: false,
            error_text: QString::default(),
            message_app: QString::default(),
            result_text: QString::default(),
            installed_ready: false,
            installed_revision: 0,
            installed_json: QString::from("[]"),
            installed_error: QString::default(),
            plan_json: QString::from("{}"),
            unused_json: QString::from("[]"),
            worker: None,
            bg_cancel: CancelToken::new(),
            fg_cancel: CancelToken::new(),
            handle: None,
            installed: Vec::new(),
            plan: None,
            plan_name: String::new(),
            unused: Vec::new(),
        }
    }
}

impl Drop for JobsRust {
    /// Quitting stops a running job cleanly and ends the worker (it leaves
    /// when the channel closes).
    fn drop(&mut self) {
        self.fg_cancel.cancel();
        self.bg_cancel.cancel();
        // Closing the channel ends the worker once the running job returns.
        self.worker = None;
        if let Some(h) = self.handle.take() {
            let start = Instant::now();
            while !h.is_finished() {
                if start.elapsed() > Duration::from_secs(10) {
                    log::error!("the Flatpak worker did not stop within 10 s; leaving it");
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = h.join();
        }
    }
}

// ---- formatting ----

fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1000.0;
    let mut unit = 0;
    while v >= 1000.0 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

fn risk_word(r: Risk) -> &'static str {
    match r {
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
    }
}

fn scope_word(s: Scope) -> &'static str {
    s.label()
}

/// `runtime/ID/arch/branch` as `ID (branch)`.
fn pretty_ref(r: &str) -> String {
    let parts: Vec<&str> = r.split('/').collect();
    if parts.len() == 4 {
        format!("{} ({})", parts[1], parts[3])
    } else {
        clean(r, 120)
    }
}

fn spans(s: &[Span]) -> String {
    s.iter().map(|x| x.text.as_str()).collect()
}

/// A description as plain paragraphs; a list is one item with a line each.
fn blocks(b: &[Block]) -> Vec<String> {
    b.iter()
        .take(MAX_BLOCKS)
        .map(|b| match b {
            Block::Paragraph(s) => spans(s),
            Block::List { ordered, items } => items
                .iter()
                .enumerate()
                .map(|(i, it)| {
                    if *ordered {
                        format!("{}. {}", i + 1, spans(it))
                    } else {
                        format!("\u{2022} {}", spans(it))
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .collect()
}

fn url_label(k: UrlKind) -> Option<&'static str> {
    match k {
        UrlKind::Homepage => Some("Website"),
        UrlKind::Help => Some("Help"),
        UrlKind::Bugtracker => Some("Report a Problem"),
        UrlKind::Donation => Some("Donate"),
        UrlKind::Faq => Some("FAQ"),
        _ => None,
    }
}

fn plan_json(plan: &InstallPlan, name: &str) -> Result<String, String> {
    let perms = Permissions::from_metadata(&plan.metadata).map_err(|e| {
        log::warn!("permissions of {} unreadable: {e}", plan.ref_);
        format!("This app's permissions could not be read ({e}), so it can't be installed.")
    })?;
    let list: Vec<Value> = perms
        .permissions()
        .iter()
        .map(|p| json!({"text": p.describe(), "risk": risk_word(p.risk())}))
        .collect();
    let runtimes: Vec<Value> = plan
        .new_runtimes
        .iter()
        .map(|o| {
            json!({
                "name": pretty_ref(&o.ref_),
                "remote": o.remote,
                "download": human_size(o.download_size),
                "installed": human_size(o.installed_size),
                "signed": o.signed,
            })
        })
        .collect();
    Ok(json!({
        "name": name,
        "ref": plan.ref_,
        "scope": scope_word(plan.scope),
        "remote": plan.remote,
        "remoteUrl": plan.remote_url,
        "signed": plan.gpg_verified,
        "download": human_size(plan.download_total),
        "installed": human_size(plan.installed_total),
        "runtimes": runtimes,
        "permissions": list,
        "maxRisk": perms.max_risk().map(risk_word).unwrap_or("none"),
    })
    .to_string())
}

// ---- the worker ----

fn say(thread: &CxxQtThread<qobject::Jobs>, line: &str) {
    let line = line.to_string();
    let _ = thread.queue(move |mut j| j.as_mut().set_status(QString::from(line.as_str())));
}

fn progress_sink(thread: &CxxQtThread<qobject::Jobs>) -> impl FnMut(Progress) + use<> {
    let thread = thread.clone();
    move |p: Progress| {
        let pct = if p.not_responding || p.ops == 0 {
            None
        } else {
            let done = (p.op.saturating_sub(1) * 100 + usize::from(p.percent.min(100))) / p.ops;
            Some(i32::try_from(done).unwrap_or(0))
        };
        let line = if p.not_responding {
            "This is taking a long time. You can cancel.".to_string()
        } else if p.ops > 1 {
            format!("{} ({} of {})", clean(&p.status, 80), p.op, p.ops)
        } else {
            clean(&p.status, 80)
        };
        let nr = p.not_responding;
        let _ = thread.queue(move |mut j| {
            if let Some(pct) = pct {
                j.as_mut().set_percent(pct);
            }
            j.as_mut().set_status(QString::from(line.as_str()));
            j.as_mut().set_not_responding(nr);
        });
    }
}

/// Takes the shared lock; says so while another update holds it.
fn take_lock(
    thread: &CxxQtThread<qobject::Jobs>,
    cancel: &CancelToken,
) -> Result<OperationLock, Error> {
    let lock_error = |e: LockError| match e {
        LockError::Cancelled => Error::Cancelled,
        other => Error::Invalid(other.to_string().trim_end_matches('.').to_string()),
    };
    match OperationLock::acquire(Duration::from_millis(400), cancel) {
        Ok(l) => return Ok(l),
        Err(LockError::Busy(_)) => {}
        Err(e) => return Err(lock_error(e)),
    }
    say(thread, "Another update is running");
    let start = Instant::now();
    loop {
        match OperationLock::acquire(Duration::from_secs(2), cancel) {
            Ok(l) => return Ok(l),
            Err(LockError::Busy(_)) if start.elapsed() < LOCK_GIVE_UP => {}
            Err(e) => return Err(lock_error(e)),
        }
    }
}

fn entry_of(r: &InstalledRef) -> Entry {
    let lib = library();
    let found = lib.as_ref().and_then(|l| l.find(&r.id).map(|id| (l, id)));
    let (name, icon) = match found {
        Some((l, id)) => {
            let c = l.component(id);
            (
                c.name.clone(),
                l.source(id)
                    .icon_path(c, ICON_SIZE)
                    .and_then(|p| safe_icon(&p))
                    .unwrap_or_default(),
            )
        }
        None => (String::new(), String::new()),
    };
    let name = if !name.is_empty() {
        name
    } else if !r.name.is_empty() {
        r.name.clone()
    } else {
        r.id.clone()
    };
    Entry {
        id: r.id.clone(),
        name,
        summary: r.summary.clone(),
        version: r.version.clone(),
        scope: r.scope,
        size: r.installed_size,
        full_ref: r.full_ref(),
        arch: r.arch.clone(),
        branch: r.branch.clone(),
        icon,
    }
}

fn list(cancel: &CancelToken) -> Listing {
    let out = list_installed_all(cancel);
    let mut entries: Vec<Entry> = Vec::new();
    let mut apps: Vec<&InstalledRef> = out
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::App && r.related_to.is_none())
        .collect();
    // One row per app ID and installation: the current branch first.
    apps.sort_by_key(|r| !r.is_current);
    for r in apps {
        if !entries.iter().any(|e| e.id == r.id && e.scope == r.scope) {
            entries.push(entry_of(r));
        }
    }
    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.scope.label().cmp(b.scope.label()))
    });
    let error = out
        .errors
        .iter()
        .take(5)
        .map(|e| e.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    Listing { entries, error }
}

fn unused_all(cancel: &CancelToken) -> Vec<Unused> {
    let mut all = Vec::new();
    for scope in [Scope::User, Scope::System] {
        match list_unused(scope, cancel) {
            Ok(v) => all.extend(v.iter().map(|r| Unused {
                scope,
                full_ref: r.full_ref(),
                id: r.id.clone(),
                branch: r.branch.clone(),
                size: r.installed_size,
            })),
            Err(e) => log::warn!("could not list unused runtimes ({}): {e}", scope.label()),
        }
    }
    all
}

fn fail(out: &mut Outcome, e: &Error) {
    if matches!(e, Error::Cancelled) {
        out.result = Some("Cancelled.".to_string());
    } else if matches!(e, Error::Partial { .. }) {
        log::warn!("job stopped part way: {e}");
        out.error = Some(format!(
            "{e} A system-wide change may still finish in the background; the list shows what is installed now."
        ));
    } else {
        log::warn!("job failed: {e}");
        out.error = Some(e.to_string());
    }
}

fn run_job(thread: &CxxQtThread<qobject::Jobs>, job: Job, cancel: &CancelToken) -> Outcome {
    let mut out = Outcome::default();
    match job {
        Job::Startup => match OperationLock::acquire(Duration::from_secs(3), cancel) {
            Ok(lock) => {
                for scope in [Scope::User, Scope::System] {
                    let o = sweep_pending_remotes(scope, &lock, cancel);
                    if !o.failed.is_empty() || o.journal_error.is_some() {
                        log::warn!(
                            "sweeping pending sources ({}): {} failed, journal: {:?}",
                            scope.label(),
                            o.failed.len(),
                            o.journal_error
                        );
                    }
                }
            }
            Err(e) => log::info!("not sweeping pending sources: {e}"),
        },
        Job::Refresh => {}
        Job::Plan {
            name,
            scope,
            remote,
            ref_,
        } => {
            say(thread, "Preparing\u{2026}");
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e),
                Ok(lock) => {
                    let planned = plan_install(scope, &remote, &ref_, cancel);
                    drop(lock);
                    match planned {
                        Ok(plan) => match plan_json(&plan, &name) {
                            Ok(json) => out.plan = Some((plan, json)),
                            Err(text) => out.error = Some(text),
                        },
                        Err(e) => fail(&mut out, &e),
                    }
                }
            }
        }
        Job::Install { plan, name } => {
            say(thread, "Starting\u{2026}");
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e),
                Ok(lock) => {
                    match install(&plan, &lock, cancel, progress_sink(thread)) {
                        Ok(done) => {
                            let mut text = format!("Installed {name}.");
                            for w in done.warnings.iter().take(3) {
                                text.push(' ');
                                text.push_str(&clean(w, 200));
                            }
                            out.result = Some(text);
                        }
                        Err(e) => fail(&mut out, &e),
                    }
                    drop(lock);
                }
            }
        }
        Job::Remove {
            name,
            app_id,
            scope,
            ref_,
            delete_data,
            close_first,
        } => {
            say(thread, "Removing\u{2026}");
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e),
                Ok(lock) => {
                    // The user confirmed closing the app. On the worker: it
                    // polls and sleeps for up to a few seconds.
                    let closed = if close_first {
                        say(thread, "Closing the app\u{2026}");
                        close_app(&app_id, cancel)
                    } else {
                        Ok(())
                    };
                    let removed = closed.and_then(|()| {
                        uninstall(
                            scope,
                            &ref_,
                            delete_data,
                            &lock,
                            cancel,
                            progress_sink(thread),
                        )
                    });
                    match removed {
                        Ok(done) => {
                            use atlas_store_core::flatpak::DataResult as D;
                            let mut text = format!("Removed {name}.");
                            match done.data {
                                D::Deleted => text.push_str(" Its data was deleted."),
                                D::Kept(why) => text
                                    .push_str(&format!(" Its data was kept: {}", clean(&why, 200))),
                                D::Failed(why) => text.push_str(&format!(
                                    " Its data could not be deleted: {}",
                                    clean(&why, 200)
                                )),
                                D::NotRequested | D::NoData => {}
                            }
                            out.result = Some(text);
                            let unused = unused_all(cancel);
                            if !unused.is_empty() {
                                out.unused = Some(unused);
                            }
                        }
                        // Asked in the dialog, not shown as an error. A second
                        // AppRunning after closing is a real failure.
                        Err(Error::AppRunning) if !close_first => out.blocked_ref = Some(ref_),
                        Err(e) => fail(&mut out, &e),
                    }
                    drop(lock);
                }
            }
        }
        Job::Unused => {
            say(thread, "Looking for unused runtimes\u{2026}");
            out.unused = Some(unused_all(cancel));
        }
        Job::RemoveUnused(lists) => {
            say(thread, "Removing\u{2026}");
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e),
                Ok(lock) => {
                    let mut removed = 0usize;
                    for (scope, refs) in lists {
                        match uninstall_unused(scope, &refs, &lock, cancel, progress_sink(thread)) {
                            Ok(done) => removed += done.removed.len(),
                            Err(e) => {
                                fail(&mut out, &e);
                                break;
                            }
                        }
                    }
                    drop(lock);
                    let noun = if removed == 1 { "runtime" } else { "runtimes" };
                    if let Some(err) = out.error.as_mut() {
                        if removed > 0 {
                            err.push_str(&format!(
                                " Removed {removed} unused {noun} before it stopped."
                            ));
                        }
                    } else if out.result.is_none() {
                        out.result = Some(format!("Removed {removed} unused {noun}."));
                    }
                }
            }
        }
        Job::Open(entry, token) => {
            // `flatpak run` with the activation token in its environment:
            // libflatpak's launch takes no environment, and Wayland does not
            // bring a window forward that arrives without a token.
            let launched = launch_app(
                entry.scope,
                &entry.id,
                &entry.arch,
                &entry.branch,
                token.as_deref(),
                cancel,
            );
            if let Err(e) = launched {
                fail(&mut out, &e);
            }
        }
    }
    out
}

fn worker(thread: CxxQtThread<qobject::Jobs>, rx: mpsc::Receiver<Msg>) {
    while let Ok(Msg { job, cancel }) = rx.recv() {
        let foreground = job.foreground();
        let ran = catch_unwind(AssertUnwindSafe(|| run_job(&thread, job, &cancel)));
        let mut out = ran.unwrap_or_else(|_| {
            log::error!("a Flatpak job panicked");
            Outcome {
                error: Some("Something went wrong. See the log for details.".into()),
                ..Outcome::default()
            }
        });
        // A plan that finished after Cancel never opens the dialog.
        if cancel.is_cancelled() {
            out.plan = None;
        }
        // After every job, whatever it did: what is installed now. A fresh
        // token, so a cancelled job doesn't cancel the listing.
        let listing = catch_unwind(AssertUnwindSafe(|| list(&CancelToken::new()))).ok();
        if thread
            .queue(move |j| j.finish(foreground, out, listing))
            .is_err()
        {
            return;
        }
    }
}

// ---- the QObject ----

impl qobject::Jobs {
    fn submit(mut self: Pin<&mut Self>, job: Job, phase: &str, app_id: &str) {
        let Some(tx) = self.rust().worker.clone() else {
            self.as_mut()
                .set_error_text(QString::from("The Store is not ready yet."));
            return;
        };
        let cancel = CancelToken::new();
        if job.foreground() {
            self.as_mut().rust_mut().fg_cancel = cancel.clone();
        } else {
            self.as_mut().rust_mut().bg_cancel = cancel.clone();
        }
        if job.foreground() {
            self.as_mut().set_message_app(QString::default());
            self.as_mut().set_phase(QString::from(phase));
            self.as_mut().set_app_id(QString::from(app_id));
            self.as_mut().set_percent(-1);
            self.as_mut().set_status(QString::default());
            self.as_mut().set_not_responding(false);
            self.as_mut().set_error_text(QString::default());
            self.as_mut().set_result_text(QString::default());
        }
        let foreground = job.foreground();
        if tx.send(Msg { job, cancel }).is_err() {
            log::error!("the Flatpak worker is gone");
            if foreground {
                self.as_mut().set_phase(QString::from("idle"));
            }
            self.as_mut().set_error_text(QString::from(
                "The Store could not run this. Please restart it.",
            ));
        }
    }

    /// An error shown for `app` (the result is cleared).
    fn set_message(mut self: Pin<&mut Self>, error: &str, app: &str) {
        self.as_mut().set_message_app(QString::from(app));
        self.as_mut().set_result_text(QString::default());
        self.as_mut().set_error_text(QString::from(error));
    }

    fn idle(&self) -> bool {
        self.phase().to_string() == "idle"
    }

    fn finish(mut self: Pin<&mut Self>, foreground: bool, out: Outcome, listing: Option<Listing>) {
        if let Some(l) = listing {
            let json: Vec<Value> = l
                .entries
                .iter()
                .map(|e| {
                    json!({
                        "appId": e.id, "name": e.name, "summary": e.summary,
                        "version": e.version, "scope": scope_word(e.scope),
                        "size": human_size(e.size), "iconSource": e.icon, "ref": e.full_ref,
                    })
                })
                .collect();
            self.as_mut().rust_mut().installed = l.entries;
            self.as_mut()
                .set_installed_json(QString::from(Value::Array(json).to_string().as_str()));
            self.as_mut()
                .set_installed_error(QString::from(l.error.as_str()));
            self.as_mut().set_installed_ready(true);
            let rev = self.installed_revision().wrapping_add(1);
            self.as_mut().set_installed_revision(rev);
        }
        let app = self.app_id().to_string();
        if foreground {
            self.as_mut().set_message_app(QString::from(app.as_str()));
            if let Some(e) = &out.error {
                self.as_mut().set_error_text(QString::from(e.as_str()));
            }
            if let Some(r) = &out.result {
                self.as_mut().set_result_text(QString::from(r.as_str()));
            }
        }
        if foreground {
            self.as_mut().set_phase(QString::from("idle"));
            self.as_mut().set_app_id(QString::default());
            self.as_mut().set_percent(-1);
            self.as_mut().set_status(QString::default());
            self.as_mut().set_not_responding(false);
        }
        if let Some(full_ref) = out.blocked_ref {
            self.as_mut().remove_blocked(
                QString::from(app.as_str()),
                QString::from(full_ref.as_str()),
            );
        }
        if let Some((plan, json)) = out.plan {
            self.as_mut().rust_mut().plan = Some(plan);
            self.as_mut().set_plan_json(QString::from(json.as_str()));
            self.as_mut().plan_ready(QString::from(app.as_str()));
        }
        if let Some(unused) = out.unused {
            let json: Vec<Value> = unused
                .iter()
                .map(|u| {
                    json!({"name": u.id, "branch": u.branch, "scope": scope_word(u.scope),
                           "size": human_size(u.size)})
                })
                .collect();
            let count = i32::try_from(unused.len()).unwrap_or(i32::MAX);
            self.as_mut().rust_mut().unused = unused;
            self.as_mut()
                .set_unused_json(QString::from(Value::Array(json).to_string().as_str()));
            if count == 0 && out.result.is_none() && out.error.is_none() {
                self.as_mut()
                    .set_result_text(QString::from("Nothing unused to remove."));
            }
            self.as_mut().unused_ready(count);
        }
    }

    pub fn start(mut self: Pin<&mut Self>) {
        if self.rust().worker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let thread = self.qt_thread();
        let spawned = std::thread::Builder::new()
            .name("atlas-store-flatpak".into())
            .spawn(move || worker(thread, rx));
        match spawned {
            Ok(handle) => {
                self.as_mut().rust_mut().worker = Some(tx);
                self.as_mut().rust_mut().handle = Some(handle);
                self.submit(Job::Startup, "idle", "");
            }
            Err(e) => {
                log::error!("could not start the Flatpak worker: {e}");
                self.as_mut().set_error_text(QString::from(
                    "The Store could not start its installer. Please restart it.",
                ));
            }
        }
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.submit(Job::Refresh, "idle", "");
    }

    pub fn plan_install(mut self: Pin<&mut Self>, app_id: &QString) {
        if !self.idle() {
            return;
        }
        let id = app_id.to_string();
        // The source comes from the catalog entry: its scope and remote.
        let found = library().and_then(|lib| {
            let entry = lib.find(&id)?;
            let c = lib.component(entry);
            let bundle = c.bundle.as_ref()?;
            let s = lib.source(entry);
            Some((
                c.name.clone(),
                s.scope,
                s.remote.clone(),
                bundle.reference.clone(),
                c.id_bare().to_string(),
            ))
        });
        let Some((name, scope, remote, ref_, bare)) = found else {
            self.as_mut().set_message(
                "This app is not in any source the Store knows, so it can't be installed.",
                &id,
            );
            return;
        };
        let parts: Vec<&str> = ref_.split('/').collect();
        let wanted = id.strip_suffix(".desktop").unwrap_or(&id);
        if parts.len() != 4 || parts[0] != "app" || parts[1] != bare || bare != wanted {
            log::warn!("catalog entry of {id} has the bundle ref {ref_}");
            self.as_mut()
                .set_message("This app's catalog entry is inconsistent.", &id);
            return;
        }
        self.as_mut().rust_mut().plan = None;
        self.as_mut().rust_mut().plan_name = name.clone();
        self.submit(
            Job::Plan {
                name,
                scope,
                remote,
                ref_,
            },
            "planning",
            &id,
        );
    }

    pub fn confirm_install(mut self: Pin<&mut Self>) {
        if !self.idle() {
            return;
        }
        let Some(plan) = self.as_mut().rust_mut().plan.take() else {
            return;
        };
        let name = self.rust().plan_name.clone();
        let id = plan.ref_.split('/').nth(1).unwrap_or_default().to_string();
        self.submit(
            Job::Install {
                plan: Box::new(plan),
                name,
            },
            "installing",
            &id,
        );
    }

    pub fn cancel(mut self: Pin<&mut Self>) {
        // A plan nobody confirmed is simply dropped.
        self.as_mut().rust_mut().plan = None;
        self.rust().fg_cancel.cancel();
    }

    pub fn remove(self: Pin<&mut Self>, app_id: &QString, full_ref: &QString, delete_data: bool) {
        self.start_remove(app_id, full_ref, delete_data, false);
    }

    pub fn close_and_remove(self: Pin<&mut Self>, app_id: &QString, full_ref: &QString) {
        self.start_remove(app_id, full_ref, true, true);
    }

    fn start_remove(
        mut self: Pin<&mut Self>,
        app_id: &QString,
        full_ref: &QString,
        delete_data: bool,
        close_first: bool,
    ) {
        if !self.idle() {
            return;
        }
        let id = app_id.to_string();
        let ref_ = full_ref.to_string();
        // Exactly what the dialog showed: the same ref in the same installation.
        let found = self
            .rust()
            .installed
            .iter()
            .find(|e| e.id == id && e.full_ref == ref_)
            .cloned();
        let Some(e) = found else {
            self.as_mut().set_message(
                "This app changed since the question was shown. Please try again.",
                &id,
            );
            return;
        };
        // The app's data folder is shared by every installation of the ID.
        let shared = self.rust().installed.iter().filter(|x| x.id == id).count() > 1;
        self.submit(
            Job::Remove {
                name: e.name,
                app_id: e.id,
                scope: e.scope,
                ref_: e.full_ref,
                delete_data: delete_data && !shared,
                close_first: close_first && delete_data && !shared,
            },
            "removing",
            &id,
        );
    }

    pub fn check_unused(mut self: Pin<&mut Self>) {
        if !self.idle() {
            return;
        }
        self.as_mut().rust_mut().unused.clear();
        self.submit(Job::Unused, "unused", "");
    }

    pub fn remove_unused(mut self: Pin<&mut Self>) {
        if !self.idle() || self.rust().unused.is_empty() {
            return;
        }
        let mut lists: Vec<(Scope, Vec<String>)> = Vec::new();
        for u in &self.rust().unused {
            match lists.iter_mut().find(|(s, _)| *s == u.scope) {
                Some((_, refs)) => refs.push(u.full_ref.clone()),
                None => lists.push((u.scope, vec![u.full_ref.clone()])),
            }
        }
        self.as_mut().rust_mut().unused.clear();
        self.submit(Job::RemoveUnused(lists), "removing", "");
    }

    pub fn open(mut self: Pin<&mut Self>, app_id: &QString, token: &QString) {
        if !self.idle() {
            return;
        }
        let id = app_id.to_string();
        let Some(e) = self.rust().installed.iter().find(|e| e.id == id).cloned() else {
            self.as_mut().set_message("This app is not installed.", &id);
            return;
        };
        // The token comes from the window system through QML; it goes into a
        // child's environment, so it is checked here.
        let token = token.to_string();
        let token = valid_activation_token(&token).map(str::to_string);
        self.submit(Job::Open(e, token), "opening", &id);
    }

    pub fn clear_messages(mut self: Pin<&mut Self>) {
        self.as_mut().set_error_text(QString::default());
        self.as_mut().set_result_text(QString::default());
    }

    pub fn installed_scope(&self, app_id: &QString) -> QString {
        let id = app_id.to_string();
        self.rust()
            .installed
            .iter()
            .find(|e| e.id == id)
            .map_or_else(QString::default, |e| QString::from(scope_word(e.scope)))
    }

    pub fn app_info(&self, app_id: &QString) -> QString {
        let id = app_id.to_string();
        let installs: Vec<&Entry> = self
            .rust()
            .installed
            .iter()
            .filter(|e| e.id == id)
            .collect();
        let installed = installs.first().copied();
        let lib = library();
        let entry = lib.as_ref().and_then(|l| l.find(&id).map(|e| (l, e)));
        if entry.is_none() && installed.is_none() {
            return QString::from(json!({"found": false, "appId": id}).to_string().as_str());
        }
        let mut v = json!({
            "found": true, "appId": id, "name": id, "summary": "", "developer": "",
            "verified": false, "iconSource": "", "license": "", "free": false,
            "version": "", "released": 0, "source": "", "canInstall": false,
            "blocks": [], "links": [],
        });
        if let Some(e) = installed {
            v["name"] = json!(e.name);
            v["summary"] = json!(e.summary);
            v["iconSource"] = json!(e.icon);
            v["version"] = json!(e.version);
            let all: Vec<Value> = installs
                .iter()
                .map(|x| {
                    json!({"scope": scope_word(x.scope), "size": human_size(x.size),
                           "version": x.version, "ref": x.full_ref})
                })
                .collect();
            v["installed"] = all[0].clone();
            v["installs"] = json!(all);
        }
        if let Some((l, eid)) = entry {
            let c = l.component(eid);
            let s = l.source(eid);
            v["name"] = json!(c.name);
            v["summary"] = json!(c.summary);
            v["developer"] = json!(c.developer);
            v["verified"] = json!(l.is_verified(eid));
            v["license"] = json!(c.license);
            v["free"] = json!(is_free_license(&c.license));
            v["source"] = json!(clean(&s.title, 100));
            v["canInstall"] = json!(c.bundle.is_some());
            v["blocks"] = json!(blocks(&c.description));
            if let Some(r) = c.releases.first() {
                if !r.version.is_empty() {
                    v["version"] = json!(r.version);
                }
                v["released"] = json!(r.timestamp);
            }
            if let Some(icon) = s.icon_path(c, ICON_SIZE).and_then(|p| safe_icon(&p)) {
                v["iconSource"] = json!(icon);
            }
            let links: Vec<Value> = c
                .urls
                .iter()
                .filter_map(|(k, u)| {
                    let url = https_url(u)?;
                    let host = url.strip_prefix("https://")?.split('/').next()?.to_string();
                    url_label(*k).map(|t| json!({"label": t, "url": url, "host": host}))
                })
                .take(6)
                .collect();
            v["links"] = json!(links);
        }
        QString::from(v.to_string().as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_500), "1.5 kB");
        assert_eq!(human_size(2_500_000), "2.5 MB");
        assert!(human_size(u64::MAX).ends_with("TB"));
    }

    #[test]
    fn refs_are_shortened() {
        assert_eq!(
            pretty_ref("runtime/org.test.Platform/x86_64/1.0"),
            "org.test.Platform (1.0)"
        );
        assert_eq!(pretty_ref("odd"), "odd");
    }

    #[test]
    fn descriptions_become_plain_lines() {
        let p = |t: &str| Span {
            text: t.into(),
            style: atlas_store_core::appstream::Style::Plain,
        };
        let b = vec![
            Block::Paragraph(vec![p("a"), p("b")]),
            Block::List {
                ordered: true,
                items: vec![vec![p("x")], vec![p("y")]],
            },
        ];
        assert_eq!(blocks(&b), vec!["ab".to_string(), "1. x\n2. y".to_string()]);
    }
}
