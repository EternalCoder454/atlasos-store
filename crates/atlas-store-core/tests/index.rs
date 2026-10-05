//! Index tests: the file on disk, as the Store meets it, including a cache
//! directory somebody tampered with.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use atlas_store_core::appstream::index::{self, FORMAT, IndexError, IndexKey};
use atlas_store_core::appstream::{Catalog, ParseOptions, parse};

const SAMPLE: &str = include_str!("fixtures/flathub-sample.xml");

fn dir(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "index-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&d);
    d
}

fn key() -> IndexKey {
    IndexKey {
        origin: "flathub".into(),
        commit: "ab".repeat(32),
        langs: vec!["de".into(), "de_DE".into()],
        format: FORMAT,
    }
}

fn catalog() -> Catalog {
    let o = ParseOptions {
        origin: "flathub".into(),
        langs: vec!["de".into()],
        ..ParseOptions::default()
    };
    parse(SAMPLE.as_bytes(), &o).expect("the sample parses")
}

fn empty(origin: &str) -> Catalog {
    Catalog {
        origin: origin.into(),
        ..Catalog::default()
    }
}

fn names(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

fn is_root() -> bool {
    fs::metadata("/proc/self")
        .map(|m| m.uid() == 0)
        .unwrap_or(false)
}

#[test]
fn roundtrip_and_permissions() {
    let d = dir("rt").join("cache").join("atlas-store");
    let k = key();
    let f = index::cache_file(&d, &k).unwrap();
    let name = f.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        name.starts_with(&format!("index-flathub-{}-", "ab".repeat(8))),
        "{name}"
    );
    assert!(name.ends_with(".bin") && name.len() == "index-flathub-".len() + 16 + 1 + 8 + 4);
    let cat = catalog();
    assert_eq!(index::write(&d, &k, &cat).unwrap(), f);
    assert_eq!(index::read(&f, &k).unwrap(), cat);
    assert_eq!(
        fs::metadata(&d).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        names(&d),
        std::slice::from_ref(&name),
        "no temp file is left"
    );
    // Writing again replaces it.
    let mut cat2 = cat.clone();
    cat2.components.truncate(3);
    index::write(&d, &k, &cat2).unwrap();
    assert_eq!(index::read(&f, &k).unwrap(), cat2);
    assert_eq!(names(&d), std::slice::from_ref(&name));
    fs::remove_dir_all(d.parent().unwrap().parent().unwrap()).unwrap();
}

#[test]
fn language_changes_the_file_name() {
    let d = Path::new("/x");
    let a = key();
    let mut b = key();
    b.langs = vec!["fr".into()];
    assert_ne!(
        index::cache_file(d, &a).unwrap(),
        index::cache_file(d, &b).unwrap()
    );
    assert_eq!(
        index::cache_file(d, &a).unwrap(),
        index::cache_file(d, &a).unwrap()
    );
}

#[test]
fn keys_that_are_not_file_names_are_refused() {
    let d = Path::new("/x");
    for origin in [
        "",
        ".hidden",
        "a/b",
        "..",
        "a b",
        "a\0b",
        &"o".repeat(65),
        "é",
    ] {
        let mut k = key();
        k.origin = origin.into();
        assert!(
            matches!(index::cache_file(d, &k), Err(IndexError::InvalidKey(_))),
            "{origin:?}"
        );
        assert!(index::write(d, &k, &empty("flathub")).is_err());
    }
    for commit in [
        "",
        "abc",
        &"g".repeat(64),
        &"A".repeat(64),
        &"a".repeat(65),
        "../../etc/passwd/xx",
    ] {
        let mut k = key();
        k.commit = commit.into();
        assert!(
            matches!(index::cache_file(d, &k), Err(IndexError::InvalidKey(_))),
            "{commit:?}"
        );
    }
    let mut k = key();
    k.commit = "0123456789abcdef".into();
    assert!(index::cache_file(d, &k).is_ok());
    let mut k = key();
    k.origin = "a.b-c_d".into();
    assert!(index::cache_file(d, &k).is_ok());
}

#[test]
fn a_different_key_is_an_error() {
    let d = dir("key");
    let k = key();
    let f = index::write(&d, &k, &catalog()).unwrap();
    let mut o = k.clone();
    o.origin = "fedora".into();
    let mut c = k.clone();
    c.commit = "cd".repeat(32);
    let mut l = k.clone();
    l.langs = vec!["de".into()];
    let mut v = k.clone();
    v.format = FORMAT + 1;
    for wrong in [o, c, l] {
        assert_eq!(index::read(&f, &wrong), Err(IndexError::KeyMismatch));
    }
    assert!(matches!(index::read(&f, &v), Err(IndexError::BadHeader(_))));
    assert!(index::read(&f, &k).is_ok());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn damaged_files_are_errors() {
    let d = dir("dmg");
    let k = key();
    let f = index::write(&d, &k, &catalog()).unwrap();
    let good = fs::read(&f).unwrap();
    // Header, a length field, the checksum and the body.
    for at in [
        0,
        3,
        8,
        9,
        12,
        20,
        40,
        80,
        100,
        good.len() / 2,
        good.len() - 1,
    ] {
        let mut b = good.clone();
        b[at] ^= 0xA5;
        fs::write(&f, &b).unwrap();
        assert!(index::read(&f, &k).is_err(), "flip at {at}");
    }
    for n in [0, 1, 7, 8, 12, 50, good.len() / 2, good.len() - 1] {
        fs::write(&f, &good[..n]).unwrap();
        assert!(index::read(&f, &k).is_err(), "cut at {n}");
    }
    let mut more = good.clone();
    more.extend_from_slice(b"junk");
    fs::write(&f, &more).unwrap();
    assert!(index::read(&f, &k).is_err());
    fs::write(&f, b"not an index at all, just text").unwrap();
    assert!(matches!(index::read(&f, &k), Err(IndexError::BadHeader(_))));
    fs::write(&f, &good).unwrap();
    assert!(index::read(&f, &k).is_ok());
    assert!(matches!(
        index::read(&d.join("missing.bin"), &k),
        Err(IndexError::Io(_))
    ));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn symlinks_and_non_files_are_refused() {
    let d = dir("sym");
    fs::create_dir_all(&d).unwrap();
    let k = key();
    let real = index::write(&d, &k, &catalog()).unwrap();
    let link = d.join("link.bin");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(matches!(index::read(&link, &k), Err(IndexError::Io(_))));
    assert!(matches!(index::read(&d, &k), Err(IndexError::Io(_))));
    // A dangling link is refused too, and writing over a link replaces the link.
    let dangling = d.join("dangling.bin");
    std::os::unix::fs::symlink(d.join("nowhere"), &dangling).unwrap();
    assert!(index::read(&dangling, &k).is_err());
    let victim = d.join("victim.txt");
    fs::write(&victim, "keep me").unwrap();
    // The index's own name is a link to the victim: the write replaces the link.
    fs::remove_file(&real).unwrap();
    std::os::unix::fs::symlink(&victim, &real).unwrap();
    index::write(&d, &k, &empty("flathub")).unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
    assert!(
        !fs::symlink_metadata(&real)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // A FIFO is not opened (and doesn't block).
    let fifo = d.join("fifo.bin");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    );
    assert!(index::read(&fifo, &k).is_err());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn oversized_files_are_refused() {
    let d = dir("big");
    fs::create_dir_all(&d).unwrap();
    let f = d.join("big.bin");
    let file = fs::File::create(&f).unwrap();
    file.set_len((32 << 20) + 1).unwrap();
    assert_eq!(index::read(&f, &key()), Err(IndexError::TooLarge));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn older_indexes_of_the_origin_are_removed() {
    let d = dir("old");
    let k1 = key();
    let mut k2 = key();
    k2.commit = "cd".repeat(32);
    let mut other = key();
    other.origin = "flathub-beta".into();
    let mut sub = key();
    sub.origin = "flat".into();
    index::write(&d, &other, &empty("flathub-beta")).unwrap();
    index::write(&d, &sub, &empty("flat")).unwrap();
    index::write(&d, &k1, &empty("flathub")).unwrap();
    fs::write(d.join("notes.txt"), "x").unwrap();
    let outside = d.join("outside.txt");
    fs::write(&outside, "target").unwrap();
    let old_link = d.join("index-flathub-0123456789abcdef-00000000.bin");
    std::os::unix::fs::symlink(&outside, &old_link).unwrap();
    index::write(&d, &k2, &empty("flathub")).unwrap();
    let left = names(&d);
    let f2 = index::cache_file(&d, &k2)
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let fo = index::cache_file(&d, &other)
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let fs_ = index::cache_file(&d, &sub)
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut want = vec![
        f2,
        fo,
        fs_,
        "notes.txt".to_string(),
        "outside.txt".to_string(),
    ];
    want.sort();
    assert_eq!(left, want);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "target");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_failed_write_leaves_no_temp_file_and_the_old_index() {
    let d = dir("fail");
    let k = key();
    let f = index::cache_file(&d, &k).unwrap();
    let cat = catalog();
    index::write(&d, &k, &cat).unwrap();
    let before = fs::read(&f).unwrap();

    // The rename fails because the target is a non-empty directory.
    let mut kb = key();
    kb.commit = "cd".repeat(32);
    let blocked = index::cache_file(&d, &kb).unwrap();
    fs::create_dir_all(blocked.join("inside")).unwrap();
    assert!(index::write(&d, &kb, &cat).is_err());
    let stray: Vec<String> = names(&d)
        .into_iter()
        .filter(|n| n.contains(".tmp."))
        .collect();
    assert!(stray.is_empty(), "{stray:?}");
    assert_eq!(fs::read(&f).unwrap(), before);

    // A directory that can't be written to (root ignores modes, so skip then).
    if !is_root() {
        let ro = dir("ro");
        fs::create_dir_all(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o500)).unwrap();
        let r = index::write(&ro, &k, &cat);
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(r.is_err());
        assert!(names(&ro).is_empty());
        fs::remove_dir_all(&ro).unwrap();
    }
    // A cache "directory" that is a file.
    let notdir = d.join("file");
    fs::write(&notdir, "x").unwrap();
    assert!(index::write(&notdir.join("sub"), &k, &cat).is_err());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn write_refuses_a_foreign_catalog_or_format() {
    let d = dir("foreign");
    let k = key();
    let mut other = catalog();
    other.origin = "fedora".into();
    assert!(index::write(&d, &k, &other).is_err());
    let mut v = key();
    v.format = FORMAT + 1;
    assert!(index::write(&d, &v, &catalog()).is_err());
    assert!(!d.exists(), "nothing is created for a refused write");
}

#[test]
fn a_cache_directory_others_can_write_is_not_used() {
    let d = dir("unsafe");
    let k = key();
    let cat = catalog();
    let f = index::write(&d, &k, &cat).unwrap();
    for mode in [0o770, 0o707, 0o777] {
        fs::set_permissions(&d, fs::Permissions::from_mode(mode)).unwrap();
        assert!(index::write(&d, &k, &cat).is_err(), "write {mode:o}");
        assert!(
            matches!(index::read(&f, &k), Err(IndexError::Io(_))),
            "read {mode:o}"
        );
    }
    // A new directory is private, and a private one works again.
    fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(index::read(&f, &k).unwrap(), cat);
    // A directory that is really a file is refused too.
    let file = d.join("plain");
    fs::write(&file, "x").unwrap();
    assert!(index::write(&file, &k, &cat).is_err());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn stale_temp_files_are_removed_and_a_reused_name_is_survived() {
    let d = dir("temp");
    fs::create_dir_all(&d).unwrap();
    let k = key();
    let f = index::cache_file(&d, &k).unwrap();
    let name = f.file_name().unwrap().to_str().unwrap().to_string();
    let mut beta = key();
    beta.origin = "flathub-beta".into();
    let beta_name = index::cache_file(&d, &beta)
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    let make = |n: &str, aged: bool| {
        let file = fs::File::create(d.join(n)).unwrap();
        if aged {
            file.set_modified(old).unwrap();
        }
    };
    let stale = format!(".{name}.tmp.4242.7");
    let fresh = format!(".{name}.tmp.4242.8");
    let others = format!(".{beta_name}.tmp.4242.7");
    make(&stale, true);
    make(&fresh, false);
    make(&others, true);
    make(".notes.tmp.1.2", true);
    // Names the next writes might pick: every other counter value is taken.
    let pid = std::process::id();
    for n in (0..2000).step_by(2) {
        make(&format!(".{name}.tmp.{pid}.{n}"), false);
    }

    let cat = catalog();
    index::write(&d, &k, &cat).unwrap();
    index::write(&d, &k, &cat).unwrap();
    assert_eq!(index::read(&f, &k).unwrap(), cat);
    let left = names(&d);
    assert!(!left.contains(&stale), "{left:?}");
    assert!(
        left.contains(&fresh),
        "a temp file a write may be using stays"
    );
    assert!(left.contains(&others), "another origin's stays");
    assert!(left.contains(&".notes.tmp.1.2".to_string()));
    fs::remove_dir_all(&d).unwrap();
}
