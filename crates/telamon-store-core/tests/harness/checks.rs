//! The invariants of every parser that reads untrusted bytes, as functions of
//! the input. One definition, two users: the proptest files in `tests/prop_*.rs`
//! (structure-aware inputs, bounded runs in `cargo test`) and the cargo-fuzz
//! targets in `fuzz/` (coverage-guided, in CI and by hand). A function here
//! panics when an invariant fails or a parser panics; an input that is
//! simply refused is fine.
//!
//! Only the public API of `telamon-store-core` is used, and nothing but the
//! standard library and the crates `telamon-store-core` itself depends on, so
//! that `fuzz/` can include this file with `#[path]`.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::os::unix::io::FromRawFd;
use std::path::{Path, PathBuf};

use telamon_store_core::appimage::{format, meta, squash};
use telamon_store_core::keyfile::{KeyFile, Limits as KeyLimits};
use telamon_store_core::launch;
use telamon_store_core::native::catalog::Catalog;
use telamon_store_core::native::desktop;
use telamon_store_core::native::github::Release;
use telamon_store_core::native::manifest::{
    self, Kind, MAX_FILE, MAX_FILES, MAX_UNPACKED, Manifest,
};
use telamon_store_core::native::version::Version;
use telamon_store_core::native::{self, archive};
use telamon_store_core::{flatpakref, net, text};

/// The app and repository the native checks use.
pub const ID: &str = "net.eterneon.telamon.gates";
pub const REPO: &str = "EternalCoder454/telamon-gates";

fn lower_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `s` is already what `text::clean` would make of it.
fn is_clean(s: &str, max_chars: usize) -> bool {
    s.chars().count() <= max_chars && text::clean(s, max_chars) == s
}

// ---- text, URLs, IDs ----

/// `text::clean`: no dropped character survives, whitespace is one plain
/// space between words and none at the ends, the cap counts characters, and
/// cleaning twice is cleaning once.
pub fn text_clean(s: &str, max_chars: usize) {
    let out = text::clean(s, max_chars);
    assert!(out.chars().count() <= max_chars, "over the cap: {out:?}");
    assert_eq!(out.trim(), out, "space at an end: {out:?}");
    assert!(!out.contains("  "), "two spaces: {out:?}");
    for c in out.chars() {
        let ok = c == ' ' || text::class(c) == text::Class::Keep;
        assert!(ok, "kept {c:?} (U+{:04X}) in {out:?}", c as u32);
        assert!(
            !c.is_control() && !matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069),
            "control or bidi character {c:?}"
        );
    }
    assert_eq!(text::clean(&out, max_chars), out, "not idempotent");
    // The buffer form agrees with the one-shot form.
    let mut b = text::LineBuf::new(max_chars);
    b.push_str(s);
    assert_eq!(b.finish(), out);
}

/// The validators for IDs, icon names, URLs and Flatpak targets.
pub fn text_validators(s: &str) {
    if text::valid_id(s) {
        assert!(s.len() <= 255 && s.split('.').count() >= 2);
        assert!(s.split('.').all(|p| !p.is_empty()));
        assert!(
            s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        );
    }
    if text::valid_icon_file(s) {
        assert!(s.ends_with(".png") || s.ends_with(".svg"));
        assert!(!s.contains("..") && !s.contains('/') && !s.starts_with(['.', '-']));
    }
    for https_only in [true, false] {
        if text::valid_url(s, https_only) {
            assert!(s.len() <= 2048);
            assert!(s.to_ascii_lowercase().starts_with("https://") || !https_only);
            assert!(!s.contains(['\\', ' ', '\n', '\t', '\r']), "{s:?}");
            // No user name or password: `@` can only be in the path or after.
            let rest = s.split_once("//").map_or("", |x| x.1);
            let auth = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
            assert!(!auth.contains('@'), "{s:?}");
            assert!(s.chars().all(|c| text::class(c) == text::Class::Keep));
        }
    }
    if text::valid_flatpak_target(s) {
        assert_eq!(s.split('/').count(), 3);
        assert!(text::valid_id(s.split('/').next().unwrap()));
    }
    if text::valid_bundle_ref(s) {
        assert!(s.starts_with("app/") || s.starts_with("runtime/"));
        assert!(text::valid_flatpak_target(s.split_once('/').unwrap().1));
    }
}

/// The host part (between `https://` and the first of `/?#`) of an accepted URL.
fn authority(u: &str) -> &str {
    let rest = &u[8..];
    &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())]
}

/// What an accepted `https_url` promises.
fn check_https_url(input: &str, u: &str) {
    assert!(u.starts_with("https://"), "{input:?} -> {u:?}");
    assert!(
        u.bytes().all(|b| b.is_ascii_graphic()),
        "not printable ASCII: {u:?}"
    );
    assert!(
        !u.contains(['\\', '"', '<', '>', '`', '{', '}', '|', '^']),
        "{u:?}"
    );
    let host = authority(u);
    // A lowercase DNS name: no user name, port or address literal.
    assert!(
        host.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-')),
        "host {host:?} in {u:?}"
    );
    let labels: Vec<&str> = host.split('.').collect();
    assert!(labels.len() >= 2, "{u:?}");
    assert!(
        labels
            .iter()
            .all(|l| (1..=63).contains(&l.len()) && !l.starts_with('-') && !l.ends_with('-')),
        "{u:?}"
    );
    assert!(
        labels
            .last()
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_lowercase()),
        "numeric top level (an address?) in {u:?}"
    );
    assert!(host.len() <= 253);
    // No dot segment, plain or percent-encoded.
    let tail = &u[8 + host.len()..];
    let path = tail.split(['?', '#']).next().unwrap();
    for seg in path.to_ascii_lowercase().replace("%2e", ".").split('/') {
        assert!(seg != "." && seg != "..", "dot segment in {u:?}");
    }
    // Idempotent: what it accepted it accepts again, unchanged.
    assert_eq!(launch::https_url(u).as_deref(), Some(u), "not idempotent");
}

pub fn https_url(s: &str) {
    if let Some(u) = launch::https_url(s) {
        check_https_url(s, &u);
    }
}

/// `net::redirect_target` from a base the launch policy accepts (a base the
/// Store fetched from is always one) and any `Location`.
pub fn redirect(base: &str, location: &str) {
    let Some(base) = launch::https_url(base) else {
        return;
    };
    if let Some(t) = net::redirect_target(&base, location) {
        assert!(t.starts_with("https://"), "left https: {t:?}");
        check_https_url(&t, &t);
        assert!(!location.starts_with("//"));
        if location.starts_with('/') {
            assert_eq!(authority(&t), authority(&base), "left the host");
        }
    }
}

pub fn app_id(s: &str) {
    if let Some(id) = launch::app_id(s) {
        assert_eq!(id, s);
        assert!(id.len() <= 255);
        let parts: Vec<&str> = id.split('.').collect();
        assert!(parts.len() >= 3);
        assert!(parts.iter().all(|p| !p.is_empty()));
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        );
        assert!(!id.starts_with(|c: char| c.is_ascii_digit()));
        assert!(text::valid_id(&id));
    }
}

/// `launch::internal_path` for the two options the Store has, as one argument
/// (`--opt=value`) or two.
pub fn internal_path(value: &str) {
    for option in ["--appimage-check", "--appimage-inspect"] {
        for args in [
            vec![option.to_string(), value.to_string()],
            vec![format!("{option}={value}")],
        ] {
            let Some(r) = launch::internal_path(option, &args) else {
                panic!("{args:?} is the option and was not recognised");
            };
            if let Ok(p) = r {
                assert!(p.is_absolute(), "{p:?}");
                assert!(
                    p.components()
                        .all(|c| !matches!(c, std::path::Component::ParentDir)),
                    "{p:?}"
                );
                let s = p.to_string_lossy();
                assert!(!s.chars().any(launch::hidden), "{s:?}");
                assert!(!s.ends_with('/'));
                assert_eq!(p.as_os_str(), Path::new(value).as_os_str());
            }
        }
    }
    // Other options are not this function's business.
    assert!(launch::internal_path("--appimage-check", &["--other".into(), value.into()]).is_none());
}

pub fn search_text(s: &str) {
    if let Some(t) = launch::search_text(s) {
        assert!(t.chars().count() <= 200);
        assert!(!t.chars().any(launch::hidden));
        assert!(!t.is_empty() && t.trim() == t);
    }
}

/// A whole launch command line: what comes out is a short list of requests that
/// are each valid, and what is refused is listed, never acted on.
pub fn launch_args(args: &[String]) {
    use launch::Request;
    let cwd = Path::new("/home/u");
    let l = launch::parse(args, cwd);
    assert!(l.requests.len() <= 8 && l.refused.len() <= 8);
    if args.is_empty() {
        assert_eq!(l.requests, [Request::Page(launch::Page::Home)]);
    }
    assert!(l.dropped >= args.len().saturating_sub(launch::MAX_ARGS));
    for r in &l.requests {
        match r {
            Request::App(id) | Request::Remove(id) => {
                assert_eq!(launch::app_id(id).as_deref(), Some(id.as_str()), "{id:?}");
            }
            Request::Search(t) => {
                assert!(t.chars().count() <= 200 && !t.chars().any(launch::hidden));
            }
            Request::File(_, p) => {
                assert!(p.is_absolute(), "{p:?}");
                assert!(
                    p.components()
                        .all(|c| !matches!(c, std::path::Component::ParentDir)),
                    "{p:?}"
                );
                assert!(!p.to_string_lossy().chars().any(launch::hidden), "{p:?}");
            }
            Request::RefUrl(u) => {
                assert_eq!(launch::https_url(u).as_deref(), Some(u.as_str()), "{u:?}");
            }
            Request::Page(_) => {}
        }
    }
    for r in &l.refused {
        // What is shown of a refused argument is short and has no hidden characters.
        assert!(
            r.arg.chars().count() <= 120 && !r.arg.chars().any(launch::hidden),
            "{:?}",
            r.arg
        );
    }
}

pub fn version(a: &str, b: &str) {
    let (Some(x), Some(y)) = (Version::parse(a), Version::parse(b)) else {
        return;
    };
    assert_eq!(x.partial_cmp(&y).map(|o| o.reverse()), y.partial_cmp(&x));
    assert_eq!(x == y, x.cmp(&y) == std::cmp::Ordering::Equal);
    // What is printed reads back as the same version.
    let again = Version::parse(&x.to_string()).expect("a version prints as a version");
    assert_eq!(again, x);
    assert_eq!(x.as_str(), a);
}

// ---- key files, .flatpakref, .flatpakrepo ----

/// What `KeyFile::parse` promises about its limits and its contents.
pub fn keyfile(data: &[u8], limits: &KeyLimits) {
    let Ok(kf) = KeyFile::parse(data, limits) else {
        return;
    };
    assert!(data.len() <= limits.max_bytes);
    assert!(!data.contains(&0));
    assert!(std::str::from_utf8(data).is_ok());
    let groups: Vec<&str> = kf.groups().collect();
    assert!(groups.len() <= limits.max_groups);
    assert_eq!(
        groups.iter().collect::<BTreeSet<_>>().len(),
        groups.len(),
        "a repeated group must merge"
    );
    let mut keys = 0;
    let mut text = String::new();
    let mut crlf_edge = false;
    for g in &groups {
        assert!(!g.is_empty() && !g.contains(['[', ']']) && !g.chars().any(char::is_control));
        text.push_str(&format!("[{g}]\n"));
        let all: Vec<&str> = kf.all_keys(g).collect();
        assert_eq!(
            all.iter().collect::<BTreeSet<_>>().len(),
            all.len(),
            "a repeated key keeps one value"
        );
        keys += all.len();
        for k in &all {
            let raw = kf.raw(g, k).expect("a listed key has a value");
            assert!(raw.len() <= limits.max_value);
            // Keys and values never hold what ends a line.
            assert!(!k.contains('\n') && !raw.contains('\n'));
            crlf_edge |= raw.ends_with('\r');
            text.push_str(&format!("{k}={raw}\n"));
        }
        // `keys` is `all_keys` without translations.
        let plain: Vec<&str> = kf.keys(g).collect();
        assert!(plain.iter().all(|k| !k.contains('[')));
    }
    assert!(keys <= limits.max_keys);
    assert!(data.split(|b| *b == b'\n').count() <= limits.max_lines + 1);
    // Written out and read again it is the same file (a value that ends in a
    // lone CR, from a last line with no newline, is the one thing a newline
    // after it changes, as in GLib).
    if !crlf_edge && text.split('\n').count() <= limits.max_lines + 1 {
        let big = KeyLimits {
            max_bytes: usize::MAX,
            max_lines: usize::MAX,
            ..*limits
        };
        let back = KeyFile::parse(text.as_bytes(), &big).expect("a written key file reads back");
        // (Line numbers differ: only groups, keys and values are compared.)
        let shape = |k: &KeyFile| -> Vec<(String, Vec<(String, String)>)> {
            k.groups()
                .map(|g| {
                    let keys = k
                        .all_keys(g)
                        .map(|key| (key.to_string(), k.raw(g, key).unwrap().to_string()))
                        .collect();
                    (g.to_string(), keys)
                })
                .collect()
        };
        assert_eq!(
            shape(&back),
            shape(&kf),
            "not the same after writing and reading"
        );
    }
}

/// `.flatpakref` and `.flatpakrepo`: refused or fully checked, and written
/// back canonically.
pub fn flatpakref(data: &[u8]) {
    if let Ok(r) = flatpakref::parse_flatpakref(data) {
        assert!(data.len() <= flatpakref::MAX_FILE_BYTES);
        assert!(text::valid_id(&r.name), "{:?}", r.name);
        assert_eq!(launch::https_url(&r.url).as_deref(), Some(r.url.as_str()));
        assert!(flatpakref::valid_remote_name(&r.suggest_remote_name));
        for u in [&r.runtime_repo].into_iter().flatten() {
            assert_eq!(launch::https_url(u).as_deref(), Some(u.as_str()));
        }
        for u in [&r.icon, &r.homepage].into_iter().flatten() {
            assert!(text::valid_url(u, true), "{u:?}");
        }
        check_texts(&[(&r.title, 200), (&r.comment, 300), (&r.description, 2000)]);
        if let Some(k) = &r.key {
            check_key(k);
        }
        let out = r.to_bytes().expect("a parsed file can be written");
        let again = flatpakref::parse_flatpakref(&out).expect("the written file reads back");
        assert_eq!(again, r, "write and read changed the value");
        assert_eq!(
            again.to_bytes().unwrap(),
            out,
            "the written form is not stable"
        );
    }
    if let Ok(r) = flatpakref::parse_flatpakrepo(data) {
        assert!(data.len() <= flatpakref::MAX_FILE_BYTES);
        assert_eq!(launch::https_url(&r.url).as_deref(), Some(r.url.as_str()));
        for u in [&r.icon, &r.homepage].into_iter().flatten() {
            assert!(text::valid_url(u, true), "{u:?}");
        }
        check_texts(&[(&r.title, 200), (&r.comment, 300), (&r.description, 2000)]);
        if let Some(k) = &r.key {
            check_key(k);
        }
        let out = r.to_bytes().expect("a parsed file can be written");
        let again = flatpakref::parse_flatpakrepo(&out).expect("the written file reads back");
        assert_eq!(again, r);
        assert_eq!(again.to_bytes().unwrap(), out);
    }
}

fn check_texts(fields: &[(&Option<String>, usize)]) {
    for (f, max) in fields {
        if let Some(s) = f {
            assert!(is_clean(s, *max), "not clean text: {s:?}");
        }
    }
}

fn check_key(k: &flatpakref::GpgKey) {
    assert!(k.bytes().len() <= flatpakref::MAX_KEY_BYTES);
    let fp = k.fingerprint();
    assert!(
        (40..=64).contains(&fp.len()) && fp.bytes().all(|b| b.is_ascii_hexdigit()),
        "fingerprint {fp:?}"
    );
    let again = flatpakref::GpgKey::from_bytes(k.bytes().to_vec()).expect("a key reads back");
    assert_eq!(again.fingerprint(), fp);
}

// ---- native apps ----

fn link_stays_inside(path: &str, target: &str) -> bool {
    // An independent model of `link_target_ok`: the folder of the link,
    // then the target's parts, never above the root.
    let mut stack: Vec<&str> = path.split('/').collect();
    stack.pop();
    for part in target.split('/') {
        match part {
            "" | "." => return false,
            ".." => {
                if stack.pop().is_none() {
                    return false;
                }
            }
            p => stack.push(p),
        }
    }
    true
}

/// The invariants of a manifest `Manifest::parse` accepted.
pub fn check_manifest(m: &Manifest, kind: Kind) {
    assert_eq!(m.schema, 1);
    assert!(native::valid_app_id(&m.id), "{:?}", m.id);
    assert!(Version::parse(&m.version).is_some());
    assert!(Version::parse(&m.min_telamon_ui).is_some());
    assert_eq!(m.arch, manifest::ARCH);
    assert!(
        !m.min_os_version.is_empty()
            && m.min_os_version.len() <= 4
            && m.min_os_version.bytes().all(|b| b.is_ascii_digit())
    );
    assert!(
        m.homepage.is_empty() || launch::https_url(&m.homepage).as_deref() == Some(&m.homepage)
    );
    assert!(!m.name.is_empty() && is_clean(&m.name, 80), "{:?}", m.name);
    assert!(is_clean(&m.summary, 300) && is_clean(&m.license, 100));
    assert!(!m.files.is_empty() && m.files.len() <= MAX_FILES && m.links.len() <= MAX_FILES);
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    for f in &m.files {
        assert!(manifest::valid_rel_path(&f.path) && f.path != manifest::NAME);
        assert!(lower_hex64(&f.sha256), "{:?}", f.sha256);
        assert!(f.size <= MAX_FILE);
        total = total.saturating_add(f.size);
        assert!(seen.insert(f.path.as_str()), "{} twice", f.path);
    }
    assert!(total <= MAX_UNPACKED);
    for l in &m.links {
        assert!(manifest::valid_rel_path(&l.path));
        assert!(seen.insert(l.path.as_str()), "{} twice", l.path);
        assert!(
            manifest::link_target_ok(&l.path, &l.target),
            "{:?} -> {:?}",
            l.path,
            l.target
        );
        assert!(link_stays_inside(&l.path, &l.target));
        assert!(!l.target.starts_with('/') && !l.target.chars().any(launch::hidden));
    }
    match (kind, &m.archive) {
        (Kind::Outer, Some(a)) => {
            assert_eq!(a.name, manifest::archive_name(&m.id, &m.version));
            assert!(lower_hex64(&a.sha256));
            assert!(a.size > 0 && a.size <= manifest::MAX_ARCHIVE);
        }
        (Kind::Inner, None) => {}
        other => panic!("accepted a {other:?}"),
    }
    // The derived views do not panic and agree with themselves.
    let progs = desktop::programs(m);
    for p in &progs {
        assert!(!p.contains('/'));
    }
    assert!(m.same_content(m));
    let _ = m.parsed_version();
    // Written as JSON it reads back as the same manifest.
    let json = serde_json::to_vec(m).expect("serializes");
    let again = Manifest::parse(&json, kind).expect("a written manifest reads back");
    assert_eq!(&again, m, "write and read changed the manifest");
}

pub fn native_manifest(data: &[u8]) {
    for kind in [Kind::Outer, Kind::Inner] {
        if let Ok(m) = Manifest::parse(data, kind) {
            assert!(data.len() as u64 <= manifest::MAX_MANIFEST);
            check_manifest(&m, kind);
        }
    }
}

/// `valid_rel_path` and `link_target_ok` on their own.
pub fn native_paths(path: &str, target: &str) {
    if manifest::valid_rel_path(path) {
        assert!(path.len() <= 1024 && !path.starts_with('/'));
        for part in path.split('/') {
            assert!(!part.is_empty() && part != "." && part != ".." && part.len() <= 255);
            assert!(!part.chars().any(launch::hidden));
        }
    }
    if manifest::link_target_ok(path, target) {
        assert!(link_stays_inside(path, target), "{path:?} -> {target:?}");
    }
}

pub fn native_catalog(data: &[u8]) {
    let Ok(c) = Catalog::parse(data) else { return };
    assert!(data.len() as u64 <= native::catalog::MAX_CATALOG);
    assert!(c.apps.len() <= native::catalog::MAX_ENTRIES);
    let mut ids = BTreeSet::new();
    for e in &c.apps {
        assert!(native::valid_app_id(&e.id));
        assert!(native::catalog::valid_repo(&e.repo), "{:?}", e.repo);
        let owner = e.repo.split('/').next().unwrap();
        assert!(
            native::ALLOWED_OWNERS
                .iter()
                .any(|o| o.eq_ignore_ascii_case(owner))
        );
        assert_eq!(e.channel, "releases");
        assert!(ids.insert(&e.id), "{} listed twice", e.id);
    }
}

pub fn github_release(data: &[u8], repo: &str) {
    let Ok(r) = Release::parse(data, repo) else {
        return;
    };
    assert!(data.len() as u64 <= native::github::MAX_RELEASE_JSON);
    assert!(
        !r.tag.is_empty()
            && r.tag.len() <= 64
            && r.tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    );
    let prefix = format!("https://github.com/{repo}/releases/download/{}/", r.tag);
    for a in &r.assets {
        assert!(a.url.len() == prefix.len() + a.name.len());
        assert!(
            a.url[..prefix.len()].eq_ignore_ascii_case(&prefix),
            "{:?}",
            a.url
        );
        assert_eq!(&a.url[prefix.len()..], a.name);
        assert!(
            !a.name.is_empty()
                && a.name.len() <= 200
                && a.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        );
        if let Some(h) = &a.sha256 {
            assert!(lower_hex64(h));
        }
        assert_eq!(r.asset(&a.name).map(|x| &x.name), Some(&a.name));
    }
    assert!(r.assets.len() <= 300);
}

fn limits_for_desktop() -> KeyLimits {
    KeyLimits {
        max_bytes: 64 * 1024,
        max_lines: 1000,
        max_groups: 32,
        max_keys: 400,
        max_value: 8192,
    }
}

/// The text a launcher gets as the first `Exec` word for `program`.
fn expected_program(prefix: &Path, program: &str) -> String {
    prefix
        .join("bin")
        .join(program)
        .to_string_lossy()
        .into_owned()
}

/// Checks the output of `rewrite_desktop` / `rewrite_dbus`: no key the input
/// did not have, none of the dropped ones, our marker, and every `Exec`
/// starting with the program's absolute path as one argument.
fn check_rewritten(
    src: &[u8],
    out: &[u8],
    prefix: &Path,
    programs: &BTreeSet<String>,
    version: &str,
    main_group: &str,
) {
    let kf = KeyFile::parse(out, &KeyLimits::default()).expect("the output reads as a key file");
    let input = KeyFile::parse(src, &KeyLimits::default()).expect("the input did");
    assert_eq!(kf.groups().next(), Some(main_group));
    assert_eq!(kf.raw(main_group, desktop::MARKER), Some(ID));
    assert_eq!(kf.raw(main_group, desktop::MARKER_VERSION), Some(version));
    // The input's keys, as the rewriter reads them (trimmed of white space).
    let mut allowed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for g in input.groups() {
        let e = allowed.entry(g).or_default();
        for k in input.all_keys(g) {
            e.insert(k.trim());
        }
    }
    for g in kf.groups() {
        for k in kf.all_keys(g) {
            assert!(
                !(k == "TryExec"
                    || k == "Path"
                    || k.starts_with("TryExec[")
                    || k.starts_with("Path[")),
                "kept {k}"
            );
            if k.starts_with("X-Telamon-Native-") {
                assert!(
                    g == main_group && (k == desktop::MARKER || k == desktop::MARKER_VERSION),
                    "a bundle's {k} in [{g}]"
                );
                continue;
            }
            assert!(
                allowed.get(g).is_some_and(|ks| ks.contains(k)),
                "key {k:?} in [{g}] is not in the input"
            );
        }
    }
    let program_paths: BTreeSet<String> = programs
        .iter()
        .map(|p| expected_program(prefix, p))
        .collect();
    for g in kf.groups() {
        let Some(raw) = kf.raw(g, "Exec") else {
            continue;
        };
        match kf.string(g, "Exec") {
            Ok(Some(value)) => {
                let words = telamon_store_core::appimage::install::split_exec(&value);
                if let Some(words) = words {
                    let first = words.first().expect("an Exec has a first word");
                    assert!(
                        program_paths.contains(first),
                        "Exec starts with {first:?}, not a program of the bundle under {prefix:?} ({value:?})"
                    );
                } else {
                    // The arguments after the program are the bundle's text and
                    // may be badly quoted; the program itself is still first.
                    assert!(
                        program_paths.iter().any(|p| {
                            let q = telamon_store_core::appimage::install::exec_arg(p);
                            value.starts_with(&q)
                        }),
                        "{value:?}"
                    );
                }
            }
            _ => {
                // An argument with a bad escape: the program's own part is
                // still the start of the raw value.
                assert!(
                    program_paths.iter().any(|p| {
                        let q = telamon_store_core::appimage::install::escape_value(
                            &telamon_store_core::appimage::install::exec_arg(p),
                        );
                        raw.starts_with(&q)
                            && raw[q.len()..]
                                .chars()
                                .next()
                                .is_none_or(|c| c == ' ' || c == '\t')
                    }),
                    "Exec={raw:?}"
                );
            }
        }
    }
}

/// `rewrite_desktop` on any bytes, for a `prefix` and a set of programs.
pub fn native_desktop(src: &[u8], prefix: &Path, programs: &BTreeSet<String>) {
    let version = "1.2.3";
    let Ok((out, exe)) = desktop::rewrite_desktop(src, ID, version, prefix, programs) else {
        return;
    };
    assert!(programs.contains(&exe), "{exe:?}");
    assert!(std::str::from_utf8(&out).is_ok());
    // The result obeys the limits the rewriter itself reads with.
    KeyFile::parse(&out, &limits_for_desktop()).expect("output within the desktop limits");
    check_rewritten(src, &out, prefix, programs, version, "Desktop Entry");
    // Rewriting what was written is refused only because the program is
    // now a path (a bare name is what a bundle must write), never a panic.
    let _ = desktop::rewrite_desktop(&out, ID, version, prefix, programs);
}

pub fn native_dbus(src: &[u8], prefix: &Path, programs: &BTreeSet<String>) {
    let version = "1.2.3";
    let Ok((out, name)) = desktop::rewrite_dbus(src, ID, version, prefix, programs) else {
        return;
    };
    assert!(name == ID || name.strip_prefix(ID).is_some_and(|r| r.starts_with('.')));
    assert!(native::valid_app_id(&name));
    check_rewritten(src, &out, prefix, programs, version, "D-BUS Service");
    let kf = KeyFile::parse(&out, &KeyLimits::default()).unwrap();
    assert_eq!(kf.groups().count(), 1);
    assert_eq!(kf.raw("D-BUS Service", "Name"), Some(name.as_str()));
}

fn scratch_dir(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "telamon-store-prop-{}-{tag}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch folder");
    dir
}

/// `desktop::plan` over a tree of `files` and `links` (paths relative to the
/// tree, content as bytes), with a manifest made to list exactly those.
pub fn native_plan(files: &[(String, Vec<u8>, bool)], links: &[(String, String)], prefix: &Path) {
    let dir = scratch_dir("plan");
    let tree = dir.join("tree");
    let mut m = native_manifest_for(files, links);
    m.files.truncate(MAX_FILES);
    for (path, bytes, _) in files {
        if !manifest::valid_rel_path(path) {
            continue;
        }
        let at = tree.join(path);
        if std::fs::create_dir_all(at.parent().unwrap()).is_err() {
            continue;
        }
        let _ = std::fs::write(&at, bytes);
    }
    for (path, target) in links {
        if !manifest::valid_rel_path(path) {
            continue;
        }
        let at = tree.join(path);
        if std::fs::create_dir_all(at.parent().unwrap()).is_ok() {
            let _ = std::os::unix::fs::symlink(target, &at);
        }
    }
    let _ = std::fs::create_dir_all(&tree);
    let Ok(tree_dir) = telamon_store_core::native::dirfd::Dir::open_following(&tree) else {
        return;
    };
    if let Ok(plan) = desktop::plan(&tree_dir, &m, prefix) {
        assert!(plan.exports.len() <= 200);
        let progs = desktop::programs(&m);
        let exe = plan.exe.strip_prefix("bin/").expect("exe is under bin/");
        assert!(progs.contains(exe), "{:?}", plan.exe);
        let mut tos = BTreeSet::new();
        let last = ID.rsplit('.').next().unwrap();
        for e in &plan.exports {
            // Only these places, only under the app's own name.
            assert!(manifest::valid_rel_path(&e.to), "{:?}", e.to);
            assert!(tos.insert(&e.to), "{} exported twice", e.to);
            let file = e.to.rsplit('/').next().unwrap();
            let top = e.to.split('/').next().unwrap();
            assert!(
                matches!(
                    top,
                    "applications" | "icons" | "metainfo" | "dbus-1" | "knotifications6"
                ),
                "exports to {:?}",
                e.to
            );
            assert!(
                file.starts_with(ID) || file.starts_with(&format!("telamon-{last}")),
                "{:?} does not carry the app ID",
                e.to
            );
            assert!(e.bytes.len() <= 1024 * 1024);
            assert!(m.files.iter().any(|f| f.path == e.from));
            if top == "applications" {
                assert_eq!(e.to, format!("applications/{ID}.desktop"));
                check_rewritten(
                    &std::fs::read(tree.join(&e.from)).unwrap(),
                    &e.bytes,
                    prefix,
                    &progs,
                    &m.version,
                    "Desktop Entry",
                );
            }
        }
        assert!(tos.contains(&format!("applications/{ID}.desktop")));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A manifest that lists `files` and `links` (not checked: `plan` reads the
/// listing, the sizes and checksums do not matter to it).
pub fn native_manifest_for(
    files: &[(String, Vec<u8>, bool)],
    links: &[(String, String)],
) -> Manifest {
    Manifest {
        schema: 1,
        id: ID.into(),
        name: "Gates".into(),
        version: "1.2.3".into(),
        summary: String::new(),
        homepage: String::new(),
        license: "MIT".into(),
        arch: "x86_64".into(),
        min_telamon_ui: "2.0.0".into(),
        min_os_version: "44".into(),
        files: files
            .iter()
            .map(|(p, b, x)| manifest::FileEntry {
                path: p.clone(),
                size: b.len() as u64,
                sha256: "0".repeat(64),
                executable: *x,
            })
            .collect(),
        links: links
            .iter()
            .map(|(p, t)| manifest::LinkEntry {
                path: p.clone(),
                target: t.clone(),
            })
            .collect(),
        commands: None,
        archive: None,
    }
}

// ---- the tar bundle ----

/// Everything under `dir`, not following links: (relative path, kind).
fn walk(dir: &Path) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    fn go(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, &'static str)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let md = std::fs::symlink_metadata(&p).unwrap();
            let rel = p.strip_prefix(base).unwrap().to_path_buf();
            if md.file_type().is_symlink() {
                out.push((rel, "link"));
            } else if md.is_dir() {
                out.push((rel.clone(), "dir"));
                go(base, &p, out);
            } else {
                out.push((rel, "file"));
            }
        }
    }
    go(dir, dir, &mut out);
    out.sort();
    out
}

/// A file name that only a bundle escaping its folder could create.
pub const ABSOLUTE_CANARY: &str = "/tmp/telamon-prop-abs-canary";

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// `archive::unpack` on `archive_bytes` (a `.tar.zst`, or anything), into a
/// fresh folder three levels down in a sandbox that has a canary file at each
/// level: an error, or a tree below the destination only.
pub fn native_unpack(archive_bytes: &[u8]) {
    let root = scratch_dir("unpack");
    let one = root.join("one");
    let two = one.join("two");
    let dest = two.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    for dir in [&root, &one, &two] {
        std::fs::write(dir.join("canary"), b"canary").unwrap();
    }
    let file = two.join("bundle.tar.zst");
    std::fs::write(&file, archive_bytes).unwrap();

    let result = archive::unpack(&file, &dest, None);

    assert_eq!(
        names(&root),
        ["canary", "one"],
        "something was created at the top"
    );
    assert_eq!(
        names(&one),
        ["canary", "two"],
        "something was created one level up"
    );
    assert_eq!(
        names(&two),
        ["bundle.tar.zst", "canary", "dest"],
        "something was created beside the destination"
    );
    for dir in [&root, &one, &two] {
        assert_eq!(std::fs::read(dir.join("canary")).unwrap(), b"canary");
    }
    assert!(
        !Path::new(ABSOLUTE_CANARY).exists(),
        "a bundle wrote to an absolute path"
    );
    let sandbox = root;

    let tree = walk(&dest);
    let root = std::fs::canonicalize(&dest).unwrap();
    for (rel, kind) in &tree {
        assert!(
            rel.components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "{rel:?}"
        );
        let full = dest.join(rel);
        match *kind {
            "link" => {
                let t = std::fs::read_link(&full).unwrap();
                assert!(t.is_relative(), "absolute link {rel:?} -> {t:?}");
                if result.is_ok() {
                    // A finished bundle's links all lead to something inside.
                    let real = std::fs::canonicalize(&full).expect("a link points somewhere");
                    assert!(
                        real.starts_with(&root),
                        "{rel:?} leaves the folder: {real:?}"
                    );
                }
            }
            _ => {
                let real = std::fs::canonicalize(&full).unwrap();
                assert!(real.starts_with(&root), "{rel:?} is outside");
            }
        }
    }
    if let Ok(m) = &result {
        check_manifest(m, Kind::Inner);
        let files: BTreeSet<String> = tree
            .iter()
            .filter(|(_, k)| *k == "file")
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();
        let mut listed: BTreeSet<String> = m.files.iter().map(|f| f.path.clone()).collect();
        listed.insert(manifest::NAME.to_string());
        assert_eq!(files, listed, "the tree is not what the manifest lists");
        use std::os::unix::fs::PermissionsExt;
        for (rel, kind) in &tree {
            let mode = std::fs::symlink_metadata(dest.join(rel))
                .unwrap()
                .permissions()
                .mode();
            if *kind != "link" {
                assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky on {rel:?}");
                assert_eq!(mode & 0o022, 0, "group/world writable {rel:?}: {mode:o}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&sandbox);
}

// ---- AppImage: ELF header, squashfs, metadata ----

/// A `File` holding `bytes`, in memory (no disk, no name).
pub fn memfile(bytes: &[u8]) -> File {
    // SAFETY: memfd_create returns a new descriptor that nothing else owns.
    let fd = unsafe { libc::memfd_create(c"telamon-fuzz".as_ptr(), libc::MFD_CLOEXEC) };
    assert!(fd >= 0, "memfd_create");
    let mut f = unsafe { File::from_raw_fd(fd) };
    f.write_all(bytes).unwrap();
    f
}

fn small_limits() -> squash::Limits {
    squash::Limits {
        max_inodes: 4096,
        max_fragments: 4096,
        max_meta_blocks: 64,
        max_file: 64 * 1024,
        max_total: 256 * 1024,
        max_kept: 512,
    }
}

fn check_meta(m: &meta::Meta) {
    assert!(is_clean(&m.name, meta::MAX_NAME), "{:?}", m.name);
    assert!(
        is_clean(&m.summary, 300)
            && is_clean(&m.version, 100)
            && is_clean(&m.publisher, meta::MAX_NAME)
    );
    assert!(
        m.app_id.is_empty() || text::valid_id(&m.app_id),
        "{:?}",
        m.app_id
    );
    if let Some(i) = &m.icon {
        assert_eq!(meta::icon_kind(&i.bytes), Some(i.kind));
        assert!(i.bytes.len() as u64 <= meta::MAX_ICON);
    }
}

/// backhand's panics inside `squash::with_tree` are contained there (an image
/// that makes it slice out of range is "damaged"), so they are not findings:
/// the panic hook libFuzzer installs would abort on them anyway. Panics from
/// anywhere else still reach the previous hook.
fn contain_backhand_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if squash::in_contained_backhand_call() {
                return;
            }
            previous(info);
        }));
    });
}

/// The squashfs at the start of `bytes`: superblock checks, the tree it
/// yields, every kept path read back. Never reads more than the limits, never
/// panics, and a path it lists is a plain relative one.
pub fn squashfs(bytes: &[u8]) {
    contain_backhand_panics();
    let file = memfile(bytes);
    let len = bytes.len() as u64;
    let limits = small_limits();
    let r = squash::with_tree(&file, 0, len, &limits, |tree| {
        let all = tree.list("");
        let mut listed = all.clone();
        for prefix in [
            "usr/share/metainfo/",
            "usr/share/applications/",
            "usr/share/icons/hicolor/",
            "usr/share/pixmaps/",
        ] {
            listed.extend(tree.list(prefix));
        }
        let mut total = 0u64;
        for p in listed {
            assert!(!p.is_empty() && !p.starts_with('/'));
            assert!(
                !p.split('/').any(|s| s.is_empty() || s == ".." || s == "."),
                "{p:?}"
            );
            match tree.kind(p) {
                Some(squash::Kind::File(n)) => {
                    if let Ok(b) = tree.read(p, 4096) {
                        assert_eq!(b.len() as u64, n);
                        assert!(n <= 4096);
                        total += n;
                    }
                }
                Some(squash::Kind::Symlink) => {
                    // A link is read as the file it ends at, inside the image.
                    let _ = tree.read(p, 4096);
                }
                _ => {}
            }
        }
        assert!(total <= limits.max_total + 4096 * 8);
        meta::extract(tree)
    });
    if let Ok(m) = r {
        check_meta(&m);
    }
}

/// An AppImage-shaped file: the ELF runtime, then the squashfs where the ELF
/// says it ends.
pub fn appimage_file(bytes: &[u8]) {
    contain_backhand_panics();
    let file = memfile(bytes);
    let len = bytes.len() as u64;
    let _ = format::sniff_file(&file);
    let _ = format::sniff(&bytes[..bytes.len().min(16)]);
    if let Ok(elf) = format::read_elf(&file, len) {
        assert!(elf.end <= len);
        for s in &elf.sections {
            assert!(s.name.chars().count() <= 64);
            for max in [0, 1024, 8192] {
                if let Some(b) = elf.read_section(&file, len, &s.name, max) {
                    assert!(b.len() as u64 <= max);
                    let sec = elf.section(&s.name).unwrap();
                    assert!(sec.offset.checked_add(sec.size).unwrap() <= len);
                }
            }
        }
        if let Ok(m) = squash::with_tree(&file, elf.end, len, &small_limits(), meta::extract) {
            check_meta(&m);
        }
    }
}

fn svg_seen_by_a_real_parser(text: &str) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut open = 0usize;
    loop {
        let event = match reader.read_event() {
            Ok(Event::Eof) | Err(_) => return,
            Ok(e) => e,
        };
        let tag = match &event {
            Event::Start(t) => {
                open += 1;
                Some(t)
            }
            Event::Empty(t) => Some(t),
            Event::End(_) => {
                open = open.saturating_sub(1);
                None
            }
            Event::DocType(_) | Event::PI(_) => {
                // Only the XML declaration is a PI the validator lets by.
                if let Event::PI(p) = &event {
                    let target = p.to_ascii_lowercase();
                    assert!(
                        target
                            .strip_prefix("xml")
                            .is_some_and(|r| r.starts_with(char::is_whitespace)),
                        "accepted an SVG with a PI"
                    );
                } else {
                    panic!("accepted an SVG with a DOCTYPE");
                }
                None
            }
            _ => None,
        };
        let Some(tag) = tag else { continue };
        let name = tag.name().as_ref().to_ascii_lowercase();
        let local = name.rsplit(':').next().unwrap_or_default();
        assert!(
            ![
                "script",
                "image",
                "use",
                "style",
                "a",
                "iframe",
                "foreignobject",
                "feimage",
            ]
            .contains(&local),
            "accepted an SVG with <{name}>"
        );
        for attr in tag.attributes() {
            // Attributes it cannot read in full: a document a strict parser
            // refuses, which no renderer shows.
            let Ok(attr) = attr else { return };
            let key = attr.key.as_ref().to_ascii_lowercase();
            let value = attr.value.to_ascii_lowercase();
            let key_local = key.rsplit(':').next().unwrap_or_default();
            assert!(!key_local.starts_with("on"), "accepted an SVG with {key}");
            if key_local == "href" {
                assert!(value.starts_with('#'), "href={value:.40}");
            }
            assert!(!value.contains("javascript:"), "{key}={value:.40}");
        }
    }
}

/// Icons: PNG header and SVG screening.
pub fn icon(bytes: &[u8]) {
    let size = meta::png_size(bytes);
    match meta::icon_kind(bytes) {
        Some(meta::IconKind::Png) => {
            let (w, h) = size.expect("a PNG icon has a size");
            assert!(
                (1..=meta::MAX_ICON_SIDE).contains(&w) && (1..=meta::MAX_ICON_SIDE).contains(&h)
            );
            assert!(bytes.len() as u64 <= meta::MAX_ICON);
        }
        Some(meta::IconKind::Svg) => {
            assert!(size.is_none());
            assert!(bytes.len() <= 512 << 10 && !bytes.contains(&0));
            let text = std::str::from_utf8(bytes).expect("an SVG is UTF-8");
            // What an independent XML parser sees in what was accepted. A
            // document it cannot read is one no renderer reads either; one it
            // reads has no element, attribute or reference the icon may not have.
            svg_seen_by_a_real_parser(text);
        }
        None => {}
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        let _ = meta::icon_name_ok(s);
    }
}

/// The helper's answer, which is untrusted: decoding sanitizes it, and the
/// sanitized form survives being encoded and decoded again.
pub fn inspection_decode(bytes: &[u8]) {
    use telamon_store_core::appimage::inspect::Inspection;
    let Ok(i) = Inspection::decode(bytes) else {
        return;
    };
    assert!(is_clean(&i.name, meta::MAX_NAME));
    assert!(
        is_clean(&i.version, 100)
            && is_clean(&i.summary, 300)
            && is_clean(&i.publisher, meta::MAX_NAME)
    );
    assert!(i.app_id.is_empty() || text::valid_id(&i.app_id));
    assert!(i.sha256.is_empty() || lower_hex64(&i.sha256));
    assert_eq!(i.icon.as_ref().map(|x| x.kind), i.icon_kind);
    if let Some(ic) = &i.icon {
        assert_eq!(meta::icon_kind(&ic.bytes), Some(ic.kind));
    }
    let again = Inspection::decode(&i.encode()).expect("an encoded answer decodes");
    assert_eq!(again, i, "decode(encode(x)) changed it");
}

/// AppStream metainfo and catalog XML.
pub fn appstream_metainfo(bytes: &[u8]) {
    use telamon_store_core::appstream::{Limits, ParseOptions, parse_metainfo};
    let opts = ParseOptions {
        origin: String::new(),
        langs: vec!["en".to_string()],
        limits: Limits::default(),
    };
    if let Ok(Some(c)) = parse_metainfo(bytes, &opts) {
        assert!(!c.id.is_empty());
        assert!(is_clean(&c.name, Limits::default().name.max(1)) || c.name.is_empty());
    }
}
