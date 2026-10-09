//! The catalog module: search, categories, merging, licences and loading.
//! Fixtures are `fixtures/catalog-alpha.xml` and `catalog-beta.xml`; the tests
//! against a real libflatpak installation use the local test remote and skip
//! without `TELAMON_STORE_TEST_REMOTE`.

mod common;

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use telamon_store_core::appstream::{Catalog, ParseOptions, parse};
use telamon_store_core::catalog::{
    CatalogSource, Category, EntryId, Filter, Library, LoadError, STALE_AFTER, Sort,
    is_free_license, list_sources, load,
};
use telamon_store_core::flatpak::{CancelToken, Scope};

const ALPHA: &str = include_str!("fixtures/catalog-alpha.xml");
const BETA: &str = include_str!("fixtures/catalog-beta.xml");

fn catalog(xml: &str, origin: &str) -> Catalog {
    let opts = ParseOptions {
        origin: origin.into(),
        ..ParseOptions::default()
    };
    parse(xml.as_bytes(), &opts).expect("the fixture parses")
}

fn source(remote: &str) -> CatalogSource {
    CatalogSource {
        scope: Scope::User,
        remote: remote.into(),
        title: remote.into(),
        priority: 0,
        dir: None,
        commit: None,
        updated: None,
    }
}

/// The alpha fixture as Flathub's catalog (its apps carry Flathub's
/// verification marks, which only that remote's catalog can vouch for).
fn alpha() -> Library {
    Library::new(vec![(source("flathub"), catalog(ALPHA, "flathub"))])
}

fn both() -> Library {
    Library::new(vec![
        (source("alpha"), catalog(ALPHA, "alpha")),
        (source("beta"), catalog(BETA, "beta")),
    ])
}

fn ids(lib: &Library, found: &[EntryId]) -> Vec<String> {
    found
        .iter()
        .map(|&i| {
            lib.component(i)
                .id_bare()
                .rsplit('.')
                .next()
                .unwrap()
                .to_owned()
        })
        .collect()
}

fn search(lib: &Library, q: &str) -> Vec<String> {
    ids(lib, &lib.search(q, Filter::default(), 100))
}

const NONE: Filter = Filter {
    verified_only: false,
    free_only: false,
};

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "catalog-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

// ---- library contents

#[test]
fn only_apps_with_an_app_bundle_are_listed() {
    let lib = alpha();
    assert_eq!(lib.len(), 12);
    assert!(!lib.is_empty());
    for name in ["NoBundle", "Addon", "Runtime"] {
        assert!(lib.find(&format!("org.example.{name}")).is_none(), "{name}");
    }
    // Console apps are listed; an ID with .desktop is found with or without.
    assert!(lib.find("org.example.Cli").is_some());
    let a = lib.find("org.example.Suffix").unwrap();
    assert_eq!(lib.find("org.example.Suffix.desktop"), Some(a));
    assert_eq!(lib.find("org.example.suffix"), None);
    assert_eq!(lib.find(""), None);
    assert!(Library::new(Vec::new()).is_empty());
    assert!(Library::new(Vec::new()).search("x", NONE, 5).is_empty());
}

#[test]
fn only_flathubs_catalog_can_verify_an_app() {
    let verified_in = |remote: &str| {
        let lib = Library::new(vec![(source(remote), catalog(ALPHA, remote))]);
        let cafe = lib.find("org.example.Cafe").unwrap();
        let filter = Filter {
            verified_only: true,
            ..NONE
        };
        let listed = lib.browse(None, filter, Sort::Name).len();
        (lib.is_verified(cafe), listed)
    };
    // The fixture marks Cafe and one more app as verified.
    assert_eq!(verified_in("flathub"), (true, 2));
    assert_eq!(verified_in("flathub-beta"), (true, 2));
    // The same marks in any other remote's catalog mean nothing: no badge,
    // no place in the "verified" filter.
    for other in ["alpha", "evil", "flathub-source", "Flathub", "kde"] {
        assert_eq!(verified_in(other), (false, 0), "{other}");
    }
    // Where an app is listed twice, Flathub's copy comes first and keeps its
    // mark; the other remote's copy is only an alternative.
    let lib = Library::new(vec![
        (source("flathub"), catalog(ALPHA, "flathub")),
        (source("evil"), catalog(ALPHA, "evil")),
    ]);
    let cafe = lib.find("org.example.Cafe").unwrap();
    assert!(lib.is_verified(cafe));
    assert_eq!(lib.source(cafe).remote, "flathub");
}

#[test]
fn dedupe_keeps_the_first_source_and_lists_the_others() {
    let lib = both();
    assert_eq!(lib.len(), 13);
    let b = lib.find("org.example.Browser").unwrap();
    assert_eq!(lib.component(b).name, "Browser");
    assert_eq!(lib.source(b).remote, "alpha");
    let alt = lib.alternatives(b);
    assert_eq!(alt.len(), 1);
    assert_eq!(alt[0].remote, "beta");
    let only = lib.find("org.example.OnlyBeta").unwrap();
    assert_eq!(lib.source(only).remote, "beta");
    assert!(lib.alternatives(only).is_empty());
    let n = lib.find("org.example.Notes").unwrap();
    assert!(lib.alternatives(n).is_empty());
    assert_eq!(lib.sources().len(), 2);
    // The other order: beta wins.
    let rev = Library::new(vec![
        (source("beta"), catalog(BETA, "beta")),
        (source("alpha"), catalog(ALPHA, "alpha")),
    ]);
    let b = rev.find("org.example.Browser").unwrap();
    assert_eq!(rev.component(b).name, "Browser Beta");
    assert_eq!(rev.alternatives(b)[0].remote, "alpha");
}

// ---- search

#[test]
fn ranking_order() {
    // Exact name, then name prefix (verified first), keywords, ID, summary,
    // developer.
    assert_eq!(
        search(&alpha(), "browser"),
        [
            "Browser",
            "FireBrowser",
            "Anon",
            "Tabs",
            "Zeta",
            "Pictures",
            "Notes"
        ]
    );
}

#[test]
fn every_word_must_match() {
    let lib = alpha();
    assert_eq!(search(&lib, "fire browser"), ["FireBrowser"]);
    assert_eq!(search(&lib, "surf fast"), ["FireBrowser"]);
    assert_eq!(search(&lib, "fire nothing"), Vec::<String>::new());
    // Worst field decides: name + summary tie on the summary, then verified,
    // then name.
    assert_eq!(
        search(&lib, "browser surf"),
        ["FireBrowser", "Browser", "Anon"]
    );
    // Prefixes of words, not substrings.
    assert_eq!(search(&lib, "rowser"), Vec::<String>::new());
    assert_eq!(search(&lib, "brow sur"), ["FireBrowser", "Browser", "Anon"]);
    // Repeated words and an ID's parts.
    assert_eq!(search(&lib, "tabs tabs"), ["Tabs"]);
    assert_eq!(search(&lib, "org example tabs"), ["Tabs"]);
}

#[test]
fn case_and_accents_do_not_matter() {
    let lib = alpha();
    for q in [
        "cafe",
        "CAFE",
        "Café",
        "café lumière",
        "LUMIERE",
        "cafe\u{301}",
        "zoe muller",
        "MÜLLER",
        "boulang",
    ] {
        assert_eq!(search(&lib, q), ["Cafe"], "{q}");
    }
    // The name is shown as written.
    let c = lib.find("org.example.Cafe").unwrap();
    assert_eq!(lib.component(c).name, "Café Lumière");
    assert_eq!(search(&lib, "ÉCOUTE"), Vec::<String>::new());
}

#[test]
fn degenerate_queries_return_nothing() {
    let lib = alpha();
    for q in ["", " ", "\t\n ", "++", "...", "\u{301}", "\0"] {
        assert!(lib.search(q, NONE, 10).is_empty(), "{q:?}");
    }
    assert!(lib.search("browser", NONE, 0).is_empty());
    // 200 characters after trimming are fine, 201 are refused.
    let ok = "a".repeat(200);
    assert!(lib.search(&format!("  {ok}  "), NONE, 10).is_empty());
    let long = format!("browser {}", "b".repeat(192));
    assert_eq!(long.chars().count(), 200);
    assert!(lib.search(&long, NONE, 10).is_empty()); // no such word
    let over = format!("b{}", "b".repeat(200));
    assert!(lib.search(&over, NONE, 10).is_empty());
    // An overlong query would otherwise match: 200 words of "browser".
    let words = ["browser "; 25].concat();
    assert_eq!(lib.search(&words, NONE, 10).len(), 7);
    let words = ["browser "; 26].concat(); // 208 characters, 207 trimmed
    assert!(lib.search(&words, NONE, 10).is_empty());
    // Hostile text does not panic.
    for q in [
        "\u{fffd}\u{fffd}",
        "ǅ",
        "İ",
        "ß",
        "𝔸𝔹",
        &"é".repeat(150),
        "a\u{301}\u{301}\u{301}",
    ] {
        let _ = lib.search(q, NONE, 10);
    }
}

#[test]
fn limit_cuts_the_ranked_list() {
    let lib = alpha();
    let all = lib.search("browser", NONE, 100);
    assert_eq!(all.len(), 7);
    for n in 1..=7 {
        assert_eq!(lib.search("browser", NONE, n), all[..n]);
    }
    assert_eq!(lib.search("browser", NONE, usize::MAX), all);
}

#[test]
fn filters_apply_to_search_and_browse() {
    let lib = alpha();
    let verified = Filter {
        verified_only: true,
        ..NONE
    };
    let free = Filter {
        free_only: true,
        ..NONE
    };
    assert_eq!(
        ids(&lib, &lib.search("browser", verified, 100)),
        ["FireBrowser"]
    );
    // Zeta is proprietary; Tabs has a free choice; Pictures needs both.
    assert_eq!(
        ids(&lib, &lib.search("browser", free, 100)),
        [
            "Browser",
            "FireBrowser",
            "Anon",
            "Tabs",
            "Pictures",
            "Notes"
        ]
    );
    let both_f = Filter {
        verified_only: true,
        free_only: true,
    };
    assert_eq!(
        ids(&lib, &lib.browse(None, both_f, Sort::Name)),
        ["Cafe", "FireBrowser"]
    );
    let zeta = lib.find("org.browser.Zeta").unwrap();
    assert!(!lib.is_free(zeta));
    assert!(!lib.is_verified(zeta));
    let cafe = lib.find("org.example.Cafe").unwrap();
    assert!(lib.is_free(cafe) && lib.is_verified(cafe));
}

// ---- browse

#[test]
fn browse_sorts_by_name_and_by_date() {
    let lib = alpha();
    let by_name = lib.browse(None, NONE, Sort::Name);
    assert_eq!(by_name.len(), lib.len());
    let names: Vec<String> = by_name
        .iter()
        .map(|&i| lib.component(i).name.to_lowercase())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(ids(&lib, &by_name[..3]), ["Player", "Browser", "Anon"]);
    assert_eq!(Sort::default(), Sort::Name);

    let recent = lib.browse(None, NONE, Sort::RecentlyUpdated);
    assert_eq!(recent.len(), lib.len());
    assert_eq!(
        ids(&lib, &recent[..6]),
        ["Browser", "FireBrowser", "Pictures", "Anon", "Tabs", "Cafe"]
    );
    let dates: Vec<Option<i64>> = recent.iter().map(|&i| lib.updated(i)).collect();
    assert!(dates[..6].iter().all(Option::is_some));
    assert!(dates[6..].iter().all(Option::is_none));
    assert!(dates[..6].windows(2).all(|w| w[0] >= w[1]));
    // Undated apps last, by name.
    let tail: Vec<String> = recent[6..]
        .iter()
        .map(|&i| lib.component(i).name.to_lowercase())
        .collect();
    let mut sorted = tail.clone();
    sorted.sort();
    assert_eq!(tail, sorted);
}

#[test]
fn categories_map_and_count() {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        Category::of(&s(&["Audio", "Video", "AudioVideo"])),
        [Category::AudioVideo]
    );
    assert_eq!(Category::of(&s(&["Game"])), [Category::Games]);
    assert_eq!(
        Category::of(&s(&["Utility", "Office"])),
        [Category::Office, Category::Utilities]
    );
    assert_eq!(
        Category::of(&s(&["game", "UTILITY", "Player", "", "Utility\0"])),
        []
    );
    assert_eq!(Category::of(&[]), []);
    for c in Category::ALL {
        assert_eq!(Category::from_key(c.key()), Some(c));
        assert!(c.key().bytes().all(|b| b.is_ascii_lowercase() || b == b'-'));
    }
    assert_eq!(Category::AudioVideo.key(), "audio-video");
    assert_eq!(Category::Games.key(), "games");
    assert_eq!(Category::from_key("Games"), None);
    assert_eq!(Category::from_key(""), None);
    assert_eq!(Category::from_key("audio"), None);
    let mut keys: Vec<_> = Category::ALL.iter().map(|c| c.key()).collect();
    keys.dedup();
    assert_eq!(keys.len(), 10);

    let lib = alpha();
    let counts = lib.category_counts(NONE);
    assert_eq!(counts.map(|c| c.0), Category::ALL);
    for (c, n) in counts {
        assert_eq!(lib.browse(Some(c), NONE, Sort::Name).len(), n, "{c:?}");
    }
    let get = |c| counts.iter().find(|x| x.0 == c).unwrap().1;
    assert_eq!(get(Category::Network), 4);
    assert_eq!(get(Category::AudioVideo), 1);
    assert_eq!(get(Category::Games), 1);
    assert_eq!(get(Category::Utilities), 4);
    assert_eq!(get(Category::Office), 2);
    let v = lib.category_counts(Filter {
        verified_only: true,
        ..NONE
    });
    assert_eq!(v.iter().find(|x| x.0 == Category::Network).unwrap().1, 1);
    assert_eq!(v.iter().map(|x| x.1).sum::<usize>(), 3);
    let p = lib.find("org.example.Player").unwrap();
    assert_eq!(lib.categories(p), [Category::AudioVideo]);
}

// ---- licences

#[test]
fn licence_expressions() {
    for free in [
        "MIT",
        "mit",
        "GPL-3.0-or-later",
        "GPL-2.0-only",
        "GPL-2.0+",
        "LGPL-2.1+",
        "AGPL-3.0-only",
        "Apache-2.0",
        "MIT AND Apache-2.0",
        "MIT OR LicenseRef-proprietary",
        "LicenseRef-proprietary OR (MIT AND BSD-3-Clause)",
        "GPL-2.0-only WITH Classpath-exception-2.0",
        "(MIT)",
        "((MIT))",
        "  MIT  ",
        "MIT\tAND\nGPL-3.0-only",
        "MIT and BSD-2-Clause",
        "CC0-1.0",
        "OFL-1.1",
    ] {
        assert!(is_free_license(free), "{free:?}");
    }
    for not in [
        "",
        " ",
        "LicenseRef-proprietary",
        "LicenseRef-proprietary=https://example.com/eula",
        "Proprietary",
        "proprietary",
        "MIT AND LicenseRef-proprietary",
        "(MIT OR GPL-3.0-only) AND LicenseRef-x",
        "MIT-only",
        "GPL-2.0++",
        "GPL-2.0-only-or-later",
        "GPL",
        "CC-BY-NC-4.0",
        "MIT OR",
        "OR MIT",
        "AND",
        "MIT AND",
        "MIT MIT",
        "MIT OR OR MIT",
        "MIT WITH",
        "MIT WITH AND",
        "MIT WITH WITH X",
        "WITH",
        "(",
        ")",
        "()",
        "(MIT",
        "MIT)",
        ")MIT(",
        "MIT)(",
        "\0",
        "MIT\0",
        "M\u{0130}T",
        "\u{202e}MIT",
        "MIT,GPL-2.0-only",
        "😀",
    ] {
        assert!(!is_free_license(not), "{not:?}");
    }
}

#[test]
fn hostile_licence_expressions_are_bounded() {
    // 16 levels of parentheses are accepted, 17 are not; neither overflows.
    let nest = |n: usize| format!("{}MIT{}", "(".repeat(n), ")".repeat(n));
    assert!(is_free_license(&nest(16)));
    assert!(!is_free_license(&nest(17)));
    assert!(!is_free_license(&nest(100_000)));
    assert!(!is_free_license(&"(".repeat(1_000_000)));
    assert!(!is_free_license(&")".repeat(1_000_000)));
    // Over 1 KiB is refused even when it is well formed.
    let long = ["MIT"; 400].join(" OR ");
    assert!(long.len() > 1024);
    assert!(!is_free_license(&long));
    let ok = ["MIT"; 120].join(" OR ");
    assert!(ok.len() <= 1024 && is_free_license(&ok));
    assert!(!is_free_license(&"M".repeat(10_000)));
    assert!(!is_free_license(&"AND ".repeat(200)));
    assert!(!is_free_license(&"MIT WITH ".repeat(100)));
    assert!(!is_free_license(&"\u{0}".repeat(2000)));
    assert!(!is_free_license(&"é".repeat(600)));
}

// ---- icons and staleness

#[test]
fn icon_paths_stay_inside_the_catalog() {
    let mut src = source("alpha");
    let lib = alpha();
    let comp = lib
        .component(lib.find("org.example.Browser").unwrap())
        .clone();
    assert_eq!(src.icon_path(&comp, 64), None, "no dir");
    src.dir = Some("/cat/active".into());
    let p = |want| src.icon_path(&comp, want);
    assert_eq!(
        p(64),
        Some("/cat/active/icons/64x64/org.example.Browser.png".into())
    );
    assert_eq!(p(1), p(64));
    assert_eq!(
        p(65),
        Some("/cat/active/icons/128x128/org.example.Browser.png".into())
    );
    assert_eq!(p(128), p(65));
    assert_eq!(p(u16::MAX), p(128), "else the largest");

    let mut c = comp.clone();
    let good = ["a.png", "a.svg", "org.x-y_z+1.png", "A.SVG.png"];
    let bad = [
        "../x.png",
        "..",
        "a/../b.png",
        "/etc/passwd.png",
        "a/b.png",
        "a\\b.png",
        "x.txt",
        "x.png.txt",
        "x",
        "",
        ".png",
        "x.PNG",
        "x .png",
        "x\0.png",
        "x\n.png",
        "é.png",
        "x.png/",
        "..png",
        "a..b.png",
        "-x.png",
    ];
    for name in good {
        c.icon.as_mut().unwrap().file = name.into();
        assert_eq!(
            src.icon_path(&c, 64),
            Some(PathBuf::from(format!("/cat/active/icons/64x64/{name}"))),
            "{name}"
        );
    }
    for name in bad {
        c.icon.as_mut().unwrap().file = name.into();
        assert_eq!(src.icon_path(&c, 64), None, "{name:?}");
    }
    c.icon.as_mut().unwrap().file = "ok.png".into();
    c.icon.as_mut().unwrap().sizes.clear();
    assert_eq!(src.icon_path(&c, 64), None, "no cached size");
    c.icon = None;
    assert_eq!(src.icon_path(&c, 64), None, "no icon");
}

#[test]
fn staleness() {
    let now = SystemTime::now();
    let mut s = source("x");
    assert!(s.is_stale(now), "never downloaded");
    s.dir = Some("/d".into());
    s.commit = Some("ab".repeat(8));
    assert!(s.is_stale(now), "no time");
    s.updated = Some(now - Duration::from_secs(60));
    assert!(!s.is_stale(now));
    s.updated = Some(now - STALE_AFTER);
    assert!(!s.is_stale(now));
    s.updated = Some(now - STALE_AFTER - Duration::from_secs(1));
    assert!(s.is_stale(now));
    s.updated = Some(now + Duration::from_secs(86400));
    assert!(!s.is_stale(now), "a future time is not old");
    s.commit = None;
    assert!(s.is_stale(now));
}

// ---- load

fn write_catalog(dir: &std::path::Path, xml: &str) {
    fs::create_dir_all(dir).unwrap();
    let f = fs::File::create(dir.join("appstream.xml.gz")).unwrap();
    let mut z = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
    z.write_all(xml.as_bytes()).unwrap();
    z.finish().unwrap();
}

fn on_disk(root: &std::path::Path, remote: &str, commit: &str) -> CatalogSource {
    let dir = root.join("catalog");
    write_catalog(&dir, ALPHA);
    CatalogSource {
        dir: Some(dir),
        commit: Some(commit.into()),
        updated: Some(SystemTime::now()),
        ..source(remote)
    }
}

#[test]
fn load_uses_the_index_the_second_time() {
    let root = scratch("load");
    let cache = root.join("cache");
    let src = on_disk(&root, "alpha", "0123456789abcdef0123");
    let langs = vec!["de".to_string()];
    let first = load(&src, &cache, &langs).unwrap();
    assert_eq!(first.origin, "alpha");
    assert_eq!(first, catalog(ALPHA, "alpha"));
    let indexes = || {
        fs::read_dir(cache.join("user"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("index-alpha-"))
            .count()
    };
    assert_eq!(indexes(), 1);
    // The XML is gone: only the index can answer.
    fs::remove_file(src.dir.as_ref().unwrap().join("appstream.xml.gz")).unwrap();
    assert_eq!(load(&src, &cache, &langs).unwrap(), first);
    // Another commit or other languages need the XML again.
    let other = CatalogSource {
        commit: Some("fedcba9876543210fedc".into()),
        ..src.clone()
    };
    assert!(matches!(
        load(&other, &cache, &langs),
        Err(LoadError::Io(_))
    ));
    assert!(matches!(
        load(&src, &cache, &["fr".to_string()]),
        Err(LoadError::Io(_))
    ));
    // A new commit with its XML parses and indexes again.
    write_catalog(other.dir.as_ref().unwrap(), BETA);
    let beta = load(&other, &cache, &langs).unwrap();
    assert_eq!(beta.components.len(), 2);
    assert_eq!(indexes(), 1, "the old commit's index is replaced");
    // A damaged index is rebuilt rather than trusted.
    for e in fs::read_dir(cache.join("user")).unwrap().flatten() {
        fs::write(e.path(), b"garbage").unwrap();
    }
    assert_eq!(load(&other, &cache, &langs).unwrap(), beta);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_same_remote_in_both_scopes_keeps_an_index_each() {
    let root = scratch("scopes");
    let cache = root.join("cache");
    let user = on_disk(&root.join("u"), "flathub", "0123456789abcdef0123");
    let system = CatalogSource {
        scope: Scope::System,
        ..on_disk(&root.join("s"), "flathub", "fedcba9876543210fedc")
    };
    let langs = vec!["en".to_string()];
    let u = load(&user, &cache, &langs).unwrap();
    let s = load(&system, &cache, &langs).unwrap();
    // Neither load evicted the other's index: both answer without the XML.
    for src in [&user, &system] {
        fs::remove_file(src.dir.as_ref().unwrap().join("appstream.xml.gz")).unwrap();
    }
    assert_eq!(load(&user, &cache, &langs).unwrap(), u);
    assert_eq!(load(&system, &cache, &langs).unwrap(), s);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn load_failures_are_errors_not_panics() {
    let root = scratch("loadfail");
    let cache = root.join("cache");
    let langs: Vec<String> = Vec::new();
    // Never downloaded.
    let e = load(&source("alpha"), &cache, &langs).unwrap_err();
    assert!(matches!(e, LoadError::NotDownloaded));
    assert!(!e.to_string().is_empty());
    // Not XML.
    let dir = root.join("bad");
    write_catalog(&dir, "this is not xml <<<");
    let src = CatalogSource {
        dir: Some(dir.clone()),
        commit: Some("0123456789abcdef".into()),
        ..source("alpha")
    };
    let e = load(&src, &cache, &langs).unwrap_err();
    assert!(matches!(e, LoadError::Parse(_)), "{e:?}");
    assert!(!e.to_string().is_empty());
    // A DOCTYPE bomb is refused.
    write_catalog(
        &dir,
        "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY a \"b\">]><components/>",
    );
    assert!(matches!(
        load(&src, &cache, &langs),
        Err(LoadError::Parse(_))
    ));
    // Not gzip at all.
    fs::write(dir.join("appstream.xml.gz"), b"plain").unwrap();
    let e = load(&src, &cache, &langs).unwrap_err();
    assert!(matches!(e, LoadError::Io(_) | LoadError::Parse(_)), "{e:?}");
    // Missing file.
    fs::remove_file(dir.join("appstream.xml.gz")).unwrap();
    let e = load(&src, &cache, &langs).unwrap_err();
    assert!(matches!(e, LoadError::Io(_)), "{e:?}");
    assert!(!e.to_string().is_empty());
    // An unusable commit never builds a path from it.
    let evil = CatalogSource {
        commit: Some("../../etc/passwd".into()),
        ..src.clone()
    };
    assert!(load(&evil, &cache, &langs).is_err());
    assert!(!root.join("etc").exists());
    // A cache folder that cannot be written is not an error.
    write_catalog(&dir, ALPHA);
    let blocked = root.join("file");
    fs::write(&blocked, b"x").unwrap();
    assert!(load(&src, &blocked.join("cache"), &langs).is_ok());
    let _ = fs::remove_dir_all(&root);
}

// ---- the test remote

#[test]
fn sources_and_load_against_the_test_remote() {
    let Some((dir, _guard)) = common::remote() else {
        return;
    };
    common::reset(&dir);
    let cancel = CancelToken::new();

    // Before the first download: listed, but with no catalog.
    let out = list_sources(&cancel);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let src = out
        .sources
        .iter()
        .find(|s| s.remote == "test")
        .expect("the test remote is listed");
    assert_eq!(src.scope, Scope::User);
    assert_eq!(src.title, "Telamon Store Test");
    assert!(src.dir.is_none() && src.commit.is_none() && src.updated.is_none());
    assert!(src.is_stale(SystemTime::now()));
    assert!(matches!(
        load(src, &scratch("nodl"), &[]),
        Err(LoadError::NotDownloaded)
    ));

    common::must(&["update", "--appstream", "test"]);
    let out = list_sources(&cancel);
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    let src = out
        .sources
        .iter()
        .find(|s| s.remote == "test")
        .unwrap()
        .clone();
    let commit = src.commit.as_deref().expect("a commit");
    assert!((16..=64).contains(&commit.len()));
    assert!(
        commit
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );
    let cat_dir = src.dir.as_ref().unwrap();
    assert!(cat_dir.is_absolute());
    assert!(cat_dir.join("appstream.xml.gz").is_file());
    assert!(cat_dir.starts_with("/work/"), "{}", cat_dir.display());
    assert!(!cat_dir.is_symlink(), "resolved");
    let now = SystemTime::now();
    assert!(!src.is_stale(now));
    assert!(src.updated.unwrap() <= now + Duration::from_secs(5));
    assert!(src.is_stale(now + STALE_AFTER + Duration::from_secs(60)));
    // The system installation is empty here, and no error for that.
    assert!(out.sources.iter().all(|s| s.scope == Scope::User));

    let cache = scratch("remote-cache");
    let cat = load(&src, &cache, &[]).unwrap();
    assert_eq!(cat.origin, "test");
    let again = load(&src, &cache, &[]).unwrap();
    assert_eq!(cat, again);
    assert_eq!(
        fs::read_dir(cache.join("user")).unwrap().count(),
        1,
        "one index file"
    );

    let lib = Library::new(vec![(src.clone(), cat)]);
    assert_eq!(lib.len(), 1, "the add-ons and the runtime are not apps");
    let hello = lib.find(common::APP).unwrap();
    assert_eq!(lib.search("greeting", NONE, 10), [hello]);
    assert_eq!(lib.search("HELLO", NONE, 10), [hello]);
    assert_eq!(
        lib.categories(hello),
        [Category::Education, Category::Utilities]
    );
    assert!(lib.is_free(hello) && !lib.is_verified(hello));
    assert_eq!(
        lib.browse(Some(Category::Education), NONE, Sort::Name),
        [hello]
    );
    assert!(
        lib.browse(Some(Category::Games), NONE, Sort::Name)
            .is_empty()
    );
    let icon = lib
        .source(hello)
        .icon_path(lib.component(hello), 64)
        .expect("an icon path");
    assert!(
        icon.starts_with(cat_dir.join("icons")),
        "{}",
        icon.display()
    );
    assert!(icon.is_file(), "{}", icon.display());

    // A cancelled call returns what it has without panicking.
    let c = CancelToken::new();
    c.cancel();
    let _ = list_sources(&c);

    common::flatpak(&["remote-delete", "--force", "test"]);
    let out = list_sources(&cancel);
    assert!(out.sources.iter().all(|s| s.remote != "test"));
    let _ = fs::remove_dir_all(&cache);
}
