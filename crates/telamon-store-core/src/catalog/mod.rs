//! What the Store browses: the AppStream catalogs of every enabled remote,
//! merged into one [`Library`] that is searched and browsed by category.
//!
//! [`list_sources`] finds the catalogs libflatpak has downloaded, [`load`]
//! reads one through the on-disk index (parsing the XML only when the commit
//! changed), and [`Library::new`] merges them. Every function here that touches
//! the disk or libflatpak blocks: call it on a worker thread. A built
//! [`Library`] is immutable and `Send + Sync`; queries on it are pure and take
//! well under the 5 ms core budget for all of Flathub.

mod license;
mod sources;
mod text;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::appstream::{Catalog, Component, Kind};
use crate::flatpak::{CancelToken, Scope};

/// The catalogs to browse are refreshed when older than this, and only while
/// the Store is open.
pub const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(6 * 3600);

/// One of the Store's categories: Flathub's main ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    AudioVideo,
    Development,
    Education,
    Games,
    Graphics,
    Network,
    Office,
    Science,
    System,
    Utilities,
}

impl Category {
    /// Every category, in the order the Store shows them.
    pub const ALL: [Category; 10] = [
        Category::AudioVideo,
        Category::Development,
        Category::Education,
        Category::Games,
        Category::Graphics,
        Category::Network,
        Category::Office,
        Category::Science,
        Category::System,
        Category::Utilities,
    ];

    /// A stable lowercase key (`audio-video`, `games`), for QML and the
    /// command line. Not shown to people.
    pub fn key(self) -> &'static str {
        match self {
            Category::AudioVideo => "audio-video",
            Category::Development => "development",
            Category::Education => "education",
            Category::Games => "games",
            Category::Graphics => "graphics",
            Category::Network => "network",
            Category::Office => "office",
            Category::Science => "science",
            Category::System => "system",
            Category::Utilities => "utilities",
        }
    }

    /// The position in [`Category::ALL`].
    fn index(self) -> usize {
        Category::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }

    /// The category with this [`Category::key`].
    pub fn from_key(key: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.key() == key)
    }

    /// The Store categories a component's freedesktop categories put it in:
    /// `AudioVideo`, `Audio` and `Video` → AudioVideo, `Game` → Games,
    /// `Graphics`, `Network`, `Office`, `Development`, `Education`,
    /// `Science`, `System`, and `Utility` → Utilities. Matching is exact
    /// (freedesktop names are case-sensitive); unknown names are ignored.
    pub fn of(desktop_categories: &[String]) -> Vec<Category> {
        let mut out: Vec<Category> = desktop_categories
            .iter()
            .filter_map(|c| match c.as_str() {
                "AudioVideo" | "Audio" | "Video" => Some(Category::AudioVideo),
                "Game" => Some(Category::Games),
                "Graphics" => Some(Category::Graphics),
                "Network" => Some(Category::Network),
                "Office" => Some(Category::Office),
                "Development" => Some(Category::Development),
                "Education" => Some(Category::Education),
                "Science" => Some(Category::Science),
                "System" => Some(Category::System),
                "Utility" => Some(Category::Utilities),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Where one remote's downloaded catalog is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSource {
    pub scope: Scope,
    /// The remote's name, valid per `flatpak::valid_remote`.
    pub remote: String,
    /// The remote's title, or its name when it has none. Untrusted text.
    pub title: String,
    /// The remote's address as flatpak compares remotes (normalized, no
    /// trailing slash). It is what says whether this is Flathub, whatever
    /// the remote is called or which installation it is in.
    pub url: String,
    /// The remote's priority (higher first).
    pub priority: i32,
    /// `.../appstream/<remote>/<arch>/active`, resolved (an OCI remote's
    /// `.../appstream/<remote>/<arch>` itself). `None` when the catalog was
    /// never downloaded.
    pub dir: Option<PathBuf>,
    /// The OSTree commit `active` points to (for an OCI remote, a digest of
    /// its catalog file's size and mtime): lowercase hex, 16 to 64
    /// characters, checked. `None` together with `dir`.
    pub commit: Option<String>,
    /// When the catalog was last downloaded (the `active` link's mtime).
    pub updated: Option<SystemTime>,
}

impl CatalogSource {
    /// True when the catalog is missing or older than [`STALE_AFTER`].
    pub fn is_stale(&self, now: SystemTime) -> bool {
        if self.dir.is_none() || self.commit.is_none() {
            return true;
        }
        match self.updated {
            None => true,
            // A time in the future (the clock moved back) is not old.
            Some(t) => now.duration_since(t).is_ok_and(|age| age > STALE_AFTER),
        }
    }

    /// The local file of an icon the catalog cached, `dir/icons/<size>x<size>/
    /// <file>`, for the smallest cached size at least `want` pixels (else the
    /// largest). `None` when the source has no `dir`, the component has no
    /// icon, or the file name is not a bare `.png`/`.svg` name. Never follows
    /// the name outside `dir`; the file's existence is not checked.
    pub fn icon_path(&self, component: &Component, want: u16) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let icon = component.icon.as_ref()?;
        if !crate::text::valid_icon_file(&icon.file) {
            return None;
        }
        let mut sizes: Vec<u16> = icon.sizes.iter().copied().filter(|s| *s > 0).collect();
        sizes.sort_unstable();
        let size = sizes
            .iter()
            .copied()
            .find(|s| *s >= want)
            .or_else(|| sizes.last().copied())?;
        Some(
            dir.join("icons")
                .join(format!("{size}x{size}"))
                .join(&icon.file),
        )
    }
}

/// The remotes whose catalogs the Store shows, and what could not be read.
#[derive(Debug, Default)]
pub struct SourcesOutcome {
    /// Enabled remotes that may be listed (not `noenumerate`), of the user
    /// and system installations. Flathub first, then by priority, then by
    /// name; the system copy before the user copy of the same name.
    pub sources: Vec<CatalogSource>,
    /// One plain-text line per installation or remote that could not be read.
    pub errors: Vec<String>,
}

/// Lists the catalog sources of both installations. Blocking: run on a
/// worker thread. When `cancel` was cancelled the outcome is incomplete,
/// without an error for it: discard it.
pub fn list_sources(cancel: &CancelToken) -> SourcesOutcome {
    sources::list(cancel)
}

/// Why a catalog could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    /// The source was never downloaded (`dir` is `None`).
    NotDownloaded,
    /// The XML could not be parsed; the message is plain text.
    Parse(String),
    /// Reading the file failed.
    Io(std::io::Error),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotDownloaded => f.write_str("The catalog has not been downloaded yet."),
            LoadError::Parse(m) => write!(f, "The catalog could not be read: {m}"),
            LoadError::Io(e) => write!(f, "The catalog file could not be read: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Loads one source's catalog: from the index in `cache_dir/<scope>` (`system`
/// or `user`, so the same remote in both installations keeps an index each)
/// when it was built from the same commit and languages, otherwise by parsing
/// `dir/appstream.xml.gz` and writing a new index (a failed write is logged,
/// not an error). Blocking: run on a worker thread.
pub fn load(
    source: &CatalogSource,
    cache_dir: &Path,
    langs: &[String],
) -> Result<Catalog, LoadError> {
    sources::load(source, cache_dir, langs)
}

/// An app in a [`Library`], by position. Valid only for the library that
/// returned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u32);

/// What [`Library::browse`] and [`Library::search`] leave out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Filter {
    /// Only apps whose publisher Flathub verified (see [`Library::is_verified`]).
    pub verified_only: bool,
    /// Only apps whose licence is free (see [`is_free_license`]).
    pub free_only: bool,
}

/// How [`Library::browse`] orders apps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// By name, case-insensitively, then by ID.
    #[default]
    Name,
    /// Newest release first; apps without a dated release last, by name.
    RecentlyUpdated,
}

/// True when an SPDX licence expression is free: every licence an `AND`
/// needs, and at least one side of each `OR`, is on the built-in list of free
/// licences (the common OSI/FSF-approved IDs, `-only`/`-or-later` and `+`
/// forms, `WITH` exceptions ignored). Empty, unknown or `LicenseRef-*` is not
/// free. Never panics on any input.
pub fn is_free_license(expr: &str) -> bool {
    license::is_free(expr)
}

/// The apps of every source, merged: desktop and console apps with an
/// `app/` bundle only. When several sources list the same app (by bare ID),
/// the first source in the order given wins and the others are kept as
/// [`Library::alternatives`].
pub struct Library {
    sources: Vec<CatalogSource>,
    entries: Vec<Entry>,
    by_id: HashMap<String, u32>,
    /// Entry indices in [`Sort::Name`] order.
    by_name: Vec<u32>,
    /// Entry indices in [`Sort::RecentlyUpdated`] order.
    by_recent: Vec<u32>,
}

/// The longest query looked at, in characters.
const MAX_QUERY_CHARS: usize = 200;
/// Distinct words of a query that are matched; the rest are ignored.
const MAX_QUERY_WORDS: usize = 10;

/// One app, with what search and browse need precomputed.
struct Entry {
    comp: Component,
    source: u32,
    /// The other sources of the same app (indices into `Library::sources`).
    alts: Vec<u32>,
    cats: Vec<Category>,
    verified: bool,
    free: bool,
    updated: Option<i64>,
    /// Position in [`Sort::Name`] order, so ties compare as integers.
    name_rank: u32,
    /// ` word word`: the normalized words (see `text::haystack`) of the name,
    /// keywords, ID, summary and developer, in the order of their rank.
    hay: [String; 5],
}

const _: () = {
    const fn send_sync<T: Send + Sync>() {}
    send_sync::<Library>();
};

fn is_listed(c: &Component) -> bool {
    matches!(c.kind, Kind::DesktopApp | Kind::ConsoleApp)
        && c.bundle
            .as_ref()
            .is_some_and(|b| b.reference.starts_with("app/"))
}

impl Library {
    /// Merges the catalogs, in the order given (that of
    /// [`SourcesOutcome::sources`]). Precomputes everything search and browse
    /// need.
    pub fn new(catalogs: Vec<(CatalogSource, Catalog)>) -> Library {
        let mut sources = Vec::with_capacity(catalogs.len());
        let mut entries: Vec<Entry> = Vec::new();
        let mut by_id: HashMap<String, u32> = HashMap::new();
        let mut free_cache: HashMap<String, bool> = HashMap::new();
        for (si, (source, catalog)) in catalogs.into_iter().enumerate() {
            let si = si as u32;
            // Flathub's "verified" is Flathub's claim: the same words in any
            // other remote's catalog are that remote's own, so they count for
            // nothing here.
            let from_flathub = crate::flatpak::sources::is_flathub_url(&source.url);
            sources.push(source);
            for comp in catalog.components {
                if !is_listed(&comp) {
                    continue;
                }
                if let Some(&ei) = by_id.get(comp.id_bare()) {
                    let e = &mut entries[ei as usize];
                    if e.source != si && !e.alts.contains(&si) {
                        e.alts.push(si);
                    }
                    continue;
                }
                let Ok(ei) = u32::try_from(entries.len()) else {
                    log::warn!(
                        "the catalog has more apps than the Store can list; the rest is left out"
                    );
                    break;
                };
                by_id.insert(comp.id_bare().to_owned(), ei);
                let free = *free_cache
                    .entry(comp.license.clone())
                    .or_insert_with(|| is_free_license(&comp.license));
                let mut kw = String::new();
                for k in &comp.keywords {
                    text::push_words(k, &mut kw);
                }
                let hay = [
                    text::haystack(&comp.name),
                    kw,
                    text::haystack(comp.id_bare()),
                    text::haystack(&comp.summary),
                    text::haystack(&comp.developer),
                ];
                entries.push(Entry {
                    cats: Category::of(&comp.categories),
                    verified: from_flathub && comp.verification.is_some(),
                    free,
                    updated: comp
                        .releases
                        .iter()
                        .map(|r| r.timestamp)
                        .filter(|t| *t > 0)
                        .max(),
                    source: si,
                    alts: Vec::new(),
                    name_rank: 0,
                    hay,
                    comp,
                });
            }
        }
        let lower: Vec<String> = entries.iter().map(|e| e.comp.name.to_lowercase()).collect();
        let mut by_name: Vec<u32> = (0..entries.len() as u32).collect();
        by_name.sort_unstable_by(|&a, &b| {
            lower[a as usize].cmp(&lower[b as usize]).then_with(|| {
                entries[a as usize]
                    .comp
                    .id
                    .cmp(&entries[b as usize].comp.id)
            })
        });
        for (rank, &i) in by_name.iter().enumerate() {
            entries[i as usize].name_rank = rank as u32;
        }
        let mut by_recent = by_name.clone();
        // Stable: equal dates stay in name order; no date sorts last.
        by_recent.sort_by_key(|&i| std::cmp::Reverse(entries[i as usize].updated));
        Library {
            sources,
            entries,
            by_id,
            by_name,
            by_recent,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The sources, in merge order.
    pub fn sources(&self) -> &[CatalogSource] {
        &self.sources
    }

    fn entry(&self, id: EntryId) -> &Entry {
        &self.entries[id.0 as usize]
    }

    pub fn component(&self, id: EntryId) -> &Component {
        &self.entry(id).comp
    }

    /// The source the entry was taken from.
    pub fn source(&self, id: EntryId) -> &CatalogSource {
        &self.sources[self.entry(id).source as usize]
    }

    /// The other sources that list the same app, in merge order.
    pub fn alternatives(&self, id: EntryId) -> Vec<&CatalogSource> {
        self.entry(id)
            .alts
            .iter()
            .map(|&s| &self.sources[s as usize])
            .collect()
    }

    pub fn categories(&self, id: EntryId) -> &[Category] {
        &self.entry(id).cats
    }

    /// Whether Flathub verified the app's publisher. Only Flathub's own
    /// catalog counts, found by the remote's address (Flathub's or Flathub
    /// Beta's), not by its name or installation: another remote can write the
    /// same words in its AppStream data, and a remote can be given any name.
    pub fn is_verified(&self, id: EntryId) -> bool {
        self.entry(id).verified
    }

    pub fn is_free(&self, id: EntryId) -> bool {
        self.entry(id).free
    }

    /// The newest release's timestamp, if any.
    pub fn updated(&self, id: EntryId) -> Option<i64> {
        self.entry(id).updated
    }

    /// The entry for an app ID, with or without `.desktop`.
    pub fn find(&self, app_id: &str) -> Option<EntryId> {
        let bare = app_id.strip_suffix(".desktop").unwrap_or(app_id);
        self.by_id.get(bare).map(|&i| EntryId(i))
    }

    /// Apps matching `query`, best first. The query is split into words
    /// (case- and accent-insensitive where `char::to_lowercase` allows);
    /// every word must be a prefix of a word in the name, keywords, ID,
    /// summary or developer. Matches in the name rank above keywords, ID,
    /// summary and developer, in that order; an exact name match ranks first;
    /// ties go to verified apps, then by name. An empty or blank query, or one
    /// longer than 200 characters after trimming, returns nothing; only 10
    /// distinct words of a longer query are matched. At most
    /// `limit` results.
    pub fn search(&self, query: &str, filter: Filter, limit: usize) -> Vec<EntryId> {
        let query = query.trim();
        if limit == 0 || query.is_empty() || query.chars().count() > MAX_QUERY_CHARS {
            return Vec::new();
        }
        let whole = text::haystack(query);
        if whole.is_empty() {
            return Vec::new();
        }
        // " w" patterns: a word of a field starts with the query word.
        // Repeated words add nothing, and a query of many words costs a
        // pass over every field each: at most MAX_QUERY_WORDS distinct ones.
        let mut pats: Vec<String> = whole
            .split(' ')
            .filter(|w| !w.is_empty())
            .map(|w| format!(" {w}"))
            .collect();
        pats.sort_unstable();
        pats.dedup();
        pats.truncate(MAX_QUERY_WORDS);
        let mut hits: Vec<((u8, u8, u8, u32), u32)> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if !self.passes(e, filter) {
                continue;
            }
            let mut worst = 0usize;
            let mut all = true;
            for p in &pats {
                match e.hay.iter().position(|h| h.contains(p.as_str())) {
                    Some(f) => worst = worst.max(f),
                    None => {
                        all = false;
                        break;
                    }
                }
            }
            if !all {
                continue;
            }
            let exact = e.hay[0] == whole;
            hits.push((
                (
                    u8::from(!exact),
                    worst as u8,
                    u8::from(!e.verified),
                    e.name_rank,
                ),
                i as u32,
            ));
        }
        hits.sort_unstable_by_key(|h| h.0);
        hits.into_iter().take(limit).map(|h| EntryId(h.1)).collect()
    }

    fn passes(&self, e: &Entry, filter: Filter) -> bool {
        (!filter.verified_only || e.verified) && (!filter.free_only || e.free)
    }

    /// Apps in `category` (all apps when `None`), filtered and sorted.
    pub fn browse(&self, category: Option<Category>, filter: Filter, sort: Sort) -> Vec<EntryId> {
        let order = match sort {
            Sort::Name => &self.by_name,
            Sort::RecentlyUpdated => &self.by_recent,
        };
        order
            .iter()
            .filter(|&&i| {
                let e = &self.entries[i as usize];
                self.passes(e, filter) && category.is_none_or(|c| e.cats.contains(&c))
            })
            .map(|&i| EntryId(i))
            .collect()
    }

    /// How many apps each category has under `filter`, in [`Category::ALL`]
    /// order.
    pub fn category_counts(&self, filter: Filter) -> [(Category, usize); 10] {
        let mut counts = [0usize; 10];
        for e in self.entries.iter().filter(|e| self.passes(e, filter)) {
            for c in &e.cats {
                counts[c.index()] += 1;
            }
        }
        Category::ALL.map(|c| (c, counts[c.index()]))
    }
}
