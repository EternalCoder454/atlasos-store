//! What the Store browses: the merged AppStream catalogs (`Catalog`) and the
//! lists of apps a page shows (`AppListModel`).
//!
//! Threading: `Catalog::start` runs one worker thread that lists the sources,
//! loads each catalog through the on-disk index, builds the `Library` and
//! queues it back to the GUI thread; a catalog older than `STALE_AFTER` is
//! then refreshed on the same worker (never interactively, and only when the
//! Updater's lock is free) and the library is rebuilt and queued again. A
//! query (`search`, `browse`) runs on a worker of its own and the latest
//! request wins: each model counts its requests, a result whose count is no
//! longer current is dropped, and the GUI thread only swaps the finished rows
//! in with one model reset.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmodelindex.h");
        type QModelIndex = cxx_qt_lib::QModelIndex;
        include!("cxx-qt-lib/qvariant.h");
        type QVariant = cxx_qt_lib::QVariant;
        include!("cxx-qt-lib/qhash.h");
        type QHash_i32_QByteArray = cxx_qt_lib::QHash<cxx_qt_lib::QHashPair_i32_QByteArray>;
        include!(<QtCore/QAbstractListModel>);
        type QAbstractListModel;
    }

    extern "RustQt" {
        #[qobject]
        /// A worker is reading the catalogs and no library is built yet.
        #[qproperty(bool, loading)]
        /// A library is built (it may be empty).
        #[qproperty(bool, ready)]
        /// Plain text: one line for each source that could not be read.
        #[qproperty(QString, error_text, cxx_name = "errorText")]
        /// Enabled sources the Store can list.
        #[qproperty(i32, source_count, cxx_name = "sourceCount")]
        /// Apps in the library.
        #[qproperty(i32, app_count, cxx_name = "appCount")]
        /// Sources whose catalog was never downloaded.
        #[qproperty(i32, missing_count, cxx_name = "missingCount")]
        /// Goes up each time the library is replaced: queries are asked again.
        #[qproperty(i32, revision)]
        #[namespace = "atlas_store"]
        type Catalog = super::CatalogRust;
    }

    unsafe extern "RustQt" {
        /// Reads the catalogs again (does nothing while one is being read).
        #[qinvokable]
        fn reload(self: Pin<&mut Catalog>);

        /// The key of the category at `index` of the Store's list, or "".
        #[qinvokable]
        #[cxx_name = "categoryKey"]
        fn category_key(self: &Catalog, index: i32) -> QString;

        /// How many apps the category at `index` has.
        #[qinvokable]
        #[cxx_name = "categoryCount"]
        fn category_count(self: &Catalog, index: i32) -> i32;
    }

    impl cxx_qt::Threading for Catalog {}

    extern "RustQt" {
        #[qobject]
        #[base = QAbstractListModel]
        /// Rows shown.
        #[qproperty(i32, count)]
        /// A query is running.
        #[qproperty(bool, busy)]
        #[namespace = "atlas_store"]
        type AppListModel = super::AppListModelRust;
    }

    unsafe extern "RustQt" {
        /// Lists the apps matching `text`, best first (at most 500).
        #[qinvokable]
        fn search(self: Pin<&mut AppListModel>, text: &QString);

        /// Lists a category (every app when `category` is ""), `sort` being
        /// "name" or "updated".
        #[qinvokable]
        fn browse(
            self: Pin<&mut AppListModel>,
            category: &QString,
            sort: &QString,
            verified_only: bool,
            free_only: bool,
        );

        /// Empties the list.
        #[qinvokable]
        fn clear(self: Pin<&mut AppListModel>);

        #[inherit]
        #[cxx_name = "beginResetModel"]
        fn begin_reset_model(self: Pin<&mut AppListModel>);
        #[inherit]
        #[cxx_name = "endResetModel"]
        fn end_reset_model(self: Pin<&mut AppListModel>);

        #[cxx_override]
        fn data(self: &AppListModel, index: &QModelIndex, role: i32) -> QVariant;
        #[cxx_override]
        #[cxx_name = "roleNames"]
        fn role_names(self: &AppListModel) -> QHash_i32_QByteArray;
        #[cxx_override]
        #[cxx_name = "rowCount"]
        fn row_count(self: &AppListModel, parent: &QModelIndex) -> i32;
    }

    impl cxx_qt::Threading for AppListModel {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn catalog_make_unique() -> UniquePtr<Catalog>;
        #[cxx_name = "make_unique"]
        fn app_list_model_make_unique() -> UniquePtr<AppListModel>;
    }
}

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, SystemTime};

use atlas_store_core::appstream::lang::langs_from_env;
use atlas_store_core::catalog::{
    self, Category, EntryId, Filter, Library, LoadError, Sort, list_sources,
};
use atlas_store_core::flatpak::{CancelToken, OperationLock, Scope, update_appstream};
use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::{QByteArray, QHash, QHashPair_i32_QByteArray, QModelIndex, QString, QVariant};

/// The most results a search lists.
const SEARCH_LIMIT: usize = 500;
/// How long a refresh waits for the Updater's lock before it is skipped.
const LOCK_WAIT: Duration = Duration::from_secs(2);
/// Role numbers start here, as Qt's user roles do.
const FIRST_ROLE: i32 = 0x0100;
const ROLES: [&str; 7] = [
    "appId",
    "name",
    "summary",
    "developer",
    "iconSource",
    "verified",
    "sourceTitle",
];
/// The icon size the grids ask the catalog for.
pub(crate) const ICON_SIZE: u16 = 64;

/// The library queries run on: replaced as a whole, on the GUI thread.
static LIBRARY: RwLock<Option<Arc<Library>>> = RwLock::new(None);

pub(crate) fn library() -> Option<Arc<Library>> {
    LIBRARY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn set_library(library: Arc<Library>) {
    *LIBRARY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(library);
}

/// `$XDG_CACHE_HOME/atlas-store`, else `~/.cache/atlas-store`. The core
/// creates it (0700) and checks it when it writes an index.
fn cache_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    absolute("XDG_CACHE_HOME")
        .or_else(|| absolute("HOME").map(|h| h.join(".cache")))
        .map(|p| p.join("atlas-store"))
}

/// Everything the GUI thread takes over when a library is ready.
struct Built {
    library: Arc<Library>,
    /// Plain text, one line each.
    errors: Vec<String>,
    sources: usize,
    missing: usize,
    counts: Vec<i32>,
}

/// Lists and loads every source. Also returns the sources to refresh.
fn read_all(cancel: &CancelToken, langs: &[String]) -> (Built, Vec<(Scope, String)>) {
    let listed = list_sources(cancel);
    let mut errors = listed.errors;
    let cache = cache_dir();
    if cache.is_none() {
        errors.push("The cache folder could not be found: HOME is not set.".to_string());
    }
    let now = SystemTime::now();
    let sources = listed.sources.len();
    let mut missing = 0;
    let mut stale = Vec::new();
    let mut loaded = Vec::new();
    for source in listed.sources {
        if source.is_stale(now) {
            stale.push((source.scope, source.remote.clone()));
        }
        let Some(cache) = cache.as_deref() else {
            continue;
        };
        match catalog::load(&source, cache, langs) {
            Ok(catalog) => loaded.push((source, catalog)),
            Err(LoadError::NotDownloaded) => missing += 1,
            Err(e) => {
                log::warn!("could not load the catalog of {}: {e}", source.remote);
                errors.push(format!(
                    "{}: {e}",
                    atlas_store_core::text::clean(&source.title, 100)
                ));
            }
        }
    }
    let library = Library::new(loaded);
    let counts = library
        .category_counts(Filter::default())
        .iter()
        .map(|(_, n)| i32::try_from(*n).unwrap_or(i32::MAX))
        .collect();
    let built = Built {
        library: Arc::new(library),
        errors,
        sources,
        missing,
        counts,
    };
    (built, stale)
}

/// The worker: load, hand over, refresh what is stale, load again, hand over.
fn work(thread: CxxQtThread<qobject::Catalog>, cancel: CancelToken) {
    let langs = langs_from_env();
    let (built, stale) = read_all(&cancel, &langs);
    let sent = thread.queue(move |catalog| catalog.apply(built));
    if sent.is_err() || stale.is_empty() {
        return;
    }
    // Only while no update or other Flatpak job runs: the Updater owns the
    // lock. Never interactive: nothing may ask for a password here.
    let lock = match OperationLock::acquire(LOCK_WAIT, &cancel) {
        Ok(lock) => lock,
        Err(e) => {
            log::info!("not refreshing the catalogs: {e}");
            return;
        }
    };
    let mut refreshed = false;
    for (scope, remote) in &stale {
        match update_appstream(*scope, remote, false, &lock, &cancel) {
            Ok(()) => refreshed = true,
            Err(e) => log::warn!("could not refresh the catalog of {remote}: {e}"),
        }
    }
    drop(lock);
    if refreshed {
        let (built, _) = read_all(&cancel, &langs);
        let _ = thread.queue(move |catalog| catalog.apply(built));
    }
}

#[derive(Default)]
pub struct CatalogRust {
    loading: bool,
    ready: bool,
    error_text: QString,
    source_count: i32,
    app_count: i32,
    missing_count: i32,
    revision: i32,
    counts: Vec<i32>,
    /// A worker thread is alive (it keeps refreshing after `loading` ends).
    /// Cleared only by the closure the worker queues as it ends.
    worker_running: bool,
    /// The running worker's token: cancelled when the Catalog is dropped, so
    /// quitting mid-refresh never holds the Updater's lock.
    cancel: CancelToken,
}

impl Drop for CatalogRust {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl qobject::Catalog {
    /// Starts the worker; the first result sets `ready`.
    pub fn start(mut self: Pin<&mut Self>) {
        if self.rust().worker_running {
            return;
        }
        let thread = self.qt_thread();
        let cancel = CancelToken::new();
        self.as_mut().rust_mut().cancel = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("atlas-store-catalog".into())
            .spawn(move || {
                if catch_unwind(AssertUnwindSafe(|| work(thread.clone(), cancel))).is_err() {
                    log::error!("the catalog worker panicked");
                    let _ = thread.queue(|c| c.failed("The catalogs could not be read."));
                }
                let _ = thread.queue(|mut c| c.as_mut().rust_mut().worker_running = false);
            });
        match spawned {
            Ok(_) => {
                self.as_mut().rust_mut().worker_running = true;
                self.as_mut().set_loading(true);
            }
            Err(e) => {
                log::error!("could not start the catalog worker: {e}");
                self.failed("The catalogs could not be read.");
            }
        }
    }

    fn failed(mut self: Pin<&mut Self>, text: &str) {
        self.as_mut().set_error_text(QString::from(text));
        self.as_mut().set_loading(false);
        self.as_mut().set_ready(true);
    }

    fn apply(mut self: Pin<&mut Self>, built: Built) {
        let apps = i32::try_from(built.library.len()).unwrap_or(i32::MAX);
        set_library(built.library);
        self.as_mut().rust_mut().counts = built.counts;
        self.as_mut()
            .set_error_text(QString::from(built.errors.join("\n").as_str()));
        self.as_mut()
            .set_source_count(i32::try_from(built.sources).unwrap_or(i32::MAX));
        self.as_mut()
            .set_missing_count(i32::try_from(built.missing).unwrap_or(i32::MAX));
        self.as_mut().set_app_count(apps);
        self.as_mut().set_ready(true);
        self.as_mut().set_loading(false);
        // Last: a page asked again by this finds everything else set.
        let revision = self.revision().wrapping_add(1);
        self.as_mut().set_revision(revision);
    }

    pub fn reload(self: Pin<&mut Self>) {
        self.start();
    }

    pub fn category_key(&self, index: i32) -> QString {
        usize::try_from(index)
            .ok()
            .and_then(|i| Category::ALL.get(i))
            .map_or_else(QString::default, |c| QString::from(c.key()))
    }

    pub fn category_count(&self, index: i32) -> i32 {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.rust().counts.get(i).copied())
            .unwrap_or(0)
    }
}

/// One row of a list: the text is plain, made on the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    app_id: String,
    name: String,
    summary: String,
    developer: String,
    /// A `file:` URL or "".
    icon: String,
    verified: bool,
    source: String,
}

/// What a model was asked to list.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Query {
    Search(String),
    Browse {
        /// `None`: every app. `Err`: an unknown key, which lists nothing.
        category: Result<Option<Category>, ()>,
        sort: Sort,
        filter: Filter,
    },
}

fn parse_category(key: &str) -> Result<Option<Category>, ()> {
    if key.is_empty() || key == "all" {
        Ok(None)
    } else {
        Category::from_key(key).map(Some).ok_or(())
    }
}

fn parse_sort(key: &str) -> Sort {
    match key {
        "updated" => Sort::RecentlyUpdated,
        _ => Sort::Name,
    }
}

/// A `file:` URL for an absolute local path: everything but unreserved
/// characters and `/` is percent-encoded.
fn file_url(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut url = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            url.push(char::from(b));
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

/// The icon's `file:` URL, only when the icon is a real file in real folders:
/// a hostile remote's checkout may make `icons/` or the size folder or the
/// file a link (to `/dev/zero`, to the user's pictures). Any doubt: no icon.
pub(crate) fn safe_icon(path: &Path) -> Option<String> {
    const MAX_ICON: u64 = 1024 * 1024;
    if !path.is_absolute() {
        return None;
    }
    let size_dir = path.parent()?;
    let icons_dir = size_dir.parent()?;
    for dir in [icons_dir, size_dir] {
        if !std::fs::symlink_metadata(dir).ok()?.is_dir() {
            return None;
        }
    }
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ICON {
        return None;
    }
    Some(file_url(path))
}

fn row(library: &Library, id: EntryId) -> Row {
    let component = library.component(id);
    let source = library.source(id);
    Row {
        app_id: component.id_bare().to_string(),
        name: component.name.clone(),
        summary: component.summary.clone(),
        developer: component.developer.clone(),
        icon: source
            .icon_path(component, ICON_SIZE)
            .and_then(|p| safe_icon(&p))
            .unwrap_or_default(),
        verified: library.is_verified(id),
        source: atlas_store_core::text::clean(&source.title, 100),
    }
}

fn run(library: &Library, query: &Query) -> Vec<Row> {
    let ids = match query {
        Query::Search(text) => library.search(text, Filter::default(), SEARCH_LIMIT),
        Query::Browse {
            category: Ok(category),
            sort,
            filter,
        } => library.browse(*category, *filter, *sort),
        Query::Browse {
            category: Err(()), ..
        } => Vec::new(),
    };
    ids.into_iter().map(|id| row(library, id)).collect()
}

#[derive(Default)]
pub struct AppListModelRust {
    count: i32,
    busy: bool,
    rows: Vec<Row>,
    /// The number of the latest request; older results are dropped.
    generation: Arc<AtomicU64>,
    /// To the model's worker, started with the first request.
    requests: Option<mpsc::Sender<Request>>,
}

/// One request to a model's worker.
struct Request {
    generation: u64,
    library: Arc<Library>,
    query: Query,
}

/// A model's long-lived worker: waits for a request, skips to the newest one
/// queued behind it, runs it and queues the rows to the GUI thread. Ends when
/// the model (the sender) is gone.
fn query_worker(
    requests: mpsc::Receiver<Request>,
    thread: CxxQtThread<qobject::AppListModel>,
    generation: Arc<AtomicU64>,
) {
    while let Ok(mut request) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            request = newer;
        }
        let this = request.generation;
        if generation.load(Ordering::SeqCst) != this {
            continue;
        }
        let rows = catch_unwind(AssertUnwindSafe(|| run(&request.library, &request.query)))
            .unwrap_or_else(|_| {
                log::error!("a catalog query panicked");
                Vec::new()
            });
        if generation.load(Ordering::SeqCst) != this {
            continue;
        }
        let queued = thread.queue(move |mut model| {
            if model.rust().generation.load(Ordering::SeqCst) != this {
                return;
            }
            model.as_mut().replace(rows);
            model.as_mut().set_busy(false);
        });
        if queued.is_err() {
            // The model is gone.
            return;
        }
    }
}

impl qobject::AppListModel {
    fn ask(mut self: Pin<&mut Self>, query: Query) {
        let generation = Arc::clone(&self.rust().generation);
        let this = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(library) = library() else {
            self.as_mut().replace(Vec::new());
            self.as_mut().set_busy(false);
            return;
        };
        if self.rust().requests.is_none() {
            let (tx, rx) = mpsc::channel();
            let thread = self.qt_thread();
            let spawned = std::thread::Builder::new()
                .name("atlas-store-query".into())
                .spawn(move || query_worker(rx, thread, generation));
            match spawned {
                Ok(_) => self.as_mut().rust_mut().requests = Some(tx),
                Err(e) => {
                    log::error!("could not start the query worker: {e}");
                    self.as_mut().replace(Vec::new());
                    self.as_mut().set_busy(false);
                    return;
                }
            }
        }
        self.as_mut().set_busy(true);
        let request = Request {
            generation: this,
            library,
            query,
        };
        let sent = self
            .rust()
            .requests
            .as_ref()
            .is_some_and(|tx| tx.send(request).is_ok());
        if !sent {
            // The worker ended: start a new one with the next request.
            log::error!("the query worker is gone");
            self.as_mut().rust_mut().requests = None;
            self.as_mut().replace(Vec::new());
            self.as_mut().set_busy(false);
        }
    }

    fn replace(mut self: Pin<&mut Self>, rows: Vec<Row>) {
        let count = i32::try_from(rows.len()).unwrap_or(i32::MAX);
        self.as_mut().begin_reset_model();
        self.as_mut().rust_mut().rows = rows;
        self.as_mut().end_reset_model();
        if count != *self.count() {
            self.as_mut().set_count(count);
        }
    }

    pub fn search(self: Pin<&mut Self>, text: &QString) {
        self.ask(Query::Search(text.to_string()));
    }

    pub fn browse(
        self: Pin<&mut Self>,
        category: &QString,
        sort: &QString,
        verified_only: bool,
        free_only: bool,
    ) {
        self.ask(Query::Browse {
            category: parse_category(&category.to_string()),
            sort: parse_sort(&sort.to_string()),
            filter: Filter {
                verified_only,
                free_only,
            },
        });
    }

    pub fn clear(mut self: Pin<&mut Self>) {
        // Anything still running is out of date.
        self.rust().generation.fetch_add(1, Ordering::SeqCst);
        self.as_mut().replace(Vec::new());
        self.as_mut().set_busy(false);
    }

    pub fn data(&self, index: &QModelIndex, role: i32) -> QVariant {
        let Some(row) = usize::try_from(index.row())
            .ok()
            .and_then(|i| self.rust().rows.get(i))
        else {
            return QVariant::default();
        };
        let text = |s: &str| QVariant::from(&QString::from(s));
        match role - FIRST_ROLE {
            0 => text(&row.app_id),
            1 => text(&row.name),
            2 => text(&row.summary),
            3 => text(&row.developer),
            4 => text(&row.icon),
            5 => QVariant::from(&row.verified),
            6 => text(&row.source),
            _ => QVariant::default(),
        }
    }

    pub fn role_names(&self) -> QHash<QHashPair_i32_QByteArray> {
        let mut roles = QHash::default();
        for (i, name) in (0..).zip(ROLES) {
            roles.insert(FIRST_ROLE + i, QByteArray::from(name));
        }
        roles
    }

    pub fn row_count(&self, parent: &QModelIndex) -> i32 {
        if parent.is_valid() {
            0
        } else {
            i32::try_from(self.rust().rows.len()).unwrap_or(i32::MAX)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn file_urls_are_encoded() {
        assert_eq!(
            file_url(Path::new("/a b/c#d%/é.png")),
            "file:///a%20b/c%23d%25/%C3%A9.png"
        );
    }

    #[test]
    fn icons_that_are_links_are_refused() {
        use std::os::unix::fs::symlink;
        let base =
            std::env::temp_dir().join(format!("atlas-store-icon-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("icons/64x64");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.png"), b"x").unwrap();
        symlink("/dev/zero", real.join("zero.png")).unwrap();
        std::fs::create_dir_all(base.join("other")).unwrap();
        std::fs::write(base.join("other/b.png"), b"x").unwrap();
        symlink(base.join("other"), base.join("icons/128x128")).unwrap();
        assert!(safe_icon(&real.join("a.png")).is_some());
        assert!(safe_icon(&real.join("zero.png")).is_none());
        assert!(safe_icon(&real.join("missing.png")).is_none());
        assert!(safe_icon(&base.join("icons/128x128/b.png")).is_none());
        std::fs::write(real.join("big.png"), vec![0u8; 1024 * 1024 + 1]).unwrap();
        assert!(safe_icon(&real.join("big.png")).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unknown_category_is_not_every_app() {
        assert_eq!(parse_category(""), Ok(None));
        assert_eq!(parse_category("nonsense"), Err(()));
    }

    #[test]
    fn sort_keys() {
        assert_eq!(parse_sort("updated"), Sort::RecentlyUpdated);
        assert_eq!(parse_sort("anything"), Sort::Name);
    }
}
