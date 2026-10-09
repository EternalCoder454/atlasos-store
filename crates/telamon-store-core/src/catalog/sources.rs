//! Finding and reading the catalogs libflatpak has downloaded.

use std::fs;
use std::path::{Path, PathBuf};

use libflatpak::prelude::*;
use sha2::{Digest, Sha256};

use super::{CatalogSource, LoadError, SourcesOutcome};
use crate::appstream::{self, Catalog, IndexKey, ParseError, ParseOptions, index};
use crate::flatpak::sources::is_flathub_url;
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

/// The catalog of an OCI remote (Fedora's registry): flatpak keeps no
/// `active` link and no commit for it, only `appstream.xml.gz` and `icons/`
/// in the arch folder itself, replaced on each refresh. Its "commit" (the
/// index key) is a digest of the file's size and mtime, so the index is
/// rebuilt whenever the catalog is.
fn locate_plain(dir: &Path) -> Result<(PathBuf, String, std::time::SystemTime), Option<String>> {
    let xml = dir.join("appstream.xml.gz");
    let meta = match fs::symlink_metadata(&xml) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Err(Some("the catalog is not a plain file".to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(None),
        Err(e) => return Err(Some(e.to_string())),
    };
    let updated = meta.modified().map_err(|e| Some(e.to_string()))?;
    let stamp = updated
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h = Sha256::new();
    h.update(b"atlas-store oci catalog\0");
    h.update(meta.len().to_le_bytes());
    h.update(stamp.to_le_bytes());
    let commit = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let dir = fs::canonicalize(dir).map_err(|e| Some(e.to_string()))?;
    Ok((dir, commit, updated))
}

/// `dir`, `commit` and `updated` of the catalog behind the `active` link
/// (or in the folder itself, for an OCI remote): all `None` when it was
/// never downloaded, an error line when it is there but unusable.
fn locate(link: &Path) -> Result<(PathBuf, String, std::time::SystemTime), Option<String>> {
    let meta = match fs::symlink_metadata(link) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(None),
        Err(e) => return Err(Some(e.to_string())),
    };
    if meta.is_dir() {
        return locate_plain(link);
    }
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
            url: flatpak::transaction::norm_url(&text::clean(
                r.url().as_deref().unwrap_or_default(),
                500,
            )),
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
    order(&mut out.sources);
    out
}

/// Flathub first (the remote named `flathub` that has Flathub's address: a
/// remote can be given any name), then by priority, then by name; the system
/// copy before the user copy of the same name.
fn order(sources: &mut [CatalogSource]) {
    let is_flathub = |s: &CatalogSource| s.remote == "flathub" && is_flathub_url(&s.url);
    sources.sort_by(|a, b| {
        let scope = |s: &CatalogSource| u8::from(s.scope == Scope::User);
        (!is_flathub(a))
            .cmp(&(!is_flathub(b)))
            .then(b.priority.cmp(&a.priority))
            .then_with(|| a.remote.cmp(&b.remote))
            .then(scope(a).cmp(&scope(b)))
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A fresh folder under the system temp dir, removed when dropped.
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir()
                .join(format!("telamon-store-sources-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn src(remote: &str, url: &str, priority: i32, scope: Scope) -> CatalogSource {
        CatalogSource {
            url: url.into(),
            scope,
            remote: remote.into(),
            title: remote.into(),
            priority,
            dir: None,
            commit: None,
            updated: None,
        }
    }

    #[test]
    fn flathub_sorts_first_by_its_address_not_by_a_name_anyone_can_give() {
        let flathub = "https://dl.flathub.org/repo";
        let mut v = vec![
            src("aaa", "https://a.example.org/repo", 9, Scope::System),
            // A remote the user (or a tool) named flathub with another address.
            src("flathub", "https://evil.example.org/repo", 1, Scope::User),
            src("flathub", flathub, 1, Scope::System),
        ];
        order(&mut v);
        assert_eq!(v[0].url, flathub);
        assert_eq!(v[1].remote, "aaa");
        assert_eq!(v[2].url, "https://evil.example.org/repo");
    }

    #[test]
    fn an_oci_catalog_folder_is_read_in_place() {
        let d = TmpDir::new();
        let arch = d.path().join("fedora").join("x86_64");
        fs::create_dir_all(arch.join("icons")).unwrap();
        // Never downloaded yet: nothing, and no error.
        assert_eq!(locate(&arch), Err(None));

        let xml = arch.join("appstream.xml.gz");
        fs::write(&xml, b"not really gzip").unwrap();
        let (dir, commit, _) = locate(&arch).unwrap();
        assert_eq!(dir, fs::canonicalize(&arch).unwrap());
        assert_eq!(commit.len(), 64);
        assert!(commit_name(&PathBuf::from(&commit)).is_ok());
        // Same file, same key; a refresh (new mtime) gives a new one.
        assert_eq!(locate(&arch).unwrap().1, commit);
        let f = fs::File::options().write(true).open(&xml).unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        assert_ne!(locate(&arch).unwrap().1, commit);
    }

    #[test]
    fn an_oci_catalog_that_is_not_a_file_is_an_error() {
        let d = TmpDir::new();
        fs::create_dir_all(d.path().join("appstream.xml.gz")).unwrap();
        assert!(matches!(locate(d.path()), Err(Some(_))));
    }

    #[test]
    fn an_ostree_catalog_needs_a_commit_named_target() {
        let d = TmpDir::new();
        let commit = "b119957b053ed222b6d5750cd10bb105cf9d5f0c62c2a40c7ae7d819729ff3ad";
        fs::create_dir(d.path().join(commit)).unwrap();
        std::os::unix::fs::symlink(commit, d.path().join("active")).unwrap();
        assert_eq!(locate(&d.path().join("active")).unwrap().1, commit);

        fs::create_dir(d.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink("elsewhere", d.path().join("bad")).unwrap();
        assert!(matches!(locate(&d.path().join("bad")), Err(Some(_))));
    }
}
