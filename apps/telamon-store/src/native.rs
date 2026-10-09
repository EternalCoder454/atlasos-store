//! Native Telamon apps in the window: the list of connected apps (the catalog
//! and each app's latest release, checked), install, update, uninstall, Open,
//! and installing a local bundle after a confirmation. The logic is
//! `telamon_store_core::native`; this is the Qt side. See docs/DESIGN.md,
//! "Native Telamon apps".
//!
//! Threading: every job runs on a short-lived thread of its own and its result
//! is queued back with `qt_thread().queue`; the GUI thread reads no file and
//! asks no server. One job at a time (`phase` is "idle" or the job's name).
//! Nothing is installed or removed except after the user's answer in the
//! Store's own dialog, and nothing runs in the background: a check starts when
//! the window is opened, when a page that shows these apps is opened with an
//! answer older than six hours, once a day while the window stays open, or
//! when the user asks. Every text shown is cleaned (manifest fields) or ours,
//! and the QML shows it as plain text.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        /// "idle", "checking", "installing", "removing", "opening" or "inspecting".
        #[qproperty(QString, phase)]
        /// Why the last job failed, in plain words, or "".
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// What the last job did, in plain words, or "".
        #[qproperty(QString, result_text, cxx_name = "resultText")]
        /// A quiet remark about the last check (offline, one app failed), or "".
        #[qproperty(QString, note_text, cxx_name = "noteText")]
        /// JSON array of the apps (connected ones with a release, and installed ones).
        #[qproperty(QString, apps_json, cxx_name = "appsJson")]
        /// JSON object: what the local bundle's confirmation shows.
        #[qproperty(QString, detail_json, cxx_name = "detailJson")]
        /// The first check of this run has answered; before that the list holds
        /// only what is installed and what the cache knew.
        #[qproperty(bool, ready)]
        /// Unix seconds of the oldest answer the list rests on, or 0.
        #[qproperty(i64, checked)]
        /// 0 to 100 while a download runs, else -1.
        #[qproperty(i32, percent)]
        /// One plain line about what the job does.
        #[qproperty(QString, status)]
        /// Apps with an update waiting.
        #[qproperty(i32, update_count, cxx_name = "updateCount")]
        /// The ID of the app the running job is about, or "".
        #[qproperty(QString, busy_id, cxx_name = "busyId")]
        #[namespace = "telamon_store"]
        type NativeApps = super::NativeAppsRust;

        /// A local bundle was looked at: `detailJson` is the confirmation.
        #[qsignal]
        #[cxx_name = "detailReady"]
        fn detail_ready(self: Pin<&mut NativeApps>);

        /// An install, update or uninstall worked: other lists may have changed.
        #[qsignal]
        fn changed(self: Pin<&mut NativeApps>);
    }

    unsafe extern "RustQt" {
        /// Reads what is installed and what the cache knows. No network.
        #[qinvokable]
        fn refresh(self: Pin<&mut NativeApps>);

        /// Asks for the catalog and the latest releases, unless the answers
        /// are less than six hours old (`force` asks regardless).
        #[qinvokable]
        fn check(self: Pin<&mut NativeApps>, force: bool);

        /// Downloads, checks and installs (or updates to) the release the
        /// last check found for `id`. The caller has asked the user.
        #[qinvokable]
        fn install(self: Pin<&mut NativeApps>, id: &QString, version: &QString);

        /// Removes an app the Store installed.
        #[qinvokable]
        fn uninstall(self: Pin<&mut NativeApps>, id: &QString);

        /// Starts an installed app. `token` is the activation token QML got
        /// from `ActivationToken` ("" for none).
        #[qinvokable]
        fn open(self: Pin<&mut NativeApps>, id: &QString, token: &QString);

        /// Looks inside the bundle file at `path` and prepares the confirmation
        /// (signals `detailReady`, or sets `errorText`).
        #[qinvokable]
        #[cxx_name = "requestLocal"]
        fn request_local(self: Pin<&mut NativeApps>, path: &QString);

        /// Installs the bundle `requestLocal` showed.
        #[qinvokable]
        #[cxx_name = "confirmLocal"]
        fn confirm_local(self: Pin<&mut NativeApps>);

        /// Drops the bundle `requestLocal` showed.
        #[qinvokable]
        #[cxx_name = "cancelLocal"]
        fn cancel_local(self: Pin<&mut NativeApps>);

        #[qinvokable]
        #[cxx_name = "clearMessages"]
        fn clear_messages(self: Pin<&mut NativeApps>);

        /// Whether `id` is an app of this list (a Telamon native app).
        #[qinvokable]
        #[cxx_name = "isNative"]
        fn is_native(self: &NativeApps, id: &QString) -> bool;
    }

    impl cxx_qt::Threading for NativeApps {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn native_apps_make_unique() -> UniquePtr<NativeApps>;
    }
}

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::{Value, json};
use telamon_store_core::flatpak::valid_activation_token;
use telamon_store_core::native::check::{self, Listed, Report, Status};
use telamon_store_core::native::fetch::{Cache, Fetcher, Net};
use telamon_store_core::native::install::{self, Dirs, Options, Origin};
use telamon_store_core::native::manifest::{Host, Manifest};

use crate::appimages::human_size;
use crate::catalog::safe_icon;

/// The network. With the `fake-github` feature (screenshot builds only) and
/// `TELAMON_STORE_FAKE_GITHUB=<folder of recorded answers>`, the recorded
/// answers; never GitHub.
fn fetcher() -> Arc<dyn Fetcher> {
    #[cfg(feature = "fake-github")]
    if let Some(dir) = std::env::var_os("TELAMON_STORE_FAKE_GITHUB") {
        match telamon_store_core::native::fake::Fake::from_dir(std::path::Path::new(&dir)) {
            Ok(f) => return Arc::new(f),
            Err(e) => log::error!("TELAMON_STORE_FAKE_GITHUB: {e}"),
        }
    }
    Arc::new(Net)
}

/// What the confirmation for a local bundle is about.
#[derive(Clone)]
struct PendingLocal {
    /// The private copy that was looked at (not the user's file).
    path: PathBuf,
    manifest: Manifest,
    /// Its SHA-256 as shown in the confirmation.
    sha256: String,
}

pub struct NativeAppsRust {
    phase: QString,
    error_text: QString,
    result_text: QString,
    note_text: QString,
    apps_json: QString,
    detail_json: QString,
    ready: bool,
    checked: i64,
    percent: i32,
    status: QString,
    update_count: i32,
    busy_id: QString,
    listed: Vec<Listed>,
    pending: Option<PendingLocal>,
    /// The running job is a check (it is not waited on: see `submit`).
    checking: bool,
    /// A request that came while a check ran; it starts when the check ends.
    queued: Option<(Job, &'static str, String)>,
}

impl Default for NativeAppsRust {
    fn default() -> Self {
        NativeAppsRust {
            phase: QString::from("idle"),
            error_text: QString::default(),
            result_text: QString::default(),
            note_text: QString::default(),
            apps_json: QString::from("[]"),
            detail_json: QString::from("{}"),
            ready: false,
            checked: 0,
            percent: -1,
            status: QString::default(),
            update_count: 0,
            busy_id: QString::default(),
            listed: Vec::new(),
            pending: None,
            checking: false,
            queued: None,
        }
    }
}

enum Job {
    Load,
    Check { force: bool },
    Install(Box<Listed>),
    Remove(String),
    Open(String, Option<String>),
    Inspect(PathBuf),
    InstallLocal(Box<PendingLocal>),
}

#[derive(Default)]
struct Outcome {
    error: Option<String>,
    result: Option<String>,
    note: Option<String>,
    report: Option<Report>,
    detail: Option<(Value, PendingLocal)>,
    changed: bool,
}

const BUSY: &str = "The Store is busy with another Telamon app. Try again in a moment.";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The JSON the QML lists: one object per app. All text is plain.
fn rows(list: &[Listed]) -> Value {
    Value::Array(
        list.iter()
            .map(|l| {
                let (state, reason) = match &l.status {
                    Status::Available => ("available", String::new()),
                    Status::UpToDate => ("installed", String::new()),
                    Status::Update => ("update", String::new()),
                    Status::Incompatible(why) => {
                        (if l.installed.is_some() { "installed" } else { "incompatible" }, why.clone())
                    }
                };
                let inst = l.installed.as_ref();
                // The key the release being offered verified with, the key
                // the installed copy was installed on (none for a local file
                // or an install from before releases were signed), and
                // whether those are two keys.
                let signer = l.candidate.as_ref().map(|c| c.signer.clone()).unwrap_or_default();
                let installed_signer = inst.and_then(|i| i.origin.signer.clone()).unwrap_or_default();
                json!({
                    "id": l.id,
                    "name": l.name,
                    "summary": l.summary,
                    "homepage": l.homepage,
                    "license": l.license,
                    "repo": l.repo.clone().unwrap_or_default(),
                    "state": state,
                    "reason": reason,
                    "installedVersion": inst.map(|i| i.version.clone()).unwrap_or_default(),
                    "availableVersion": l.candidate.as_ref().map(|c| c.manifest.version.clone()).unwrap_or_default(),
                    "size": l.candidate.as_ref().and_then(|c| c.manifest.archive.as_ref()).map(|a| human_size(a.size)).unwrap_or_default(),
                    "signer": signer,
                    "installedSigner": installed_signer,
                    "signerChanged": !installed_signer.is_empty() && !signer.is_empty() && installed_signer != signer,
                    "installedSize": inst.map(|i| human_size(i.size)).unwrap_or_default(),
                    "installed": inst.is_some(),
                    "present": inst.is_none_or(|i| i.present),
                    "local": inst.is_some_and(|i| i.origin.kind == "local"),
                    "iconSource": inst.and_then(|i| i.icon.as_deref()).and_then(safe_icon).unwrap_or_default(),
                })
            })
            .collect(),
    )
}

fn problems_note(report: &Report) -> Option<String> {
    let mut parts: Vec<String> = report.problems.iter().map(|p| p.text.clone()).collect();
    if report.stale && parts.is_empty() {
        parts.push("GitHub could not be reached. Showing what was found last time.".into());
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn work_dir(cache: &Cache) -> PathBuf {
    cache.dir().join("work")
}

fn run_job(job: Job, progress: &mut dyn FnMut(i32, &str)) -> Outcome {
    let (Some(dirs), Some(cache)) = (Dirs::from_env(), Cache::from_env()) else {
        return Outcome {
            error: Some("There is no home folder to install to.".into()),
            ..Outcome::default()
        };
    };
    let host = Host::detect();
    let mut out = Outcome::default();
    match job {
        Job::Load => out.report = Some(check::cached(&cache, &dirs, &host)),
        Job::Check { force } => {
            progress(-1, "Checking for Telamon app updates…");
            let report = check::check(fetcher().as_ref(), &cache, &dirs, &host, now(), force);
            out.note = problems_note(&report);
            out.report = Some(report);
        }
        Job::Install(listed) => {
            let Some(cand) = listed.candidate.clone() else {
                out.error = Some("There is no release of this app to install.".into());
                return out;
            };
            let verb = if listed.installed.is_some() {
                "Updating"
            } else {
                "Installing"
            };
            progress(0, &format!("{verb} {}…", listed.name));
            let name = listed.name.clone();
            let mut last = -1;
            let f = fetcher();
            let result = check::install_candidate(
                f.as_ref(),
                &dirs,
                &host,
                &cand,
                &work_dir(&cache),
                &mut |done, total| {
                    let pct = (done * 100).checked_div(total).map_or(-1, |p| p as i32);
                    if pct != last {
                        last = pct;
                        progress(pct, &format!("{verb} {name}…"));
                    }
                },
            );
            match result {
                Ok(done) => {
                    out.result = Some(match done.replaced {
                        Some(old) => {
                            format!("{} was updated from {old} to {}.", done.name, done.version)
                        }
                        None => format!("{} {} was installed.", done.name, done.version),
                    });
                    out.changed = true;
                }
                Err(e) => out.error = Some(e.0),
            }
            out.report = Some(check::cached(&cache, &dirs, &host));
        }
        Job::Remove(id) => {
            match install::uninstall(&dirs, &id) {
                Ok(r) if r.left.is_empty() => {
                    out.result = Some(format!(
                        "{} was removed. Its settings and files were kept.",
                        r.name
                    ));
                    out.changed = true;
                }
                Ok(r) => {
                    let what: Vec<String> = r
                        .left
                        .iter()
                        .map(|(p, why)| format!("{} {why}", p.display()))
                        .collect();
                    out.error = Some(format!(
                        "{} was only partly removed. Left alone: {}.",
                        r.name,
                        what.join("; ")
                    ));
                    out.changed = true;
                }
                Err(e) => out.error = Some(e.0),
            }
            out.report = Some(check::cached(&cache, &dirs, &host));
        }
        Job::Open(id, token) => {
            if let Err(e) = install::launch(&dirs, &id, token.as_deref()) {
                out.error = Some(e.0);
            }
        }
        Job::Inspect(path) => {
            progress(-1, "Looking inside the bundle…");
            match check::inspect_local(&path, &work_dir(&cache)) {
                Ok((m, sha, size, copy)) => {
                    let installed = install::read_record(&dirs, &m.id);
                    let fits = m.compatible(&host).err().map(|e| e.0).unwrap_or_default();
                    out.detail = Some((
                        json!({
                            "id": m.id,
                            "name": m.name,
                            "version": m.version,
                            "summary": m.summary,
                            "license": m.license,
                            "homepage": m.homepage,
                            "fileName": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                            "size": human_size(size),
                            "sha256": sha,
                            // A file the user opened has no signature to check.
                            "signed": false,
                            "files": m.files.len(),
                            "replaces": installed.is_some(),
                            "installedVersion": installed.as_ref().map(|r| r.version.clone()).unwrap_or_default(),
                            "fromCatalog": installed.as_ref().is_some_and(|r| r.origin.kind == "release"),
                            "problem": fits,
                        }),
                        PendingLocal {
                            path: copy,
                            manifest: m,
                            sha256: sha,
                        },
                    ));
                }
                Err(e) => out.error = Some(e.0),
            }
        }
        Job::InstallLocal(p) => {
            progress(-1, &format!("Installing {}…", p.manifest.name));
            let same = telamon_store_core::native::archive::sha256_file(&p.path)
                .is_ok_and(|(h, _)| h == p.sha256);
            let result = if !same {
                Err(telamon_store_core::native::Error(
                    "The file changed after it was looked at. Nothing was installed.".into(),
                ))
            } else {
                install::install_bundle(
                    &dirs,
                    &p.path,
                    &Options {
                        expect_id: Some(&p.manifest.id),
                        outer: None,
                        origin: Origin::local(),
                        host: &host,
                    },
                )
            };
            let _ = std::fs::remove_file(&p.path);
            match result {
                Ok(done) => {
                    out.result = Some(format!("{} {} was installed.", done.name, done.version));
                    out.changed = true;
                }
                Err(e) => out.error = Some(e.0),
            }
            out.report = Some(check::cached(&cache, &dirs, &host));
        }
    }
    out
}

impl qobject::NativeApps {
    fn idle(&self) -> bool {
        let p = self.phase().to_string();
        p.is_empty() || p == "idle"
    }

    fn submit(mut self: Pin<&mut Self>, job: Job, phase: &'static str, busy_id: &str) {
        // A load and an Open do not take the phase: neither changes what the
        // others work on, and Open needs no network.
        let foreground = !matches!(job, Job::Load | Job::Open(..));
        if foreground {
            if !self.idle() {
                // A check does not make the user's request wait to be refused:
                // it starts when the check ends (the newest request wins).
                if self.rust().checking && !matches!(job, Job::Check { .. }) {
                    self.as_mut().rust_mut().queued = Some((job, phase, busy_id.to_string()));
                }
                return;
            }
            self.as_mut().rust_mut().checking = matches!(job, Job::Check { .. });
            self.as_mut().set_phase(QString::from(phase));
            self.as_mut().set_busy_id(QString::from(busy_id));
            self.as_mut().set_error_text(QString::default());
            self.as_mut().set_result_text(QString::default());
            self.as_mut().set_percent(-1);
            self.as_mut().set_status(QString::default());
        }
        let is_check = matches!(job, Job::Check { .. });
        let thread: CxxQtThread<qobject::NativeApps> = self.qt_thread();
        let spawned = std::thread::Builder::new()
            .name("telamon-store-native".into())
            .spawn(move || {
                let t = thread.clone();
                let mut progress = move |pct: i32, text: &str| {
                    let text = text.to_string();
                    let _ = t.queue(move |mut s| {
                        s.as_mut().set_percent(pct);
                        s.as_mut().set_status(QString::from(text.as_str()));
                    });
                };
                let out = catch_unwind(AssertUnwindSafe(|| run_job(job, &mut progress)))
                    .unwrap_or_else(|_| Outcome {
                        error: Some("Something went wrong with the Telamon apps.".into()),
                        ..Outcome::default()
                    });
                let _ = thread.queue(move |s| s.finish(foreground, is_check, out));
            });
        if let Err(e) = spawned {
            log::error!("could not start the native apps job: {e}");
            if foreground {
                self.as_mut().set_phase(QString::from("idle"));
                self.as_mut().set_busy_id(QString::default());
                self.as_mut().set_error_text(QString::from(
                    "The Store could not run this. Please restart it.",
                ));
            }
        }
    }

    fn finish(mut self: Pin<&mut Self>, foreground: bool, is_check: bool, out: Outcome) {
        if let Some(report) = out.report {
            self.as_mut().rust_mut().listed = report.apps.clone();
            self.as_mut()
                .set_apps_json(QString::from(rows(&report.apps).to_string().as_str()));
            let updates = report
                .apps
                .iter()
                .filter(|l| l.status == Status::Update)
                .count();
            self.as_mut().set_update_count(updates as i32);
            if report.checked_at > 0 || is_check {
                self.as_mut().set_checked(report.checked_at as i64);
            }
            // Ready once a check has answered (from the network or, failing
            // that, the cache): until then the list may be missing apps.
            if !*self.ready() && is_check {
                self.as_mut().set_ready(true);
            }
        }
        if is_check {
            self.as_mut()
                .set_note_text(QString::from(out.note.unwrap_or_default().as_str()));
        }
        if foreground {
            self.as_mut().rust_mut().checking = false;
            self.as_mut().set_phase(QString::from("idle"));
            self.as_mut().set_busy_id(QString::default());
            self.as_mut().set_percent(-1);
            self.as_mut().set_status(QString::default());
            if let Some(r) = &out.result {
                self.as_mut().set_result_text(QString::from(r.as_str()));
            }
        }
        // An error (Open's too, which ran beside whatever else was going on).
        if let Some(e) = &out.error {
            self.as_mut().set_error_text(QString::from(e.as_str()));
        }
        if let Some((detail, pending)) = out.detail {
            self.as_mut().rust_mut().pending = Some(pending);
            self.as_mut()
                .set_detail_json(QString::from(detail.to_string().as_str()));
            self.as_mut().detail_ready();
        }
        if out.changed {
            self.as_mut().changed();
        }
        if foreground && let Some((job, phase, busy_id)) = self.as_mut().rust_mut().queued.take() {
            self.submit(job, phase, &busy_id);
        }
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.submit(Job::Load, "idle", "");
    }

    pub fn check(self: Pin<&mut Self>, force: bool) {
        self.submit(Job::Check { force }, "checking", "");
    }

    pub fn install(mut self: Pin<&mut Self>, id: &QString, version: &QString) {
        if !self.idle() {
            self.as_mut().set_result_text(QString::default());
            self.as_mut().set_error_text(QString::from(BUSY));
            return;
        }
        let id = id.to_string();
        let version = version.to_string();
        let found = self.listed.iter().find(|l| l.id == id).cloned();
        match found {
            // The version the user was shown is the one that installs.
            Some(l)
                if l.candidate
                    .as_ref()
                    .is_some_and(|c| c.manifest.version != version) =>
            {
                self.as_mut().set_result_text(QString::default());
                self.as_mut().set_error_text(QString::from(
                    "A different version came out while you were deciding. Look at it again.",
                ));
            }
            Some(l)
                if l.candidate.is_some()
                    && matches!(l.status, Status::Available | Status::Update) =>
            {
                self.submit(Job::Install(Box::new(l)), "installing", &id);
            }
            _ => {
                self.as_mut().set_result_text(QString::default());
                self.as_mut().set_error_text(QString::from(
                    "There is nothing to install for this app. Check for updates and try again.",
                ));
            }
        }
    }

    pub fn uninstall(mut self: Pin<&mut Self>, id: &QString) {
        if !self.idle() && !self.rust().checking {
            self.as_mut().set_result_text(QString::default());
            self.as_mut().set_error_text(QString::from(BUSY));
            return;
        }
        let id = id.to_string();
        self.submit(Job::Remove(id.clone()), "removing", &id);
    }

    pub fn open(self: Pin<&mut Self>, id: &QString, token: &QString) {
        let token = token.to_string();
        let token = valid_activation_token(&token).map(str::to_string);
        let id = id.to_string();
        self.submit(Job::Open(id.clone(), token), "opening", &id);
    }

    pub fn request_local(mut self: Pin<&mut Self>, path: &QString) {
        if !self.idle() && !self.rust().checking {
            self.as_mut().set_result_text(QString::default());
            self.as_mut().set_error_text(QString::from(BUSY));
            return;
        }
        self.as_mut().rust_mut().pending = None;
        self.submit(
            Job::Inspect(PathBuf::from(path.to_string())),
            "inspecting",
            "",
        );
    }

    pub fn confirm_local(mut self: Pin<&mut Self>) {
        if !self.idle() {
            self.as_mut().set_result_text(QString::default());
            self.as_mut().set_error_text(QString::from(BUSY));
            return;
        }
        let Some(p) = self.as_mut().rust_mut().pending.take() else {
            return;
        };
        let id = p.manifest.id.clone();
        self.submit(Job::InstallLocal(Box::new(p)), "installing", &id);
    }

    pub fn cancel_local(mut self: Pin<&mut Self>) {
        if let Some(p) = self.as_mut().rust_mut().pending.take() {
            let _ = std::fs::remove_file(&p.path);
        }
        self.as_mut().set_detail_json(QString::from("{}"));
    }

    pub fn clear_messages(mut self: Pin<&mut Self>) {
        self.as_mut().set_error_text(QString::default());
        self.as_mut().set_result_text(QString::default());
    }

    pub fn is_native(&self, id: &QString) -> bool {
        let id = id.to_string();
        self.rust().listed.iter().any(|l| l.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use telamon_store_core::native::check::build_list;
    use telamon_store_core::native::fake::{BundleBuilder, Fake, Signing, TestKey, default_key};
    use telamon_store_core::native::version::Version;

    fn host() -> Host {
        Host {
            os_version: Some(44),
            telamon_ui: Version::parse("2.0.2"),
            arch: "x86_64".into(),
        }
    }

    #[test]
    fn rows_say_what_the_page_needs() {
        let fake = Fake::new();
        let id = "net.eterneon.telamon.gates";
        let repo = "EternalCoder454/telamon-gates";
        fake.catalog(&[(id, repo)]);
        fake.publish(
            repo,
            "v0.2.0",
            &BundleBuilder::new(id, "Telamon Gates", "0.2.0")
                .exe("telamon-gates")
                .build(),
        );
        let root = std::env::temp_dir().join(format!("native-rows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs {
            data: root.join("data"),
            home: root.join("home"),
            system: Vec::new(),
        };
        std::fs::create_dir_all(&dirs.data).unwrap();
        let cache = Cache::new(root.join("cache"));
        let report = check::check(&fake, &cache, &dirs, &host(), 1_800_000_000, false);
        let v = rows(&report.apps);
        let row = &v[0];
        assert_eq!(row["id"], id);
        assert_eq!(row["state"], "available");
        assert_eq!(row["availableVersion"], "0.2.0");
        assert_eq!(row["installed"], false);
        // Who vouches for it: the key that verified, and nothing to compare.
        assert_eq!(row["signer"], default_key().key_id());
        assert_eq!(row["installedSigner"], "");
        assert_eq!(row["signerChanged"], false);
        assert!(
            row["size"].as_str().unwrap().ends_with("kB")
                || row["size"].as_str().unwrap().ends_with(" B")
        );
        assert_eq!(
            rows(&build_list(&[], &[], &Default::default(), &host())),
            json!([])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rows_say_when_the_signing_key_changed() {
        let id = "net.eterneon.telamon.gates";
        let repo = "EternalCoder454/telamon-gates";
        let (old, new) = (TestKey::new(1), TestKey::new(2));
        let fake = Fake::new();
        let bundle = |v: &str| {
            BundleBuilder::new(id, "Telamon Gates", v)
                .exe("telamon-gates")
                .build()
        };
        let root = std::env::temp_dir().join(format!("native-rows-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dirs = Dirs {
            data: root.join("data"),
            home: root.join("home"),
            system: Vec::new(),
        };
        std::fs::create_dir_all(&dirs.data).unwrap();
        std::fs::create_dir_all(&dirs.home).unwrap();
        let cache = Cache::new(root.join("cache"));
        // Installed on a release signed with the old key.
        fake.catalog_with_keys(&[(id, repo, vec![&old, &new])]);
        fake.publish_with(repo, "v0.1.0", &bundle("0.1.0"), Signing::With(&old));
        let report = check::check(&fake, &cache, &dirs, &host(), 1_800_000_000, false);
        let cand = report.apps[0].candidate.clone().unwrap();
        check::install_candidate(
            &fake,
            &dirs,
            &host(),
            &cand,
            &root.join("work"),
            &mut |_, _| {},
        )
        .unwrap();
        // The next release is signed with the same key: nothing to say.
        fake.publish_with(repo, "v0.2.0", &bundle("0.2.0"), Signing::With(&old));
        let report = check::check(&fake, &cache, &dirs, &host(), 1_800_000_100, true);
        let row = &rows(&report.apps)[0];
        assert_eq!(row["state"], "update");
        assert_eq!(row["installedSigner"], old.key_id());
        assert_eq!(row["signer"], old.key_id());
        assert_eq!(row["signerChanged"], false);
        // Signed with the other listed key (a rotation): the dialog says so.
        fake.publish_with(repo, "v0.3.0", &bundle("0.3.0"), Signing::With(&new));
        let report = check::check(&fake, &cache, &dirs, &host(), 1_800_000_200, true);
        let row = &rows(&report.apps)[0];
        assert_eq!(row["state"], "update");
        assert_eq!(row["signer"], new.key_id());
        assert_eq!(row["signerChanged"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_note_is_made_for_a_failed_check() {
        let mut r = Report::default();
        assert_eq!(problems_note(&r), None);
        r.stale = true;
        assert!(problems_note(&r).unwrap().contains("could not be reached"));
        r.problems.push(telamon_store_core::native::check::Problem {
            id: None,
            text: "X failed.".into(),
        });
        assert_eq!(problems_note(&r).as_deref(), Some("X failed."));
    }
}
