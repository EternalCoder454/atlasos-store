//! Index tests: the file on disk, as the Store meets it, including a cache
//! directory somebody tampered with.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use telamon_store_core::appstream::index::{self, FORMAT, IndexError, IndexKey};
use telamon_store_core::appstream::{Catalog, Limits, ParseOptions, parse};

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
    let d = dir("rt").join("cache").join("telamon-store");
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
        Err(IndexError::Missing)
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
    let err = index::write(&d, &kb, &cat).unwrap_err().to_string();
    assert!(
        err.contains("move the index into place") && err.contains(&*blocked.to_string_lossy()),
        "the error names the operation and the path: {err}"
    );
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
fn a_cache_directory_others_can_write_is_refused_by_read_and_repaired_by_write() {
    let d = dir("unsafe");
    let k = key();
    let cat = catalog();
    let f = index::write(&d, &k, &cat).unwrap();
    for mode in [0o770, 0o707, 0o777, 0o775] {
        // A read refuses and changes nothing; the caller rebuilds.
        fs::set_permissions(&d, fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            matches!(index::read(&f, &k), Err(IndexError::Io(_))),
            "read {mode:o}"
        );
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            mode,
            "read must not chmod {mode:o}"
        );
        // A write repairs the folder, and then the read works.
        index::write(&d, &k, &cat).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700,
            "write {mode:o}"
        );
        assert_eq!(index::read(&f, &k).unwrap(), cat, "read after {mode:o}");
    }
    // A directory that is really a file is refused.
    let file = d.join("plain");
    fs::write(&file, "x").unwrap();
    assert!(index::write(&file, &k, &cat).is_err());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn no_index_file_is_missing_not_a_fault() {
    let d = dir("missing");
    let k = key();
    let f = index::cache_file(&d, &k).unwrap();
    // Neither the folder nor the file exists yet.
    assert_eq!(index::read(&f, &k), Err(IndexError::Missing));
    fs::create_dir_all(&d).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(index::read(&f, &k), Err(IndexError::Missing));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_index_written_meanwhile_is_not_removed() {
    let d = dir("newer");
    let k1 = key();
    let mut k2 = key();
    k2.commit = "cd".repeat(32);
    let mut k3 = key();
    k3.commit = "ef".repeat(32);
    let f1 = index::write(&d, &k1, &empty("flathub")).unwrap();
    // k2 stands for an index another writer finished after this write began:
    // a time just ahead of now, inside the clock slack.
    let f2 = index::write(&d, &k2, &empty("flathub")).unwrap();
    let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(3);
    fs::File::options()
        .write(true)
        .open(&f2)
        .unwrap()
        .set_modified(ahead)
        .unwrap();
    let f3 = index::write(&d, &k3, &empty("flathub")).unwrap();
    assert!(f3.exists());
    assert!(f2.exists(), "a newer index stays");
    assert!(!f1.exists(), "an older one goes");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_file_from_the_future_is_removed() {
    let d = dir("future");
    let k1 = key();
    let mut k2 = key();
    k2.commit = "cd".repeat(32);
    let mut k3 = key();
    k3.commit = "ef".repeat(32);
    index::write(&d, &k1, &empty("flathub")).unwrap();
    let f2 = index::write(&d, &k2, &empty("flathub")).unwrap();
    // The clock stepped back after this one was written: it would otherwise
    // look newer than every later write, forever.
    let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&f2)
        .unwrap()
        .set_modified(ahead)
        .unwrap();
    let f3 = index::write(&d, &k3, &empty("flathub")).unwrap();
    assert!(f3.exists());
    assert!(!f2.exists(), "an index from the future goes");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_index_of_the_old_format_is_removed() {
    let d = dir("oldformat");
    let k = key();
    index::write(&d, &k, &empty("flathub")).unwrap();
    // FORMAT 1 named its files `index-<origin>-<16 hex>-<8 hex>.bin`.
    let old = d.join("index-flathub-0123456789abcdef-89abcdef.bin");
    fs::write(&old, "old").unwrap();
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut k2 = key();
    k2.commit = "cd".repeat(32);
    let f2 = index::write(&d, &k2, &empty("flathub")).unwrap();
    assert!(!old.exists(), "{:?}", names(&d));
    assert!(f2.exists());
    fs::remove_dir_all(&d).unwrap();
}

/// `n` copies of `s` joined by nothing.
fn rep(s: &str, n: usize) -> String {
    s.repeat(n)
}

/// One component with every list and string at the parser's cap, and a few
/// past it.
fn every_cap_xml(lim: &Limits) -> String {
    let more = 3;
    let mut x = String::from("<components>");
    x += "<component type=\"desktop-application\"><id>org.example.Caps</id>";
    x += &format!("<name>{}</name>", rep("n", lim.name + more));
    x += &format!("<summary>{}</summary>", rep("s", lim.summary + more));
    x += &format!(
        "<developer><name>{}</name></developer>",
        rep("d", lim.developer + more)
    );
    x += &format!(
        "<project_license>{}</project_license>",
        rep("l", lim.license + more)
    );
    x += "<categories>";
    for i in 0..lim.categories + more {
        x += &format!("<category>c{i}{}</category>", rep("c", lim.category));
    }
    x += "</categories><keywords>";
    for i in 0..lim.keywords + more {
        x += &format!("<keyword>k{i}{}</keyword>", rep("k", lim.keyword));
    }
    x += "</keywords><description>";
    // A paragraph with the most spans, one with the most characters, a list
    // with the most items, and paragraphs up to the block cap.
    x += &format!("<p>{}</p>", rep("<em>a</em><code>b</code>", 128 + more));
    x += &format!("<p>{}</p>", rep("p", lim.desc_para + more));
    x += &format!("<ul>{}</ul>", rep("<li>i</li>", lim.desc_items + more));
    for i in 0..lim.desc_blocks + more {
        x += &format!("<p>block {i}</p>");
    }
    x += "</description><icon type=\"cached\" width=\"64\">small.png</icon>";
    for w in 101..=120 {
        x += &format!("<icon type=\"cached\" width=\"{w}\">big.png</icon>");
    }
    for i in 0..lim.urls + more {
        x += &format!("<url type=\"homepage\">https://example.org/{i}</url>");
    }
    x += "<screenshots>";
    for s in 0..lim.screenshots + more {
        let kind = if s == lim.screenshots + 1 {
            " type=\"default\""
        } else {
            ""
        };
        x += &format!("<screenshot{kind}><caption>shot {s}</caption>");
        for i in 0..lim.images + more {
            x += &format!(
                "<image type=\"source\" width=\"{i}\" height=\"{i}\">https://example.org/{s}/{i}.png</image>"
            );
        }
        x += "</screenshot>";
    }
    x += "</screenshots><releases>";
    for r in 0..70 {
        x += &format!(
            "<release version=\"1.{r}\" timestamp=\"{}\"><description><p>r{r}</p></description></release>",
            1_000 + r
        );
    }
    x += "</releases><content_rating type=\"oars-1.1\">";
    for i in 0..64 + more {
        x += &format!("<content_attribute id=\"attr-{i}\">mild</content_attribute>");
    }
    x += "</content_rating>";
    for i in 0..16 + more {
        x += &format!("<extends>org.example.Base{i}</extends>");
    }
    x += "<launchable type=\"desktop-id\">org.example.Caps.desktop</launchable>";
    x += "<bundle type=\"flatpak\" runtime=\"org.example.Platform/x86_64/1\" \
          sdk=\"org.example.Sdk/x86_64/1\">app/org.example.Caps/x86_64/stable</bundle>";
    x += "<custom><value key=\"flathub::verification::verified\">true</value></custom>";
    x += "<branding><color type=\"primary\" scheme_preference=\"light\">#112233</color></branding>";
    x += "</component></components>";
    x
}

#[test]
fn a_catalog_at_every_parser_cap_round_trips() {
    let lim = Limits::default();
    let o = ParseOptions {
        origin: "flathub".into(),
        langs: vec!["de".into()],
        ..ParseOptions::default()
    };
    let cat = parse(every_cap_xml(&lim).as_bytes(), &o).expect("parses");
    assert_eq!(cat.components.len(), 1, "{cat:?}");
    let c = &cat.components[0];
    // Each one is at its cap, not under it.
    assert_eq!(c.name.chars().count(), lim.name);
    assert_eq!(c.summary.chars().count(), lim.summary);
    assert_eq!(c.developer.chars().count(), lim.developer);
    assert_eq!(c.license.chars().count(), lim.license);
    assert_eq!(c.categories.len(), lim.categories);
    assert_eq!(c.keywords.len(), lim.keywords);
    assert_eq!(c.description.len(), lim.desc_blocks);
    assert_eq!(c.urls.len(), lim.urls);
    assert_eq!(c.screenshots.len(), lim.screenshots);
    assert!(c.screenshots.iter().all(|s| s.images.len() == lim.images));
    assert_eq!(c.releases.len(), 10);
    assert_eq!(c.content_rating.as_ref().unwrap().attrs.len(), 64);
    assert_eq!(c.extends.len(), 16);
    let icon = c.icon.as_ref().unwrap();
    assert_eq!((icon.file.as_str(), icon.sizes.len()), ("big.png", 16));
    // The default one past the cap was kept, and is the only default, first.
    assert!(c.screenshots[0].default);
    assert_eq!(c.screenshots.iter().filter(|s| s.default).count(), 1);
    assert_eq!(
        c.screenshots[0].caption,
        format!("shot {}", lim.screenshots + 1)
    );
    assert!(
        matches!(&c.description[0], telamon_store_core::appstream::Block::Paragraph(s) if s.len() == 256)
    );

    let d = dir("caps");
    let k = key();
    let f = index::write(&d, &k, &cat).expect("the index is written");
    assert_eq!(index::read(&f, &k).expect("and read back"), cat);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_symlinked_cache_directory_is_refused() {
    let d = dir("linked");
    let k = key();
    let cat = catalog();
    let real = d.join("real");
    let f = index::write(&real, &k, &cat).unwrap();
    let link = d.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert!(index::write(&link, &k, &cat).is_err());
    let through = link.join(f.file_name().unwrap());
    assert!(matches!(index::read(&through, &k), Err(IndexError::Io(_))));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn an_empty_or_missing_directory_is_refused() {
    let k = key();
    let cat = catalog();
    // Never the working directory, for writing as for reading.
    assert!(index::write(Path::new(""), &k, &cat).is_err());
    assert!(index::write(Path::new("."), &k, &cat).is_err());
    for p in ["index.bin", "./index.bin", ""] {
        assert!(
            matches!(index::read(Path::new(p), &k), Err(IndexError::Io(_))),
            "{p:?}"
        );
    }
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

    let old = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    let make = |n: &str, aged: bool| {
        let file = fs::File::create(d.join(n)).unwrap();
        if aged {
            file.set_modified(old).unwrap();
        }
    };
    // A temp file's age is its later of modification and status change, and
    // the second can't be set back; one from the future goes the same way.
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
