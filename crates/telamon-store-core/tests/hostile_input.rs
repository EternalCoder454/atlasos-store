//! Hostile input: the parsers of what a remote, a file, a link or a launch
//! hands the Store get real samples with random damage, many times over. They
//! must not panic, and what they accept must still obey the rules the dialogs
//! rely on (https only, clean text, plain IDs, a file that survives being
//! written back). A fixed seed keeps a failure repeatable: the failing input
//! is printed.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use telamon_store_core::appstream::{ParseOptions, parse as parse_catalog};
use telamon_store_core::flathub::{self, List};
use telamon_store_core::flatpakref::{
    FlatpakRef, FlatpakRepo, parse_flatpakref, parse_flatpakrepo, valid_remote_name,
};
use telamon_store_core::keyfile::{KeyFile, Limits};
use telamon_store_core::launch::{self, Request, https_url};
use telamon_store_core::permissions::Permissions;
use telamon_store_core::text::{self, Class};

const ITERATIONS: usize = 3000;

/// xorshift64*: small, fast, repeatable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Pieces that tend to break parsers.
const NASTY: &[&[u8]] = &[
    b"\n",
    b"\r\n",
    b"\r",
    b"=",
    b"[",
    b"]",
    b";",
    b"\\",
    b"\\;",
    b"\\s",
    b"\\q",
    b":",
    b"/",
    b"..",
    b"%",
    b"%2e",
    b"%00",
    b"!",
    b"#",
    b"?",
    b"@",
    b" ",
    b"\t",
    b"\0",
    b"https://",
    b"HTTPS://",
    b"file://",
    b"oci+https://",
    b"--",
    b"-",
    b"[Context]",
    b"[Flatpak Repo]",
    b"[Flatpak Ref]",
    b"Filter=/etc/passwd\n",
    b"GPGKey=",
    "\u{202e}".as_bytes(),
    "\u{200b}".as_bytes(),
    "\u{feff}".as_bytes(),
    "\u{2028}".as_bytes(),
    "\u{e9}".as_bytes(),
    b"\xff",
    b"\xc3",
    b"\xe2\x80",
];

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..1 + rng.below(6) {
        if v.is_empty() {
            v.push(b'a');
        }
        let at = rng.below(v.len());
        match rng.below(8) {
            0 => v[at] ^= 1 << rng.below(8),
            1 => {
                let end = (at + 1 + rng.below(16)).min(v.len());
                v.drain(at..end);
            }
            2 | 3 => {
                let piece = NASTY[rng.below(NASTY.len())];
                for (i, b) in piece.iter().enumerate() {
                    v.insert(at + i, *b);
                }
            }
            4 => {
                let end = (at + 1 + rng.below(64)).min(v.len());
                let chunk = v[at..end].to_vec();
                for (i, b) in chunk.iter().enumerate() {
                    v.insert(end + i, *b);
                }
            }
            5 => v.truncate(at),
            6 => {
                let other = rng.below(v.len());
                v.swap(at, other);
            }
            _ => v[at] = rng.next() as u8,
        }
    }
    v
}

fn fixture(path: &str) -> Vec<u8> {
    let p = format!("{}/tests/fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Runs `f` on `input`; a panic fails the test with the input shown.
fn never_panics<T>(what: &str, input: &[u8], f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => panic!("{what} panicked on {:?}", String::from_utf8_lossy(input)),
    }
}

/// Text meant for a person: nothing that hides itself or reorders what is
/// around it.
fn clean_text(s: &str) -> bool {
    !s.chars().any(|c| text::class(c) == Class::Drop) && s == s.trim()
}

fn check_ref(r: &FlatpakRef, input: &[u8]) {
    let same = https_url(&r.url).as_deref() == Some(r.url.as_str());
    assert!(
        same,
        "ref url {:?} for {:?}",
        r.url,
        String::from_utf8_lossy(input)
    );
    assert!(valid_remote_name(&r.suggest_remote_name));
    for t in [&r.title, &r.comment, &r.description].into_iter().flatten() {
        assert!(clean_text(t), "{t:?}");
    }
    for u in [&r.icon, &r.homepage, &r.runtime_repo]
        .into_iter()
        .flatten()
    {
        assert_eq!(https_url(u).as_deref(), Some(u.as_str()));
    }
    // What libflatpak would get parses back to the same thing.
    let bytes = r.to_bytes().expect("an accepted ref can be written back");
    assert_eq!(&parse_flatpakref(&bytes).expect("written ref reads"), r);
}

fn check_repo(r: &FlatpakRepo, input: &[u8]) {
    let same = https_url(&r.url).as_deref() == Some(r.url.as_str());
    assert!(
        same,
        "repo url {:?} for {:?}",
        r.url,
        String::from_utf8_lossy(input)
    );
    for t in [&r.title, &r.comment, &r.description].into_iter().flatten() {
        assert!(clean_text(t), "{t:?}");
    }
    let bytes = r.to_bytes().expect("an accepted repo can be written back");
    assert_eq!(&parse_flatpakrepo(&bytes).expect("written repo reads"), r);
    // The rewrite names no key the Store did not choose.
    let text = String::from_utf8(bytes).expect("the rewrite is text");
    for line in text.lines().filter(|l| !l.starts_with('[')) {
        let key = line.split('=').next().unwrap_or_default();
        assert!(
            [
                "Url",
                "Title",
                "Comment",
                "Description",
                "Icon",
                "Homepage",
                "DefaultBranch",
                "GPGKey",
                "CollectionID",
                "DeployCollectionID"
            ]
            .contains(&key),
            "unexpected key {key:?}"
        );
    }
}

#[test]
fn flatpakref_and_flatpakrepo_survive_damage() {
    let mut rng = Rng(0x5eed_0001);
    let seeds = [
        fixture("flatpakref/hello.flatpakref"),
        fixture("flatpakref/test.flatpakrepo"),
        fixture("flatpakref/unsigned.flatpakrepo"),
        fixture("flatpakref/filter.flatpakrepo"),
    ];
    let (mut refs, mut repos) = (0, 0);
    for i in 0..ITERATIONS {
        let input = mutate(&mut rng, &seeds[i % seeds.len()]);
        if let Ok(r) = never_panics("parse_flatpakref", &input, || parse_flatpakref(&input)) {
            refs += 1;
            never_panics("a parsed ref", &input, || check_ref(&r, &input));
        }
        if let Ok(r) = never_panics("parse_flatpakrepo", &input, || parse_flatpakrepo(&input)) {
            repos += 1;
            never_panics("a parsed repo", &input, || check_repo(&r, &input));
        }
    }
    // The damage is mild enough that some samples still parse: the checks
    // above ran on real values.
    assert!(refs > 20 && repos > 20, "{refs} refs, {repos} repos");
}

#[test]
fn key_files_survive_damage() {
    let mut rng = Rng(0x5eed_0002);
    let seeds = [
        fixture("flatpakref/hello.flatpakref"),
        fixture("permissions/firefox.metadata"),
        fixture("permissions/override-user.ini"),
    ];
    for i in 0..ITERATIONS {
        let input = mutate(&mut rng, &seeds[i % seeds.len()]);
        never_panics("KeyFile", &input, || {
            if let Ok(kf) = KeyFile::parse(&input, &Limits::default()) {
                let groups: Vec<String> = kf.groups().map(str::to_string).collect();
                for g in groups {
                    let keys: Vec<String> = kf.all_keys(&g).map(str::to_string).collect();
                    for k in keys {
                        let _ = kf.raw(&g, &k);
                        let _ = kf.string(&g, &k);
                        let _ = kf.list(&g, &k);
                        let _ = kf.bool(&g, &k);
                    }
                }
            }
        });
    }
}

#[test]
fn permissions_survive_damage_and_are_shown_clean() {
    let mut rng = Rng(0x5eed_0003);
    let seeds: Vec<Vec<u8>> = [
        "firefox.metadata",
        "obs.metadata",
        "steam.metadata",
        "gimp.metadata",
        "runtime.metadata",
    ]
    .iter()
    .map(|n| fixture(&format!("permissions/{n}")))
    .collect();
    let overrides = fixture("permissions/override-user.ini");
    let mut parsed = 0;
    for i in 0..ITERATIONS {
        let input = mutate(&mut rng, &seeds[i % seeds.len()]);
        never_panics("permissions", &input, || {
            let Ok(p) = Permissions::from_metadata(&input) else {
                return;
            };
            parsed += 1;
            let shown = p.permissions();
            for x in &shown {
                assert!(clean_text(x.describe()), "{:?}", x.describe());
                assert!(!x.describe().is_empty() && !x.code().is_empty());
            }
            // Nothing is added by itself, unless part of it cannot be read
            // the way flatpak reads it: that counts every time, by design.
            if shown.iter().all(|x| !x.is_unknown()) {
                assert!(p.added_since(&p).is_empty());
            }
            if let Ok(o) = p.with_overrides(&overrides) {
                let _ = o.permissions();
                let _ = o.added_since(&p);
            }
            let _ = p.with_runtime(&p).permissions();
            let _ = p.max_risk();
        });
    }
    assert!(parsed > 100, "{parsed} parsed");
}

#[test]
fn launch_arguments_survive_damage_and_stay_plain() {
    let mut rng = Rng(0x5eed_0004);
    let seeds: &[&str] = &[
        "--app=org.gimp.GIMP",
        "--remove",
        "org.gimp.GIMP",
        "--search",
        "photo editor",
        "--page=updates",
        "appstream://org.gimp.GIMP",
        "appstream:org.gimp.GIMP.desktop",
        "flatpak+https://dl.example.org/x/app.flatpakref",
        "file:///home/u/a%20b.flatpakrepo",
        "/home/u/Downloads/x.flatpakref",
        "./rel/x.flatpak",
        "--appimage-install=/home/u/a.AppImage",
        "--install-bundle",
        "/tmp/x.tar.zst",
        "--",
        "-weird.flatpakref",
    ];
    for _ in 0..ITERATIONS {
        let count = 1 + rng.below(6);
        let args: Vec<String> = (0..count)
            .map(|_| {
                let seed = seeds[rng.below(seeds.len())].as_bytes();
                // Launch arguments are text; damage that is not UTF-8 is lossy.
                String::from_utf8_lossy(&mutate(&mut rng, seed)).into_owned()
            })
            .collect();
        let cwd = if rng.below(4) == 0 { "" } else { "/home/u" };
        let shown = format!("{args:?}");
        let l = catch_unwind(AssertUnwindSafe(|| launch::parse(&args, Path::new(cwd))))
            .unwrap_or_else(|_| panic!("launch::parse panicked on {shown}"));
        for r in &l.requests {
            match r {
                Request::App(id) | Request::Remove(id) => {
                    assert!(launch::app_id(id).is_some() && !id.starts_with('-'), "{id}");
                }
                Request::File(_, p) => {
                    let s = p.to_string_lossy();
                    assert!(p.is_absolute() && !s.chars().any(launch::hidden), "{s}");
                    assert!(
                        !p.components().any(|c| c == std::path::Component::ParentDir),
                        "{s}"
                    );
                }
                Request::RefUrl(u) => assert_eq!(https_url(u).as_deref(), Some(u.as_str())),
                Request::Search(t) => {
                    assert!(clean_text(t) && t.chars().count() <= 200, "{t:?}");
                }
                Request::Page(_) => {}
            }
        }
        for r in &l.refused {
            assert!(!r.arg.chars().any(launch::hidden), "{:?}", r.arg);
            assert!(r.arg.chars().count() <= 120);
        }
    }
}

#[test]
fn flathub_answers_survive_damage() {
    let mut rng = Rng(0x5eed_0005);
    let seeds = [
        ("flathub/popular.json", List::Popular),
        ("flathub/apps-of-the-week.json", List::Picks),
        ("flathub/recently-updated.json", List::RecentlyUpdated),
    ];
    let bodies: Vec<Vec<u8>> = seeds.iter().map(|(f, _)| fixture(f)).collect();
    for i in 0..ITERATIONS {
        let which = i % seeds.len();
        let input = mutate(&mut rng, &bodies[which]);
        let list = seeds[which].1;
        let ids = never_panics("flathub::parse", &input, || flathub::parse(list, &input));
        if let Ok(ids) = ids {
            assert!(ids.len() <= flathub::MAX_IDS);
            let mut seen = std::collections::HashSet::new();
            for id in &ids {
                assert!(text::valid_id(id) && seen.insert(id.clone()), "{id}");
            }
        }
    }
}

#[test]
fn appstream_catalogs_survive_damage() {
    let mut rng = Rng(0x5eed_0006);
    let seeds = [
        fixture("catalog-alpha.xml"),
        fixture("catalog-beta.xml"),
        fixture("flathub-sample.xml"),
    ];
    let opts = ParseOptions {
        origin: "test".into(),
        ..ParseOptions::default()
    };
    // Fewer rounds: the samples are larger.
    for i in 0..ITERATIONS / 6 {
        let input = mutate(&mut rng, &seeds[i % seeds.len()]);
        let cat = never_panics("appstream::parse", &input, || {
            parse_catalog(input.as_slice(), &opts)
        });
        if let Ok(cat) = cat {
            for c in &cat.components {
                assert!(text::valid_id(&c.id), "{}", c.id);
                for t in [&c.name, &c.summary, &c.developer] {
                    assert!(clean_text(t), "{t:?}");
                }
                for (_, u) in &c.urls {
                    assert!(text::valid_url(u, true), "{u}");
                }
            }
        }
    }
}

#[test]
fn clean_text_is_clean_whatever_goes_in() {
    let mut rng = Rng(0x5eed_0007);
    let seed = "  A\u{202e}b\u{0}c\u{200b}d \t\n e\u{2066}f\u{feff}g\u{e0041}h \u{7f}i\u{85}j  "
        .as_bytes();
    for _ in 0..ITERATIONS {
        let input = String::from_utf8_lossy(&mutate(&mut rng, seed)).into_owned();
        let max = 1 + rng.below(40);
        let out = text::clean(&input, max);
        assert!(clean_text(&out), "{out:?}");
        assert!(out.chars().count() <= max);
        // Cleaning twice changes nothing.
        assert_eq!(text::clean(&out, max), out);
    }
}
