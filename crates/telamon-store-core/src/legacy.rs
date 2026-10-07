//! The names before the rename to Telamon (`atlas-store`).
//!
//! Until 0.2.0 the Store kept its catalog cache in
//! `$XDG_CACHE_HOME/atlas-store` and its pending-sources journal in
//! `$XDG_STATE_HOME/atlas-store`. The first run of the renamed app moves such
//! a folder to its new name, once: one `renameat2(RENAME_NOREPLACE)`, which is
//! atomic and never replaces anything, so a folder that exists under the new
//! name wins and the old one is left alone. Nothing is copied, and nothing is
//! read from the old name afterwards. (The settings file, the crash state and
//! the notification choices are moved by the framework itself.)

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The old name of every folder the Store owns.
pub const OLD_NAME: &str = "atlas-store";
/// The new one.
pub const NAME: &str = "telamon-store";

/// Moves `base/atlas-store` to `base/telamon-store`. `Ok(true)` when it
/// moved, `Ok(false)` when there was nothing to do (no old folder, it is not a
/// plain folder, or the new one exists already). A link or a file under the
/// old name is not moved: the folders are checked to be plain folders of ours
/// where they are used, and a link would be refused there anyway.
pub fn move_once(base: &Path) -> io::Result<bool> {
    let old = base.join(OLD_NAME);
    match std::fs::symlink_metadata(&old) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }
    let c = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other);
    let (from, to) = (c(&old)?, c(&base.join(NAME))?);
    // SAFETY: both are NUL-terminated paths that outlive the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        // The new name exists (a folder, an empty one too): leave both.
        Some(libc::EEXIST | libc::ENOTEMPTY) => Ok(false),
        _ => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmpdir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("telamon-store-legacy-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn moves_the_folder_with_its_files_once() {
        let base = tmpdir("move");
        fs::create_dir_all(base.join(OLD_NAME).join("user")).unwrap();
        fs::write(base.join(OLD_NAME).join("user/index.bin"), "cache").unwrap();
        fs::write(base.join(OLD_NAME).join("pending-remotes"), "j").unwrap();

        assert!(move_once(&base).unwrap());
        assert!(!base.join(OLD_NAME).exists());
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("user/index.bin")).unwrap(),
            "cache"
        );
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("pending-remotes")).unwrap(),
            "j"
        );
        assert!(
            !move_once(&base).unwrap(),
            "the second run has nothing to move"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn nothing_to_move_is_not_an_error() {
        let base = tmpdir("none");
        assert!(!move_once(&base).unwrap());
        assert!(!move_once(&base.join("missing")).unwrap());
        assert!(!base.join(NAME).exists());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn the_new_folder_wins_and_the_old_one_is_kept() {
        let base = tmpdir("both");
        fs::create_dir_all(base.join(OLD_NAME)).unwrap();
        fs::write(base.join(OLD_NAME).join("f"), "old").unwrap();
        fs::create_dir_all(base.join(NAME)).unwrap();
        fs::write(base.join(NAME).join("f"), "new").unwrap();

        assert!(!move_once(&base).unwrap());
        assert_eq!(
            fs::read_to_string(base.join(NAME).join("f")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(base.join(OLD_NAME).join("f")).unwrap(),
            "old"
        );
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_link_or_a_file_under_the_old_name_stays() {
        let base = tmpdir("link");
        let target = tmpdir("link-target");
        std::os::unix::fs::symlink(&target, base.join(OLD_NAME)).unwrap();
        assert!(!move_once(&base).unwrap());
        assert!(base.join(OLD_NAME).symlink_metadata().is_ok());
        assert!(!base.join(NAME).exists());
        fs::remove_file(base.join(OLD_NAME)).unwrap();
        fs::write(base.join(OLD_NAME), "x").unwrap();
        assert!(!move_once(&base).unwrap());
        fs::remove_dir_all(&base).unwrap();
        fs::remove_dir_all(&target).unwrap();
    }
}
