//! What the window shows: the catalog, each connected app's latest release,
//! and what is installed, put together; and installing a release's bundle.
//!
//! A check asks GitHub for at most one release per connected app, and reads
//! the cache for anything fetched less than [`TTL`] ago unless the user asked
//! (`force`). A failure for one app does not hide the others; with no network
//! the cache is used however old it is, and the report says so.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::archive::hex;
use super::catalog::{Catalog, Entry};
use super::fetch::{CATALOG_CACHE, Cache, Fetcher, release_cache_name};
use super::github::{self, Release};
use super::install::{self, Dirs, Installed, Origin};
use super::manifest::{self, Host, Kind, Manifest};
use super::version::Version;
use super::{CATALOG_URL, Error, err, io_err};
use crate::net::NetError;

/// How long a fetched catalog or release is trusted without asking again.
pub const TTL: u64 = 6 * 60 * 60;

/// A release the Store could install: everything checked except the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub entry: Entry,
    pub tag: String,
    /// The release's manifest (outer).
    pub manifest: Manifest,
    pub archive_url: String,
}

/// Turns a repository's release answer and its `telamon-bundle.json` into a
/// candidate, or says why not. Pure: used for fresh and cached answers alike.
pub fn candidate_from(
    entry: &Entry,
    release_json: &[u8],
    manifest_json: &[u8],
) -> Result<Candidate, Error> {
    let release = Release::parse(release_json, &entry.repo)?;
    // The manifest must be the release's own file.
    release.manifest_asset()?;
    let m = Manifest::parse(manifest_json, Kind::Outer)?;
    if m.id != entry.id {
        return Err(err(
            "The release is for a different app than the one listed.",
        ));
    }
    let archive = release.archive_asset(&m)?;
    Ok(Candidate {
        entry: entry.clone(),
        tag: release.tag.clone(),
        archive_url: archive.url.clone(),
        manifest: m,
    })
}

/// Why a GitHub request failed, in plain words; `None` for "no release".
fn net_reason(e: &NetError) -> Option<String> {
    match e {
        NetError::Status(404) => None,
        NetError::Status(403 | 429) => {
            Some("GitHub is limiting requests from this network. Try again later.".into())
        }
        other => Some(other.to_string()),
    }
}

/// Fetches the latest release of `entry` and its manifest.
fn fetch_candidate(
    f: &dyn Fetcher,
    entry: &Entry,
) -> Result<(Candidate, String, String), CandidateError> {
    let release = f
        .get(
            &github::latest_url(&entry.repo),
            "application/vnd.github+json",
            github::MAX_RELEASE_JSON,
        )
        .map_err(|e| match net_reason(&e) {
            None => CandidateError::NoRelease,
            Some(t) => CandidateError::Failed(t),
        })?;
    let parsed = Release::parse(&release, &entry.repo).map_err(|e| CandidateError::Failed(e.0))?;
    let asset = match parsed.manifest_asset() {
        Ok(a) => a,
        Err(_) => return Err(CandidateError::NoRelease),
    };
    let manifest = f
        .get(
            &asset.url,
            "application/json",
            github::MAX_MANIFEST_DOWNLOAD,
        )
        .map_err(|e| {
            CandidateError::Failed(
                net_reason(&e).unwrap_or_else(|| "The release's manifest is missing.".into()),
            )
        })?;
    let cand =
        candidate_from(entry, &release, &manifest).map_err(|e| CandidateError::Failed(e.0))?;
    let (r, m) = (
        String::from_utf8_lossy(&release).into_owned(),
        String::from_utf8_lossy(&manifest).into_owned(),
    );
    Ok((cand, r, m))
}

enum CandidateError {
    /// No release, or none with a bundle: nothing to show, nothing wrong.
    NoRelease,
    Failed(String),
}

/// What the Store knows about one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Not installed; this version can be.
    Available,
    UpToDate,
    /// Installed; the candidate is newer and can be installed.
    Update,
    /// The candidate cannot run here (the reason).
    Incompatible(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub homepage: String,
    pub license: String,
    pub repo: Option<String>,
    pub installed: Option<Installed>,
    pub candidate: Option<Candidate>,
    pub status: Status,
}

/// Puts the pieces together. Apps with no installed copy and no usable
/// release are not listed. Pure.
pub fn build_list(
    installed: &[Installed],
    entries: &[Entry],
    candidates: &BTreeMap<String, Candidate>,
    host: &Host,
) -> Vec<Listed> {
    let mut out = Vec::new();
    for entry in entries {
        let inst = installed.iter().find(|i| i.id == entry.id).cloned();
        let cand = candidates.get(&entry.id).cloned();
        match (&inst, &cand) {
            (None, None) => {}
            (None, Some(c)) => {
                let status = match c.manifest.compatible(host) {
                    Ok(()) => Status::Available,
                    Err(e) => Status::Incompatible(e.0),
                };
                out.push(listed_from_candidate(c, None, status));
            }
            (Some(i), Some(c)) => {
                let newer =
                    Version::parse(&i.version).is_some_and(|v| c.manifest.parsed_version() > v);
                let status = if !newer {
                    Status::UpToDate
                } else {
                    match c.manifest.compatible(host) {
                        Ok(()) => Status::Update,
                        Err(e) => Status::Incompatible(e.0),
                    }
                };
                out.push(listed_from_candidate(c, inst.clone(), status));
            }
            (Some(i), None) => out.push(listed_installed(i, Some(entry.repo.clone()))),
        }
    }
    for i in installed {
        if !entries.iter().any(|e| e.id == i.id) {
            out.push(listed_installed(i, i.origin.repo.clone()));
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    out
}

fn listed_from_candidate(c: &Candidate, installed: Option<Installed>, status: Status) -> Listed {
    Listed {
        id: c.entry.id.clone(),
        name: c.manifest.name.clone(),
        summary: c.manifest.summary.clone(),
        homepage: c.manifest.homepage.clone(),
        license: c.manifest.license.clone(),
        repo: Some(c.entry.repo.clone()),
        installed,
        candidate: Some(c.clone()),
        status,
    }
}

fn listed_installed(i: &Installed, repo: Option<String>) -> Listed {
    Listed {
        id: i.id.clone(),
        name: i.name.clone(),
        summary: i.summary.clone(),
        homepage: String::new(),
        license: String::new(),
        repo,
        installed: Some(i.clone()),
        candidate: None,
        status: Status::UpToDate,
    }
}

/// Something a check could not do, for the log and, unless `quiet`, the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub id: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub apps: Vec<Listed>,
    pub problems: Vec<Problem>,
    /// Unix time of the oldest answer used.
    pub checked_at: u64,
    /// Some answers came from the cache because the network failed.
    pub stale: bool,
}

/// The catalog: fresh from the cache, else the network, else any cache.
fn load_catalog(
    f: &dyn Fetcher,
    cache: &Cache,
    now: u64,
    force: bool,
    report: &mut Report,
) -> Option<(Catalog, u64)> {
    let cached = cache.read(CATALOG_CACHE).and_then(|(t, texts)| {
        let c = Catalog::parse(texts.first()?.as_bytes()).ok()?;
        Some((c, t))
    });
    if let Some((c, t)) = &cached
        && !force
        && now.saturating_sub(*t) < TTL
    {
        return Some((c.clone(), *t));
    }
    match f.get(CATALOG_URL, "application/json", super::catalog::MAX_CATALOG) {
        Ok(bytes) => match Catalog::parse(&bytes) {
            Ok(c) => {
                for s in &c.skipped {
                    log::warn!("native apps catalog: {s}");
                }
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    cache.write(CATALOG_CACHE, now, &[text]);
                }
                Some((c, now))
            }
            Err(e) => {
                report.problems.push(Problem {
                    id: None,
                    text: e.0,
                });
                cached
            }
        },
        Err(e) => {
            log::warn!("native apps catalog: {e}");
            report.stale = true;
            if cached.is_none() {
                report.problems.push(Problem {
                    id: None,
                    text: format!("The list of Telamon apps could not be loaded. {e}"),
                });
            }
            cached
        }
    }
}

/// Checks the catalog and every app in it. Never installs.
pub fn check(
    f: &dyn Fetcher,
    cache: &Cache,
    dirs: &Dirs,
    host: &Host,
    now: u64,
    force: bool,
) -> Report {
    let mut report = Report {
        checked_at: now,
        ..Report::default()
    };
    let installed = install::list(dirs);
    let Some((catalog, fetched)) = load_catalog(f, cache, now, force, &mut report) else {
        report.apps = build_list(&installed, &[], &BTreeMap::new(), host);
        return report;
    };
    report.checked_at = report.checked_at.min(fetched);
    let mut candidates = BTreeMap::new();
    for entry in &catalog.apps {
        let name = release_cache_name(&entry.id);
        let cached = cache.read(&name).and_then(|(t, texts)| {
            let c =
                candidate_from(entry, texts.first()?.as_bytes(), texts.get(1)?.as_bytes()).ok()?;
            Some((c, t))
        });
        if let Some((c, t)) = &cached
            && !force
            && now.saturating_sub(*t) < TTL
        {
            report.checked_at = report.checked_at.min(*t);
            candidates.insert(entry.id.clone(), c.clone());
            continue;
        }
        match fetch_candidate(f, entry) {
            Ok((c, release, manifest)) => {
                cache.write(&name, now, &[&release, &manifest]);
                candidates.insert(entry.id.clone(), c);
            }
            Err(CandidateError::NoRelease) => {
                // Nothing to offer (yet): a cached older answer is not kept.
            }
            Err(CandidateError::Failed(text)) => {
                report.stale = true;
                let who = installed
                    .iter()
                    .find(|i| i.id == entry.id)
                    .map(|i| i.name.clone())
                    .unwrap_or_else(|| entry.id.clone());
                log::warn!("native apps: {who}: {text}");
                report.problems.push(Problem {
                    id: Some(entry.id.clone()),
                    text: format!("{}: {text}", crate::text::clean(&who, 60)),
                });
                if let Some((c, t)) = cached {
                    report.checked_at = report.checked_at.min(t);
                    candidates.insert(entry.id.clone(), c);
                }
            }
        }
    }
    report.apps = build_list(&installed, &catalog.apps, &candidates, host);
    report
}

/// What the window shows before (or without) any network: the installed
/// apps and whatever the cache holds, however old. Never asks anyone.
pub fn cached(cache: &Cache, dirs: &Dirs, host: &Host) -> Report {
    let installed = install::list(dirs);
    let mut report = Report::default();
    let mut oldest = u64::MAX;
    let catalog = cache.read(CATALOG_CACHE).and_then(|(t, texts)| {
        let c = Catalog::parse(texts.first()?.as_bytes()).ok()?;
        Some((c, t))
    });
    let mut candidates = BTreeMap::new();
    let mut entries = Vec::new();
    if let Some((c, t)) = catalog {
        oldest = oldest.min(t);
        for entry in c.apps {
            if let Some((cand, t)) =
                cache
                    .read(&release_cache_name(&entry.id))
                    .and_then(|(t, texts)| {
                        let c = candidate_from(
                            &entry,
                            texts.first()?.as_bytes(),
                            texts.get(1)?.as_bytes(),
                        )
                        .ok()?;
                        Some((c, t))
                    })
            {
                oldest = oldest.min(t);
                candidates.insert(entry.id.clone(), cand);
            }
            entries.push(entry);
        }
    }
    report.checked_at = if oldest == u64::MAX { 0 } else { oldest };
    report.apps = build_list(&installed, &entries, &candidates, host);
    report
}

/// Downloads the candidate's archive into `work`, checks its size and SHA-256
/// against the manifest, installs it and removes the download.
/// `progress(done, total)` is called as bytes arrive.
pub fn install_candidate(
    f: &dyn Fetcher,
    dirs: &Dirs,
    host: &Host,
    cand: &Candidate,
    work: &Path,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<install::Done, Error> {
    let info = cand
        .manifest
        .archive
        .as_ref()
        .ok_or_else(|| err("The release names no archive."))?;
    crate::appimage::fsutil::private_dir(work)
        .map_err(|e| io_err("make the download folder", &e))?;
    let file = work.join(format!("download-{}.tar.zst", std::process::id()));
    let result = (|| {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&file)
            .map_err(|e| io_err("save the download", &e))?;
        let mut hasher = Sha256::new();
        let mut done = 0u64;
        let got = f
            .download(&cand.archive_url, info.size, &mut |chunk| {
                hasher.update(chunk);
                done += chunk.len() as u64;
                progress(done, info.size);
                out.write_all(chunk)
            })
            .map_err(|e| err(e.to_string()))?;
        out.flush().map_err(|e| io_err("save the download", &e))?;
        drop(out);
        if got != info.size || hex(&hasher.finalize()) != info.sha256 {
            return Err(err(
                "The download is not what the release says it is (size or checksum differ). Nothing was installed.",
            ));
        }
        install::install_bundle(
            dirs,
            &file,
            &install::Options {
                expect_id: Some(&cand.entry.id),
                outer: Some(&cand.manifest),
                origin: Origin::release(&cand.entry.repo, &cand.tag),
                host,
            },
        )
    })();
    let _ = std::fs::remove_file(&file);
    result
}

/// A local bundle: the inner manifest, read without installing, for the
/// confirmation. The file is unpacked into a scratch folder under `work`,
/// checked, and the folder removed.
pub fn inspect_local(archive: &Path, work: &Path) -> Result<(Manifest, String, u64), Error> {
    crate::appimage::fsutil::private_dir(work).map_err(|e| io_err("make a work folder", &e))?;
    let (sha, size) =
        super::archive::sha256_file(archive).map_err(|e| io_err("read the file", &e))?;
    if size > manifest::MAX_ARCHIVE {
        return Err(err(
            "The file is larger than the Store accepts for a bundle.",
        ));
    }
    let dir = work.join(format!("inspect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).map_err(|e| io_err("make a work folder", &e))?;
    let result = super::archive::unpack(archive, &dir, None);
    let _ = std::fs::remove_dir_all(&dir);
    result.map(|m| (m, sha, size))
}
