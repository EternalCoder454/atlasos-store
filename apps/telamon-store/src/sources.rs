//! The Sources place: the Flatpak remotes (list, enable and disable, add from
//! a `.flatpakrepo` file or link, remove).
//!
//! Threading: `Sources::start` runs ONE worker thread that takes one job at a
//! time from a channel, like `jobs.rs`. A job that changes an installation
//! (turning a source on or off, adding, removing) takes the shared
//! `OperationLock` on that worker, showing "Another update is running" while
//! it waits; reading the list, downloading or reading a source file, and
//! checking what a removal would hit change nothing and take no lock. The GUI
//! thread only sends jobs and applies results queued back with
//! `qt_thread().queue`. One job at a time: `phase` is "idle" or the running
//! job, and a request made while it is not idle is refused.
//!
//! The add flow is two steps. `prepareAddFromUrl` / `prepareAddFromFile`
//! fetch and parse the file (with the core's limits) and keep it here, and
//! `previewJson` shows what the dialog must show BEFORE anything is added:
//! title, address, key fingerprint or the unsigned warning, and the name it
//! would get. `confirmAdd` then adds exactly that parsed file (never the file
//! again), and a source without a key only when the dialog sent the extra
//! acknowledgement. Removal is two steps as well: `checkRemove` finds what
//! is installed from the source (the dialog then confirms or explains why
//! it can't), `confirmRemove` removes it if nothing is.
//!
//! All text the QML shows from here is plain, cleaned on the way in (the
//! core cleans titles and addresses), and lists go to QML as JSON.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        /// The object is started.
        #[qproperty(bool, ready)]
        /// "idle", "enabling", "fetching", "adding" or "removing".
        #[qproperty(QString, phase)]
        /// One plain line about what the job is doing.
        #[qproperty(QString, status)]
        /// 0 to 100 while a figure is known, else -1.
        #[qproperty(i32, percent)]
        /// The list was read at least once.
        #[qproperty(bool, sources_ready, cxx_name = "sourcesReady")]
        /// JSON array of the sources.
        #[qproperty(QString, sources_json, cxx_name = "sourcesJson")]
        /// Problems reading the installations, one per line.
        #[qproperty(QString, sources_error, cxx_name = "sourcesError")]
        /// Goes up each time the list is replaced.
        #[qproperty(i32, revision)]
        /// Why the last change failed, in plain words, or "".
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// What the last change did, in plain words, or "".
        #[qproperty(QString, result_text, cxx_name = "resultText")]
        /// Why the add dialog's last step failed, in plain words, or "".
        #[qproperty(QString, add_error, cxx_name = "addError")]
        /// JSON object: what the Add Source confirmation shows.
        #[qproperty(QString, preview_json, cxx_name = "previewJson")]
        /// JSON object: what the Remove confirmation shows.
        #[qproperty(QString, remove_json, cxx_name = "removeJson")]
        #[namespace = "telamon_store"]
        type Sources = super::SourcesRust;

        /// A source was added, removed, turned on or off: the catalog and
        /// the installed list should be read again.
        #[qsignal]
        fn changed(self: Pin<&mut Sources>);

        /// The source file is read: `previewJson` is the confirmation.
        #[qsignal]
        #[cxx_name = "previewReady"]
        fn preview_ready(self: Pin<&mut Sources>);

        /// The confirmed source was added; the dialog closes.
        #[qsignal]
        fn added(self: Pin<&mut Sources>);

        /// What a removal would hit was checked: `removeJson` says whether
        /// the dialog confirms or explains why it can't.
        #[qsignal]
        #[cxx_name = "removeReady"]
        fn remove_ready(self: Pin<&mut Sources>);
    }

    unsafe extern "RustQt" {
        /// Starts the worker. Once.
        #[qinvokable]
        fn start(self: Pin<&mut Sources>);

        /// Reads the list of sources again (on the worker).
        #[qinvokable]
        fn refresh(self: Pin<&mut Sources>);

        /// Turns a source on or off. `scope` is "user" or "system".
        #[qinvokable]
        #[cxx_name = "setEnabled"]
        fn set_enabled(self: Pin<&mut Sources>, scope: &QString, name: &QString, enabled: bool);

        /// Downloads the `.flatpakrepo` at an https address and prepares the
        /// confirmation (signals `previewReady`, or sets `addError`).
        #[qinvokable]
        #[cxx_name = "prepareAddFromUrl"]
        fn prepare_add_from_url(self: Pin<&mut Sources>, url: &QString);

        /// The same for a local file (a path or a `file:` URL).
        #[qinvokable]
        #[cxx_name = "prepareAddFromFile"]
        fn prepare_add_from_file(self: Pin<&mut Sources>, path: &QString);

        /// Adds the source last prepared, in the installation `scope` ("user"
        /// or "system"). A source without a key is added only with
        /// `acknowledge_unsigned`. Signals `added` or sets `addError`.
        #[qinvokable]
        #[cxx_name = "confirmAdd"]
        fn confirm_add(self: Pin<&mut Sources>, scope: &QString, acknowledge_unsigned: bool);

        /// Drops the prepared source, and stops a download or an add.
        #[qinvokable]
        #[cxx_name = "cancelAdd"]
        fn cancel_add(self: Pin<&mut Sources>);

        /// Finds what is installed from a source (signals `removeReady`).
        #[qinvokable]
        #[cxx_name = "checkRemove"]
        fn check_remove(self: Pin<&mut Sources>, scope: &QString, name: &QString);

        /// Removes the source `checkRemove` found nothing installed from.
        #[qinvokable]
        #[cxx_name = "confirmRemove"]
        fn confirm_remove(self: Pin<&mut Sources>);

        /// Drops the pending removal.
        #[qinvokable]
        #[cxx_name = "cancelRemove"]
        fn cancel_remove(self: Pin<&mut Sources>);

        /// Stops the running job.
        #[qinvokable]
        fn cancel(self: Pin<&mut Sources>);

        #[qinvokable]
        #[cxx_name = "clearMessages"]
        fn clear_messages(self: Pin<&mut Sources>);
    }

    impl cxx_qt::Threading for Sources {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn sources_make_unique() -> UniquePtr<Sources>;
    }
}

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::{Value, json};
use telamon_store_core::flatpak::{
    CancelToken, Error, LockError, OperationLock, Placement, RemoteInfo, RemotesOutcome, Scope,
    SourcePreview, add_source, blocked_message, fetch_repo, group_fingerprint, list_remotes,
    preview_repo, read_repo_file, remove_source, set_enabled, source_users,
};
use telamon_store_core::flatpakref::FlatpakRepo;
use telamon_store_core::text::clean;

/// How long a job waits for the Updater's lock before it gives up.
const LOCK_GIVE_UP: Duration = Duration::from_secs(600);
/// Apps named in a row of the list.
const APPS_SHOWN: usize = 5;

/// A source as the list last showed it: what a change is checked against.
#[derive(Clone, Debug)]
struct Known {
    scope: Scope,
    name: String,
    title: String,
    url: String,
}

/// The removal the dialog is asking about.
#[derive(Clone, Debug)]
struct PendingRemoval {
    scope: Scope,
    name: String,
    title: String,
    url: String,
}

enum Job {
    List,
    SetEnabled {
        scope: Scope,
        name: String,
        title: String,
        enabled: bool,
    },
    FetchUrl(String),
    ReadFile(String),
    Add {
        scope: Scope,
        name: String,
        title: String,
        repo: Box<FlatpakRepo>,
        allow_unsigned: bool,
    },
    CheckRemove(PendingRemoval),
    Remove(PendingRemoval),
}

impl Job {
    /// Whether the job sets the phase (so it must clear it).
    fn foreground(&self) -> bool {
        !matches!(self, Job::List)
    }

    /// Whether the list is read again afterwards.
    fn relist(&self) -> bool {
        matches!(
            self,
            Job::SetEnabled { .. } | Job::Add { .. } | Job::Remove(_)
        )
    }

    /// Whether a failure belongs in the add dialog.
    fn in_add_dialog(&self) -> bool {
        matches!(self, Job::FetchUrl(_) | Job::ReadFile(_) | Job::Add { .. })
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
    preview: Option<Box<SourcePreview>>,
    removal: Option<(PendingRemoval, Vec<String>)>,
    added: bool,
    changed: bool,
}

pub struct SourcesRust {
    ready: bool,
    phase: QString,
    status: QString,
    percent: i32,
    sources_ready: bool,
    sources_json: QString,
    sources_error: QString,
    revision: i32,
    error_text: QString,
    result_text: QString,
    add_error: QString,
    preview_json: QString,
    remove_json: QString,
    worker: Option<mpsc::Sender<Msg>>,
    /// The token of the last list read.
    bg_cancel: CancelToken,
    /// The token of the last foreground job (what Cancel stops).
    fg_cancel: CancelToken,
    handle: Option<std::thread::JoinHandle<()>>,
    known: Vec<Known>,
    preview: Option<SourcePreview>,
    removal: Option<PendingRemoval>,
}

impl Default for SourcesRust {
    fn default() -> Self {
        SourcesRust {
            ready: false,
            phase: QString::from("idle"),
            status: QString::default(),
            percent: -1,
            sources_ready: false,
            sources_json: QString::from("[]"),
            sources_error: QString::default(),
            revision: 0,
            error_text: QString::default(),
            result_text: QString::default(),
            add_error: QString::default(),
            preview_json: QString::from("{}"),
            remove_json: QString::from("{}"),
            worker: None,
            bg_cancel: CancelToken::new(),
            fg_cancel: CancelToken::new(),
            handle: None,
            known: Vec::new(),
            preview: None,
            removal: None,
        }
    }
}

impl Drop for SourcesRust {
    /// Quitting stops a running job cleanly and ends the worker (it leaves
    /// when the channel closes).
    fn drop(&mut self) {
        self.fg_cancel.cancel();
        self.bg_cancel.cancel();
        self.worker = None;
        if let Some(h) = self.handle.take() {
            let start = Instant::now();
            while !h.is_finished() {
                if start.elapsed() > Duration::from_secs(10) {
                    log::error!("the Sources worker did not stop within 10 s; leaving it");
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let _ = h.join();
        }
    }
}

// ---- text ----

fn scope_word(s: Scope) -> &'static str {
    s.label()
}

fn parse_scope(s: &str) -> Option<Scope> {
    match s {
        "user" => Some(Scope::User),
        "system" => Some(Scope::System),
        _ => None,
    }
}

/// "that source has changed" as "That source has changed.".
fn sentence(s: &str) -> String {
    let s = clean(s, 400);
    let mut c = s.chars();
    let mut out = match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect::<String>(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// A core error as a sentence for the page (`title` is the source's name).
/// libflatpak's own text in it was cleaned and capped by the core.
fn explain(e: &Error, title: &str) -> String {
    match e {
        Error::Invalid(s) => sentence(s),
        Error::InUse(labels) => blocked_message(title, labels),
        Error::Cancelled => "Cancelled.".to_string(),
        other => clean(&other.to_string(), 400),
    }
}

fn placement_json(p: &Placement) -> Value {
    match p {
        Placement::Free { name } => json!({"state": "free", "name": name, "text": ""}),
        Placement::Exists { name, enabled } => json!({
            "state": "exists", "name": name,
            "text": if *enabled {
                format!("This source is already added as \"{name}\".")
            } else {
                format!("This source is already added as \"{name}\", and turned off. Turn it on in the list.")
            },
        }),
        Placement::Unavailable(why) => json!({
            "state": "unavailable", "name": "",
            "text": format!("This installation can't take it: {}", clean(why, 200)),
        }),
    }
}

fn preview_json(p: &SourcePreview) -> String {
    json!({
        "title": p.title,
        "url": p.url,
        "comment": p.comment.clone().unwrap_or_default(),
        "fingerprint": p.fingerprint.as_deref().map(group_fingerprint).unwrap_or_default(),
        "unsigned": p.unsigned(),
        "user": placement_json(&p.user),
        "system": placement_json(&p.system),
    })
    .to_string()
}

fn sources_json(remotes: &[RemoteInfo]) -> String {
    let list: Vec<Value> = remotes
        .iter()
        .map(|r| {
            let apps: Vec<&str> = r.apps().into_iter().take(APPS_SHOWN).collect();
            json!({
                "scope": scope_word(r.scope),
                "name": r.name,
                "title": r.title,
                "url": r.url,
                "enabled": r.enabled,
                "signed": r.signed,
                "unsigned": r.unsigned(),
                "singleApp": r.single_app,
                "priority": r.priority,
                "apps": apps,
                "appCount": r.apps().len(),
                "installedCount": r.installed.len(),
            })
        })
        .collect();
    Value::Array(list).to_string()
}

fn remove_json(p: &PendingRemoval, labels: &[String]) -> String {
    let blocked = !labels.is_empty();
    let note = if p.name == "flathub" && !blocked {
        "Flathub is where most apps come from. Without it you won't find or update them."
    } else {
        ""
    };
    json!({
        "scope": scope_word(p.scope),
        "name": p.name,
        "title": p.title,
        "url": p.url,
        "blocked": blocked,
        "message": if blocked { blocked_message(&p.title, labels) } else { String::new() },
        "note": note,
    })
    .to_string()
}

// ---- the worker ----

fn say(thread: &CxxQtThread<qobject::Sources>, line: &str) {
    let line = line.to_string();
    let _ = thread.queue(move |mut s| s.as_mut().set_status(QString::from(line.as_str())));
}

/// Takes the shared lock; says so while another update holds it.
fn take_lock(
    thread: &CxxQtThread<qobject::Sources>,
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

fn fail(out: &mut Outcome, e: &Error, title: &str) {
    if matches!(e, Error::Cancelled) {
        out.result = Some("Cancelled.".to_string());
    } else {
        log::warn!("a Sources job failed: {e}");
        out.error = Some(explain(e, title));
    }
}

fn prepared(out: &mut Outcome, repo: FlatpakRepo, hint: &str, cancel: &CancelToken, title: &str) {
    match preview_repo(repo, hint, cancel) {
        Ok(p) => out.preview = Some(Box::new(p)),
        Err(e) => fail(out, &e, title),
    }
}

fn run_job(thread: &CxxQtThread<qobject::Sources>, job: Job, cancel: &CancelToken) -> Outcome {
    let mut out = Outcome::default();
    match job {
        Job::List => {}
        Job::SetEnabled {
            scope,
            name,
            title,
            enabled,
        } => {
            say(
                thread,
                if enabled {
                    "Turning on\u{2026}"
                } else {
                    "Turning off\u{2026}"
                },
            );
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e, &title),
                Ok(lock) => {
                    match set_enabled(scope, &name, enabled, &lock, cancel) {
                        Ok(()) => {
                            out.changed = true;
                            out.result = Some(if enabled {
                                format!("{title} is on.")
                            } else {
                                format!(
                                    "{title} is off. Its apps are left out of the catalog; nothing was removed."
                                )
                            });
                        }
                        Err(e) => fail(&mut out, &e, &title),
                    }
                    drop(lock);
                }
            }
        }
        Job::FetchUrl(url) => {
            say(thread, "Downloading the source file\u{2026}");
            match fetch_repo(&url, cancel) {
                Ok(repo) => prepared(&mut out, repo, &hint_of_url(&url), cancel, ""),
                Err(e) => fail(&mut out, &e, ""),
            }
        }
        Job::ReadFile(path) => {
            say(thread, "Reading the file\u{2026}");
            match read_repo_file(&path) {
                Ok((repo, stem)) => prepared(&mut out, repo, &stem, cancel, ""),
                Err(e) => fail(&mut out, &e, ""),
            }
        }
        Job::Add {
            scope,
            name,
            title,
            repo,
            allow_unsigned,
        } => {
            say(thread, "Adding the source\u{2026}");
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e, &title),
                Ok(lock) => {
                    match add_source(scope, &repo, &name, allow_unsigned, &lock, cancel) {
                        Ok(done) => {
                            out.changed = true;
                            out.added = true;
                            out.result = Some(match done.appstream {
                                None => format!("Added {title}."),
                                Some(Error::Cancelled) => format!(
                                    "Added {title}. Its list of apps was not downloaded; the Store tries again when it is next open."
                                ),
                                Some(why) => {
                                    log::warn!("the app list of {title} was not downloaded: {why}");
                                    format!(
                                        "Added {title}, but its list of apps could not be downloaded yet. Its apps show up once the Store can reach it; it tries again whenever it is open."
                                    )
                                }
                            });
                        }
                        Err(e) => fail(&mut out, &e, &title),
                    }
                    drop(lock);
                }
            }
        }
        Job::CheckRemove(p) => {
            say(thread, "Checking what is installed from it\u{2026}");
            match source_users(p.scope, &p.name, cancel) {
                Ok(users) => {
                    let labels = users.iter().map(|u| u.label()).collect();
                    out.removal = Some((p, labels));
                }
                Err(e) => {
                    let title = p.title.clone();
                    fail(&mut out, &e, &title);
                }
            }
        }
        Job::Remove(p) => {
            say(thread, "Removing\u{2026}");
            match take_lock(thread, cancel) {
                Err(e) => fail(&mut out, &e, &p.title),
                Ok(lock) => {
                    match remove_source(p.scope, &p.name, &p.url, &lock, cancel) {
                        Ok(()) => {
                            out.changed = true;
                            out.result = Some(format!("Removed {}.", p.title));
                        }
                        Err(e) => {
                            // The source changed since it was shown: the list
                            // is read again either way.
                            out.changed = true;
                            fail(&mut out, &e, &p.title);
                        }
                    }
                    drop(lock);
                }
            }
        }
    }
    out
}

/// The part of a source link that names the file, for a suggested name:
/// `https://host/dir/name.flatpakrepo` gives `name`.
fn hint_of_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let stem = last.strip_suffix(".flatpakrepo").unwrap_or(last);
    // A bare host ("example.org") is no file name.
    if path.trim_end_matches('/').matches('/').count() <= 2 {
        String::new()
    } else {
        stem.to_string()
    }
}

fn worker(thread: CxxQtThread<qobject::Sources>, rx: mpsc::Receiver<Msg>) {
    while let Ok(Msg { job, cancel }) = rx.recv() {
        let foreground = job.foreground();
        let relist = job.relist() || matches!(job, Job::List);
        let in_add = job.in_add_dialog();
        let ran = catch_unwind(AssertUnwindSafe(|| run_job(&thread, job, &cancel)));
        let out = ran.unwrap_or_else(|_| {
            log::error!("a Sources job panicked");
            Outcome {
                error: Some("Something went wrong. See the log for details.".into()),
                ..Outcome::default()
            }
        });
        // A fresh token, so a cancelled job doesn't cancel the listing.
        let listing = if relist {
            catch_unwind(AssertUnwindSafe(|| list_remotes(&CancelToken::new()))).ok()
        } else {
            None
        };
        if thread
            .queue(move |s| s.finish(foreground, in_add, out, listing))
            .is_err()
        {
            return;
        }
    }
}

// ---- the QObject ----

impl qobject::Sources {
    fn submit(mut self: Pin<&mut Self>, job: Job, phase: &str) {
        let Some(tx) = self.rust().worker.clone() else {
            self.as_mut()
                .set_error_text(QString::from("The Store is not ready yet."));
            return;
        };
        let cancel = CancelToken::new();
        let foreground = job.foreground();
        if foreground {
            self.as_mut().rust_mut().fg_cancel = cancel.clone();
            self.as_mut().set_phase(QString::from(phase));
            self.as_mut().set_percent(-1);
            self.as_mut().set_status(QString::default());
            if job.in_add_dialog() {
                self.as_mut().set_add_error(QString::default());
            } else {
                self.as_mut().set_error_text(QString::default());
            }
            self.as_mut().set_result_text(QString::default());
        } else {
            self.as_mut().rust_mut().bg_cancel = cancel.clone();
        }
        if tx.send(Msg { job, cancel }).is_err() {
            log::error!("the Sources worker is gone");
            if foreground {
                self.as_mut().set_phase(QString::from("idle"));
            }
            self.as_mut().set_error_text(QString::from(
                "The Store could not run this. Please restart it.",
            ));
        }
    }

    fn idle(&self) -> bool {
        self.phase().to_string() == "idle"
    }

    /// A message for the page (the result is cleared).
    fn set_message(mut self: Pin<&mut Self>, error: &str) {
        self.as_mut().set_result_text(QString::default());
        self.as_mut().set_error_text(QString::from(error));
    }

    /// A message for the add dialog.
    fn set_add_message(mut self: Pin<&mut Self>, error: &str) {
        self.as_mut().set_add_error(QString::from(error));
    }

    fn known(&self, scope: Scope, name: &str) -> Option<Known> {
        self.rust()
            .known
            .iter()
            .find(|k| k.scope == scope && k.name == name)
            .cloned()
    }

    fn apply_listing(mut self: Pin<&mut Self>, l: RemotesOutcome) {
        if l.cancelled {
            return;
        }
        let known = l
            .remotes
            .iter()
            .map(|r| Known {
                scope: r.scope,
                name: r.name.clone(),
                title: r.title.clone(),
                url: r.url.clone(),
            })
            .collect();
        self.as_mut().rust_mut().known = known;
        let json = sources_json(&l.remotes);
        self.as_mut().set_sources_json(QString::from(json.as_str()));
        let errors = l
            .errors
            .iter()
            .take(5)
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("\n");
        self.as_mut()
            .set_sources_error(QString::from(errors.as_str()));
        self.as_mut().set_sources_ready(true);
        let rev = self.revision().wrapping_add(1);
        self.as_mut().set_revision(rev);
    }

    fn finish(
        mut self: Pin<&mut Self>,
        foreground: bool,
        in_add: bool,
        out: Outcome,
        listing: Option<RemotesOutcome>,
    ) {
        if let Some(l) = listing {
            self.as_mut().apply_listing(l);
        }
        if foreground {
            if let Some(e) = &out.error {
                if in_add {
                    self.as_mut().set_add_error(QString::from(e.as_str()));
                } else {
                    self.as_mut().set_error_text(QString::from(e.as_str()));
                }
            }
            if let Some(r) = &out.result {
                // A cancelled step of the add dialog says nothing on the page.
                if !(in_add && !out.added) {
                    self.as_mut().set_result_text(QString::from(r.as_str()));
                }
            }
            self.as_mut().set_phase(QString::from("idle"));
            self.as_mut().set_percent(-1);
            self.as_mut().set_status(QString::default());
        }
        if let Some(p) = out.preview {
            let json = preview_json(&p);
            self.as_mut().rust_mut().preview = Some(*p);
            self.as_mut().set_preview_json(QString::from(json.as_str()));
            self.as_mut().preview_ready();
        }
        if out.added {
            self.as_mut().rust_mut().preview = None;
            self.as_mut().set_preview_json(QString::from("{}"));
            self.as_mut().added();
        }
        if let Some((p, labels)) = out.removal {
            let json = remove_json(&p, &labels);
            // A blocked removal has nothing to confirm.
            self.as_mut().rust_mut().removal = labels.is_empty().then_some(p);
            self.as_mut().set_remove_json(QString::from(json.as_str()));
            self.as_mut().remove_ready();
        }
        if out.changed {
            self.as_mut().rust_mut().removal = None;
            self.as_mut().changed();
        }
    }

    pub fn start(mut self: Pin<&mut Self>) {
        if self.rust().worker.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let thread = self.qt_thread();
        let spawned = std::thread::Builder::new()
            .name("telamon-store-sources".into())
            .spawn(move || worker(thread, rx));
        match spawned {
            Ok(handle) => {
                self.as_mut().rust_mut().worker = Some(tx);
                self.as_mut().rust_mut().handle = Some(handle);
                if !*self.ready() {
                    self.as_mut().set_ready(true);
                }
            }
            Err(e) => {
                log::error!("could not start the Sources worker: {e}");
                self.as_mut().set_error_text(QString::from(
                    "The Store could not start. Please restart it.",
                ));
            }
        }
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.submit(Job::List, "idle");
    }

    pub fn set_enabled(mut self: Pin<&mut Self>, scope: &QString, name: &QString, enabled: bool) {
        if !self.idle() {
            return;
        }
        let found = parse_scope(&scope.to_string()).and_then(|s| self.known(s, &name.to_string()));
        let Some(k) = found else {
            self.as_mut()
                .set_message("That source is not in the list any more. The list was reloaded.");
            self.refresh();
            return;
        };
        self.submit(
            Job::SetEnabled {
                scope: k.scope,
                name: k.name,
                title: k.title,
                enabled,
            },
            "enabling",
        );
    }

    pub fn prepare_add_from_url(mut self: Pin<&mut Self>, url: &QString) {
        if !self.idle() {
            return;
        }
        self.as_mut().rust_mut().preview = None;
        self.submit(Job::FetchUrl(url.to_string()), "fetching");
    }

    pub fn prepare_add_from_file(mut self: Pin<&mut Self>, path: &QString) {
        if !self.idle() {
            return;
        }
        self.as_mut().rust_mut().preview = None;
        self.submit(Job::ReadFile(path.to_string()), "fetching");
    }

    pub fn confirm_add(self: Pin<&mut Self>, scope: &QString, acknowledge_unsigned: bool) {
        if !self.idle() {
            return;
        }
        let Some(scope) = parse_scope(&scope.to_string()) else {
            self.set_add_message("Choose who the source is for.");
            return;
        };
        let Some(p) = self.rust().preview.clone() else {
            self.set_add_message("There is nothing to add. Choose a file or paste a link first.");
            return;
        };
        if p.unsigned() && !acknowledge_unsigned {
            self.set_add_message(
                "This source is not signed. Tick the box to say you accept that, or cancel.",
            );
            return;
        }
        let name = match p.placement(scope) {
            Placement::Free { name } => name.clone(),
            other => {
                let text = placement_json(other)["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                self.set_add_message(&text);
                return;
            }
        };
        let title = p.title.clone();
        self.submit(
            Job::Add {
                scope,
                name,
                title,
                repo: Box::new(p.repo),
                allow_unsigned: acknowledge_unsigned,
            },
            "adding",
        );
    }

    pub fn cancel_add(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().preview = None;
        self.as_mut().set_preview_json(QString::from("{}"));
        self.as_mut().set_add_error(QString::default());
        let phase = self.phase().to_string();
        if matches!(phase.as_str(), "fetching" | "adding") {
            self.rust().fg_cancel.cancel();
        }
    }

    pub fn check_remove(mut self: Pin<&mut Self>, scope: &QString, name: &QString) {
        if !self.idle() {
            return;
        }
        let found = parse_scope(&scope.to_string()).and_then(|s| self.known(s, &name.to_string()));
        let Some(k) = found else {
            self.as_mut()
                .set_message("That source is not in the list any more. The list was reloaded.");
            self.refresh();
            return;
        };
        self.as_mut().rust_mut().removal = None;
        self.submit(
            Job::CheckRemove(PendingRemoval {
                scope: k.scope,
                name: k.name,
                title: k.title,
                url: k.url,
            }),
            "removing",
        );
    }

    pub fn confirm_remove(mut self: Pin<&mut Self>) {
        if !self.idle() {
            return;
        }
        let Some(p) = self.as_mut().rust_mut().removal.take() else {
            return;
        };
        self.submit(Job::Remove(p), "removing");
    }

    pub fn cancel_remove(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().removal = None;
    }

    pub fn cancel(self: Pin<&mut Self>) {
        self.rust().fg_cancel.cancel();
    }

    pub fn clear_messages(mut self: Pin<&mut Self>) {
        self.as_mut().set_error_text(QString::default());
        self.as_mut().set_result_text(QString::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_errors_become_plain_sentences() {
        assert_eq!(
            explain(&Error::Invalid("that source has changed".into()), "X"),
            "That source has changed."
        );
        assert_eq!(explain(&Error::Cancelled, "X"), "Cancelled.");
        assert_eq!(
            explain(&Error::RemoteNameTaken("a".into()), "X"),
            "A source named \"a\" already exists."
        );
        let s = explain(
            &Error::InUse(vec!["Hello".into(), "org.fd.Platform (runtime)".into()]),
            "Test",
        );
        assert!(
            s.starts_with(
                "Can't remove Test: these apps and runtimes are installed from it: Hello, "
            ),
            "{s}"
        );
        // Control and bidi characters never reach the page.
        assert!(!sentence("a\u{202e}b\nc").contains(['\u{202e}', '\n']));
    }

    #[test]
    fn the_file_name_hint_comes_from_the_path_only() {
        assert_eq!(
            hint_of_url("https://dl.example.org/repo/foo.flatpakrepo"),
            "foo"
        );
        assert_eq!(
            hint_of_url("https://dl.example.org/foo.flatpakrepo?x=1#y"),
            "foo"
        );
        assert_eq!(hint_of_url("https://dl.example.org/"), "");
        assert_eq!(hint_of_url("https://dl.example.org"), "");
    }

    #[test]
    fn the_list_json_flags_unsigned_off_and_single_app_sources() {
        use telamon_store_core::flatpak::{RefKind, SourceUse};
        let r = |name: &str, enabled: bool, signed: bool, single: bool| RemoteInfo {
            scope: Scope::System,
            name: name.into(),
            title: format!("Title of {name}"),
            url: "https://dl.example.org/repo".into(),
            enabled,
            signed,
            registry: false,
            single_app: single,
            priority: 1,
            installed: (0..7)
                .map(|i| SourceUse {
                    kind: RefKind::App,
                    id: format!("org.x.App{i}"),
                    name: format!("App{i}"),
                })
                .collect(),
        };
        let v: Value = serde_json::from_str(&sources_json(&[
            r("a", true, true, false),
            r("b", false, false, true),
        ]))
        .unwrap();
        assert_eq!(v[0]["scope"], "system");
        assert_eq!(v[0]["unsigned"], false);
        assert_eq!(v[1]["unsigned"], true);
        assert_eq!(v[1]["enabled"], false);
        assert_eq!(v[1]["singleApp"], true);
        // Seven apps are counted, the first five named.
        assert_eq!(v[0]["appCount"], 7);
        assert_eq!(v[0]["apps"].as_array().unwrap().len(), APPS_SHOWN);
    }

    #[test]
    fn the_confirmation_json_carries_what_must_be_shown() {
        let p = PendingRemoval {
            scope: Scope::User,
            name: "flathub".into(),
            title: "Flathub".into(),
            url: "https://dl.flathub.org/repo".into(),
        };
        let v: Value = serde_json::from_str(&remove_json(&p, &[])).unwrap();
        assert_eq!(v["blocked"], false);
        assert!(v["note"].as_str().unwrap().contains("Flathub is where"));
        let labels = vec!["Hello".to_string()];
        let v: Value = serde_json::from_str(&remove_json(&p, &labels)).unwrap();
        assert_eq!(v["blocked"], true);
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .contains("Remove them first.")
        );
        assert_eq!(v["note"], "");
    }
}
