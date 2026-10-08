//! Flathub's curated lists for Home and the category pages: Popular Apps, New
//! & Updated, Editor's Picks, and "Popular in <Category>".
//!
//! What is listed comes from `telamon_store_core::flathub` (which IDs, cached
//! and checked); what is shown is the local catalog's own name, summary,
//! developer and icon for each ID it has. Nothing here shows API text.
//!
//! Threading: one worker thread, started by the first request, handles the
//! requests in turn (so one refresh runs at a time). A request reads the
//! cached list first and publishes it, however old, then fetches a new one
//! only when the cached one has expired, the Store's retry gate allows it and
//! a page asked: pages ask when they are shown (`ensureHome`,
//! `ensureCategory`), and again when the catalog is replaced. There is no
//! timer and no retry loop; a failed fetch is tried again only when a page
//! asks after ten minutes. Results come back as JSON in properties with
//! `qt_thread().queue`; the GUI thread only swaps strings.

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
        /// Popular Apps: a JSON array of `{appId, name, summary, developer,
        /// iconSource, verified}`, only apps the local catalog has; "[]" when
        /// there are none.
        #[qproperty(QString, popular_json, cxx_name = "popularJson")]
        /// New & Updated, as above.
        #[qproperty(QString, new_json, cxx_name = "newJson")]
        /// Editor's Picks, as above.
        #[qproperty(QString, picks_json, cxx_name = "picksJson")]
        /// Popular in the category last asked for with `ensureCategory`:
        /// `{"key": <the category's key>, "apps": [as above]}`; "" before any.
        #[qproperty(QString, category_json, cxx_name = "categoryJson")]
        #[namespace = "telamon_store"]
        type Featured = super::FeaturedRust;
    }

    unsafe extern "RustQt" {
        /// Starts the object. Once.
        #[qinvokable]
        fn start(self: Pin<&mut Featured>);

        /// Shows the cached Home lists at once and fetches the ones that have
        /// expired. Call when Home is shown and the catalog has apps; asking
        /// again is cheap.
        #[qinvokable]
        #[cxx_name = "ensureHome"]
        fn ensure_home(self: Pin<&mut Featured>);

        /// The same for "Popular in <Category>" of the category with this key
        /// (see `Catalog.categoryKey`); an unknown key does nothing.
        #[qinvokable]
        #[cxx_name = "ensureCategory"]
        fn ensure_category(self: Pin<&mut Featured>, key: &QString);
    }

    impl cxx_qt::Threading for Featured {}

    #[namespace = "rust::cxxqtlib1"]
    unsafe extern "C++" {
        include!("cxx-qt-lib/common.h");

        #[cxx_name = "make_unique"]
        fn featured_make_unique() -> UniquePtr<Featured>;
    }
}

use core::pin::Pin;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use telamon_store_core::catalog::{Category, EntryId, Library};
use telamon_store_core::flathub::{self, Fetch, List, RetryGate};

/// Apps on each Home shelf.
const POPULAR_SHOWN: usize = 12;
const NEW_SHOWN: usize = 12;
const PICKS_SHOWN: usize = 8;
/// Apps on a category's shelf.
const CATEGORY_SHOWN: usize = 10;

/// What a page asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    Home,
    Category(Category),
}

impl Job {
    fn lists(self) -> Vec<List> {
        match self {
            Job::Home => vec![List::Popular, List::RecentlyUpdated, List::Picks],
            Job::Category(c) => vec![List::Category(c)],
        }
    }
}

/// The requests to do now out of those that piled up: Home once, and only the
/// newest category (the user has moved on from the others).
fn coalesce(jobs: Vec<Job>) -> Vec<Job> {
    let mut out = Vec::new();
    if jobs.contains(&Job::Home) {
        out.push(Job::Home);
    }
    if let Some(category) = jobs.iter().rev().find(|j| matches!(j, Job::Category(_))) {
        out.push(*category);
    }
    out
}

/// The JSON of apps, in the order given: the catalog's own text.
fn apps_json(library: &Library, entries: &[EntryId]) -> serde_json::Value {
    let apps = entries.iter().map(|&entry| {
        let component = library.component(entry);
        let icon = library
            .source(entry)
            .icon_path(component, crate::catalog::ICON_SIZE)
            .and_then(|p| crate::catalog::safe_icon(&p))
            .unwrap_or_default();
        serde_json::json!({
            "appId": component.id_bare(),
            "name": component.name,
            "summary": component.summary,
            "developer": component.developer,
            "iconSource": icon,
            "verified": library.is_verified(entry),
        })
    });
    serde_json::Value::Array(apps.collect())
}

/// What a list shows, as JSON text, for a library.
fn list_json(library: &Library, list: List, ids: &[String]) -> String {
    let (category, limit) = match list {
        List::Popular => (None, POPULAR_SHOWN),
        List::RecentlyUpdated => (None, NEW_SHOWN),
        List::Picks => (None, PICKS_SHOWN),
        List::Category(c) => (Some(c), CATEGORY_SHOWN),
    };
    let found = flathub::resolve(library, ids, category, limit);
    let apps = apps_json(library, &found);
    match list {
        List::Category(c) => serde_json::json!({ "key": c.key(), "apps": apps }).to_string(),
        _ => apps.to_string(),
    }
}

/// The worker's memory: where the cache is, which fetches failed, the IDs of
/// each list and what was last published for it.
struct Worker {
    /// `None` when there is no cache folder (no HOME): nothing is fetched.
    dir: Option<PathBuf>,
    gate: RetryGate,
    ids: HashMap<List, Vec<String>>,
    published: HashMap<List, String>,
}

impl Worker {
    fn new(dir: Option<PathBuf>) -> Worker {
        Worker {
            dir,
            gate: RetryGate::default(),
            ids: HashMap::new(),
            published: HashMap::new(),
        }
    }

    /// Hands `emit` the JSON of `list` for `library`, if there is a library
    /// and it differs from what was last handed over. False when `emit` says
    /// the GUI object is gone.
    fn publish(
        &mut self,
        list: List,
        library: Option<&Library>,
        emit: &mut dyn FnMut(List, String) -> bool,
    ) -> bool {
        // No library yet: the IDs wait here until a page asks again.
        let (Some(library), Some(ids)) = (library, self.ids.get(&list)) else {
            return true;
        };
        let json = list_json(library, list, ids);
        if self.published.get(&list) == Some(&json) {
            return true;
        }
        self.published.insert(list, json.clone());
        emit(list, json)
    }

    /// One list: the cached copy is published first, whatever its age; a new
    /// one is fetched only when the cached one has expired (or there is none)
    /// and the gate allows it.
    fn load(
        &mut self,
        list: List,
        now: u64,
        fetch: Fetch<'_>,
        library: Option<&Library>,
        emit: &mut dyn FnMut(List, String) -> bool,
    ) -> bool {
        let Some(dir) = self.dir.clone() else {
            return true;
        };
        let mut fresh = false;
        if let Some(cached) = flathub::read_cache(&dir, list) {
            fresh = cached.is_fresh(list, now);
            self.ids.insert(list, cached.ids);
            if !self.publish(list, library, emit) {
                return false;
            }
        }
        if fresh || !self.gate.allowed(list, now) {
            return true;
        }
        match flathub::refresh(&dir, list, now, fetch) {
            Ok(ids) => {
                self.gate.succeeded(list);
                self.ids.insert(list, ids);
                self.publish(list, library, emit)
            }
            Err(e) => {
                // Not an error to show: the lists are a bonus. The cached
                // copy (if any) stays on the screen.
                log::info!("could not update the {} list: {e}", list.name());
                self.gate.failed(list, &e, now);
                true
            }
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The worker thread: runs the requests in turn until the object is gone.
fn work(requests: mpsc::Receiver<Job>, thread: CxxQtThread<qobject::Featured>) {
    let mut worker = Worker::new(crate::catalog::cache_dir().map(|d| flathub::cache_dir(&d)));
    while let Ok(first) = requests.recv() {
        let mut jobs = vec![first];
        while let Ok(more) = requests.try_recv() {
            jobs.push(more);
        }
        for job in coalesce(jobs) {
            let alive = catch_unwind(AssertUnwindSafe(|| {
                let mut alive = true;
                for list in job.lists() {
                    let library = crate::catalog::library();
                    let mut emit = |list: List, json: String| {
                        thread
                            .queue(move |featured| featured.apply(list, &json))
                            .is_ok()
                    };
                    alive = worker.load(
                        list,
                        now_secs(),
                        &flathub::http_fetch,
                        library.as_deref(),
                        &mut emit,
                    );
                    if !alive {
                        break;
                    }
                }
                alive
            }))
            .unwrap_or_else(|_| {
                log::error!("the featured lists worker panicked");
                true
            });
            if !alive {
                return;
            }
        }
    }
}

#[derive(Default)]
pub struct FeaturedRust {
    ready: bool,
    popular_json: QString,
    new_json: QString,
    picks_json: QString,
    category_json: QString,
    /// To the worker, started with the first request.
    requests: Option<mpsc::Sender<Job>>,
}

impl qobject::Featured {
    pub fn start(mut self: Pin<&mut Self>) {
        if !*self.ready() {
            self.as_mut().set_ready(true);
        }
    }

    pub fn ensure_home(self: Pin<&mut Self>) {
        self.ask(Job::Home);
    }

    pub fn ensure_category(self: Pin<&mut Self>, key: &QString) {
        if let Some(category) = Category::from_key(&key.to_string()) {
            self.ask(Job::Category(category));
        }
    }

    fn ask(mut self: Pin<&mut Self>, job: Job) {
        if self.rust().requests.is_none() {
            let (tx, rx) = mpsc::channel();
            let thread = self.qt_thread();
            let spawned = std::thread::Builder::new()
                .name("telamon-store-featured".into())
                .spawn(move || work(rx, thread));
            match spawned {
                Ok(_) => self.as_mut().rust_mut().requests = Some(tx),
                Err(e) => {
                    log::error!("could not start the featured lists worker: {e}");
                    return;
                }
            }
        }
        let sent = self
            .rust()
            .requests
            .as_ref()
            .is_some_and(|tx| tx.send(job).is_ok());
        if !sent {
            // The worker ended: start a new one with the next request.
            self.as_mut().rust_mut().requests = None;
        }
    }

    /// On the GUI thread: a list's new JSON.
    fn apply(mut self: Pin<&mut Self>, list: List, json: &str) {
        let json = QString::from(json);
        match list {
            List::Popular => self.as_mut().set_popular_json(json),
            List::RecentlyUpdated => self.as_mut().set_new_json(json),
            List::Picks => self.as_mut().set_picks_json(json),
            List::Category(_) => self.as_mut().set_category_json(json),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use telamon_store_core::appstream::{ParseOptions, parse};
    use telamon_store_core::catalog::CatalogSource;
    use telamon_store_core::flatpak::Scope;
    use telamon_store_core::net::NetError;

    const ALPHA: &str =
        include_str!("../../../crates/telamon-store-core/tests/fixtures/catalog-alpha.xml");
    const NOW: u64 = 1_791_331_200;

    fn library() -> Library {
        let opts = ParseOptions {
            origin: "alpha".into(),
            ..ParseOptions::default()
        };
        let catalog = parse(ALPHA.as_bytes(), &opts).unwrap();
        let source = CatalogSource {
            scope: Scope::User,
            remote: "alpha".into(),
            title: "alpha".into(),
            priority: 0,
            dir: None,
            commit: None,
            updated: None,
        };
        Library::new(vec![(source, catalog)])
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "telamon-store-featured-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.join("telamon-store").join("flathub")
    }

    fn answer(ids: &[&str]) -> Vec<u8> {
        let hits: Vec<String> = ids
            .iter()
            .map(|i| format!(r#"{{"app_id":"{i}"}}"#))
            .collect();
        format!(r#"{{"hits":[{}]}}"#, hits.join(",")).into_bytes()
    }

    #[test]
    fn pending_requests_are_merged() {
        let net = Category::Network;
        let games = Category::Games;
        assert_eq!(coalesce(vec![Job::Home, Job::Home]), [Job::Home]);
        assert_eq!(
            coalesce(vec![Job::Category(net), Job::Home, Job::Category(games)]),
            [Job::Home, Job::Category(games)]
        );
        assert_eq!(
            coalesce(vec![Job::Category(net), Job::Category(net)]),
            [Job::Category(net)]
        );
    }

    #[test]
    fn a_shelf_shows_the_catalogs_text_and_only_its_apps() {
        let lib = library();
        let ids = vec![
            "org.not.Here".to_string(),
            "org.example.Browser".to_string(),
            "org.example.Pictures".to_string(),
        ];
        let json = list_json(&lib, List::Popular, &ids);
        let apps: serde_json::Value = serde_json::from_str(&json).unwrap();
        let apps = apps.as_array().unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0]["appId"], "org.example.Browser");
        assert_eq!(
            apps[0]["name"],
            lib.component(lib.find("org.example.Browser").unwrap()).name
        );
        for key in [
            "appId",
            "name",
            "summary",
            "developer",
            "iconSource",
            "verified",
        ] {
            assert!(apps[0].get(key).is_some(), "{key}");
        }
        // A category shelf says which category it is for.
        let json = list_json(&lib, List::Category(Category::Graphics), &ids);
        let shelf: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(shelf["key"], "graphics");
        assert_eq!(shelf["apps"].as_array().unwrap().len(), 1);
        assert_eq!(shelf["apps"][0]["appId"], "org.example.Pictures");
        // Nothing in common with the catalog: an empty array, not an error.
        assert_eq!(
            list_json(&lib, List::Popular, &["org.not.Here".to_string()]),
            "[]"
        );
    }

    #[test]
    fn the_worker_shows_the_cache_then_fetches_only_what_expired() {
        let dir = scratch("flow");
        let lib = library();
        let mut worker = Worker::new(Some(dir.clone()));
        let calls = Cell::new(0);
        let online = Cell::new(true);
        let fetch = |_: &str| {
            calls.set(calls.get() + 1);
            if online.get() {
                Ok(answer(&["org.example.Browser", "org.example.Pictures"]))
            } else {
                Err(NetError::Failed("no route".into()))
            }
        };
        let emitted: RefCell<Vec<(List, String)>> = RefCell::new(Vec::new());
        let mut emit = |l: List, j: String| {
            emitted.borrow_mut().push((l, j));
            true
        };

        // Nothing cached, online: fetched once and published.
        assert!(worker.load(List::Popular, NOW, &fetch, Some(&lib), &mut emit));
        assert_eq!(calls.get(), 1);
        assert_eq!(emitted.borrow().len(), 1);

        // Fresh: not fetched, and the same JSON is not published twice.
        assert!(worker.load(List::Popular, NOW + 60, &fetch, Some(&lib), &mut emit));
        assert_eq!(calls.get(), 1);
        assert_eq!(emitted.borrow().len(), 1);

        // A new worker (the Store was restarted), expired cache, offline: the
        // old list is published and one fetch fails.
        let mut worker = Worker::new(Some(dir.clone()));
        online.set(false);
        let later = NOW + 3 * 24 * 3600;
        assert!(worker.load(List::Popular, later, &fetch, Some(&lib), &mut emit));
        assert_eq!(calls.get(), 2);
        assert_eq!(emitted.borrow().len(), 2);
        assert_eq!(emitted.borrow()[1].1, emitted.borrow()[0].1);

        // Asked again at once and after 9 minutes: no new attempt. After 10
        // minutes: one, and it works.
        assert!(worker.load(List::Popular, later + 60, &fetch, Some(&lib), &mut emit));
        assert!(worker.load(List::Popular, later + 539, &fetch, Some(&lib), &mut emit));
        assert_eq!(calls.get(), 2);
        online.set(true);
        assert!(worker.load(List::Popular, later + 600, &fetch, Some(&lib), &mut emit));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn without_a_library_the_ids_wait_for_one() {
        let dir = scratch("nolib");
        let mut worker = Worker::new(Some(dir));
        let fetch = |_: &str| Ok(answer(&["org.example.Browser"]));
        let emitted: RefCell<Vec<(List, String)>> = RefCell::new(Vec::new());
        let mut emit = |l: List, j: String| {
            emitted.borrow_mut().push((l, j));
            true
        };
        assert!(worker.load(List::Popular, NOW, &fetch, None, &mut emit));
        assert!(emitted.borrow().is_empty());
        // The catalog arrived: the next request publishes from the cache.
        let lib = library();
        assert!(worker.load(List::Popular, NOW + 1, &fetch, Some(&lib), &mut emit));
        assert_eq!(emitted.borrow().len(), 1);
        assert!(emitted.borrow()[0].1.contains("org.example.Browser"));
    }

    #[test]
    fn with_no_cache_and_no_network_nothing_is_published() {
        let dir = scratch("nonet");
        let lib = library();
        let mut worker = Worker::new(Some(dir));
        let fetch = |_: &str| Err(NetError::Failed("offline".into()));
        let emitted: RefCell<Vec<(List, String)>> = RefCell::new(Vec::new());
        let mut emit = |l: List, j: String| {
            emitted.borrow_mut().push((l, j));
            true
        };
        for list in [List::Popular, List::RecentlyUpdated, List::Picks] {
            assert!(worker.load(list, NOW, &fetch, Some(&lib), &mut emit));
        }
        assert!(emitted.borrow().is_empty());
        // And with no cache folder at all, nothing is even asked for.
        let mut worker = Worker::new(None);
        let never = |_: &str| -> Result<Vec<u8>, NetError> { panic!("fetched without a cache") };
        assert!(worker.load(List::Popular, NOW, &never, Some(&lib), &mut emit));
    }

    #[test]
    fn a_gone_gui_object_ends_the_worker() {
        let dir = scratch("gone");
        let lib = library();
        let mut worker = Worker::new(Some(dir));
        let fetch = |_: &str| Ok(answer(&["org.example.Browser"]));
        let mut emit = |_: List, _: String| false;
        assert!(!worker.load(List::Popular, NOW, &fetch, Some(&lib), &mut emit));
    }
}
