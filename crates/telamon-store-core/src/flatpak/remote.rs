//! What a remote says about a ref, before anything is installed.

use libflatpak::prelude::*;

use super::supervise::run_supervised;
use super::{CancelToken, Error, RefKind, Scope};
use crate::text;

/// Largest metadata accepted from a remote (1 MiB).
const REMOTE_METADATA_MAX: usize = 1 << 20;

/// The facts the install dialog shows. All text has passed its check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteRefInfo {
    pub kind: RefKind,
    pub id: String,
    pub arch: String,
    pub branch: String,
    pub remote: String,
    pub commit: String,
    pub download_size: u64,
    pub installed_size: u64,
    pub eol: Option<String>,
    pub eol_rebase: Option<String>,
    /// The ref's `metadata` file (for `Permissions::from_metadata`), at most
    /// 1 MiB.
    pub metadata: Vec<u8>,
}

/// Splits `app/ID/arch/branch` or `runtime/ID/arch/branch`, checking each part.
fn parse_ref(s: &str) -> Result<(RefKind, &str, &str, &str), Error> {
    let bad =
        || Error::Invalid("the ref is not app/ID/arch/branch or runtime/ID/arch/branch".into());
    let mut p = s.split('/');
    let (Some(kind), Some(id), Some(arch), Some(branch), None) =
        (p.next(), p.next(), p.next(), p.next(), p.next())
    else {
        return Err(bad());
    };
    let kind = match kind {
        "app" => RefKind::App,
        "runtime" => RefKind::Runtime,
        _ => return Err(bad()),
    };
    if !text::valid_id(id) || !super::valid_arch(arch) || !super::valid_branch(branch) {
        return Err(bad());
    }
    Ok((kind, id, arch, branch))
}

/// Asks `remote` about `ref_` (`app/ID/arch/branch`): download and installed
/// size, commit, end of life and the metadata. It reads from the remote (a
/// network request) and installs nothing, but libflatpak may write its own
/// summary cache while doing so. The 1 MiB cap bounds the Store's copy of the
/// metadata only, not what libflatpak downloads.
///
/// It runs on a thread of its own under the stall watchdog (see
/// `supervise`): a request that gets no answer for 300 seconds is cancelled
/// and ends as [`Error::TimedOut`]. `cancel` stops it sooner.
///
/// Blocking: run on a worker thread.
pub fn remote_ref_info(
    scope: Scope,
    remote: &str,
    ref_: &str,
    cancel: &CancelToken,
) -> Result<RemoteRefInfo, Error> {
    cancel.check()?;
    if !super::valid_remote(remote) {
        return Err(Error::Invalid("the remote name is not valid".into()));
    }
    let (kind, id, arch, branch) = parse_ref(ref_)?;
    let (res, _timed_out) = run_supervised(cancel, &mut |_| {}, |_tx| {
        fetch(scope, remote, (kind, id, arch, branch), cancel)
    })?;
    res
}

/// The blocking part of [`remote_ref_info`], run on the supervised thread.
fn fetch(
    scope: Scope,
    remote: &str,
    (kind, id, arch, branch): (RefKind, &str, &str, &str),
    cancel: &CancelToken,
) -> Result<RemoteRefInfo, Error> {
    let inst = super::open(scope)?;
    let fp_kind = match kind {
        RefKind::App => libflatpak::RefKind::App,
        RefKind::Runtime => libflatpak::RefKind::Runtime,
    };
    let r = inst
        .fetch_remote_ref_sync(
            remote,
            fp_kind,
            id,
            Some(arch),
            Some(branch),
            Some(cancel.cancellable()),
        )
        .map_err(|e| super::from_glib("read the app's details from the remote", &e, cancel))?;
    let commit = r.commit().map(|s| s.to_string()).unwrap_or_default();
    if !super::valid_commit(&commit) {
        return Err(Error::Invalid("the remote sent an invalid commit".into()));
    }
    let metadata = match r.metadata() {
        Some(b) if b.len() > REMOTE_METADATA_MAX => return Err(Error::TooLarge("remote metadata")),
        Some(b) => b.as_ref().to_vec(),
        None => Vec::new(),
    };
    let clean = |s: Option<libflatpak::glib::GString>| {
        s.map(|s| text::clean(&s, 300)).filter(|s| !s.is_empty())
    };
    Ok(RemoteRefInfo {
        kind,
        id: id.to_string(),
        arch: arch.to_string(),
        branch: branch.to_string(),
        remote: remote.to_string(),
        commit,
        download_size: r.download_size(),
        installed_size: r.installed_size(),
        eol: clean(r.eol()),
        eol_rebase: clean(r.eol_rebase()),
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_checked() {
        assert!(parse_ref("app/org.test.Hello/x86_64/stable").is_ok());
        assert!(parse_ref("runtime/org.test.Platform/x86_64/23.08").is_ok());
        assert!(parse_ref("app/org.test.Hello/x86_64").is_err());
        assert!(parse_ref("app/org.test.Hello/x86_64/stable/x").is_err());
        assert!(parse_ref("file/org.test.Hello/x86_64/stable").is_err());
        assert!(parse_ref("app/../x86_64/stable").is_err());
        assert!(parse_ref("app/nodots/x86_64/stable").is_err());
    }
}
