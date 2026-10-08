//! Flathub's curated lists for Home and the category pages: which apps are
//! popular, recently updated or picked by the editors. Only app IDs are taken
//! from the Flathub API; what the Store shows about an app (name, summary,
//! icon, developer) is the local AppStream catalog's, so nothing the API says
//! is ever displayed, and an ID the catalog doesn't have is never shown.
//!
//! Rules, from docs/DESIGN.md (Trust):
//!
//! - the answer is untrusted: a size cap ([`MAX_BODY`]), a schema checked with
//!   serde into the few fields used (unknown fields are ignored, the typed
//!   ones must have their type), every ID checked with
//!   [`crate::text::valid_id`], at most [`MAX_IDS`] kept per list;
//! - the request carries nothing about the user: no cookies, no IDs, the
//!   User-Agent of [`crate::net`] (the Store and its version) and the URL's
//!   own path and query, which only name a list;
//! - the answer is kept as a cache file (IDs and the time fetched), under the
//!   Store's cache folder, written atomically and never through a link, read
//!   back as untrusted input again; an expired file is still used when the
//!   network fails, so the lists work offline;
//! - a failed fetch is not retried before [`RETRY_AFTER`] ([`RetryGate`]).
//!
//! Everything here blocks (the network, the disk): run it on a worker thread.
//! The clock and the HTTP client are parameters, so the tests use neither.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::catalog::{Category, EntryId, Library};
use crate::net::{self, NetError, Request};
use crate::text::valid_id;

/// The Flathub API.
pub const API: &str = "https://flathub.org/api/v2";
/// The most bytes of an answer taken (the answers asked for are under
/// 100 KB).
pub const MAX_BODY: u64 = 1024 * 1024;
/// The whole request, connecting and reading.
pub const TIMEOUT: Duration = Duration::from_secs(15);
/// The most IDs kept of one list.
pub const MAX_IDS: usize = 64;
/// A list that failed to load is not asked for again before this.
pub const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);
/// The most bytes of a cache file read.
const MAX_CACHE_FILE: u64 = 64 * 1024;
/// A cache file may be this far ahead of the clock before it is bogus.
const CLOCK_SLACK: u64 = 300;
/// The layout of the cache files.
const CACHE_VERSION: u32 = 1;
/// The folder of the cache files, inside the Store's cache folder.
pub const CACHE_SUBDIR: &str = "flathub";

/// One list Flathub curates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum List {
    /// Most installed in the last month.
    Popular,
    /// Most recently updated.
    RecentlyUpdated,
    /// The editors' apps of the week.
    Picks,
    /// Most installed in the last month in one category.
    Category(Category),
}

impl List {
    /// A stable name (the cache file's name without `.json`, and stored in it).
    pub fn name(self) -> String {
        match self {
            List::Popular => "popular".into(),
            List::RecentlyUpdated => "recently-updated".into(),
            List::Picks => "picks".into(),
            List::Category(c) => format!("category-{}", c.key()),
        }
    }

    /// How long a fetched list is current: the fast-changing one 6 hours, the
    /// others 24.
    pub fn max_age(self) -> Duration {
        match self {
            List::RecentlyUpdated => Duration::from_secs(6 * 3600),
            _ => Duration::from_secs(24 * 3600),
        }
    }

    /// The request URL at time `now` (seconds since the epoch; only the
    /// editors' picks, which are asked for by date, use it).
    pub fn url(self, now: u64) -> String {
        match self {
            List::Popular => format!("{API}/collection/popular?page=1&per_page=48"),
            List::RecentlyUpdated => {
                format!("{API}/collection/recently-updated?page=1&per_page=24")
            }
            List::Picks => format!("{API}/app-picks/apps-of-the-week/{}", utc_date(now)),
            List::Category(c) => format!(
                "{API}/collection/category/{}?page=1&per_page=40",
                api_category(c)
            ),
        }
    }
}

/// The name Flathub's API gives a category of the Store (its `MainCategory`):
/// the freedesktop main categories in lower case.
pub fn api_category(category: Category) -> &'static str {
    match category {
        Category::AudioVideo => "audiovideo",
        Category::Development => "development",
        Category::Education => "education",
        Category::Games => "game",
        Category::Graphics => "graphics",
        Category::Network => "network",
        Category::Office => "office",
        Category::Science => "science",
        Category::System => "system",
        Category::Utilities => "utility",
    }
}

/// `YYYY-MM-DD` (UTC) of a time in seconds since the epoch.
pub fn utc_date(secs: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm.
    let z = (secs / 86400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

// ---- the answer

/// Why an answer is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooLarge,
    /// Not the JSON shape the API documents. Plain words.
    Schema(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooLarge => f.write_str("The answer is larger than the Store accepts."),
            ParseError::Schema(why) => write!(f, "The answer is not what the Store expects: {why}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// `{"hits": [{"app_id": ...}, ...]}`: the collections and categories.
#[derive(Deserialize)]
struct Hits {
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    app_id: String,
}

/// `{"apps": [{"app_id": ..., "position": 1}, ...]}`: the apps of the week.
#[derive(Deserialize)]
struct Week {
    apps: Vec<Pick>,
}

#[derive(Deserialize)]
struct Pick {
    app_id: String,
    position: i64,
}

/// The app IDs of an answer to [`List::url`], in the order of the list:
/// valid ones only, without repeats, at most [`MAX_IDS`]. A hit with a bad ID
/// is skipped; an answer of another shape (or with an ID that is not text) is
/// an error. An empty list is not an error.
pub fn parse(list: List, body: &[u8]) -> Result<Vec<String>, ParseError> {
    if body.len() as u64 > MAX_BODY {
        return Err(ParseError::TooLarge);
    }
    let schema = |e: serde_json::Error| ParseError::Schema(crate::text::clean(&e.to_string(), 160));
    let raw: Vec<String> = match list {
        List::Picks => {
            let mut week: Week = serde_json::from_slice(body).map_err(schema)?;
            // The editors' order; equal positions keep the order they came in.
            week.apps.sort_by_key(|p| p.position);
            week.apps.into_iter().map(|p| p.app_id).collect()
        }
        _ => {
            let page: Hits = serde_json::from_slice(body).map_err(schema)?;
            page.hits.into_iter().map(|h| h.app_id).collect()
        }
    };
    let mut ids: Vec<String> = Vec::new();
    for id in raw {
        if ids.len() >= MAX_IDS {
            break;
        }
        if valid_id(&id) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

// ---- the cache

/// A list read back from the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    pub ids: Vec<String>,
    /// Seconds since the epoch.
    pub fetched_at: u64,
}

impl Cached {
    /// Whether the list is younger than [`List::max_age`]. A time ahead of
    /// the clock (it was set back) is not young.
    pub fn is_fresh(&self, list: List, now: u64) -> bool {
        self.fetched_at <= now.saturating_add(CLOCK_SLACK)
            && now.saturating_sub(self.fetched_at) < list.max_age().as_secs()
    }
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    v: u32,
    list: String,
    fetched_at: u64,
    ids: Vec<String>,
}

/// The cache folder inside the Store's cache folder `base`.
pub fn cache_dir(base: &Path) -> std::path::PathBuf {
    base.join(CACHE_SUBDIR)
}

/// Reads the cached list. `dir` is [`cache_dir`]. Untrusted input: the folder
/// must be the user's own and not a link, the file a regular file the user
/// owns of at most 64 KiB, opened without following a link, and every field is
/// checked again; anything wrong, or no file, is `None` (the list is fetched).
/// Never changes the disk.
pub fn read_cache(dir: &Path, list: List) -> Option<Cached> {
    crate::appstream::index::check_dir(dir, false).ok()?;
    let path = dir.join(format!("{}.json", list.name()));
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .ok()?;
    let meta = file.metadata().ok()?;
    // SAFETY: geteuid has no preconditions and can't fail.
    let me = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.uid() != me || meta.len() > MAX_CACHE_FILE {
        log::debug!(
            "not using the cached list {}: not a plain file of ours",
            path.display()
        );
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_CACHE_FILE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_CACHE_FILE {
        return None;
    }
    let parsed: CacheFile = serde_json::from_slice(&bytes).ok()?;
    let mut seen = std::collections::HashSet::new();
    let good = parsed.v == CACHE_VERSION
        && parsed.list == list.name()
        && parsed.ids.len() <= MAX_IDS
        && parsed
            .ids
            .iter()
            .all(|id| valid_id(id) && seen.insert(id.as_str()));
    if !good {
        log::debug!(
            "not using the cached list {}: it is not valid",
            path.display()
        );
        return None;
    }
    Some(Cached {
        ids: parsed.ids,
        fetched_at: parsed.fetched_at,
    })
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes the list to the cache: the folders are created 0700 (an existing one
/// must be the user's own and not a link; one that group or others can write
/// to is set to 0700), then a temp file made exclusively without following a
/// link (0600), synced and renamed into place.
pub fn write_cache(dir: &Path, list: List, ids: &[String], now: u64) -> io::Result<()> {
    if dir.as_os_str().is_empty() || dir == Path::new(".") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the cache directory is empty",
        ));
    }
    let bytes = serde_json::to_vec(&CacheFile {
        v: CACHE_VERSION,
        list: list.name(),
        fetched_at: now,
        ids: ids.iter().take(MAX_IDS).cloned().collect(),
    })
    .map_err(io::Error::other)?;
    if let Some(parent) = dir.parent() {
        crate::appstream::index::check_dir(parent, true)?;
    }
    crate::appstream::index::check_dir(dir, true)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    crate::appstream::index::check_dir(dir, true)?;

    let name = format!("{}.json", list.name());
    let path = dir.join(&name);
    let mut attempts = 0;
    let (mut file, tmp) = loop {
        let tmp = dir.join(format!(
            ".{name}.tmp.{}.{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
        {
            Ok(f) => break (f, tmp),
            // A leftover of a crashed run with the same pid and counter.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) => return Err(e),
        }
    };
    let written = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            fs::rename(&tmp, &path)
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

// ---- fetching

/// Gets a URL: [`http_fetch`] in the Store, a fake in the tests.
pub type Fetch<'a> = &'a dyn Fn(&str) -> Result<Vec<u8>, NetError>;

/// The Store's HTTPS client ([`crate::net::get`]) with this module's caps.
pub fn http_fetch(url: &str) -> Result<Vec<u8>, NetError> {
    net::get(
        url,
        &Request {
            accept: "application/json",
            max_bytes: MAX_BODY,
            timeout: TIMEOUT,
        },
    )
}

/// Why a list could not be fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshError {
    Fetch(NetError),
    Bad(ParseError),
}

impl RefreshError {
    /// The network itself is down or too slow (not one list that is wrong).
    pub fn is_network(&self) -> bool {
        matches!(
            self,
            RefreshError::Fetch(NetError::Failed(_) | NetError::TimedOut | NetError::NotPublic)
        )
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::Fetch(e) => e.fmt(f),
            RefreshError::Bad(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RefreshError {}

/// Fetches the list, checks it and keeps it in the cache. A cache that can't
/// be written is logged and does not fail the call: the IDs are still good.
pub fn refresh(
    dir: &Path,
    list: List,
    now: u64,
    fetch: Fetch<'_>,
) -> Result<Vec<String>, RefreshError> {
    let body = fetch(&list.url(now)).map_err(RefreshError::Fetch)?;
    let ids = parse(list, &body).map_err(RefreshError::Bad)?;
    if let Err(e) = write_cache(dir, list, &ids, now) {
        log::warn!("could not cache the {} list: {e}", list.name());
    }
    Ok(ids)
}

/// Remembers, in memory only, which fetches failed and when, so a list that
/// failed is asked for again only after [`RETRY_AFTER`], and when the network
/// itself failed, nothing is asked for before then.
#[derive(Debug, Default)]
pub struct RetryGate {
    network: Option<u64>,
    lists: HashMap<List, u64>,
}

impl RetryGate {
    /// Whether `list` may be fetched at `now` (seconds since the epoch).
    pub fn allowed(&self, list: List, now: u64) -> bool {
        let open = |failed: Option<&u64>| {
            // A failure ahead of the clock (it was set back) is over.
            failed.is_none_or(|&t| t > now || now - t >= RETRY_AFTER.as_secs())
        };
        open(self.network.as_ref()) && open(self.lists.get(&list))
    }

    /// Records a failed fetch.
    pub fn failed(&mut self, list: List, error: &RefreshError, now: u64) {
        if error.is_network() {
            self.network = Some(now);
        } else {
            self.lists.insert(list, now);
        }
    }

    /// Records a good fetch: the network works.
    pub fn succeeded(&mut self, list: List) {
        self.network = None;
        self.lists.remove(&list);
    }
}

// ---- matching the local catalog

/// The apps of the library for `ids`, in the order of `ids`: the ones the
/// library has (with or without a `.desktop` suffix), once each, and, when a
/// `category` is given, only those the library puts in it. At most `limit`.
pub fn resolve(
    library: &Library,
    ids: &[String],
    category: Option<Category>,
    limit: usize,
) -> Vec<EntryId> {
    let mut found: Vec<EntryId> = Vec::new();
    for id in ids {
        if found.len() >= limit {
            break;
        }
        let Some(entry) = library.find(id) else {
            continue;
        };
        if found.contains(&entry) {
            continue;
        }
        if category.is_some_and(|c| !library.categories(entry).contains(&c)) {
            continue;
        }
        found.push(entry);
    }
    found
}
