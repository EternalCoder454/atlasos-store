//! What the Store tells the user about an AppImage: origin, the warnings and
//! the Flathub match, and the helper's answer taken as untrusted input.
#[path = "common/appimage.rs"]
mod build;

use std::os::unix::ffi::OsStrExt;

use build::scratch;
use telamon_store_core::appimage::Format;
use telamon_store_core::appimage::inspect::{InspectError, Inspection, inspect};
use telamon_store_core::appimage::meta::IconKind;
use telamon_store_core::appimage::origin::{self, Origin, classify};
use telamon_store_core::appimage::sign::Signature;
use telamon_store_core::appimage::squash::Limits;
use telamon_store_core::appimage::trust::{self, Severity, assess, flathub_match};
use telamon_store_core::appstream::{ParseOptions, parse};
use telamon_store_core::catalog::{CatalogSource, Library};
use telamon_store_core::flatpak::Scope;

fn sample() -> Inspection {
    let dir = scratch("trust");
    let p = build::write(&dir, "Sample.AppImage", &build::normal());
    inspect(&p, &Limits::default()).unwrap()
}

fn set_xattr(path: &std::path::Path, name: &str, value: &str) -> bool {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let n = std::ffi::CString::new(name).unwrap();
    // SAFETY: valid NUL-terminated strings and a buffer of the length given.
    let rc = unsafe {
        libc::setxattr(
            c.as_ptr(),
            n.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    rc == 0
}

#[test]
fn addresses_are_classified_by_what_they_prove() {
    assert_eq!(
        classify("https://github.com/o/r/releases/download/v1/App.AppImage"),
        Origin::Https {
            host: "github.com".into()
        }
    );
    assert_eq!(
        classify("HTTPS://Example.ORG/x"),
        Origin::Https {
            host: "example.org".into()
        }
    );
    assert_eq!(
        classify("http://example.org:8080/x.AppImage"),
        Origin::Http {
            host: "example.org".into()
        }
    );
    assert_eq!(
        classify("http://user:pw@evil.example/x"),
        Origin::Http {
            host: "evil.example".into()
        }
    );
    for other in [
        "",
        "file:///home/u/x",
        "ftp://example.org/x",
        "blob:https://example.org/uuid",
        "https://localhost/x",
        "https://127.0.0.1/x",
        "http://",
        "http://ex ample.org/",
        "data:text/plain,x",
        "https://exa\u{202e}mple.org/x",
        "http://exämple.org/",
    ] {
        assert_eq!(classify(other), Origin::Other, "{other:?}");
    }
}

#[test]
fn the_browsers_address_is_read_from_the_file() {
    let dir = scratch("xattr");
    let p = build::write(&dir, "d.AppImage", &build::normal());
    if !set_xattr(
        &p,
        "user.xdg.origin.url",
        "https://example.org/dl/d.AppImage",
    ) {
        eprintln!("skipped: this file system has no user extended attributes");
        return;
    }
    let i = inspect(&p, &Limits::default()).unwrap();
    assert_eq!(
        i.origin,
        Origin::Https {
            host: "example.org".into()
        }
    );
    assert!(set_xattr(
        &p,
        "user.xdg.origin.url",
        "http://example.org/dl/d.AppImage"
    ));
    assert_eq!(
        inspect(&p, &Limits::default()).unwrap().origin,
        Origin::Http {
            host: "example.org".into()
        }
    );
    // No origin, but a referrer page.
    let q = build::write(&dir, "r.AppImage", &build::normal());
    assert!(set_xattr(
        &q,
        "user.xdg.referrer.url",
        "https://example.net/download"
    ));
    assert_eq!(
        origin::read(&std::fs::File::open(&q).unwrap()),
        Origin::Https {
            host: "example.net".into()
        }
    );
    // A blob: origin falls back to the referrer.
    assert!(set_xattr(
        &q,
        "user.xdg.origin.url",
        "blob:https://example.net/abc"
    ));
    assert_eq!(
        origin::read(&std::fs::File::open(&q).unwrap()),
        Origin::Https {
            host: "example.net".into()
        }
    );
    // A value far over the cap is ignored.
    let r = build::write(&dir, "big.AppImage", &build::normal());
    assert!(set_xattr(
        &r,
        "user.xdg.origin.url",
        &format!("https://example.org/{}", "a".repeat(3000))
    ));
    assert_eq!(
        inspect(&r, &Limits::default()).unwrap().origin,
        Origin::Unknown
    );
    // Nothing at all.
    let n = build::write(&dir, "n.AppImage", &build::normal());
    assert_eq!(
        inspect(&n, &Limits::default()).unwrap().origin,
        Origin::Unknown
    );
}

fn texts(t: &trust::Trust) -> Vec<&str> {
    t.lines.iter().map(|l| l.text.as_str()).collect()
}

#[test]
fn the_first_line_always_says_it_is_not_sandboxed_or_checked() {
    let t = assess(&sample());
    assert_eq!(
        t.lines[0].text,
        "This app isn't sandboxed and isn't checked by Telamon."
    );
    assert!(trust::ACCESS.contains("read and change all your files"));
    for l in &t.lines {
        let lower = l.text.to_lowercase();
        assert!(
            !lower.contains("safe") || lower.contains("sandboxed") || lower.contains("isn't"),
            "{}",
            l.text
        );
        assert!(
            !lower.contains("trusted") && !lower.contains("verified"),
            "{}",
            l.text
        );
    }
}

#[test]
fn unsigned_with_no_origin_is_the_strongest_warning() {
    let t = assess(&sample());
    assert!(t.strong);
    let all = texts(&t);
    assert!(all.contains(&"Not signed. Nothing shows who made this file or that it is unchanged."));
    assert!(all.contains(&"We can't tell where this file came from."));
}

#[test]
fn plain_http_is_strong_and_names_the_host() {
    let mut i = sample();
    i.signature = Signature::Signed {
        fingerprint: "ABCD".repeat(10),
    };
    i.origin = Origin::Http {
        host: "files.example".into(),
    };
    let t = assess(&i);
    assert!(t.strong);
    assert!(
        texts(&t)
            .iter()
            .any(|l| l.contains("files.example") && l.contains("without encryption"))
    );
    assert!(
        t.lines
            .iter()
            .any(|l| l.severity == Severity::Danger && l.text.contains("files.example"))
    );
}

#[test]
fn a_signature_by_an_unknown_key_is_a_caution_and_https_alone_is_not_strong() {
    let mut i = sample();
    i.signature = Signature::Signed {
        fingerprint: "0123456789ABCDEF".repeat(2) + "01234567",
    };
    i.origin = Origin::Https {
        host: "example.org".into(),
    };
    let t = assess(&i);
    assert!(!t.strong, "{:?}", t);
    let line = t
        .lines
        .iter()
        .find(|l| l.text.starts_with("Signed by"))
        .unwrap();
    assert_eq!(line.severity, Severity::Caution);
    assert!(line.text.contains("0123 4567 89AB CDEF"));
    assert!(line.text.ends_with("but this key isn't one Telamon knows."));
}

#[test]
fn a_wrong_signature_and_an_uncheckable_one_are_strong() {
    for (sig, text) in [
        (
            Signature::Wrong,
            "The signature is wrong (the file was changed).",
        ),
        (
            Signature::Unchecked,
            "It carries a signature, but this computer couldn't check it.",
        ),
    ] {
        let mut i = sample();
        i.origin = Origin::Https {
            host: "example.org".into(),
        };
        i.signature = sig;
        let t = assess(&i);
        assert!(t.strong);
        assert!(texts(&t).contains(&text));
    }
}

#[test]
fn a_file_that_cannot_be_inspected_is_strong_whatever_else_is_good() {
    let dir = scratch("old");
    let p = build::write(&dir, "old.AppImage", &build::type1());
    let mut i = inspect(&p, &Limits::default()).unwrap();
    assert_eq!(i.format, Format::Type1);
    i.origin = Origin::Https {
        host: "example.org".into(),
    };
    i.signature = Signature::Signed {
        fingerprint: "AB".repeat(20),
    };
    let t = assess(&i);
    assert!(t.strong);
    assert!(texts(&t).iter().any(|l| l.contains("old kind of AppImage")));
}

// ---- Flathub ----

fn flathub_library() -> Library {
    let xml = r#"<components version="0.16">
      <component type="desktop-application"><id>org.example.Sample</id><name>Sample Draw</name><summary>s</summary>
        <bundle type="flatpak">app/org.example.Sample/x86_64/stable</bundle></component>
      <component type="desktop-application"><id>org.other.Same</id><name>Twin App</name><summary>s</summary>
        <bundle type="flatpak">app/org.other.Same/x86_64/stable</bundle></component>
      <component type="desktop-application"><id>org.other.Same2</id><name>Twin App</name><summary>s</summary>
        <bundle type="flatpak">app/org.other.Same2/x86_64/stable</bundle></component>
      <component type="desktop-application"><id>org.example.Solo</id><name>Solo Tool</name><summary>s</summary>
        <bundle type="flatpak">app/org.example.Solo/x86_64/stable</bundle></component>
    </components>"#;
    let cat = |origin: &str| {
        parse(
            xml.as_bytes(),
            &ParseOptions {
                origin: origin.into(),
                ..ParseOptions::default()
            },
        )
        .unwrap()
    };
    let src = |remote: &str| CatalogSource {
        scope: Scope::User,
        remote: remote.into(),
        title: remote.into(),
        priority: 0,
        dir: None,
        commit: None,
        updated: None,
    };
    Library::new(vec![(src("flathub"), cat("flathub"))])
}

fn named(name: &str, app_id: &str) -> Inspection {
    let mut i = sample();
    i.name = name.into();
    i.app_id = app_id.into();
    i
}

#[test]
fn an_app_on_flathub_is_found_by_its_id_or_exact_name() {
    let lib = flathub_library();
    let m = flathub_match(&lib, &named("Something Else", "org.example.Sample")).unwrap();
    assert_eq!(m.app_id, "org.example.Sample");
    assert_eq!(m.name, "Sample Draw");
    // By name, ignoring case and spacing.
    assert_eq!(
        flathub_match(&lib, &named("sample  DRAW", ""))
            .unwrap()
            .app_id,
        "org.example.Sample"
    );
    assert_eq!(
        flathub_match(&lib, &named("Solo-Tool", "")).unwrap().app_id,
        "org.example.Solo"
    );
    // A name two apps have is not guessed, a prefix is not a match.
    assert!(flathub_match(&lib, &named("Twin App", "")).is_none());
    assert!(flathub_match(&lib, &named("Sample", "")).is_none());
    assert!(flathub_match(&lib, &named("Unknown", "org.nobody.Unknown")).is_none());
    // Very short names are not matched.
    assert!(flathub_match(&lib, &named("So", "")).is_none());
}

#[test]
fn only_the_flathub_remote_counts() {
    let lib = {
        let xml = r#"<components version="0.16"><component type="desktop-application"><id>org.example.Sample</id><name>Sample Draw</name><summary>s</summary>
          <bundle type="flatpak">app/org.example.Sample/x86_64/stable</bundle></component></components>"#;
        let cat = parse(xml.as_bytes(), &ParseOptions::default()).unwrap();
        Library::new(vec![(
            CatalogSource {
                scope: Scope::User,
                remote: "fedora".into(),
                title: "Fedora".into(),
                priority: 0,
                dir: None,
                commit: None,
                updated: None,
            },
            cat,
        )])
    };
    assert!(flathub_match(&lib, &named("Sample Draw", "org.example.Sample")).is_none());
}

// ---- the helper's answer is untrusted ----

fn header(json: &str, icon: &[u8]) -> Vec<u8> {
    let mut v = json.as_bytes().to_vec();
    v.push(b'\n');
    v.extend_from_slice(icon);
    v
}

#[test]
fn the_helpers_answer_round_trips() {
    let i = sample();
    let back = Inspection::decode(&i.encode()).unwrap();
    assert_eq!(back, i);
    assert_eq!(back.icon_kind, Some(IconKind::Png));
}

#[test]
fn a_hostile_answer_is_cleaned_or_refused() {
    let base = |name: &str, extra: &str| {
        format!(
            r#"{{"format":"type2","size":5000,"sha256":"{}","file_name":"f.AppImage","inspected":true,"note":"","name":{name},"version":"1","publisher":"p","summary":"s","app_id":"not an id!","icon_kind":null,"signature":{{"state":"none"}},"origin":{{"kind":"unknown"}}{extra}}}"#,
            "a".repeat(64)
        )
    };
    // Control and bidi characters in a name are removed, an invalid ID is dropped.
    let ok = Inspection::decode(&header(&base("\"Evil\\u202eApp\\n\\u0007x\"", ""), b"")).unwrap();
    assert_eq!(ok.name, "EvilApp x");
    assert_eq!(ok.app_id, "");
    // Unknown fields, a bad shape, no newline: refused.
    assert!(matches!(
        Inspection::decode(&header(&base("\"x\"", ",\"extra\":1"), b"")),
        Err(InspectError::Helper(_))
    ));
    assert!(matches!(
        Inspection::decode(b"{}"),
        Err(InspectError::Helper(_))
    ));
    assert!(matches!(
        Inspection::decode(b"not json\n"),
        Err(InspectError::Helper(_))
    ));
    // An icon that is not an image is dropped, even when declared.
    let with_icon = base("\"x\"", "").replace("\"icon_kind\":null", "\"icon_kind\":\"png\"");
    let i = Inspection::decode(&header(&with_icon, b"<html>not a png")).unwrap();
    assert!(i.icon.is_none() && i.icon_kind.is_none());
    let i = Inspection::decode(&header(&with_icon, &build::fake_png(64, 64))).unwrap();
    assert_eq!(i.icon_kind, Some(IconKind::Png));
    // A declared kind that the bytes do not have.
    let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
    assert!(
        Inspection::decode(&header(&with_icon, svg))
            .unwrap()
            .icon
            .is_none()
    );
    // A huge PNG, and an SVG with a script, are not icons.
    assert!(
        Inspection::decode(&header(&with_icon, &build::fake_png(50_000, 50_000)))
            .unwrap()
            .icon
            .is_none()
    );
    let evil_svg = with_icon.replace("\"png\"", "\"svg\"");
    assert!(
        Inspection::decode(&header(&evil_svg, b"<svg><script>alert(1)</script></svg>"))
            .unwrap()
            .icon
            .is_none()
    );
    assert!(
        Inspection::decode(&header(
            &evil_svg,
            b"<svg xmlns='http://www.w3.org/2000/svg'><circle r='4'/></svg>"
        ))
        .unwrap()
        .icon
        .is_some()
    );
    // A bad hash, host and fingerprint are dropped, not shown.
    let bad = base("\"x\"", "")
        .replace(&"a".repeat(64), "ZZ")
        .replace(
            "{\"kind\":\"unknown\"}",
            "{\"kind\":\"https\",\"host\":\"ex ample\"}",
        )
        .replace(
            "{\"state\":\"none\"}",
            "{\"state\":\"signed\",\"fingerprint\":\"nothex\"}",
        );
    let i = Inspection::decode(&header(&bad, b"")).unwrap();
    assert_eq!(i.sha256, "");
    assert_eq!(i.origin, Origin::Other);
    assert_eq!(i.signature, Signature::Unchecked);
    // The helper's own error line.
    assert_eq!(
        Inspection::decode(b"ERROR The file could not be read (permission denied).\n").unwrap_err(),
        InspectError::Helper("The file could not be read (permission denied).".into())
    );
}
