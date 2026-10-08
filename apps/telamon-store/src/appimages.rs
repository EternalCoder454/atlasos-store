//! AppImages in the window: the install confirmation (`request`, then
//! `confirm`), the list of the ones the Store installed, Open and Uninstall.
//!
//! Threading: every job runs on a short-lived thread of its own and the
//! result is queued back with `qt_thread().queue`; the GUI thread never reads
//! the file or runs anything. One job at a time (`phase` is "idle" or the
//! job). Looking inside a file is done by a helper process under resource
//! limits (`telamon_store_core::appimage::helper`); what comes back is
//! cleaned again before it is shown. Nothing is installed or removed except
//! after the user's answer in the Store's own dialog, and the file is never
//! run except by Open on an app the Store installed. All text the QML shows
//! from here is plain.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        /// "idle", "inspecting", "installing", "removing" or "opening".
        #[qproperty(QString, phase)]
        /// Why the last job failed, in plain words, or "".
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// What the last job did, in plain words, or "".
        #[qproperty(QString, result_text, cxx_name = "resultText")]
        /// JSON object: what the install dialog shows.
        #[qproperty(QString, detail_json, cxx_name = "detailJson")]
        /// JSON array of the installed AppImages.
        #[qproperty(QString, installed_json, cxx_name = "installedJson")]
        /// The list was read at least once.
        #[qproperty(bool, installed_ready, cxx_name = "installedReady")]
        #[namespace = "telamon_store"]
        type AppImages = super::AppImagesRust;

        /// A file was looked at: `detailJson` is the confirmation.
        #[qsignal]
        #[cxx_name = "detailReady"]
        fn detail_ready(self: Pin<&mut AppImages>);

        /// An install worked.
        #[qsignal]
        fn installed(self: Pin<&mut AppImages>);
    }

    unsafe extern "RustQt" {
        /// Reads the list of installed AppImages again.
        #[qinvokable]
        fn refresh(self: Pin<&mut AppImages>);

        /// Looks inside the AppImage at `path` (never runs it) and prepares
        /// the confirmation (signals `detailReady`, or sets `errorText`).
        #[qinvokable]
        fn request(self: Pin<&mut AppImages>, path: &QString);

        /// Installs the file `request` showed, exactly as it was looked at.
        #[qinvokable]
        fn confirm(self: Pin<&mut AppImages>);

        /// Drops the file `request` showed.
        #[qinvokable]
        fn cancel(self: Pin<&mut AppImages>);

        /// Removes an installed AppImage (its file, icon and menu entry).
        #[qinvokable]
        fn uninstall(self: Pin<&mut AppImages>, id: &QString);

        /// Starts an installed AppImage. `token` is the activation token QML
        /// got from `ActivationToken` ("" for none).
        #[qinvokable]
        fn open(self: Pin<&mut AppImages>, id: &QString, token: &QString);

        #[qinvokable]
        #[cxx_name = "clearMessages"]
        fn clear_messages(self: Pin<&mut AppImages>);
    }

    impl cxx_qt::Threading for AppImages {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn app_images_make_unique() -> UniquePtr<AppImages>;
    }
}

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use serde_json::{Value, json};
use telamon_store_core::appimage::Format;
use telamon_store_core::appimage::fsutil;
use telamon_store_core::appimage::helper;
use telamon_store_core::appimage::inspect::Inspection;
use telamon_store_core::appimage::install::{self, Dirs, Installed, Plan};
use telamon_store_core::appimage::trust;
use telamon_store_core::flatpak::valid_activation_token;

use crate::catalog::{file_url, library, safe_icon};

/// How long the install dialog waits for the catalog (to look for the app on
/// Flathub) when the Store was started by opening the file.
const CATALOG_WAIT: Duration = Duration::from_secs(4);

/// The file the dialog is asking about.
struct Pending {
    path: PathBuf,
    inspection: Inspection,
    plan: Plan,
}

pub struct AppImagesRust {
    phase: QString,
    error_text: QString,
    result_text: QString,
    detail_json: QString,
    installed_json: QString,
    installed_ready: bool,
    pending: Option<Pending>,
}

impl Default for AppImagesRust {
    fn default() -> Self {
        AppImagesRust {
            phase: QString::from("idle"),
            error_text: QString::default(),
            result_text: QString::default(),
            detail_json: QString::from("{}"),
            installed_json: QString::from("[]"),
            installed_ready: false,
            pending: None,
        }
    }
}

enum Job {
    List,
    Inspect(PathBuf),
    Install(Box<Pending>),
    Remove(String),
    Open(String, Option<String>),
}

#[derive(Default)]
struct Outcome {
    error: Option<String>,
    result: Option<String>,
    pending: Option<Box<Pending>>,
    detail: Option<Value>,
    listing: Option<Vec<Installed>>,
    installed: bool,
}

pub(crate) fn human_size(n: u64) -> String {
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

/// `$XDG_CACHE_HOME/telamon-store/appimage`: the icon the dialog shows.
fn preview_dir() -> Option<PathBuf> {
    Some(
        fsutil::cache_home()?
            .join(telamon_store_core::legacy::NAME)
            .join("appimage"),
    )
}

/// What the page says when a request is ignored because a job is running.
const BUSY: &str = "The Store is busy with another AppImage. Try again in a moment.";

/// Removes the icons written for the dialog (`save_preview`): they are the
/// file's, shown only while the question is open.
fn remove_previews() {
    let Some(dir) = preview_dir() else { return };
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten().take(64) {
            if e.file_name().to_string_lossy().starts_with("preview-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Writes the icon of the file being asked about where QML can read it,
/// under a new name each time (the image cache is keyed by URL), and removes
/// the earlier ones. `""` for none.
fn save_preview(insp: &Inspection) -> String {
    let Some(icon) = insp.icon.as_ref() else {
        return String::new();
    };
    let Some(dir) = preview_dir() else {
        return String::new();
    };
    if fsutil::private_dir(&dir).is_err() {
        return String::new();
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten().take(64) {
            let name = e.file_name();
            if name.to_string_lossy().starts_with("preview-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let name = format!(
        "preview-{}-{}.{}",
        std::process::id(),
        insp.sha256.get(..12).unwrap_or("x"),
        icon.kind.ext()
    );
    let path = dir.join(name);
    match fsutil::write_atomic(&path, &icon.bytes, 0o600) {
        Ok(()) => file_url(&path),
        Err(e) => {
            log::warn!("could not save the preview icon: {e}");
            String::new()
        }
    }
}

fn installed_rows(dirs: &Dirs, list: &[Installed]) -> Value {
    let _ = dirs;
    Value::Array(
        list.iter()
            .map(|i| {
                json!({
                    "id": i.id,
                    "name": i.name,
                    "version": i.version,
                    "size": if i.present { human_size(i.size) } else { String::new() },
                    "path": i.path.to_string_lossy(),
                    "present": i.present,
                    "iconSource": i.icon.as_deref().and_then(safe_icon).unwrap_or_default(),
                })
            })
            .collect(),
    )
}

fn inspect_job(path: &Path) -> Outcome {
    let mut out = Outcome::default();
    let Some(dirs) = Dirs::from_env() else {
        out.error = Some("There is no home folder to install to.".into());
        return out;
    };
    // The running program itself: it survives a package upgrade that
    // replaced the file under a running Store.
    let insp = match helper::run(Path::new(helper::SELF), path, helper::TIMEOUT) {
        Ok(i) => i,
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    };
    let plan = match install::plan(&dirs, &insp) {
        Ok(p) => p,
        Err(e) => {
            out.error = Some(e.to_string());
            return out;
        }
    };
    // The catalog loads in the background; when the Store was started by
    // opening this file it may not be there yet.
    let start = Instant::now();
    let lib = loop {
        if let Some(l) = library() {
            break Some(l);
        }
        if start.elapsed() > CATALOG_WAIT {
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let flathub = lib.as_ref().and_then(|l| trust::flathub_match(l, &insp));
    let t = trust::assess(&insp);
    let installed_version = if plan.replaces {
        install::list(&dirs)
            .into_iter()
            .find(|i| i.id == plan.id)
            .map(|i| i.version)
            .unwrap_or_default()
    } else {
        String::new()
    };
    out.detail = Some(json!({
        "fileName": insp.file_name,
        "name": insp.name,
        "version": insp.version,
        "publisher": insp.publisher,
        "summary": insp.summary,
        "size": human_size(insp.size),
        "oldFormat": insp.format == Format::Type1,
        "inspected": insp.inspected,
        "iconSource": save_preview(&insp),
        "strong": t.strong,
        "lines": t.lines,
        "access": trust::ACCESS,
        "flathub": flathub,
        "installsAs": plan.target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "replaces": plan.replaces,
        "renamed": plan.renamed,
        "installedVersion": installed_version,
        "extractAndRun": !dirs.fuse_available(),
    }));
    out.pending = Some(Box::new(Pending {
        path: path.to_path_buf(),
        inspection: insp,
        plan,
    }));
    out
}

fn run_job(job: Job) -> Outcome {
    let Some(dirs) = Dirs::from_env() else {
        return Outcome {
            error: Some("There is no home folder to install to.".into()),
            ..Outcome::default()
        };
    };
    match job {
        Job::List => Outcome {
            listing: Some(install::list(&dirs)),
            ..Outcome::default()
        },
        Job::Inspect(path) => inspect_job(&path),
        Job::Install(p) => {
            let mut out = Outcome::default();
            match install::install(&dirs, &p.path, &p.inspection, &p.plan) {
                Ok(done) => {
                    out.result = Some(if p.plan.renamed {
                        format!(
                            "{} was installed as {}.",
                            done.name,
                            p.plan
                                .target
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        )
                    } else {
                        format!("{} was installed.", done.name)
                    });
                    out.installed = true;
                }
                Err(e) => out.error = Some(e.to_string()),
            }
            out.listing = Some(install::list(&dirs));
            remove_previews();
            out
        }
        Job::Remove(id) => {
            let mut out = Outcome::default();
            match install::uninstall(&dirs, &id) {
                Ok(r) if r.left.is_empty() => out.result = Some(format!("{} was removed.", r.name)),
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
                }
                Err(e) => out.error = Some(e.to_string()),
            }
            out.listing = Some(install::list(&dirs));
            out
        }
        Job::Open(id, token) => {
            let mut out = Outcome::default();
            if let Err(e) = install::launch(&dirs, &id, token.as_deref()) {
                out.error = Some(e.to_string());
            }
            out
        }
    }
}

impl qobject::AppImages {
    fn submit(mut self: Pin<&mut Self>, job: Job, phase: &str) {
        let foreground = !matches!(job, Job::List);
        if foreground {
            if !self.idle() {
                return;
            }
            self.as_mut().set_phase(QString::from(phase));
            self.as_mut().set_error_text(QString::default());
            self.as_mut().set_result_text(QString::default());
        }
        let thread: CxxQtThread<qobject::AppImages> = self.qt_thread();
        let spawned = std::thread::Builder::new()
            .name("telamon-store-appimage".into())
            .spawn(move || {
                let out =
                    catch_unwind(AssertUnwindSafe(|| run_job(job))).unwrap_or_else(|_| Outcome {
                        error: Some("Something went wrong while looking at the file.".into()),
                        ..Outcome::default()
                    });
                let _ = thread.queue(move |s| s.finish(foreground, out));
            });
        if let Err(e) = spawned {
            log::error!("could not start the AppImage job: {e}");
            if foreground {
                self.as_mut().set_phase(QString::from("idle"));
                self.as_mut().set_error_text(QString::from(
                    "The Store could not run this. Please restart it.",
                ));
            }
        }
    }

    fn finish(mut self: Pin<&mut Self>, foreground: bool, out: Outcome) {
        if let Some(list) = out.listing {
            let dirs = Dirs::from_env();
            let rows = dirs
                .as_ref()
                .map_or(json!([]), |d| installed_rows(d, &list));
            self.as_mut()
                .set_installed_json(QString::from(rows.to_string().as_str()));
            if !*self.installed_ready() {
                self.as_mut().set_installed_ready(true);
            }
        }
        if foreground {
            self.as_mut().set_phase(QString::from("idle"));
            // A second request that was turned away while this one ran must
            // not stay on the page once this one is done.
            self.as_mut().clear_busy();
            if let Some(e) = &out.error {
                self.as_mut().set_error_text(QString::from(e.as_str()));
            }
            if let Some(r) = &out.result {
                self.as_mut().set_result_text(QString::from(r.as_str()));
            }
        }
        if let (Some(detail), Some(pending)) = (out.detail, out.pending) {
            self.as_mut().rust_mut().pending = Some(*pending);
            self.as_mut()
                .set_detail_json(QString::from(detail.to_string().as_str()));
            self.as_mut().detail_ready();
        }
        if out.installed {
            self.as_mut().installed();
        }
    }

    fn clear_busy(mut self: Pin<&mut Self>) {
        if self.error_text().to_string() == BUSY {
            self.as_mut().set_error_text(QString::default());
        }
    }

    /// Says on the page that a request was ignored because a job runs.
    fn say_busy(mut self: Pin<&mut Self>) {
        self.as_mut().set_result_text(QString::default());
        self.as_mut().set_error_text(QString::from(BUSY));
    }

    fn idle(&self) -> bool {
        let p = self.phase().to_string();
        p.is_empty() || p == "idle"
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.submit(Job::List, "idle");
    }

    pub fn request(mut self: Pin<&mut Self>, path: &QString) {
        if !self.idle() {
            self.say_busy();
            return;
        }
        self.as_mut().rust_mut().pending = None;
        self.submit(Job::Inspect(PathBuf::from(path.to_string())), "inspecting");
    }

    pub fn confirm(mut self: Pin<&mut Self>) {
        if !self.idle() {
            self.say_busy();
            return;
        }
        let Some(p) = self.as_mut().rust_mut().pending.take() else {
            return;
        };
        self.submit(Job::Install(Box::new(p)), "installing");
    }

    pub fn cancel(mut self: Pin<&mut Self>) {
        self.as_mut().rust_mut().pending = None;
        self.as_mut().set_detail_json(QString::from("{}"));
        self.as_mut().clear_busy();
        // The preview icon belongs to the question that was just closed.
        remove_previews();
    }

    pub fn uninstall(self: Pin<&mut Self>, id: &QString) {
        if !self.idle() {
            self.say_busy();
            return;
        }
        self.submit(Job::Remove(id.to_string()), "removing");
    }

    pub fn open(self: Pin<&mut Self>, id: &QString, token: &QString) {
        if !self.idle() {
            self.say_busy();
            return;
        }
        let token = token.to_string();
        let token = valid_activation_token(&token).map(str::to_string);
        self.submit(Job::Open(id.to_string(), token), "opening");
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
    fn sizes_are_decimal() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(2_500_000), "2.5 MB");
    }
}
