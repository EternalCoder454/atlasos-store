//! Finding and reading the catalogs libflatpak has downloaded.

use std::fs;
use std::path::{Path, PathBuf};

use libflatpak::prelude::*;

use super::{CatalogSource, LoadError, SourcesOutcome};
use crate::appstream::{self, Catalog, IndexKey, ParseError, ParseOptions, index};
use crate::flatpak::{self, CancelToken, Scope};
use crate::text;

/// The commit a resolved catalog folder is named after: lowercase hex, 16 to
/// 64 characters.
fn commit_name(dir: &Path) -> Result<String, String> {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the link has no usable target name")?;
    if (16..=64).contains(&name.len())
        && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        Ok(name.to_owned())
    } else {
        Err("the link does not point to a commit".to_owned())
    }
}

/// `dir`, `commit` and `updated` of the catalog behind the `active` link:
/// all `None` when it was never downloaded, an error line when it is there
/// but unusable.
fn locate(link: &Path) -> Result<(PathBuf, String, std::time::SystemTime), Option<String>> {
    let meta = match fs::symlink_metadata(link) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(None),
        Err(e) => return Err(Some(e.to_string())),
    };
    // One resolution gives both: flatpak swaps `active` during a refresh, and
    // a commit from one look with the folder of another would index one
    // commit's catalog under the other's name.
    let dir = fs::canonicalize(link).map_err(|e| Some(e.to_string()))?;
    let parent = link.parent().and_then(|p| fs::canonicalize(p).ok());
    if parent.is_none_or(|p| dir.parent() != Some(p.as_path())) {
        return Err(Some("the link points outside its folder".to_owned()));
    }
    let commit = commit_name(&dir).map_err(Some)?;
    let updated = meta.modified().map_err(|e| Some(e.to_string()))?;
    Ok((dir, commit, updated))
}

fn scan(scope: Scope, arch: &str, cancel: &CancelToken, out: &mut SourcesOutcome) {
    let name = scope.label();
    let inst = match flatpak::open(scope) {
        Ok(i) => i,
        Err(e) => {
            out.errors
                .push(format!("The {name} installation could not be read: {e}"));
            return;
        }
    };
    let remotes = match inst.list_remotes(Some(cancel.cancellable())) {
        Ok(r) => r,
        Err(_) if cancel.is_cancelled() => return,
        Err(e) => {
            out.errors.push(format!(
                "The sources of the {name} installation could not be listed: {}",
                flatpak::scrub(e.message())
            ));
            return;
        }
    };
    for r in remotes.iter() {
        if cancel.is_cancelled() {
            return;
        }
        if r.is_disabled() || r.is_noenumerate() {
            continue;
        }
        let rname = r.name().map(|n| n.to_string()).unwrap_or_default();
        if !flatpak::valid_remote(&rname) {
            out.errors.push(format!(
                "A source of the {name} installation has a name that is not accepted and was skipped."
            ));
            continue;
        }
        let title = r.title().map(|t| text::clean(&t, 200)).unwrap_or_default();
        let mut source = CatalogSource {
            scope,
            title: if title.is_empty() {
                rname.clone()
            } else {
                title
            },
            remote: rname,
            priority: r.prio(),
            dir: None,
            commit: None,
            updated: None,
        };
        let link = r.appstream_dir(Some(arch)).and_then(|f| f.path());
        match link.map(|l| locate(&l)) {
            Some(Ok((dir, commit, updated))) => {
                source.dir = Some(dir);
                source.commit = Some(commit);
                source.updated = Some(updated);
            }
            Some(Err(None)) | None => {}
            Some(Err(Some(why))) => out.errors.push(format!(
                "The catalog of \"{}\" ({name}) could not be read: {}",
                source.remote,
                text::clean(&why, 300)
            )),
        }
        out.sources.push(source);
    }
}

pub(super) fn list(cancel: &CancelToken) -> SourcesOutcome {
    let mut out = SourcesOutcome::default();
    let arch = match libflatpak::default_arch() {
        Some(a) if flatpak::valid_arch(&a) => a.to_string(),
        _ => {
            out.errors
                .push("This system's architecture is not known to Flatpak.".to_owned());
            return out;
        }
    };
    for scope in [Scope::System, Scope::User] {
        if cancel.is_cancelled() {
            break;
        }
        scan(scope, &arch, cancel, &mut out);
    }
    out.sources.sort_by(|a, b| {
        let scope = |s: &CatalogSource| u8::from(s.scope == Scope::User);
        (a.remote != "flathub")
            .cmp(&(b.remote != "flathub"))
            .then(b.priority.cmp(&a.priority))
            .then_with(|| a.remote.cmp(&b.remote))
            .then(scope(a).cmp(&scope(b)))
    });
    out
}

pub(super) fn load(
    source: &CatalogSource,
    cache_dir: &Path,
    langs: &[String],
) -> Result<Catalog, LoadError> {
    let (Some(dir), Some(commit)) = (&source.dir, &source.commit) else {
        return Err(LoadError::NotDownloaded);
    };
    let key = IndexKey {
        origin: source.remote.clone(),
        commit: commit.clone(),
        langs: langs.to_vec(),
        format: index::FORMAT,
    };
    // One folder per installation: the index of a remote replaces its older
    // commits' files, and the same remote name in both scopes would evict
    // the other's index on every load.
    let cache_dir = &cache_dir.join(source.scope.label());
    let path = index::cache_file(cache_dir, &key);
    match &path {
        Ok(p) => match index::read(p, &key) {
            Ok(cat) => return Ok(cat),
            Err(appstream::IndexError::Missing | appstream::IndexError::KeyMismatch) => {}
            Err(e) => log::warn!(
                "the index of {} is not usable and is rebuilt: {e}",
                source.remote
            ),
        },
        Err(e) => log::warn!("no index for {}: {e}", source.remote),
    }
    let opts = ParseOptions {
        origin: source.remote.clone(),
        langs: langs.to_vec(),
        ..ParseOptions::default()
    };
    let cat =
        appstream::parse_gz_file(&dir.join("appstream.xml.gz"), &opts).map_err(|e| match e {
            // The XML error can quote the remote's tag names: cleaned, as
            // it is logged and shown.
            ParseError::Io(m) => LoadError::Io(std::io::Error::other(text::clean(&m, 300))),
            e => LoadError::Parse(text::clean(&e.to_string(), 300)),
        })?;
    if path.is_ok()
        && let Err(e) = index::write(cache_dir, &key, &cat)
    {
        log::warn!("could not write the index of {}: {e}", source.remote);
    }
    Ok(cat)
}
