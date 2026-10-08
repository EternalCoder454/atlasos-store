//! The Sources layer: listing remotes of both installations, turning them on
//! and off, removing them, and reading and previewing a source file. The
//! flatpak CLI is test setup only; the module under test never shells out.
//! The parts that need the local test remote only run when
//! `TELAMON_STORE_TEST_REMOTE` names a built test dir and every Flatpak and XDG
//! directory (HOME too) is under /work/, so a real installation is never
//! opened. The system installation is a scratch folder under /work and the
//! tests run as root, where libflatpak writes it directly: the logic is
//! covered for both scopes, flatpak's polkit helper is not. The file reader
//! needs no installation and always runs.

mod common;

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::*;
use telamon_store_core::catalog::list_sources;
use telamon_store_core::flatpak::{
    CancelToken, Error, OperationLock, Placement, RefKind, RemoteInfo, Scope, add_source,
    blocked_message, fetch_repo, list_remotes, preview_repo, read_repo_file, remove_source,
    set_enabled, source_users,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/flatpakref")
        .join(name)
}

fn scratch_dir(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("remotes-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn find<'a>(remotes: &'a [RemoteInfo], scope: Scope, name: &str) -> &'a RemoteInfo {
    remotes
        .iter()
        .find(|r| r.scope == scope && r.name == name)
        .unwrap_or_else(|| panic!("no {name} in {remotes:?}"))
}

// ---- the file reader (no installation needed) ----

#[test]
fn a_source_file_is_read_with_the_launch_and_file_rules() {
    let signed = fixture("test.flatpakrepo");
    let (repo, stem) = read_repo_file(signed.to_str().unwrap()).unwrap();
    assert_eq!(repo.url, "https://dl.example.org/test/repo/");
    assert_eq!(repo.title.as_deref(), Some("Telamon Store Test"));
    assert!(repo.key.is_some());
    assert_eq!(stem, "test");

    // A file: URL (what a file dialog returns) is the same file.
    let url = format!("file://{}", signed.display());
    assert_eq!(read_repo_file(&url).unwrap().0, repo);
    assert_eq!(read_repo_file(&format!("  {url}\n")).unwrap().0, repo);

    let (unsigned, _) = read_repo_file(fixture("unsigned.flatpakrepo").to_str().unwrap()).unwrap();
    assert!(unsigned.key.is_none());
}

#[test]
fn a_source_file_that_breaks_a_rule_is_refused_in_plain_words() {
    let bad = |p: &str| match read_repo_file(p).unwrap_err() {
        Error::Invalid(s) => s,
        other => panic!("{p}: {other:?}"),
    };
    // Launch rules: absolute and plain, a .flatpakrepo, no hidden characters.
    assert!(bad("relative/a.flatpakrepo").contains("not accepted"));
    assert!(bad("/tmp/../etc/a.flatpakrepo").contains(".."));
    assert!(bad("/tmp//a.flatpakrepo").contains("plain"));
    assert!(bad("/tmp/a.txt").contains("not accepted"));
    assert!(bad("/tmp/a\u{202e}.flatpakrepo").contains("hidden"));
    assert!(bad("https://example.org/a.flatpakrepo").contains("not accepted"));
    // Another kind of file the Store opens is not a source.
    assert!(bad("/tmp/a.flatpakref").contains("not a source file"));
    assert!(bad("").contains("not a source file") || bad("").contains("not accepted"));
    // Content rules of the parser: a local Filter is refused.
    let e = read_repo_file(fixture("filter.flatpakrepo").to_str().unwrap()).unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "{e:?}");
    // A ref file is not a repo file, whatever its name.
    let d = scratch_dir("reader");
    let wrong = d.join("a.flatpakrepo");
    std::fs::copy(fixture("hello.flatpakref"), &wrong).unwrap();
    assert!(matches!(
        read_repo_file(wrong.to_str().unwrap()).unwrap_err(),
        Error::Invalid(_)
    ));
}

#[test]
fn a_source_file_must_be_a_small_regular_file() {
    let d = scratch_dir("limits");
    // Missing.
    let e = read_repo_file(d.join("none.flatpakrepo").to_str().unwrap()).unwrap_err();
    assert!(matches!(e, Error::Io { .. }), "{e:?}");
    // A folder named like one.
    let folder = d.join("dir.flatpakrepo");
    std::fs::create_dir(&folder).unwrap();
    assert!(matches!(
        read_repo_file(folder.to_str().unwrap()).unwrap_err(),
        Error::Invalid(_)
    ));
    // Too large: one byte over the cap.
    let big = d.join("big.flatpakrepo");
    let mut body = std::fs::read(fixture("test.flatpakrepo")).unwrap();
    body.resize(telamon_store_core::flatpakref::MAX_FILE_BYTES + 1, b'#');
    std::fs::write(&big, &body).unwrap();
    assert_eq!(
        read_repo_file(big.to_str().unwrap()).unwrap_err(),
        Error::TooLarge("source file")
    );
    // Exactly at the cap is read (and judged by the parser, not the size).
    body.truncate(telamon_store_core::flatpakref::MAX_FILE_BYTES);
    std::fs::write(&big, &body).unwrap();
    assert!(!matches!(
        read_repo_file(big.to_str().unwrap()),
        Err(Error::TooLarge(_))
    ));
    // A named pipe must not hang the reader.
    let fifo = d.join("pipe.flatpakrepo");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        read_repo_file(fifo.to_str().unwrap()).unwrap_err(),
        Error::Invalid(_)
    ));
    // A link to a real file is the user's choice and is read.
    let link = d.join("link.flatpakrepo");
    symlink(fixture("test.flatpakrepo"), &link).unwrap();
    assert!(read_repo_file(link.to_str().unwrap()).is_ok());
}

#[test]
fn a_link_that_is_not_https_never_connects() {
    let c = CancelToken::new();
    for bad in [
        "http://dl.example.org/a.flatpakrepo",
        "https://192.168.1.1/a.flatpakrepo",
        "https://localhost/a.flatpakrepo",
        "file:///etc/passwd",
        "dl.example.org/a.flatpakrepo",
        "",
    ] {
        assert!(
            matches!(fetch_repo(bad, &c).unwrap_err(), Error::Invalid(_)),
            "{bad}"
        );
    }
}

// ---- remotes (the local test remote) ----

#[test]
fn the_list_shows_each_remote_with_what_is_installed_from_it() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let out = list_remotes(&c);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert!(!out.cancelled);
    let t = find(&out.remotes, Scope::User, "test");
    assert!(t.enabled && t.signed && !t.unsigned() && !t.single_app && !t.registry);
    // (flatpak reads the title from the repository itself.)
    assert!(!t.title.is_empty());
    assert!(
        t.url.starts_with("file://") && t.url.contains("/repo"),
        "{}",
        t.url
    );
    let kinds: Vec<(RefKind, &str)> = t
        .installed
        .iter()
        .map(|u| (u.kind, u.id.as_str()))
        .collect();
    // The app, then the runtime; the add-on goes with its app.
    assert_eq!(
        kinds,
        vec![(RefKind::App, APP), (RefKind::Runtime, RUNTIME)],
        "{:?}",
        t.installed
    );
    assert_eq!(t.apps().len(), 1);
    // The system installation is empty and hides nothing.
    assert!(out.remotes.iter().all(|r| r.scope == Scope::User));
}

#[test]
fn flags_for_unsigned_single_app_and_off_sources() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let url = format!("file://{}/repo/", dir.display());
    let _c1 = RemoteCleanup("plain");
    let _c2 = RemoteCleanup("one-origin");
    must(&["remote-add", "--no-gpg-verify", "plain", &url]);
    must(&["remote-modify", "--title=Plain Title", "plain"]);
    must(&[
        "remote-add",
        "--no-gpg-verify",
        "--no-enumerate",
        "one-origin",
        &url,
    ]);
    must(&["remote-modify", "--prio=7", "plain"]);
    must(&["remote-modify", "--disable", "plain"]);
    let c = CancelToken::new();
    let out = list_remotes(&c);
    let plain = find(&out.remotes, Scope::User, "plain");
    assert_eq!(plain.title, "Plain Title");
    assert!(!plain.enabled && plain.unsigned() && !plain.single_app);
    assert_eq!(plain.priority, 7);
    let one = find(&out.remotes, Scope::User, "one-origin");
    assert!(one.single_app && one.enabled);
    assert!(find(&out.remotes, Scope::User, "test").signed);
    // Highest priority first within the installation.
    let order: Vec<&str> = out.remotes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(order[0], "plain", "{order:?}");
    assert!(plain.same_url(&format!("file://{}/repo", dir.display())));
}

#[test]
fn turning_a_source_off_and_on_is_seen_by_the_catalog() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let catalog_has = || list_sources(&c).sources.iter().any(|s| s.remote == "test");
    assert!(catalog_has());
    set_enabled(Scope::User, "test", false, &lock, &c).unwrap();
    assert!(!find(&list_remotes(&c).remotes, Scope::User, "test").enabled);
    assert!(!catalog_has(), "a disabled source feeds the catalog");
    // Again is fine, and nothing else changed.
    set_enabled(Scope::User, "test", false, &lock, &c).unwrap();
    set_enabled(Scope::User, "test", true, &lock, &c).unwrap();
    let t = list_remotes(&c);
    let t = find(&t.remotes, Scope::User, "test");
    assert!(t.enabled && t.signed);
    assert!(catalog_has());

    // Guards.
    assert!(matches!(
        set_enabled(Scope::User, "../x", false, &lock, &c).unwrap_err(),
        Error::Invalid(_)
    ));
    assert!(matches!(
        set_enabled(Scope::User, "nosuch", false, &lock, &c).unwrap_err(),
        Error::Flatpak { .. }
    ));
    let gone = CancelToken::new();
    gone.cancel();
    assert_eq!(
        set_enabled(Scope::User, "test", false, &lock, &gone).unwrap_err(),
        Error::Cancelled
    );
    assert!(find(&list_remotes(&c).remotes, Scope::User, "test").enabled);
}

#[test]
fn a_source_with_apps_installed_is_not_removed_and_the_error_names_them() {
    let Some((dir, _g)) = remote() else { return };
    with_app(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let url = format!("file://{}/repo/", dir.display());

    let e = remove_source(Scope::User, "test", &url, &lock, &c).unwrap_err();
    let Error::InUse(labels) = e else {
        panic!("{e:?}")
    };
    assert!(
        labels
            .iter()
            .any(|l| l.contains("Hello") || l.contains(APP)),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == &format!("{RUNTIME} (runtime)")),
        "{labels:?}"
    );
    let s = blocked_message("Test Source", &labels);
    assert!(
        s.starts_with("Can't remove Test Source: these apps and runtimes are installed from it: "),
        "{s}"
    );
    assert!(s.ends_with("Remove them first."), "{s}");
    // Nothing changed.
    assert!(list_remotes(&c).remotes.iter().any(|r| r.name == "test"));
    assert!(!installed().is_empty());

    // The app alone gone: the runtime still blocks, and is named.
    must(&["uninstall", "-y", "--noninteractive", APP]);
    let users = source_users(Scope::User, "test", &c).unwrap();
    assert!(
        users.iter().all(|u| u.kind == RefKind::Runtime),
        "{users:?}"
    );
    assert!(matches!(
        remove_source(Scope::User, "test", &url, &lock, &c).unwrap_err(),
        Error::InUse(_)
    ));

    // Nothing installed from it: the guards, then the removal.
    must(&["uninstall", "-y", "--noninteractive", "--all"]);
    clear_pins();
    assert!(source_users(Scope::User, "test", &c).unwrap().is_empty());
    let other = "https://other.example.org/repo";
    let e = remove_source(Scope::User, "test", other, &lock, &c).unwrap_err();
    assert!(
        matches!(e, Error::Invalid(ref s) if s.contains("changed")),
        "{e:?}"
    );
    assert!(matches!(
        remove_source(Scope::User, "../test", &url, &lock, &c).unwrap_err(),
        Error::Invalid(_)
    ));
    assert!(matches!(
        remove_source(Scope::User, "Test", &url, &lock, &c).unwrap_err(),
        Error::Invalid(_)
    ));
    let gone = CancelToken::new();
    gone.cancel();
    assert_eq!(
        remove_source(Scope::User, "test", &url, &lock, &gone).unwrap_err(),
        Error::Cancelled
    );
    assert!(list_remotes(&c).remotes.iter().any(|r| r.name == "test"));
    // Spelling of the address does not matter (a trailing slash).
    remove_source(Scope::User, "test", url.trim_end_matches('/'), &lock, &c).unwrap();
    assert!(!list_remotes(&c).remotes.iter().any(|r| r.name == "test"));
    // Gone is said, not repeated.
    let e = remove_source(Scope::User, "test", &url, &lock, &c).unwrap_err();
    assert!(
        matches!(e, Error::Invalid(ref s) if s.contains("not in the list")),
        "{e:?}"
    );
}

#[test]
fn the_system_installation_is_listed_changed_and_guarded_the_same_way() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let _clean = SystemCleanup;
    let gpg = format!("--gpg-import={}", dir.join("key.gpg").display());
    let url = format!("file://{}/repo/", dir.display());
    must_system(&["remote-add", &gpg, "test", &url]);
    must_system(&["install", "-y", "--noninteractive", "test", &app_ref()]);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();

    let out = list_remotes(&c);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let sys = find(&out.remotes, Scope::System, "test");
    let usr = find(&out.remotes, Scope::User, "test");
    // The same name in both installations are two sources, with their own apps.
    assert!(sys.installed.iter().any(|u| u.id == APP));
    assert!(usr.installed.is_empty(), "{:?}", usr.installed);
    // System first.
    let first_user = out
        .remotes
        .iter()
        .position(|r| r.scope == Scope::User)
        .unwrap();
    assert!(
        out.remotes[..first_user]
            .iter()
            .all(|r| r.scope == Scope::System)
    );

    set_enabled(Scope::System, "test", false, &lock, &c).unwrap();
    let out = list_remotes(&c);
    assert!(!find(&out.remotes, Scope::System, "test").enabled);
    assert!(find(&out.remotes, Scope::User, "test").enabled);
    set_enabled(Scope::System, "test", true, &lock, &c).unwrap();

    assert!(matches!(
        remove_source(Scope::System, "test", &url, &lock, &c).unwrap_err(),
        Error::InUse(_)
    ));
    // The user's source of the same name has nothing installed: it goes, the
    // system one stays.
    remove_source(Scope::User, "test", &url, &lock, &c).unwrap();
    let out = list_remotes(&c);
    assert!(
        out.remotes
            .iter()
            .any(|r| r.scope == Scope::System && r.name == "test")
    );
    assert!(
        !out.remotes
            .iter()
            .any(|r| r.scope == Scope::User && r.name == "test")
    );
    must_system(&["uninstall", "-y", "--noninteractive", "--all"]);
    clear_pins_system();
    remove_source(Scope::System, "test", &url, &lock, &c).unwrap();
    assert!(!list_remotes(&c).remotes.iter().any(|r| r.name == "test"));
}

#[test]
fn listing_stops_when_cancelled() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    c.cancel();
    let out = list_remotes(&c);
    assert!(out.cancelled && out.remotes.is_empty());
}

// ---- the add flow's confirmation and guards ----

#[test]
fn the_preview_of_a_file_shows_the_key_and_where_it_would_go() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();

    let (repo, stem) = read_repo_file(fixture("test.flatpakrepo").to_str().unwrap()).unwrap();
    let key = repo.key.clone().unwrap();
    let p = preview_repo(repo, &stem, &c).unwrap();
    assert_eq!(p.title, "Telamon Store Test");
    assert_eq!(p.url, "https://dl.example.org/test/repo");
    assert_eq!(p.fingerprint.as_deref(), Some(key.fingerprint()));
    assert!(!p.unsigned());
    // The `test` remote has this name but another address: numbered, not overwritten.
    assert_eq!(
        p.user,
        Placement::Free {
            name: "test-2".into()
        }
    );
    assert_eq!(p.placement(Scope::User), &p.user);
    assert!(matches!(p.system, Placement::Free { ref name } if name == "test"));

    let (repo, stem) = read_repo_file(fixture("unsigned.flatpakrepo").to_str().unwrap()).unwrap();
    let p = preview_repo(repo, &stem, &c).unwrap();
    assert!(p.unsigned() && p.fingerprint.is_none());
    assert_eq!(p.comment.as_deref(), Some("A source without a signing key"));
    assert_eq!(
        p.user,
        Placement::Free {
            name: "unsigned".into()
        }
    );

    // An address that is a remote already.
    let (mut repo, _) = read_repo_file(fixture("test.flatpakrepo").to_str().unwrap()).unwrap();
    must(&[
        "remote-modify",
        "--url=https://dl.example.org/test/repo/",
        "test",
    ]);
    repo.title = Some("Again".into());
    let p = preview_repo(repo, "again", &c).unwrap();
    assert_eq!(
        p.user,
        Placement::Exists {
            name: "test".into(),
            enabled: true
        }
    );
}

#[test]
fn an_unsigned_source_is_refused_without_the_acknowledgement_and_nothing_is_added() {
    let Some((dir, _g)) = remote() else { return };
    reset(&dir);
    let c = CancelToken::new();
    let lock = OperationLock::try_acquire().unwrap();
    let (repo, _) = read_repo_file(fixture("unsigned.flatpakrepo").to_str().unwrap()).unwrap();
    let e = add_source(Scope::User, &repo, "unsigned", false, &lock, &c).unwrap_err();
    assert!(
        matches!(e, Error::Invalid(ref s) if s.contains("not signed")),
        "{e:?}"
    );
    // Reserved and invalid names are refused before libflatpak is asked.
    let (signed, _) = read_repo_file(fixture("test.flatpakrepo").to_str().unwrap()).unwrap();
    for name in ["flathub", "../x", "", "a b"] {
        let e = add_source(Scope::User, &signed, name, true, &lock, &c).unwrap_err();
        assert!(matches!(e, Error::Invalid(_)), "{name}: {e:?}");
    }
    // The name of an existing source is never taken over.
    let e = add_source(Scope::User, &signed, "TEST", true, &lock, &c).unwrap_err();
    assert_eq!(e, Error::RemoteNameTaken("TEST".into()));
    let out = list_remotes(&c);
    let names: Vec<&str> = out.remotes.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["test"], "{names:?}");
}
