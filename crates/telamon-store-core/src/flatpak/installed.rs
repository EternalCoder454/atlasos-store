//! What is installed, per installation, as plain data.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use libflatpak::prelude::*;

use super::{CancelToken, Error, Scope};
use crate::text;

/// Largest deployed `metadata` read (1 MiB).
pub const METADATA_MAX: usize = 1 << 20;

const NAME_MAX_CHARS: usize = 200;
const SUMMARY_MAX_CHARS: usize = 300;
const VERSION_MAX_CHARS: usize = 64;
const EOL_MAX_CHARS: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefKind {
    App,
    Runtime,
}

/// One installed ref. Every text field has passed its check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledRef {
    pub kind: RefKind,
    pub id: String,
    pub arch: String,
    pub branch: String,
    /// The remote it came from; empty for a sideloaded or removed remote.
    pub origin: String,
    pub scope: Scope,
    /// The deployed commit (hex).
    pub commit: String,
    /// Size on disk, in bytes.
    pub installed_size: u64,
    /// From the app's AppStream data: empty when it has none.
    pub name: String,
    pub summary: String,
    pub version: String,
    /// Whether this branch is the app's current one (apps only).
    pub is_current: bool,
    /// End-of-life reason, if the remote marked the ref so.
    pub eol: Option<String>,
    /// The ref that replaces this one, if any.
    pub eol_rebase: Option<String>,
    /// For an extension (add-on, locale, debug): the ID of the installed app
    /// libflatpak lists it as related to.
    pub related_to: Option<String>,
    deploy_dir: Option<PathBuf>,
    /// The installation's own directory, which `deploy_dir` must be under.
    root: Option<PathBuf>,
}

impl InstalledRef {
    /// `app/ID/arch/branch` or `runtime/ID/arch/branch`.
    pub fn full_ref(&self) -> String {
        let kind = match self.kind {
            RefKind::App => "app",
            RefKind::Runtime => "runtime",
        };
        format!("{kind}/{}/{}/{}", self.id, self.arch, self.branch)
    }

    /// The deployed `metadata` file, read now (not kept), at most
    /// [`METADATA_MAX`] bytes: for `Permissions::from_metadata`. The file is
    /// not followed through a symlink and must be a regular file, and the
    /// deploy directory must (after canonicalizing both) be inside the
    /// installation's own directory.
    ///
    /// It reads the commit deployed when the ref was listed: if the ref was
    /// updated or removed since, that directory's name no longer matches
    /// `commit` (or it is gone) and this is an error, so list again.
    ///
    /// Blocking file read: run on a worker thread.
    pub fn metadata(&self, cancel: &CancelToken) -> Result<Vec<u8>, Error> {
        cancel.check()?;
        let dir = self
            .deploy_dir
            .as_ref()
            .ok_or_else(|| Error::Invalid("the ref has no deployed files".into()))?;
        let root = self
            .root
            .as_ref()
            .and_then(|r| std::fs::canonicalize(r).ok())
            .ok_or_else(|| Error::Invalid("the installation folder is unknown".into()))?;
        let dir = std::fs::canonicalize(dir).map_err(|_| Error::Stale)?;
        if !dir.starts_with(&root) {
            return Err(Error::Invalid(
                "the deployed files are outside the installation".into(),
            ));
        }
        if dir.file_name().and_then(|n| n.to_str()) != Some(self.commit.as_str()) {
            return Err(Error::Stale);
        }
        let io = |e: std::io::Error| Error::Io {
            action: "read the installed metadata",
            message: match e.raw_os_error() {
                Some(libc::ELOOP) => "it is a symbolic link".to_string(),
                _ => e.kind().to_string(),
            },
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(dir.join("metadata"))
            .map_err(io)?;
        if !file.metadata().map_err(io)?.is_file() {
            return Err(Error::Invalid(
                "the installed metadata is not a regular file".into(),
            ));
        }
        let mut buf = Vec::new();
        file.take(METADATA_MAX as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(io)?;
        if buf.len() > METADATA_MAX {
            return Err(Error::TooLarge("installed metadata"));
        }
        Ok(buf)
    }
}

/// What kind of problem a listing had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListErrorKind {
    /// The installation could not be opened or read.
    ScopeUnreadable,
    /// A ref failed its checks and was skipped.
    BadRef,
    /// The add-ons of an app could not be listed.
    ExtensionLink,
    /// More problems than are kept; the message says how many.
    TooMany,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListError {
    pub scope: Scope,
    pub kind: ListErrorKind,
    pub message: String,
    /// For a [`ListErrorKind::BadRef`]: what kind the skipped ref is, or
    /// `None` when even that could not be told. `None` for every other kind.
    pub ref_kind: Option<RefKind>,
}

/// At most this many errors are kept, then one "N more" entry (an
/// unreadable installation is always kept, on top of the cap).
pub const MAX_LIST_ERRORS: usize = 50;

/// The refs of one or both installations. One installation failing leaves
/// the other's refs in place; `errors` says which failed and why (a bad ref
/// is skipped and listed here too). `cancelled` is set when the token was
/// cancelled: the refs are then partial, and no error says so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstalledOutcome {
    pub refs: Vec<InstalledRef>,
    pub errors: Vec<ListError>,
    pub cancelled: bool,
    more: usize,
}

impl InstalledOutcome {
    fn push(&mut self, scope: Scope, kind: ListErrorKind, message: String) {
        self.push_ref(scope, kind, message, None);
    }

    fn push_ref(
        &mut self,
        scope: Scope,
        kind: ListErrorKind,
        message: String,
        ref_kind: Option<RefKind>,
    ) {
        // An unreadable installation is never folded into the "N more"
        // count: what checks the installation needs it by name.
        if self.errors.len() < MAX_LIST_ERRORS || kind == ListErrorKind::ScopeUnreadable {
            self.errors.push(ListError {
                scope,
                kind,
                message,
                ref_kind,
            });
        } else {
            self.more += 1;
        }
    }

    /// Adds the "N more" entry for errors beyond the cap.
    fn finish(&mut self, scope: Scope) {
        if self.more > 0 {
            let n = std::mem::take(&mut self.more);
            self.errors.push(ListError {
                scope,
                kind: ListErrorKind::TooMany,
                message: format!("{n} more problems were not listed"),
                ref_kind: None,
            });
        }
    }
}

fn opt(s: Option<libflatpak::glib::GString>, max: usize) -> Option<String> {
    s.map(|s| text::clean(&s, max)).filter(|s| !s.is_empty())
}

/// Checks and copies one ref from libflatpak.
fn convert(
    scope: Scope,
    root: &Option<PathBuf>,
    r: &libflatpak::InstalledRef,
) -> Result<InstalledRef, (Option<RefKind>, String)> {
    let kind = match r.kind() {
        libflatpak::RefKind::App => RefKind::App,
        libflatpak::RefKind::Runtime => RefKind::Runtime,
        _ => return Err((None, "a ref of an unknown kind was skipped".into())),
    };
    let bad = |why: String| Err((Some(kind), why));
    let id = r.name().map(|s| s.to_string()).unwrap_or_default();
    if !text::valid_id(&id) {
        return bad("a ref with an invalid ID was skipped".into());
    }
    let arch = r.arch().map(|s| s.to_string()).unwrap_or_default();
    let branch = r.branch().map(|s| s.to_string()).unwrap_or_default();
    let origin = r.origin().map(|s| s.to_string()).unwrap_or_default();
    let commit = r.commit().map(|s| s.to_string()).unwrap_or_default();
    // Not echoed: the values are untrusted.
    if !super::valid_arch(&arch) || !super::valid_branch(&branch) {
        return bad(format!(
            "{id} has an invalid architecture or branch and was skipped"
        ));
    }
    if !origin.is_empty() && !super::valid_remote(&origin) {
        return bad(format!("{id} has an invalid remote name and was skipped"));
    }
    if !super::valid_commit(&commit) {
        return bad(format!("{id} has an invalid commit and was skipped"));
    }
    let deploy_dir = r
        .deploy_dir()
        .map(|s| PathBuf::from(s.as_str()))
        .filter(|p| p.is_absolute());
    Ok(InstalledRef {
        kind,
        id,
        arch,
        branch,
        origin,
        scope,
        commit,
        installed_size: r.installed_size(),
        name: opt(r.appdata_name(), NAME_MAX_CHARS).unwrap_or_default(),
        summary: opt(r.appdata_summary(), SUMMARY_MAX_CHARS).unwrap_or_default(),
        version: opt(r.appdata_version(), VERSION_MAX_CHARS).unwrap_or_default(),
        is_current: kind == RefKind::App && r.is_current(),
        eol: opt(r.eol(), EOL_MAX_CHARS),
        eol_rebase: opt(r.eol_rebase(), EOL_MAX_CHARS),
        related_to: None,
        deploy_dir,
        root: root.clone(),
    })
}

fn install_root(inst: &libflatpak::Installation) -> Option<PathBuf> {
    inst.path().and_then(|f| f.path())
}

fn convert_all(
    scope: Scope,
    root: &Option<PathBuf>,
    raw: &[libflatpak::InstalledRef],
    out: &mut InstalledOutcome,
) -> Vec<InstalledRef> {
    let mut refs = Vec::with_capacity(raw.len());
    for r in raw {
        match convert(scope, root, r) {
            Ok(r) => refs.push(r),
            Err((kind, e)) => out.push_ref(scope, ListErrorKind::BadRef, e, kind),
        }
    }
    refs.sort_by(|a, b| {
        (a.kind, &a.id, &a.arch, &a.branch).cmp(&(b.kind, &b.id, &b.arch, &b.branch))
    });
    refs
}

/// Whether the installation's folder is missing: that is an empty scope (the
/// user installation is created on first use and many systems have no system
/// installation), not an error. A folder that exists but can't be read is
/// still an error.
fn is_missing(root: &Option<PathBuf>) -> bool {
    match root {
        Some(p) => {
            matches!(std::fs::metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        }
        None => false,
    }
}

/// Lists one installation's refs, with `related_to` set on each extension of
/// an installed app (from libflatpak's related refs, one call per app; an
/// extension of two apps is linked to the first in name order). A failure is
/// in `errors`, never a panic; a cancelled token sets `cancelled`. A missing
/// installation is an empty one.
///
/// Blocking: run on a worker thread.
pub fn list_installed(scope: Scope, cancel: &CancelToken) -> InstalledOutcome {
    let mut out = list_installed_raw(scope, cancel);
    out.finish(scope);
    out
}

/// [`list_installed`] without the "N more" entry, so that two listings can be
/// merged into one count.
fn list_installed_raw(scope: Scope, cancel: &CancelToken) -> InstalledOutcome {
    let mut out = InstalledOutcome::default();
    if cancel.is_cancelled() {
        out.cancelled = true;
        return out;
    }
    let inst = match super::open(scope) {
        Ok(i) => i,
        Err(e) => {
            out.push(scope, ListErrorKind::ScopeUnreadable, e.to_string());
            return out;
        }
    };
    let root = install_root(&inst);
    if is_missing(&root) {
        log::debug!(
            "the {} installation does not exist: nothing installed",
            scope.label()
        );
        return out;
    }
    // libflatpak treats a folder it cannot open as empty: look ourselves.
    if let Some(p) = &root
        && let Err(e) = std::fs::read_dir(p)
    {
        let why = match e.kind() {
            std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
            k => k.to_string(),
        };
        out.push(
            scope,
            ListErrorKind::ScopeUnreadable,
            format!(
                "the {} installation's folder cannot be read ({why})",
                scope.label()
            ),
        );
        return out;
    }
    let raw = match inst.list_installed_refs(Some(cancel.cancellable())) {
        Ok(r) => r,
        Err(e) => {
            match super::from_glib("list the installed apps", &e, cancel) {
                Error::Cancelled | Error::TimedOut => out.cancelled = true,
                other => {
                    if is_missing(&root) {
                        log::debug!("the {} installation does not exist", scope.label());
                    } else {
                        out.push(scope, ListErrorKind::ScopeUnreadable, other.to_string());
                    }
                }
            }
            return out;
        }
    };
    out.refs = convert_all(scope, &root, &raw, &mut out);
    link_extensions(&inst, scope, cancel, &mut out);
    out
}

/// Sets `related_to` from each installed app's related refs (the first app,
/// in name order, that lists an extension keeps it).
fn link_extensions(
    inst: &libflatpak::Installation,
    scope: Scope,
    cancel: &CancelToken,
    out: &mut InstalledOutcome,
) {
    let apps: Vec<(String, String, String)> = out
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::App && !r.origin.is_empty())
        .map(|r| (r.id.clone(), r.origin.clone(), r.full_ref()))
        .collect();
    let index: HashMap<(RefKind, String, String, String), usize> = out
        .refs
        .iter()
        .enumerate()
        .map(|(i, r)| ((r.kind, r.id.clone(), r.arch.clone(), r.branch.clone()), i))
        .collect();
    for (app_id, origin, full) in apps {
        let related =
            match inst.list_installed_related_refs_sync(&origin, &full, Some(cancel.cancellable()))
            {
                Ok(r) => r,
                Err(e) => {
                    match super::from_glib("list the add-ons of an app", &e, cancel) {
                        Error::Cancelled | Error::TimedOut => {
                            out.cancelled = true;
                            return;
                        }
                        err => out.push(
                            scope,
                            ListErrorKind::ExtensionLink,
                            format!("{app_id}: {err}"),
                        ),
                    }
                    continue;
                }
            };
        for rel in &related {
            let kind = match rel.kind() {
                libflatpak::RefKind::App => RefKind::App,
                libflatpak::RefKind::Runtime => RefKind::Runtime,
                _ => continue,
            };
            let (Some(name), Some(arch), Some(branch)) = (rel.name(), rel.arch(), rel.branch())
            else {
                continue;
            };
            if let Some(&i) =
                index.get(&(kind, name.to_string(), arch.to_string(), branch.to_string()))
                && out.refs[i].id != app_id
                && out.refs[i].related_to.is_none()
            {
                out.refs[i].related_to = Some(app_id.clone());
            }
        }
    }
}

/// Both installations. Each one's failure is in `errors` and the other's
/// refs are still returned. When the token is cancelled it returns right
/// after the scope that saw it, with `cancelled` set once.
///
/// Blocking: run on a worker thread.
pub fn list_installed_all(cancel: &CancelToken) -> InstalledOutcome {
    let system = list_installed_raw(Scope::System, cancel);
    if system.cancelled {
        let mut system = system;
        system.finish(Scope::System);
        return system;
    }
    let user = list_installed_raw(Scope::User, cancel);
    let mut all = merge(system, user);
    all.finish(Scope::User);
    all
}

/// Joins two unfinished listings: refs and errors in order, the error cap
/// applied to the sum, and the errors beyond it counted together (`finish`
/// then adds one "N more" entry with the total).
fn merge(mut all: InstalledOutcome, user: InstalledOutcome) -> InstalledOutcome {
    all.refs.extend(user.refs);
    for e in user.errors {
        all.push_ref(e.scope, e.kind, e.message, e.ref_kind);
    }
    all.more += user.more;
    all.cancelled = user.cancelled;
    all
}

/// An app found by [`find_app_everywhere`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppInstance {
    /// `user`, `system` (the default one) or the other system installation's
    /// ID.
    pub installation: String,
    /// `app/ID/arch/branch`.
    pub full_ref: String,
}

/// Every installed ref of the app `app_id`, in the default system
/// installation, the user installation and every other system installation
/// libflatpak knows (`/etc/flatpak/installations.d`). Read-only, no
/// interaction. An installation that exists but cannot be read is an error,
/// not "not installed": the answer must not be a guess.
///
/// Blocking: run on a worker thread.
pub(crate) fn find_app_everywhere(
    app_id: &str,
    cancel: &CancelToken,
) -> Result<Vec<AppInstance>, Error> {
    cancel.check()?;
    if !text::valid_id(app_id) {
        return Err(Error::Invalid("the app ID is not valid".into()));
    }
    let mut all: Vec<(String, libflatpak::Installation)> = vec![
        ("system".into(), super::open(Scope::System)?),
        ("user".into(), super::open(Scope::User)?),
    ];
    let others = libflatpak::system_installations(Some(cancel.cancellable()))
        .map_err(|e| super::from_glib("list the system installations", &e, cancel))?;
    for inst in others {
        inst.set_no_interaction(true);
        let id = inst
            .id()
            .map(|s| text::clean(&s, 64))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "system".into());
        all.push((id, inst));
    }
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut found = Vec::new();
    for (label, inst) in all {
        cancel.check()?;
        let root = install_root(&inst);
        if is_missing(&root) {
            continue;
        }
        if let Some(p) = &root {
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            if seen.contains(&canon) {
                continue;
            }
            seen.push(canon);
            if let Err(e) = std::fs::read_dir(p) {
                return Err(Error::Io {
                    action: "look for the app",
                    message: format!("the {label} installation cannot be read ({})", e.kind()),
                });
            }
        }
        let refs = inst
            .list_installed_refs_by_kind(libflatpak::RefKind::App, Some(cancel.cancellable()))
            .map_err(|e| super::from_glib("list the installed apps", &e, cancel))?;
        for r in refs {
            if r.name().as_deref() != Some(app_id) {
                continue;
            }
            let (arch, branch) = (
                r.arch().map(|s| s.to_string()).unwrap_or_default(),
                r.branch().map(|s| s.to_string()).unwrap_or_default(),
            );
            if !super::valid_arch(&arch) || !super::valid_branch(&branch) {
                continue;
            }
            found.push(AppInstance {
                installation: label.clone(),
                full_ref: format!("app/{app_id}/{arch}/{branch}"),
            });
        }
    }
    Ok(found)
}

/// Runtimes (and extensions) that nothing installed needs any more
/// (`flatpak uninstall --unused`): libflatpak's `list_unused_refs` for the
/// installation, minus every runtime that an installed app or runtime in
/// either installation still uses (libflatpak only looks at one
/// installation, so a user app that uses a system runtime would not count).
/// This is exactly the list `uninstall_unused` accepts. Not removed here. If
/// the installed refs cannot all be read the error is
/// [`Error::CouldNotCheck`]: not knowing is not permission.
///
/// Blocking: run on a worker thread.
pub fn list_unused(scope: Scope, cancel: &CancelToken) -> Result<Vec<InstalledRef>, Error> {
    let raw = list_unused_raw(scope, cancel)?;
    if raw.is_empty() {
        return Ok(raw);
    }
    let deps = super::transaction::scan_deps(cancel)?;
    Ok(super::transaction::split_unused(raw, &deps).0)
}

/// libflatpak's unused list for one installation, as it gives it.
pub(super) fn list_unused_raw(
    scope: Scope,
    cancel: &CancelToken,
) -> Result<Vec<InstalledRef>, Error> {
    cancel.check()?;
    let inst = super::open(scope)?;
    let raw = inst
        .list_unused_refs(None, Some(cancel.cancellable()))
        .map_err(|e| super::from_glib("list the unused runtimes", &e, cancel))?;
    let mut out = InstalledOutcome::default();
    let refs = convert_all(scope, &install_root(&inst), &raw, &mut out);
    // A bad ref among them is skipped; the rest are still useful, but never
    // silently: log what was dropped.
    for e in &out.errors {
        log::warn!("unused refs: {}", e.message);
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flatpak::testenv::scratch;
    use std::os::unix::fs::symlink;

    /// A ref whose deployment is `<root>/app/x/<commit>`; returns it and the dir.
    fn fake(name: &str) -> Option<(InstalledRef, PathBuf, PathBuf)> {
        let root = scratch(name)?;
        let commit = "ab".repeat(32);
        let dir = root.join("app/org.test.Hello/x86_64/stable").join(&commit);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("metadata"),
            b"[Application]\nname=org.test.Hello\n",
        )
        .unwrap();
        let r = InstalledRef {
            kind: RefKind::App,
            id: "org.test.Hello".into(),
            arch: "x86_64".into(),
            branch: "stable".into(),
            origin: "test".into(),
            scope: Scope::User,
            commit,
            installed_size: 1,
            name: String::new(),
            summary: String::new(),
            version: String::new(),
            is_current: true,
            eol: None,
            eol_rebase: None,
            related_to: None,
            deploy_dir: Some(dir.clone()),
            root: Some(root.clone()),
        };
        Some((r, root, dir))
    }

    fn runtime(id: &str) -> InstalledRef {
        InstalledRef {
            kind: RefKind::Runtime,
            id: id.into(),
            arch: "x86_64".into(),
            branch: "1".into(),
            origin: "test".into(),
            scope: Scope::System,
            commit: "ab".repeat(32),
            installed_size: 1,
            name: String::new(),
            summary: String::new(),
            version: String::new(),
            is_current: false,
            eol: None,
            eol_rebase: None,
            related_to: None,
            deploy_dir: None,
            root: None,
        }
    }

    #[test]
    fn what_another_installation_uses_is_not_unused_and_the_filter_settles() {
        use crate::flatpak::transaction::{Dep, split_unused};
        let dep = |r: &str, uses: &[&str]| Dep {
            full_ref: r.into(),
            ext_of: None,
            uses: uses.iter().map(|s| s.to_string()).collect(),
        };
        let (a, b, c, d) = (
            runtime("org.t.A"),
            runtime("org.t.B"),
            runtime("org.t.C"),
            runtime("org.t.D"),
        );
        let fr = |r: &InstalledRef| r.full_ref();
        // A user app uses A. B is only used by a runtime that is itself
        // dropped (A), C is used by nothing, D by an app in the other
        // installation and by the unused C.
        let deps = vec![
            dep("app/org.t.App/x86_64/1", &["org.t.A/x86_64/1"]),
            dep(&fr(&a), &["org.t.B/x86_64/1"]),
            dep(&fr(&b), &[]),
            dep(&fr(&c), &["org.t.D/x86_64/1"]),
            dep(&fr(&d), &[]),
        ];
        let (keep, used) = split_unused(vec![a.clone(), b.clone(), c.clone()], &deps);
        assert_eq!(keep.iter().map(fr).collect::<Vec<_>>(), vec![fr(&c)]);
        let used: Vec<String> = used.into_iter().map(|(r, _)| r).collect();
        assert_eq!(used, vec![fr(&a), fr(&b)]);
        // Nothing uses anything: everything stays listed, in order.
        let (keep, used) = split_unused(vec![a.clone(), b.clone()], &[]);
        assert_eq!(keep.len(), 2);
        assert!(used.is_empty());
        // An app (not a runtime) in the list is left alone.
        let mut app = a.clone();
        app.kind = RefKind::App;
        let (keep, _) = split_unused(vec![app], &deps);
        assert_eq!(keep.len(), 1);
    }

    #[test]
    fn metadata_is_read_and_capped() {
        let Some((r, root, dir)) = fake("md-ok") else {
            return;
        };
        let c = CancelToken::new();
        assert!(r.metadata(&c).unwrap().starts_with(b"[Application]"));
        std::fs::write(dir.join("metadata"), vec![b'x'; METADATA_MAX]).unwrap();
        assert_eq!(r.metadata(&c).unwrap().len(), METADATA_MAX);
        std::fs::write(dir.join("metadata"), vec![b'x'; METADATA_MAX + 1]).unwrap();
        assert_eq!(
            r.metadata(&c).unwrap_err(),
            Error::TooLarge("installed metadata")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_refuses_links_and_non_files() {
        let Some((r, root, dir)) = fake("md-link") else {
            return;
        };
        let c = CancelToken::new();
        let real = dir.join("real");
        std::fs::write(&real, b"x").unwrap();
        std::fs::remove_file(dir.join("metadata")).unwrap();
        symlink(&real, dir.join("metadata")).unwrap();
        assert!(matches!(r.metadata(&c).unwrap_err(), Error::Io { .. }));
        std::fs::remove_file(dir.join("metadata")).unwrap();
        std::fs::create_dir(dir.join("metadata")).unwrap();
        assert!(matches!(
            r.metadata(&c).unwrap_err(),
            Error::Invalid(_) | Error::Io { .. }
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_notices_a_ref_that_changed_or_lies_outside() {
        let Some((mut r, root, dir)) = fake("md-stale") else {
            return;
        };
        let c = CancelToken::new();
        // The commit changed since listing.
        let mut other = r.clone();
        other.commit = "cd".repeat(32);
        assert_eq!(other.metadata(&c).unwrap_err(), Error::Stale);
        // A deploy dir outside the installation.
        let out = scratch("md-outside").unwrap();
        let odir = out.join(&r.commit);
        std::fs::create_dir_all(&odir).unwrap();
        std::fs::write(odir.join("metadata"), b"x").unwrap();
        let mut o = r.clone();
        o.deploy_dir = Some(odir);
        assert!(matches!(o.metadata(&c).unwrap_err(), Error::Invalid(_)));
        // No deploy dir known, or no installation folder.
        o.deploy_dir = None;
        assert!(matches!(o.metadata(&c).unwrap_err(), Error::Invalid(_)));
        let mut n = r.clone();
        n.root = None;
        assert!(matches!(n.metadata(&c).unwrap_err(), Error::Invalid(_)));
        // The deployment was removed.
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(r.metadata(&c).unwrap_err(), Error::Stale);
        r.deploy_dir = Some(dir);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn errors_are_capped_with_a_count() {
        let mut o = InstalledOutcome::default();
        for i in 0..(MAX_LIST_ERRORS + 7) {
            o.push(Scope::User, ListErrorKind::BadRef, format!("e{i}"));
        }
        o.finish(Scope::User);
        assert_eq!(o.errors.len(), MAX_LIST_ERRORS + 1);
        let last = o.errors.last().unwrap();
        assert_eq!(last.kind, ListErrorKind::TooMany);
        assert!(last.message.starts_with("7 more"));
    }

    #[test]
    fn too_many_errors_across_scopes_make_one_total() {
        let mk = |scope, n| {
            let mut o = InstalledOutcome::default();
            for i in 0..n {
                o.push(scope, ListErrorKind::BadRef, format!("e{i}"));
            }
            o
        };
        // 60 + 70 errors: 50 kept, 80 counted, one entry.
        let mut all = merge(mk(Scope::System, 60), mk(Scope::User, 70));
        all.finish(Scope::User);
        assert_eq!(all.errors.len(), MAX_LIST_ERRORS + 1);
        let more: Vec<_> = all
            .errors
            .iter()
            .filter(|e| e.kind == ListErrorKind::TooMany)
            .collect();
        assert_eq!(more.len(), 1);
        assert!(
            more[0].message.starts_with("80 more"),
            "{}",
            more[0].message
        );
        // Under the cap in one, over in the other.
        let mut all = merge(mk(Scope::System, 10), mk(Scope::User, 55));
        all.finish(Scope::User);
        assert_eq!(all.errors.len(), MAX_LIST_ERRORS + 1);
        assert!(all.errors.last().unwrap().message.starts_with("15 more"));
        // Under the cap overall: no entry.
        let mut all = merge(mk(Scope::System, 10), mk(Scope::User, 10));
        all.finish(Scope::User);
        assert_eq!(all.errors.len(), 20);
    }

    #[test]
    fn finds_an_app_in_every_installation_and_nothing_for_others() {
        let Some((dir, _g)) = crate::flatpak::testenv::guard() else {
            return;
        };
        use crate::flatpak::testenv::{must, reset};
        reset(&dir);
        let c = CancelToken::new();
        assert!(
            find_app_everywhere("org.test.Hello", &c)
                .unwrap()
                .is_empty()
        );
        must(&[
            "install",
            "-y",
            "--noninteractive",
            "test",
            "app/org.test.Hello/x86_64/stable",
        ]);
        let f = find_app_everywhere("org.test.Hello", &c).unwrap();
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].installation, "user");
        assert!(f[0].full_ref.starts_with("app/org.test.Hello/"));
        assert!(
            find_app_everywhere("org.test.Other", &c)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            find_app_everywhere("../x", &c).unwrap_err(),
            Error::Invalid(_)
        ));
        let gone = CancelToken::new();
        gone.cancel();
        assert_eq!(
            find_app_everywhere("org.test.Hello", &gone).unwrap_err(),
            Error::Cancelled
        );
        reset(&dir);
    }

    /// libflatpak reads FLATPAK_SYSTEM_DIR once per process, so these cases
    /// run in a child process (this test binary, one test) with it set.
    const CASE: &str = "TELAMON_STORE_SYSDIR_CASE";

    #[test]
    fn system_dir_child() {
        let Ok(case) = std::env::var(CASE) else {
            return;
        };
        let c = CancelToken::new();
        match case.as_str() {
            "missing" => {
                let out = list_installed(Scope::System, &c);
                assert!(out.errors.is_empty(), "{:?}", out.errors);
                assert!(out.refs.is_empty() && !out.cancelled);
                let all = list_installed_all(&c);
                assert!(all.errors.is_empty(), "{:?}", all.errors);
            }
            "locked" => {
                // Only this thread loses root's permission overrides.
                assert!(
                    crate::flatpak::testenv::drop_dac_caps(),
                    "could not drop caps"
                );
                let out = list_installed(Scope::System, &c);
                assert_eq!(out.errors.len(), 1, "{:?}", out.errors);
                assert_eq!(out.errors[0].kind, ListErrorKind::ScopeUnreadable);
                assert_eq!(out.errors[0].scope, Scope::System);
                assert!(out.refs.is_empty());
            }
            other => panic!("unknown case {other}"),
        }
    }

    #[test]
    fn a_missing_system_installation_is_empty_and_an_unreadable_one_is_an_error() {
        let Some((_dir, _g)) = crate::flatpak::testenv::guard() else {
            return;
        };
        let Some(tmp) = scratch("sysdir") else { return };
        let locked = tmp.join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o0))
            .unwrap();
        let exe = std::env::current_exe().unwrap();
        for (case, dir) in [
            ("missing", tmp.join("does-not-exist")),
            ("locked", locked.join("inner")),
        ] {
            let out = std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "flatpak::installed::tests::system_dir_child",
                    "--nocapture",
                ])
                .env(CASE, case)
                .env("FLATPAK_SYSTEM_DIR", &dir)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&out.stdout).to_string()
                + &String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.success() && text.contains("1 passed"),
                "{case}: {text}"
            );
        }
        std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
