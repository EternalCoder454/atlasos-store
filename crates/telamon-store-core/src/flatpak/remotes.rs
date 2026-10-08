//! The Sources place, without Qt: list the remotes of both installations,
//! turn one on or off, add one from a `.flatpakrepo` (a file or a link) after
//! a preview, and remove one that nothing is installed from.
//!
//! # Rules
//!
//! - **Reads never ask for privilege** ([`list_remotes`], [`source_users`],
//!   [`fetch_repo`], [`read_repo_file`], [`preview_repo`]). Only the changing
//!   functions ([`set_enabled`], [`add_source`], [`remove_source`]) open the
//!   installation with interaction allowed (polkit for the system one), take
//!   `&OperationLock` as proof the caller holds the locks, and run only after
//!   the user confirmed in the Store's own dialog.
//! - **A file or a link is untrusted.** It is read with the limits of
//!   [`crate::flatpakref`] (256 KiB, known keys only, https only) and what is
//!   shown and added is exactly what was parsed: libflatpak gets the Store's
//!   own rewrite of the file ([`FlatpakRepo::to_bytes`]), never the original.
//! - **A source is added unsigned only on purpose**: [`add_source`] refuses a
//!   file without a GPG key unless the caller passes `allow_unsigned`, which
//!   the dialog sets only after an extra, explicit acknowledgement.
//! - **A source with something installed from it is never removed** (no
//!   force): the error names what is installed, in plain words
//!   ([`blocked_message`]).
//! - Every function here blocks: run it on a worker thread.
//!
//! The system installation is changed through flatpak's polkit helper; the
//! tests here run as root in a container, where libflatpak writes the
//! scratch system installation directly, so they cover the logic for both
//! scopes but never the polkit prompt.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use libflatpak::prelude::*;

use super::installed::ListErrorKind;
use super::lock::OperationLock;
use super::sources::{free_name, journal_clear, reserved_name, update_appstream};
use super::transaction::{RemoteCfg, norm_url, remote_configs};
use super::{
    CancelToken, Error, InstalledRef, RefKind, Scope, from_glib, list_installed, open,
    open_for_change, valid_remote,
};
use crate::flatpakref::{FlatpakRepo, MAX_FILE_BYTES, parse_flatpakrepo, valid_remote_name};
use crate::launch::{self, FileKind, Request, https_url};
use crate::net;
use crate::text;

/// Names shown in a "can't remove" sentence before "and N more".
pub const NAMES_SHOWN: usize = 5;
/// Most names kept in an [`Error::InUse`] from [`remove_source`].
const NAMES_KEPT: usize = 100;
/// How long a source file may take to download.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

// ---- the list ----

/// An app or runtime installed from a remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceUse {
    pub kind: RefKind,
    pub id: String,
    /// The app's name when it has one, else its ID.
    pub name: String,
}

impl SourceUse {
    /// How it reads in a sentence: an app by its name, a runtime by its ID
    /// with "(runtime)" after it.
    pub fn label(&self) -> String {
        match self.kind {
            RefKind::App => self.name.clone(),
            RefKind::Runtime => format!("{} (runtime)", self.id),
        }
    }
}

/// One remote of one installation, as plain data. Every text has been cleaned
/// and capped; the URL had its user name and password removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteInfo {
    pub scope: Scope,
    /// Passed [`valid_remote`].
    pub name: String,
    /// The remote's title, else its name.
    pub title: String,
    /// For display. Compare with [`RemoteInfo::same_url`], not `==`.
    pub url: String,
    pub enabled: bool,
    /// Whether apps from it are checked against a GPG key.
    pub signed: bool,
    /// An OCI registry (Fedora's), which has no GPG key by design.
    pub registry: bool,
    /// A remote of a single app (flatpak's `noenumerate`: the `-origin`
    /// remotes a `.flatpakref` creates). The page folds these away.
    pub single_app: bool,
    pub priority: i32,
    /// What is installed from it: apps first, then runtimes, no add-on that
    /// goes with an app listed here.
    pub installed: Vec<SourceUse>,
}

impl RemoteInfo {
    /// True for a source that is neither signed nor a registry: the page
    /// flags it.
    pub fn unsigned(&self) -> bool {
        !self.signed && !self.registry
    }

    /// Whether this remote's URL is `url`, compared as flatpak remotes are.
    pub fn same_url(&self, url: &str) -> bool {
        norm_url(&self.url) == norm_url(url)
    }

    /// The apps installed from it, by name.
    pub fn apps(&self) -> Vec<&str> {
        self.installed
            .iter()
            .filter(|u| u.kind == RefKind::App)
            .map(|u| u.name.as_str())
            .collect()
    }
}

/// Something that went wrong reading one installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotesError {
    pub scope: Scope,
    /// Plain words; the installation is named in the text.
    pub message: String,
}

/// The remotes of both installations. One installation failing leaves the
/// other's remotes in place; `errors` says which failed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemotesOutcome {
    /// System first, then user; in each, highest priority first.
    pub remotes: Vec<RemoteInfo>,
    pub errors: Vec<RemotesError>,
    /// The token was cancelled; the list is partial.
    pub cancelled: bool,
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::System => "system-wide",
        Scope::User => "user",
    }
}

/// The part of an installed ref that says where it came from.
struct Origin {
    kind: RefKind,
    id: String,
    name: String,
    origin: String,
    scope: Scope,
    related_to: Option<String>,
}

fn origins(refs: &[InstalledRef]) -> Vec<Origin> {
    refs.iter()
        .map(|r| Origin {
            kind: r.kind,
            id: r.id.clone(),
            name: r.name.clone(),
            origin: r.origin.clone(),
            scope: r.scope,
            related_to: r.related_to.clone(),
        })
        .collect()
}

/// What the installed refs of `scope` say about `remote`: the apps and
/// runtimes whose origin it is. An add-on (or locale or debug extension) of an
/// app that is also listed goes with that app and is left out.
fn uses_from(refs: &[Origin], scope: Scope, remote: &str) -> Vec<SourceUse> {
    let from: Vec<&Origin> = refs
        .iter()
        .filter(|r| r.scope == scope && r.origin == remote)
        .collect();
    let app_ids: Vec<&str> = from
        .iter()
        .filter(|r| r.kind == RefKind::App && r.related_to.is_none())
        .map(|r| r.id.as_str())
        .collect();
    let mut uses: Vec<SourceUse> = Vec::new();
    for r in from {
        if let Some(app) = &r.related_to
            && app_ids.contains(&app.as_str())
        {
            continue;
        }
        let name = if r.name.is_empty() {
            r.id.clone()
        } else {
            r.name.clone()
        };
        if !uses.iter().any(|u| u.kind == r.kind && u.id == r.id) {
            uses.push(SourceUse {
                kind: r.kind,
                id: r.id.clone(),
                name,
            });
        }
    }
    uses.sort_by(|a, b| {
        (a.kind, a.name.to_lowercase(), &a.id).cmp(&(b.kind, b.name.to_lowercase(), &b.id))
    });
    uses
}

fn read_scope(
    scope: Scope,
    refs: &[Origin],
    cancel: &CancelToken,
    out: &mut RemotesOutcome,
) -> Result<(), Error> {
    let inst = open(scope)?;
    let raw = inst
        .list_remotes(Some(cancel.cancellable()))
        .map_err(|e| from_glib("list the sources", &e, cancel))?;
    for r in raw.iter() {
        let name = r.name().map(|n| n.to_string()).unwrap_or_default();
        if !valid_remote(&name) {
            out.errors.push(RemotesError {
                scope,
                message: format!(
                    "A source of the {} installation has a name that is not accepted and was left out.",
                    scope_name(scope)
                ),
            });
            continue;
        }
        let title = text::clean(r.title().as_deref().unwrap_or_default(), 200);
        let raw_url = r.url().map(|u| u.to_string()).unwrap_or_default();
        out.remotes.push(RemoteInfo {
            scope,
            title: if title.is_empty() {
                name.clone()
            } else {
                title
            },
            url: super::scrub(&raw_url),
            enabled: !r.is_disabled(),
            signed: r.is_gpg_verify(),
            registry: raw_url.starts_with("oci+"),
            single_app: r.is_noenumerate(),
            priority: r.prio(),
            installed: uses_from(refs, scope, &name),
            name,
        });
    }
    Ok(())
}

/// The remotes of both installations, with what is installed from each. An
/// installation that cannot be read is in `errors` and hides nothing of the
/// other. Never asks for privilege.
///
/// Blocking: run on a worker thread.
pub fn list_remotes(cancel: &CancelToken) -> RemotesOutcome {
    let mut out = RemotesOutcome::default();
    for scope in [Scope::System, Scope::User] {
        if cancel.is_cancelled() {
            out.cancelled = true;
            return out;
        }
        // The installed list is read per installation, so one that cannot
        // be read costs only its own "installed from" lists.
        let listing = list_installed(scope, cancel);
        if listing.cancelled {
            out.cancelled = true;
            return out;
        }
        let mut one = RemotesOutcome::default();
        match read_scope(scope, &origins(&listing.refs), cancel, &mut one) {
            Ok(()) => {
                if let Some(e) = listing
                    .errors
                    .iter()
                    .find(|e| e.kind != ListErrorKind::ExtensionLink)
                {
                    log::warn!(
                        "could not list what is installed ({}): {}",
                        scope.label(),
                        e.message
                    );
                    one.errors.push(RemotesError {
                        scope,
                        message: format!(
                            "Could not check which apps are installed in the {} installation, so the lists of installed apps may be incomplete.",
                            scope_name(scope)
                        ),
                    });
                }
                out.remotes.append(&mut one.remotes);
                out.errors.append(&mut one.errors);
            }
            Err(Error::Cancelled | Error::TimedOut) => {
                out.cancelled = true;
                return out;
            }
            Err(e) => {
                log::warn!("could not read the sources ({}): {e}", scope.label());
                out.errors.push(RemotesError {
                    scope,
                    message: format!(
                        "The {} installation's sources could not be read: {e}",
                        scope_name(scope)
                    ),
                });
            }
        }
    }
    // (libflatpak lists in priority order already; the scopes are joined in
    // the order above, and a stable sort keeps both.)
    out.remotes
        .sort_by_key(|r| (r.scope == Scope::User, -r.priority));
    out
}

// ---- enable and disable ----

/// Turns a remote on or off (`flatpak remote-modify --enable|--disable`).
/// Nothing installed from it is touched; a source that is off is left out of
/// the catalog and of updates' source list but stays configured. In the system
/// installation this goes through flatpak's polkit helper.
///
/// Blocking: run on a worker thread.
pub fn set_enabled(
    scope: Scope,
    name: &str,
    enabled: bool,
    _lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    cancel.check()?;
    if !valid_remote(name) {
        return Err(Error::Invalid("the source name is not valid".into()));
    }
    let inst = open_for_change(scope)?;
    let remote = inst
        .remote_by_name(name, Some(cancel.cancellable()))
        .map_err(|e| from_glib("read the source", &e, cancel))?;
    if remote.is_disabled() == !enabled {
        return Ok(());
    }
    remote.set_disabled(!enabled);
    inst.modify_remote(&remote, Some(cancel.cancellable()))
        .map_err(|e| from_glib("change the source", &e, cancel))
}

// ---- remove ----

/// Everything installed from the remote `name` of `scope`: apps first, then
/// runtimes. Empty means nothing, and the remote may be removed. Not knowing
/// is not permission: an installation or ref that cannot be read is
/// [`Error::CouldNotCheck`] (a failed add-on link alone is not).
///
/// Blocking: run on a worker thread.
pub fn source_users(
    scope: Scope,
    name: &str,
    cancel: &CancelToken,
) -> Result<Vec<SourceUse>, Error> {
    cancel.check()?;
    if !valid_remote(name) {
        return Err(Error::Invalid("the source name is not valid".into()));
    }
    let all = list_installed(scope, cancel);
    if all.cancelled {
        return Err(Error::Cancelled);
    }
    if let Some(e) = all
        .errors
        .iter()
        .find(|e| e.kind != ListErrorKind::ExtensionLink)
    {
        log::warn!(
            "could not list what is installed from {name}: {}",
            e.message
        );
        return Err(Error::CouldNotCheck(
            "what is installed from the source".into(),
        ));
    }
    Ok(uses_from(&origins(&all.refs), scope, name))
}

/// "Can't remove X: these apps are installed from it: A, B, C (and N more).
/// Remove them first." `labels` are [`SourceUse::label`]s (or the names of an
/// [`Error::InUse`] from [`remove_source`]).
pub fn blocked_message(title: &str, labels: &[String]) -> String {
    let runtimes = labels.iter().filter(|l| l.ends_with(" (runtime)")).count();
    let what = if runtimes == 0 {
        "apps"
    } else if runtimes == labels.len() {
        "runtimes"
    } else {
        "apps and runtimes"
    };
    let shown: Vec<&str> = labels
        .iter()
        .take(NAMES_SHOWN)
        .map(String::as_str)
        .collect();
    let more = labels.len().saturating_sub(NAMES_SHOWN);
    let mut s = format!(
        "Can't remove {}: these {what} are installed from it: {}",
        text::clean(title, 100),
        shown.join(", ")
    );
    if more > 0 {
        s.push_str(&format!(" (and {more} more)"));
    }
    s.push_str(". Remove them first.");
    if runtimes == labels.len() {
        s.push_str(" Remove Unused on the Installed page removes runtimes nothing needs.");
    }
    s
}

/// Removes the remote `name`, which must still have the URL `expected_url`
/// (a source that was changed or replaced since the user saw it is left
/// alone). Refused with [`Error::InUse`] (the labels of what is installed
/// from it, see [`blocked_message`]) while an app or runtime is installed
/// from it; there is no force, and libflatpak's own check is the last guard.
/// The pending-sources journal forgets it. In the system installation this
/// goes through flatpak's polkit helper.
///
/// Blocking: run on a worker thread.
pub fn remove_source(
    scope: Scope,
    name: &str,
    expected_url: &str,
    _lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<(), Error> {
    cancel.check()?;
    if !valid_remote(name) {
        return Err(Error::Invalid("the source name is not valid".into()));
    }
    let remotes = remote_configs(scope, cancel)?;
    let Some(cfg) = remotes.iter().find(|r| r.name == name) else {
        return Err(Error::Invalid(
            "that source is not in the list any more".into(),
        ));
    };
    if cfg.url != norm_url(expected_url) {
        return Err(Error::Invalid(
            "that source has changed since it was shown, so it was not removed".into(),
        ));
    }
    let users = source_users(scope, name, cancel)?;
    if !users.is_empty() {
        return Err(Error::InUse(
            users
                .iter()
                .take(NAMES_KEPT)
                .map(SourceUse::label)
                .collect(),
        ));
    }
    cancel.check()?;
    open_for_change(scope)?
        .remove_remote(name, Some(cancel.cancellable()))
        .map_err(|e| from_glib("remove the source", &e, cancel))?;
    if let Err(e) = journal_clear(scope, name, expected_url) {
        log::warn!("could not forget the removed source: {e}");
    }
    Ok(())
}

// ---- add: fetch, read, preview ----

/// Downloads a `.flatpakrepo` from `url` (https, a public address, at most
/// 256 KiB, 15 s; see [`net::get`]) and parses it with the file rules of
/// [`parse_flatpakrepo`]. The token stops the wait.
///
/// Blocking: run on a worker thread.
pub fn fetch_repo(url: &str, cancel: &CancelToken) -> Result<FlatpakRepo, Error> {
    cancel.check()?;
    let url = url.trim();
    let Some(url) = https_url(url) else {
        return Err(Error::Invalid(
            "that is not a secure web address (it has to start with https://)".into(),
        ));
    };
    let (tx, rx) = mpsc::channel();
    let target = url.clone();
    std::thread::Builder::new()
        .name("telamon-store-fetch".into())
        .spawn(move || {
            let request = net::Request {
                accept: "application/x-flatpak-repo, text/plain;q=0.8, */*;q=0.1",
                max_bytes: MAX_FILE_BYTES as u64 + 1,
                timeout: FETCH_TIMEOUT,
            };
            let _ = tx.send(net::get(&target, &request));
        })
        .map_err(|e| Error::Io {
            action: "download the source file",
            message: e.kind().to_string(),
        })?;
    let body = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => break r,
            // The download thread ends by its own timeout.
            Err(mpsc::RecvTimeoutError::Timeout) => cancel.check()?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Io {
                    action: "download the source file",
                    message: "the download stopped".into(),
                });
            }
        }
    };
    let bytes = body.map_err(|e| match e {
        net::NetError::TooLarge => Error::TooLarge("source file"),
        other => Error::Io {
            action: "download the source file",
            message: other.to_string(),
        },
    })?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge("source file"));
    }
    parse_repo_bytes(&bytes)
}

/// The parse of a source file's bytes, as a plain error.
pub fn parse_repo_bytes(bytes: &[u8]) -> Result<FlatpakRepo, Error> {
    parse_flatpakrepo(bytes).map_err(|e| Error::Invalid(e.to_string().to_lowercase_first()))
}

trait LowercaseFirst {
    fn to_lowercase_first(&self) -> String;
}

impl LowercaseFirst for str {
    /// "The file is not ..." as "the file is not ...", for `Error::Invalid`,
    /// which is shown after a lead-in or capitalized again.
    fn to_lowercase_first(&self) -> String {
        let mut c = self.chars();
        match c.next() {
            Some(f) => f.to_lowercase().chain(c).collect(),
            None => String::new(),
        }
    }
}

/// Reads a local `.flatpakrepo`: `path` is an absolute path or a `file:` URL,
/// checked by the launch rules (plain path, no `..`, no hidden characters, the
/// name ends in `.flatpakrepo`), a regular file of at most 256 KiB. Returns
/// the parsed file and the file's name without its extension (a hint for the
/// source's name).
///
/// Blocking: run on a worker thread.
pub fn read_repo_file(path: &str) -> Result<(FlatpakRepo, String), Error> {
    let launch = launch::parse(&[path.trim().to_string()], Path::new(""));
    let file = match launch.requests.as_slice() {
        [Request::File(FileKind::Repo, p)] => p.clone(),
        [Request::File(..)] => {
            return Err(Error::Invalid(
                "that is not a source file (.flatpakrepo)".into(),
            ));
        }
        _ => {
            return Err(Error::Invalid(match launch.refused.first() {
                Some(r) => format!("the file was not accepted: it {}", r.reason),
                None => "that is not a source file (.flatpakrepo)".into(),
            }));
        }
    };
    let io = |e: std::io::Error| Error::Io {
        action: "read the source file",
        message: match e.kind() {
            std::io::ErrorKind::NotFound => "the file does not exist".into(),
            std::io::ErrorKind::PermissionDenied => "permission denied".into(),
            k => k.to_string(),
        },
    };
    // O_NONBLOCK and O_NOCTTY: a named pipe or a terminal must not hang this.
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(&file)
        .map_err(io)?;
    if !f.metadata().map_err(io)?.is_file() {
        return Err(Error::Invalid("that is not a file".into()));
    }
    let mut buf = Vec::new();
    f.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(io)?;
    if buf.len() > MAX_FILE_BYTES {
        return Err(Error::TooLarge("source file"));
    }
    let repo = parse_repo_bytes(&buf)?;
    let stem = file
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    Ok((repo, stem))
}

/// A name for the remote of `repo`, from a hint (a file's name), else the
/// title, else the address: lowercase, `[a-z0-9_.-]`, a well-known name only
/// for that source's own URL. Not yet checked against the existing remotes
/// ([`preview_repo`] numbers it).
pub fn suggest_name(repo: &FlatpakRepo, hint: &str) -> String {
    fn slug(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars().flat_map(char::to_lowercase) {
            let c = if c.is_ascii_alphanumeric() || matches!(c, '_' | '.') {
                c
            } else {
                '-'
            };
            if c == '-' && out.ends_with('-') {
                continue;
            }
            out.push(c);
        }
        let out = out.trim_matches(['-', '.']).replace("..", ".");
        out.chars()
            .take(40)
            .collect::<String>()
            .trim_matches(['-', '.'])
            .to_string()
    }
    let host = repo
        .url
        .strip_prefix("https://")
        .and_then(|r| r.split('/').next())
        .unwrap_or_default();
    let base = [hint, repo.title.as_deref().unwrap_or_default(), host]
        .iter()
        .map(|s| slug(s))
        .find(|s| valid_remote_name(s))
        .unwrap_or_else(|| "source".to_string());
    if reserved_name(&base, &repo.url) {
        format!("{base}-source")
    } else {
        base
    }
}

/// Where a source would go in one installation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// It can be added under this free name.
    Free { name: String },
    /// A remote with this URL is already there (`Error::RemoteExists`).
    Exists { name: String, enabled: bool },
    /// The installation cannot take it: why, in plain words.
    Unavailable(String),
}

/// What the "Add Source" confirmation shows, built from a parsed file before
/// anything is added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePreview {
    /// Exactly what [`add_source`] will add.
    pub repo: FlatpakRepo,
    /// The title, else the address' host.
    pub title: String,
    pub url: String,
    pub comment: Option<String>,
    /// The GPG key's fingerprint, uppercase hex; `None` means unsigned.
    pub fingerprint: Option<String>,
    pub user: Placement,
    pub system: Placement,
}

impl SourcePreview {
    pub fn unsigned(&self) -> bool {
        self.fingerprint.is_none()
    }

    pub fn placement(&self, scope: Scope) -> &Placement {
        match scope {
            Scope::User => &self.user,
            Scope::System => &self.system,
        }
    }
}

/// A fingerprint in groups of four, for reading aloud: `ABCD 1234 ...`.
pub fn group_fingerprint(fp: &str) -> String {
    fp.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn place(
    scope: Scope,
    repo: &FlatpakRepo,
    base: &str,
    cancel: &CancelToken,
) -> Result<Placement, Error> {
    let remotes: Vec<RemoteCfg> = match remote_configs(scope, cancel) {
        Ok(r) => r,
        Err(Error::Cancelled) => return Err(Error::Cancelled),
        Err(Error::TimedOut) => return Err(Error::TimedOut),
        Err(e) => return Ok(Placement::Unavailable(e.to_string())),
    };
    let url = norm_url(&repo.url);
    if let Some(r) = remotes.iter().find(|r| r.url == url) {
        return Ok(Placement::Exists {
            name: r.name.clone(),
            enabled: !r.disabled,
        });
    }
    Ok(match free_name(base, &remotes) {
        Ok(name) => Placement::Free { name },
        Err(e) => Placement::Unavailable(e.to_string()),
    })
}

/// Builds the confirmation for `repo`: the title, address and key to show,
/// and for each installation the free name it would get or the remote that
/// already has the address. `name_hint` is the file's name without its
/// extension (see [`read_repo_file`]), or "". Changes nothing.
///
/// Blocking: run on a worker thread.
pub fn preview_repo(
    repo: FlatpakRepo,
    name_hint: &str,
    cancel: &CancelToken,
) -> Result<SourcePreview, Error> {
    cancel.check()?;
    let base = suggest_name(&repo, name_hint);
    let user = place(Scope::User, &repo, &base, cancel)?;
    let system = place(Scope::System, &repo, &base, cancel)?;
    let url = norm_url(&repo.url);
    let host = url
        .strip_prefix("https://")
        .and_then(|r| r.split('/').next())
        .unwrap_or_default()
        .to_string();
    let title = repo
        .title
        .as_deref()
        .map(|t| text::clean(t, 200))
        .filter(|t| !t.is_empty())
        .unwrap_or(host);
    Ok(SourcePreview {
        title,
        url,
        comment: repo
            .comment
            .as_deref()
            .map(|c| text::clean(c, 300))
            .filter(|c| !c.is_empty()),
        fingerprint: repo.key.as_ref().map(|k| k.fingerprint().to_string()),
        user,
        system,
        repo,
    })
}

// ---- add ----

/// What [`add_source`] did.
#[derive(Debug, PartialEq, Eq)]
pub struct AddOutcome {
    /// The name the source was added under.
    pub name: String,
    /// Why its app list could not be downloaded just now, if it could not.
    /// The source is added anyway; the catalog refreshes it later.
    pub appstream: Option<Error>,
}

/// Adds `repo` as the remote `name` of `scope`, then downloads its app list
/// (`update_appstream`) so its apps show up. Refused: a file without a GPG
/// key unless `allow_unsigned` (the dialog sets it only after an extra
/// acknowledgement), a URL that is not https, a name that is taken
/// ([`Error::RemoteNameTaken`]) or reserved, and a URL that is already a
/// remote under any name ([`Error::RemoteExists`]); nothing is overwritten.
/// A download that fails or is cancelled after the source was added does not
/// undo it: see [`AddOutcome::appstream`].
///
/// Blocking: run on a worker thread.
pub fn add_source(
    scope: Scope,
    repo: &FlatpakRepo,
    name: &str,
    allow_unsigned: bool,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<AddOutcome, Error> {
    if repo.key.is_none() && !allow_unsigned {
        return Err(Error::Invalid(
            "this source is not signed, and that was not accepted".into(),
        ));
    }
    if https_url(&repo.url).is_none() {
        return Err(Error::Invalid(
            "the source's URL is not an https address".into(),
        ));
    }
    let bytes = repo
        .to_bytes()
        .map_err(|e| Error::Invalid(e.to_string().to_lowercase_first()))?;
    add_source_bytes(scope, &norm_url(&repo.url), &bytes, name, lock, cancel)
}

/// The libflatpak-facing part of [`add_source`] (tests call it with a
/// `file://` remote, which the URL policy refuses).
pub(crate) fn add_source_bytes(
    scope: Scope,
    url: &str,
    bytes: &[u8],
    name: &str,
    lock: &OperationLock,
    cancel: &CancelToken,
) -> Result<AddOutcome, Error> {
    super::sources::add_remote_bytes(scope, url, bytes, name, lock, cancel)?;
    // Interactive: the user just confirmed this source in the Store's own dialog.
    let appstream = match update_appstream(scope, name, true, lock, cancel) {
        Ok(()) => None,
        Err(e) => {
            log::warn!("the app list of the new source {name} could not be downloaded: {e}");
            Some(e)
        }
    };
    Ok(AddOutcome {
        name: name.to_string(),
        appstream,
    })
}

#[cfg(test)]
mod tests {
    use super::super::testenv::*;
    use super::*;
    use crate::flatpakref::GpgKey;

    fn app(id: &str, name: &str, origin: &str, scope: Scope) -> Origin {
        Origin {
            kind: RefKind::App,
            id: id.into(),
            name: name.into(),
            origin: origin.into(),
            scope,
            related_to: None,
        }
    }

    fn runtime(id: &str, origin: &str, scope: Scope, related: Option<&str>) -> Origin {
        Origin {
            kind: RefKind::Runtime,
            related_to: related.map(str::to_string),
            ..app(id, "", origin, scope)
        }
    }

    #[test]
    fn uses_are_by_remote_and_scope_apps_first_and_addons_go_with_their_app() {
        let refs = vec![
            app("org.b.Beta", "Beta", "r", Scope::User),
            app("org.a.Alpha", "alpha", "r", Scope::User),
            app("org.other.App", "Other", "other", Scope::User),
            app("org.sys.App", "Sys", "r", Scope::System),
            runtime("org.fd.Platform", "r", Scope::User, None),
            runtime("org.a.Alpha.Locale", "r", Scope::User, Some("org.a.Alpha")),
            // An add-on whose app comes from elsewhere still counts.
            runtime("org.x.Addon", "r", Scope::User, Some("org.other.App")),
        ];
        let u = uses_from(&refs, Scope::User, "r");
        let labels: Vec<String> = u.iter().map(SourceUse::label).collect();
        assert_eq!(
            labels,
            vec![
                "alpha",
                "Beta",
                "org.fd.Platform (runtime)",
                "org.x.Addon (runtime)"
            ]
        );
        assert_eq!(uses_from(&refs, Scope::System, "r").len(), 1);
        assert!(uses_from(&refs, Scope::User, "nothing").is_empty());
        // The same ref on two branches is listed once.
        let two = vec![
            app("org.a.Alpha", "Alpha", "r", Scope::User),
            app("org.a.Alpha", "Alpha", "r", Scope::User),
        ];
        assert_eq!(uses_from(&two, Scope::User, "r").len(), 1);
    }

    #[test]
    fn the_blocked_sentence_names_apps_and_caps_them() {
        let one = vec!["Hello".to_string()];
        assert_eq!(
            blocked_message("Test", &one),
            "Can't remove Test: these apps are installed from it: Hello. Remove them first."
        );
        let many: Vec<String> = (1..=8).map(|i| format!("App{i}")).collect();
        let s = blocked_message("Test", &many);
        assert!(
            s.contains("App1, App2, App3, App4, App5 (and 3 more). Remove them first."),
            "{s}"
        );
        assert!(!s.contains("App6"), "{s}");
        let mixed = vec!["Hello".to_string(), "org.fd.Platform (runtime)".to_string()];
        assert!(blocked_message("T", &mixed).contains("these apps and runtimes are installed"),);
        let rt = vec!["org.fd.Platform (runtime)".to_string()];
        let s = blocked_message("T", &rt);
        assert!(s.contains("these runtimes are installed") && s.contains("Remove Unused"));
        // Hostile titles are cleaned.
        assert!(!blocked_message("a\u{202e}b\n", &one).contains('\u{202e}'));
    }

    fn repo(url: &str, title: Option<&str>, key: bool) -> FlatpakRepo {
        FlatpakRepo {
            url: url.into(),
            title: title.map(str::to_string),
            comment: None,
            description: None,
            icon: None,
            homepage: None,
            default_branch: None,
            key: key.then(|| {
                GpgKey::from_base64(include_str!("../../tests/fixtures/flatpakref/ed.b64").trim())
                    .unwrap()
            }),
            collection_id: None,
            deploy_collection_id: None,
        }
    }

    #[test]
    fn names_are_suggested_from_the_hint_the_title_then_the_address() {
        let r = repo("https://dl.example.org/x/repo", Some("My Apps!"), true);
        assert_eq!(suggest_name(&r, "tools"), "tools");
        assert_eq!(suggest_name(&r, ""), "my-apps");
        assert_eq!(suggest_name(&r, "--"), "my-apps");
        let r = repo("https://dl.example.org/x/repo", None, true);
        assert_eq!(suggest_name(&r, ""), "dl.example.org");
        let r = repo("https://dl.example.org/x/repo", Some("???"), true);
        assert_eq!(suggest_name(&r, ""), "dl.example.org");
        // Hostile hints stay inside the remote-name alphabet.
        let n = suggest_name(&r, "../../Etc/Pass wd");
        assert!(valid_remote_name(&n), "{n}");
        // A well-known name only for that source's own address.
        let own = repo("https://dl.flathub.org/repo", Some("Flathub"), true);
        assert_eq!(suggest_name(&own, "flathub"), "flathub");
        let fake = repo("https://evil.example.org/repo", Some("Flathub"), true);
        assert_eq!(suggest_name(&fake, "flathub"), "flathub-source");
        assert_eq!(suggest_name(&fake, ""), "flathub-source");
    }

    #[test]
    fn fingerprints_are_grouped_in_fours() {
        assert_eq!(group_fingerprint("ABCD1234EF"), "ABCD 1234 EF");
        assert_eq!(group_fingerprint(""), "");
    }

    #[test]
    fn a_link_must_be_https_and_nothing_is_fetched_otherwise() {
        let c = CancelToken::new();
        for bad in [
            "http://example.org/a.flatpakrepo",
            "ftp://example.org/a.flatpakrepo",
            "file:///etc/passwd",
            "https://127.0.0.1/a.flatpakrepo",
            "https://localhost/a.flatpakrepo",
            "https://user:pw@example.org/a.flatpakrepo",
            "example.org/a.flatpakrepo",
            "",
            "https://exa mple.org/",
        ] {
            let e = fetch_repo(bad, &c).unwrap_err();
            assert!(matches!(e, Error::Invalid(_)), "{bad}: {e:?}");
        }
        let gone = CancelToken::new();
        gone.cancel();
        assert_eq!(
            fetch_repo("https://example.org/a.flatpakrepo", &gone).unwrap_err(),
            Error::Cancelled
        );
    }

    #[test]
    fn a_source_without_a_key_needs_the_acknowledgement() {
        let Some((_dir, _g)) = guard() else { return };
        let lock = OperationLock::try_acquire().unwrap();
        let c = CancelToken::new();
        let unsigned = repo("https://dl.example.org/repo", Some("U"), false);
        let e = add_source(Scope::User, &unsigned, "u", false, &lock, &c).unwrap_err();
        assert!(
            matches!(e, Error::Invalid(ref s) if s.contains("not signed")),
            "{e:?}"
        );
        // The https rule holds with the acknowledgement too.
        let mut http = unsigned.clone();
        http.url = "http://dl.example.org/repo".into();
        assert!(matches!(
            add_source(Scope::User, &http, "u", true, &lock, &c).unwrap_err(),
            Error::Invalid(_)
        ));
        assert!(!list_remotes(&c).remotes.iter().any(|r| r.name == "u"));
    }

    #[test]
    fn the_preview_numbers_taken_names_and_knows_the_existing_address() {
        let Some((dir, _g)) = guard() else { return };
        reset(&dir);
        let c = CancelToken::new();
        // A remote named like the suggestion, another address.
        let r = repo("https://dl.example.org/x/repo", Some("Test"), true);
        let p = preview_repo(r.clone(), "test", &c).unwrap();
        assert_eq!(
            p.user,
            Placement::Free {
                name: "test-2".into()
            }
        );
        assert!(matches!(p.system, Placement::Free { .. }));
        assert_eq!(p.title, "Test");
        assert_eq!(p.url, "https://dl.example.org/x/repo");
        assert_eq!(
            p.fingerprint.as_deref(),
            r.key.as_ref().map(|k| k.fingerprint())
        );
        assert!(!p.unsigned());
        // The title falls back to the host; no key is "unsigned".
        let bare = repo("https://dl.example.org/x/repo/", None, false);
        let p = preview_repo(bare, "", &c).unwrap();
        assert_eq!(p.title, "dl.example.org");
        assert!(p.unsigned() && p.fingerprint.is_none());
        // An address that is already a remote is said, whatever the name.
        must(&[
            "remote-add",
            "--no-gpg-verify",
            "--disable",
            "known",
            "https://dl.example.org/known/repo/",
        ]);
        let known = repo("https://dl.example.org/known/repo", Some("Other"), true);
        let p = preview_repo(known, "other", &c).unwrap();
        assert_eq!(
            p.user,
            Placement::Exists {
                name: "known".into(),
                enabled: false
            }
        );
        let gone = CancelToken::new();
        gone.cancel();
        assert_eq!(preview_repo(r, "", &gone).unwrap_err(), Error::Cancelled);
    }

    #[test]
    fn adding_makes_the_remote_and_its_app_list_and_never_overwrites() {
        let Some((dir, _g)) = guard() else { return };
        reset_empty();
        let c = CancelToken::new();
        let lock = OperationLock::try_acquire().unwrap();
        let url = format!("file://{}/repo", dir.display());
        let signed = format!(
            "[Flatpak Repo]\nUrl={url}\nTitle=Added\n{}\n",
            gpg_line(&dir, "test.flatpakrepo")
        )
        .into_bytes();
        let out = add_source_bytes(Scope::User, &url, &signed, "added", &lock, &c).unwrap();
        assert_eq!(out.name, "added");
        assert!(out.appstream.is_none(), "{:?}", out.appstream);
        let listed = list_remotes(&c);
        let r = listed.remotes.iter().find(|r| r.name == "added").unwrap();
        assert!(r.enabled && r.signed && !r.single_app && r.title == "Added");
        assert_eq!(r.scope, Scope::User);
        // Same address under any name, same name for another address: refused.
        assert_eq!(
            add_source_bytes(Scope::User, &url, &signed, "again", &lock, &c).unwrap_err(),
            Error::RemoteExists("added".into())
        );
        let other = b"[Flatpak Repo]\nUrl=file:///elsewhere\n".to_vec();
        assert_eq!(
            add_source_bytes(Scope::User, "file:///elsewhere", &other, "ADDED", &lock, &c)
                .unwrap_err(),
            Error::RemoteNameTaken("ADDED".into())
        );
        // An unsigned file makes an unverified remote, flagged.
        let unsigned = b"[Flatpak Repo]\nUrl=file:///nokey\nTitle=No Key\n".to_vec();
        let out =
            add_source_bytes(Scope::User, "file:///nokey", &unsigned, "nokey", &lock, &c).unwrap();
        // (no repository there: the app list can't be downloaded, the source stays)
        assert!(out.appstream.is_some());
        let listed = list_remotes(&c);
        let r = listed.remotes.iter().find(|r| r.name == "nokey").unwrap();
        assert!(!r.signed && r.unsigned() && r.enabled);
        reset_empty();
    }
}
