//! Flathub's curated lists for Home and the category pages: Popular Apps, New
//! & Updated, Editor's Picks, and "Popular in <Category>".
//!
//! What is listed comes from `telamon_store_core::flathub` (which IDs, cached
//! and checked); what is shown is the local catalog's own name, summary,
//! developer and icon for each ID it has. Nothing here shows API text.
//!
//! Threading: one worker thread, started by the first request, owns the
//! state. A request (`ensureHome`, `ensureCategory`, made when a page is shown
//! and again when the catalog is replaced) is answered at once from the cache,
//! whatever its age; a list that has expired is then fetched, one at a time,
//! by a thread of its own, so a slow server never holds back what is cached.
//! A failed fetch is not tried again for ten minutes, and only when a page
//! asks: there is no timer and no retry loop. Results come back as JSON in
//! properties with `qt_thread().queue`; the GUI thread only swaps strings.

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
use std::collections::{HashMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use cxx_qt::{CxxQtThread, CxxQtType, Threading};
use cxx_qt_lib::QString;
use telamon_store_core::catalog::{Category, EntryId, Library};
use telamon_store_core::flathub::{self, List, RetryGate};

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

/// What the worker is told.
enum Msg {
    /// A page asked for its lists.
    Ask(Job),
    /// A fetch the worker started has ended.
    Fetched(List, Result<Vec<String>, flathub::RefreshError>),
}

/// The worker's memory: where the cache is, which fetches failed, the IDs of
/// each list, what was last published for it and which fetches are to come.
///
/// It never waits for the network: a request is answered from the cache at
/// once and the fetches it needs are done one at a time by a thread of their
/// own, whose end comes back as [`Msg::Fetched`]. So a slow server delays
/// the new list, never the cached one.
struct Worker {
    /// `None` when there is no cache folder (no HOME): nothing is fetched.
    dir: Option<PathBuf>,
    gate: RetryGate,
    ids: HashMap<List, Vec<String>>,
    published: HashMap<List, String>,
    /// Lists to fetch, the next first.
    wanted: VecDeque<List>,
    /// The list being fetched.
    busy: Option<List>,
}

type Emit<'a> = &'a mut dyn FnMut(List, String) -> bool;

impl Worker {
    fn new(dir: Option<PathBuf>) -> Worker {
        Worker {
            dir,
            gate: RetryGate::default(),
            ids: HashMap::new(),
            published: HashMap::new(),
            wanted: VecDeque::new(),
            busy: None,
        }
    }

    /// Hands `emit` the JSON of `list` for `library`, if there is a library
    /// and it differs from what was last handed over. False when `emit` says
    /// the GUI object is gone.
    fn publish(&mut self, list: List, library: Option<&Library>, emit: Emit<'_>) -> bool {
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

    /// A page asked: the cached copy of each of its lists is published first,
    /// whatever its age, and a list that has expired (or has no copy) is put
    /// on the list of fetches unless the retry gate holds it back. A category
    /// goes before the Home lists: the user is looking at it.
    fn ask(&mut self, job: Job, now: u64, library: Option<&Library>, emit: Emit<'_>) -> bool {
        let Some(dir) = self.dir.clone() else {
            return true;
        };
        for list in job.lists() {
            let mut fresh = false;
            if let Some(cached) = flathub::read_cache(&dir, list) {
                fresh = cached.is_fresh(list, now);
                self.ids.insert(list, cached.ids);
                if !self.publish(list, library, emit) {
                    return false;
                }
            }
            let queued = self.busy == Some(list) || self.wanted.contains(&list);
            if !fresh && !queued && self.gate.allowed(list, now) {
                match job {
                    Job::Home => self.wanted.push_back(list),
                    Job::Category(_) => self.wanted.push_front(list),
                }
            }
        }
        true
    }

    /// The next list to fetch, if none is being fetched; skips the ones the
    /// retry gate has meanwhile closed. The caller fetches it
    /// ([`flathub::refresh`]) and reports with [`Worker::fetched`].
    fn next_fetch(&mut self, now: u64) -> Option<(PathBuf, List)> {
        if self.busy.is_some() {
            return None;
        }
        let dir = self.dir.clone()?;
        while let Some(list) = self.wanted.pop_front() {
            if self.gate.allowed(list, now) {
                self.busy = Some(list);
                return Some((dir, list));
            }
        }
        None
    }

    /// A fetch ended: a good list is published; a failed one is logged, held
    /// back for ten minutes and leaves the cached copy (if any) as it is.
    fn fetched(
        &mut self,
        list: List,
        result: Result<Vec<String>, flathub::RefreshError>,
        now: u64,
        library: Option<&Library>,
        emit: Emit<'_>,
    ) -> bool {
        self.busy = None;
        match result {
            Ok(ids) => {
                self.gate.succeeded(list);
                self.ids.insert(list, ids);
                self.publish(list, library, emit)
            }
            Err(e) => {
                // Not an error to show: the lists are a bonus.
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

/// The worker thread: handles what pages ask and what the fetches report,
/// until the object is gone.
fn work(
    messages: mpsc::Receiver<Msg>,
    sender: mpsc::Sender<Msg>,
    thread: CxxQtThread<qobject::Featured>,
) {
    let mut worker = Worker::new(crate::catalog::cache_dir().map(|d| flathub::cache_dir(&d)));
    let mut emit = |list: List, json: String| {
        thread
            .queue(move |featured| featured.apply(list, &json))
            .is_ok()
    };
    while let Ok(message) = messages.recv() {
        let library = crate::catalog::library();
        let now = now_secs();
        let alive = catch_unwind(AssertUnwindSafe(|| match message {
            Msg::Ask(job) => worker.ask(job, now, library.as_deref(), &mut emit),
            Msg::Fetched(list, result) => {
                worker.fetched(list, result, now, library.as_deref(), &mut emit)
            }
        }))
        .unwrap_or_else(|_| {
            log::error!("the featured lists worker panicked");
            true
        });
        if !alive {
            return;
        }
        // At most one fetch runs, on a thread of its own.
        if let Some((dir, list)) = worker.next_fetch(now) {
            let sender = sender.clone();
            let spawned = std::thread::Builder::new()
                .name("telamon-store-flathub".into())
                .spawn(move || {
                    let result = flathub::refresh(&dir, list, now_secs(), &flathub::http_fetch);
                    let _ = sender.send(Msg::Fetched(list, result));
                });
            if let Err(e) = spawned {
                log::error!("could not start a fetch of the featured lists: {e}");
                worker.busy = None;
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
    requests: Option<mpsc::Sender<Msg>>,
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
            let own = tx.clone();
            let spawned = std::thread::Builder::new()
                .name("telamon-store-featured".into())
                .spawn(move || work(rx, own, thread));
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
            .is_some_and(|tx| tx.send(Msg::Ask(job)).is_ok());
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
    const DAY: u64 = 24 * 3600;

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

    /// The API's answer to `url` listing `ids`.
    fn answer(url: &str, ids: &[&str]) -> Vec<u8> {
        if url.contains("/app-picks/") {
            let apps: Vec<String> = ids
                .iter()
                .enumerate()
                .map(|(n, i)| format!(r#"{{"app_id":"{i}","position":{n}}}"#))
                .collect();
            return format!(r#"{{"apps":[{}]}}"#, apps.join(",")).into_bytes();
        }
        let hits: Vec<String> = ids
            .iter()
            .map(|i| format!(r#"{{"app_id":"{i}"}}"#))
            .collect();
        format!(r#"{{"hits":[{}]}}"#, hits.join(",")).into_bytes()
    }

    type Published = RefCell<Vec<(List, String)>>;

    /// What the worker thread does: asks, then fetches what the worker wants
    /// one by one with `fetch` (the fake network) and reports each.
    fn drive(
        worker: &mut Worker,
        job: Job,
        now: u64,
        library: Option<&Library>,
        fetch: &dyn Fn(&str) -> Result<Vec<u8>, NetError>,
        published: &Published,
    ) {
        let mut emit = |l: List, j: String| {
            published.borrow_mut().push((l, j));
            true
        };
        assert!(worker.ask(job, now, library, &mut emit));
        while let Some((dir, list)) = worker.next_fetch(now) {
            let result = flathub::refresh(&dir, list, now, fetch);
            assert!(worker.fetched(list, result, now, library, &mut emit));
        }
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
        let calls = Cell::new(0);
        let online = Cell::new(true);
        let fetch = |url: &str| {
            calls.set(calls.get() + 1);
            if online.get() {
                Ok(answer(
                    url,
                    &["org.example.Browser", "org.example.Pictures"],
                ))
            } else {
                Err(NetError::Failed("no route".into()))
            }
        };
        let published: Published = RefCell::new(Vec::new());

        // Nothing cached, online: the three Home lists are fetched, each
        // published once.
        let mut worker = Worker::new(Some(dir.clone()));
        drive(&mut worker, Job::Home, NOW, Some(&lib), &fetch, &published);
        assert_eq!(calls.get(), 3);
        assert_eq!(published.borrow().len(), 3);

        // Fresh: nothing is fetched, and the same JSON is not published twice.
        drive(
            &mut worker,
            Job::Home,
            NOW + 60,
            Some(&lib),
            &fetch,
            &published,
        );
        assert_eq!(calls.get(), 3);
        assert_eq!(published.borrow().len(), 3);

        // The Store was restarted, the lists have expired and the network is
        // down: the old lists are published and the first fetch fails; the
        // others are not tried.
        let mut worker = Worker::new(Some(dir.clone()));
        online.set(false);
        let later = NOW + 3 * DAY;
        published.borrow_mut().clear();
        drive(
            &mut worker,
            Job::Home,
            later,
            Some(&lib),
            &fetch,
            &published,
        );
        assert_eq!(calls.get(), 4);
        assert_eq!(published.borrow().len(), 3);
        assert!(
            published
                .borrow()
                .iter()
                .all(|(_, j)| j.contains("Browser"))
        );

        // Asked again at once and after 9 minutes: no new attempt. After 10
        // minutes: the network is back and all three work.
        drive(
            &mut worker,
            Job::Home,
            later + 60,
            Some(&lib),
            &fetch,
            &published,
        );
        drive(
            &mut worker,
            Job::Home,
            later + 539,
            Some(&lib),
            &fetch,
            &published,
        );
        assert_eq!(calls.get(), 4);
        online.set(true);
        drive(
            &mut worker,
            Job::Home,
            later + 600,
            Some(&lib),
            &fetch,
            &published,
        );
        assert_eq!(calls.get(), 7);
    }

    #[test]
    fn a_category_does_not_wait_for_the_home_lists() {
        let dir = scratch("order");
        let lib = library();
        let mut worker = Worker::new(Some(dir.clone()));
        let mut emit = |_: List, _: String| true;
        // Home asked, its first fetch is running...
        assert!(worker.ask(Job::Home, NOW, Some(&lib), &mut emit));
        let first = worker.next_fetch(NOW).unwrap().1;
        assert_eq!(first, List::Popular);
        assert!(worker.next_fetch(NOW).is_none(), "one fetch at a time");
        // ... a category is asked: its cached list needs no fetch to show,
        // and its fetch goes before the rest of Home's.
        let games = List::Category(Category::Games);
        assert!(worker.ask(Job::Category(Category::Games), NOW, Some(&lib), &mut emit));
        assert!(worker.ask(Job::Category(Category::Games), NOW, Some(&lib), &mut emit));
        let result = Err(flathub::RefreshError::Fetch(NetError::Status(404)));
        assert!(worker.fetched(first, result, NOW, Some(&lib), &mut emit));
        assert_eq!(worker.next_fetch(NOW).unwrap().1, games);
        assert!(worker.fetched(
            games,
            Ok(vec!["org.example.GameOne".into()]),
            NOW,
            Some(&lib),
            &mut emit
        ));
        // Asked twice, fetched once; then Home's other lists.
        assert_eq!(worker.next_fetch(NOW).unwrap().1, List::RecentlyUpdated);
    }

    #[test]
    fn without_a_library_the_ids_wait_for_one() {
        let dir = scratch("nolib");
        let mut worker = Worker::new(Some(dir));
        let fetch = |url: &str| Ok(answer(url, &["org.example.Browser"]));
        let published: Published = RefCell::new(Vec::new());
        drive(&mut worker, Job::Home, NOW, None, &fetch, &published);
        assert!(published.borrow().is_empty());
        // The catalog arrived: the next request publishes from the cache.
        let lib = library();
        drive(
            &mut worker,
            Job::Home,
            NOW + 1,
            Some(&lib),
            &fetch,
            &published,
        );
        assert_eq!(published.borrow().len(), 3);
        assert!(published.borrow()[0].1.contains("org.example.Browser"));
    }

    #[test]
    fn with_no_cache_and_no_network_nothing_is_published() {
        let dir = scratch("nonet");
        let lib = library();
        let mut worker = Worker::new(Some(dir));
        let fetch = |_: &str| Err(NetError::Failed("offline".into()));
        let published: Published = RefCell::new(Vec::new());
        drive(&mut worker, Job::Home, NOW, Some(&lib), &fetch, &published);
        assert!(published.borrow().is_empty());
        // And with no cache folder at all, nothing is even asked for.
        let mut worker = Worker::new(None);
        let never = |_: &str| -> Result<Vec<u8>, NetError> { panic!("fetched without a cache") };
        drive(&mut worker, Job::Home, NOW, Some(&lib), &never, &published);
        assert!(published.borrow().is_empty());
    }

    #[test]
    fn a_gone_gui_object_ends_the_worker() {
        let dir = scratch("gone");
        let lib = library();
        let mut worker = Worker::new(Some(dir.clone()));
        let mut emit = |_: List, _: String| false;
        flathub::write_cache(
            &dir,
            List::Popular,
            &["org.example.Browser".to_string()],
            NOW,
        )
        .unwrap();
        assert!(!worker.ask(Job::Home, NOW, Some(&lib), &mut emit));
    }
}
