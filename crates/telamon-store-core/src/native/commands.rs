//! A native app's commands on the user's `PATH`: `~/.local/bin/<name>`, a
//! link to `$XDG_DATA_HOME/telamon-apps/<id>/current/bin/<name>`. The link
//! goes through `current`, so an update switches it with the version and the
//! link itself never changes.
//!
//! `~/.local/bin` comes before `/usr/bin` on `PATH`, so a name there wins over
//! the system's. Hence the rules, all of which skip a command (logged), never
//! fail an install:
//!
//! - the name carries the app's identity: the last part of its ID (`gates`),
//!   or `telamon-` and it, alone or followed by `-` (`telamon-gates`,
//!   `gates-cli`), as a plain name ([`name_ok`]);
//! - nothing of that name is in any other folder of `PATH` or the system's
//!   (`/usr/bin`...): a command is never shadowed;
//! - nothing is replaced in `~/.local/bin` but a link that already points at
//!   this app's place in `telamon-apps` (the Store's own): a file, a folder or
//!   another link there is the user's, or another app's, and stays;
//! - a link is removed (uninstall, or a command an update dropped) only while
//!   it still points at this app's place.

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use super::APPS_DIR;
use super::dirfd::{Dir, Kind};
use super::install::Dirs;

/// Most commands one app may put on `PATH`.
pub const MAX_COMMANDS: usize = 16;

/// A plain file name a command can have: ASCII letters, digits, `.`, `_`,
/// `-` and `+`, not starting with `.` or `-`, at most 64 bytes.
pub fn plain_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with(['.', '-'])
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
}

/// Whether app `id` may have a command called `name` on `PATH`: a plain name
/// that is the last part of the ID, or `telamon-` and it, alone or followed
/// by `-` and more (case does not matter).
pub fn name_ok(id: &str, name: &str) -> bool {
    let Some(last) = id.rsplit('.').next().filter(|l| !l.is_empty()) else {
        return false;
    };
    let last = last.to_ascii_lowercase();
    let name_l = name.to_ascii_lowercase();
    plain_name(name)
        && [last.clone(), format!("telamon-{last}")]
            .iter()
            .any(|stem| name_l == *stem || name_l.starts_with(&format!("{stem}-")))
}

/// Where the link of `name` points.
fn target(dirs: &Dirs, id: &str, name: &str) -> String {
    dirs.app(id)
        .join("current/bin")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// Whether a link to `to` is the Store's link of `name` for app `id`: an
/// absolute path ending in `telamon-apps/<id>/current/bin/<name>` (the data
/// folder may have moved since it was made).
fn is_ours(to: &str, id: &str, name: &str) -> bool {
    to.starts_with('/') && to.ends_with(&format!("/{APPS_DIR}/{id}/current/bin/{name}"))
}

/// `~/.local/bin`, opened (a link there is followed: dotfile managers make
/// them), made 0755 when `create` and missing; it must be this user's.
fn open_bin(dirs: &Dirs, create: bool) -> std::io::Result<Dir> {
    let bin = dirs.bin();
    if create {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&bin)?;
    }
    let dir = Dir::open_following(&bin)?;
    dir.require_owner()?;
    Ok(dir)
}

/// The folder of `PATH` (or the system's) that already has a `name`, if any.
fn shadowed<'a>(dirs: &'a Dirs, bin: &Dir, name: &str) -> Option<&'a Path> {
    dirs.path
        .iter()
        .filter(|p| !bin.is_same_dir(p))
        .find(|p| p.join(name).symlink_metadata().is_ok())
        .map(|p| p.as_path())
}

/// Links each of `names` of app `id` into `~/.local/bin`, by the rules above;
/// returns the names that are linked now (made, or already there), or
/// `None` when `~/.local/bin` itself could not be made or used.
pub(crate) fn link(dirs: &Dirs, id: &str, names: &[String]) -> Option<Vec<String>> {
    let mut linked = Vec::new();
    if names.is_empty() {
        return Some(linked);
    }
    let bin = match open_bin(dirs, true) {
        Ok(b) => b,
        Err(e) => {
            log::warn!(
                "{id}: no command put on PATH, {} can't be used: {e}",
                dirs.bin().display()
            );
            return None;
        }
    };
    for name in names {
        if !name_ok(id, name) {
            log::warn!("{id}: the command {name:?} is not named after the app; not put on PATH");
            continue;
        }
        if let Some(there) = shadowed(dirs, &bin, name) {
            log::warn!(
                "{id}: {name} is already a command in {}; not put on PATH",
                there.display()
            );
            continue;
        }
        let want = target(dirs, id, name);
        let shown = dirs.bin().join(name);
        match bin.stat_opt(name) {
            Ok(None) => match bin.symlink(&want, name) {
                Ok(()) => linked.push(name.clone()),
                Err(e) => log::warn!("{id}: could not make {}: {e}", shown.display()),
            },
            Ok(Some(m)) if m.kind == Kind::Link => match bin.read_link(name) {
                Ok(to) if to == want => linked.push(name.clone()),
                Ok(to) if is_ours(&to, id, name) => {
                    // The data folder moved: point it at the new place.
                    let tmp = super::dirfd::temp_name(name);
                    let swapped = bin
                        .symlink(&want, &tmp)
                        .and_then(|()| bin.rename(&tmp, &bin, name));
                    match swapped {
                        Ok(()) => linked.push(name.clone()),
                        Err(e) => {
                            let _ = bin.unlink(&tmp);
                            log::warn!("{id}: could not update {}: {e}", shown.display());
                        }
                    }
                }
                Ok(_) => log::warn!(
                    "{id}: {} is a link the Store didn't make for this app; left alone",
                    shown.display()
                ),
                Err(e) => log::warn!("{id}: could not read {}: {e}", shown.display()),
            },
            Ok(Some(_)) => log::warn!(
                "{id}: {} is already there and isn't the Store's; left alone",
                shown.display()
            ),
            Err(e) => log::warn!("{id}: could not check {}: {e}", shown.display()),
        }
    }
    Some(linked)
}

/// Removes the links of `names` that are still the Store's for app `id`;
/// anything else of those names is left alone.
pub(crate) fn unlink(dirs: &Dirs, id: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let Ok(bin) = open_bin(dirs, false) else {
        return;
    };
    for name in names {
        if !plain_name(name) {
            continue;
        }
        let ours = bin.stat_opt(name).is_ok_and(|m| {
            m.is_some_and(|m| m.kind == Kind::Link)
                && bin.read_link(name).is_ok_and(|to| is_ours(&to, id, name))
        });
        if ours && let Err(e) = bin.unlink(name) {
            log::warn!(
                "{id}: could not remove {}: {e}",
                dirs.bin().join(name).display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_the_app() {
        let id = "net.eterneon.telamon.gates";
        for ok in [
            "gates",
            "telamon-gates",
            "gates-cli",
            "telamon-gates-cli",
            "Gates",
        ] {
            assert!(name_ok(id, ok), "{ok}");
        }
        for bad in [
            "",
            "sudo",
            "ls",
            "gatesx",
            "telamon",
            "telamon-gatesx",
            "x-gates",
            "gates/../sudo",
            "-gates",
            ".gates",
            "gates cli",
            "gates\n",
            &format!("gates-{}", "x".repeat(64)),
        ] {
            assert!(!name_ok(id, bad), "{bad:?}");
        }
    }

    #[test]
    fn our_links_are_recognized_by_where_they_point() {
        let id = "net.eterneon.telamon.gates";
        let n = "telamon-gates";
        assert!(is_ours(
            "/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current/bin/telamon-gates",
            id,
            n
        ));
        assert!(!is_ours(
            "telamon-apps/net.eterneon.telamon.gates/current/bin/telamon-gates",
            id,
            n
        ));
        assert!(!is_ours(
            "/home/u/.local/share/telamon-apps/net.eterneon.telamon.other/current/bin/telamon-gates",
            id,
            n
        ));
        assert!(!is_ours("/usr/bin/telamon-gates", id, n));
    }
}
