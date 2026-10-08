//! The network behind a trait, and the cache of what it fetched.
//!
//! [`Net`] is the real thing (`crate::net`: https, public addresses, size
//! caps, redirects checked). Tests and the window's screenshot runs use
//! [`super::fake::Fake`], which answers from recorded GitHub responses, so
//! nothing here ever needs the real GitHub to be tested.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::appimage::fsutil;
use crate::net::{self, NetError, Request};

/// Seconds for an API answer or a manifest.
const SMALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Seconds for an archive (up to 256 MiB).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub trait Fetcher: Send + Sync {
    /// The body of `url`, at most `max_bytes`.
    fn get(&self, url: &str, accept: &str, max_bytes: u64) -> Result<Vec<u8>, NetError>;
    /// The body of `url` given to `sink` in pieces, at most `max_bytes` in all.
    fn download(
        &self,
        url: &str,
        max_bytes: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<u64, NetError>;
}

/// The real network.
#[derive(Debug, Clone, Copy, Default)]
pub struct Net;

impl Fetcher for Net {
    fn get(&self, url: &str, accept: &str, max_bytes: u64) -> Result<Vec<u8>, NetError> {
        net::get(
            url,
            &Request {
                accept,
                max_bytes,
                timeout: SMALL_TIMEOUT,
            },
        )
    }

    fn download(
        &self,
        url: &str,
        max_bytes: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<u64, NetError> {
        net::download(
            url,
            &Request {
                accept: "application/octet-stream",
                max_bytes,
                timeout: DOWNLOAD_TIMEOUT,
            },
            sink,
        )
    }
}

/// `$XDG_CACHE_HOME/telamon-store/native`: the catalog and each app's latest
/// release, with the time they were fetched. A file is the Store's own; what
/// it holds is untrusted when read, like the network's answer it came from.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    fetched: u64,
    texts: Vec<String>,
}

const MAX_CACHED: u64 = 3 * 1024 * 1024;

impl Cache {
    pub fn new(dir: PathBuf) -> Cache {
        Cache { dir }
    }

    /// The user's cache folder for this.
    pub fn from_env() -> Option<Cache> {
        Some(Cache::new(
            fsutil::cache_home()?
                .join(crate::legacy::NAME)
                .join("native"),
        ))
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// The fetch time and texts stored as `name` (a plain file name).
    pub fn read(&self, name: &str) -> Option<(u64, Vec<String>)> {
        let bytes = fsutil::read_private(&self.dir.join(name), MAX_CACHED).ok()??;
        let s: Stored = serde_json::from_slice(&bytes).ok()?;
        Some((s.fetched, s.texts))
    }

    /// Stores `texts` as `name`, atomically. A failure is only logged: the
    /// cache is an optimisation.
    pub fn write(&self, name: &str, fetched: u64, texts: &[&str]) {
        let stored = Stored {
            fetched,
            texts: texts.iter().map(|t| (*t).to_string()).collect(),
        };
        let result = (|| -> io::Result<()> {
            fsutil::private_dir(&self.dir)?;
            let bytes = serde_json::to_vec(&stored).map_err(io::Error::other)?;
            fsutil::write_atomic(&self.dir.join(name), &bytes, 0o600)
        })();
        if let Err(e) = result {
            log::warn!("could not write the native apps cache: {e}");
        }
    }
}

/// A plain cache file name for an app: `release-<id>.json`.
pub fn release_cache_name(id: &str) -> String {
    format!("release-{id}.json")
}

pub const CATALOG_CACHE: &str = "catalog.json";
