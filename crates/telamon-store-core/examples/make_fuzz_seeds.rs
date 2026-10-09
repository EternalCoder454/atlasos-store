//! Writes the seed corpus of the fuzz targets in `fuzz/` (small files, built
//! from the test fixtures and the test builders, so they are real examples of
//! each format):
//!
//!   cargo run --example make_fuzz_seeds -p telamon-store-core -- fuzz/corpus
//!
//! The corpus is checked in; run this again only to add a seed. Seeds of the
//! multi-input targets are fields separated by 0x1F, as `fuzz/src/lib.rs` says.

use std::fs;
use std::path::Path;

use telamon_store_core::native::fake::BundleBuilder;

#[path = "../tests/common/appimage.rs"]
mod image;

const SEP: u8 = 0x1F;
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn fixture(rel: &str) -> Vec<u8> {
    fs::read(Path::new(FIXTURES).join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn join(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.extend_from_slice(p);
    }
    out
}

struct Out<'a> {
    root: &'a Path,
}

impl Out<'_> {
    fn put(&self, target: &str, name: &str, bytes: &[u8]) {
        let dir = self.root.join(target);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), bytes).unwrap();
    }
}

const DESKTOP: &[u8] = b"[Desktop Entry]\nType=Application\nName=Telamon Gates\nName[de]=Tor\nExec=telamon-gates %U\nTryExec=telamon-gates\nPath=/tmp\nIcon=net.eterneon.telamon.gates\nTerminal=false\nCategories=Utility;\n\n[Desktop Action new]\nName=New Chat\nExec=telamon-gates --new\n";
const DBUS: &[u8] = b"[D-BUS Service]\nName=net.eterneon.telamon.gates\nExec=telamon-gates --gapplication-service\n";
const PREFIX: &[u8] = b"/home/u/.local/share/telamon-apps/net.eterneon.telamon.gates/current";

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fuzz/corpus".into());
    let o = Out {
        root: Path::new(&root),
    };

    // ---- native apps ----
    let good = BundleBuilder::new("net.eterneon.telamon.gates", "Telamon Gates", "1.0.0")
        .exe("telamon-gates")
        .link("bin/gates", "telamon-gates")
        .build();
    o.put(
        "native_manifest",
        "outer.json",
        &serde_json::to_vec_pretty(&good.outer).unwrap(),
    );
    o.put(
        "native_manifest",
        "inner.json",
        &serde_json::to_vec_pretty(&good.inner).unwrap(),
    );
    let mut bad = good.outer.clone();
    bad.links[0].target = "../../x".into();
    o.put(
        "native_manifest",
        "link-out.json",
        &serde_json::to_vec(&bad).unwrap(),
    );
    o.put(
        "native_catalog",
        "repo-catalog.json",
        &fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog/native-apps.json"))
            .unwrap(),
    );
    o.put(
        "native_catalog",
        "two-apps.json",
        br#"{"schema":1,"apps":[{"id":"net.eterneon.telamon.gates","repo":"EternalCoder454/telamon-gates","channel":"releases"},{"id":"org.evil.App","repo":"evil/app","channel":"releases"}]}"#,
    );
    o.put(
        "github_release",
        "recorded.bin",
        &join(&[
            b"EternalCoder454/atlasos-store",
            &fixture("native/github-release-latest.json"),
        ]),
    );
    o.put(
        "github_release",
        "small.bin",
        &join(&[
            b"EternalCoder454/telamon-gates",
            br#"{"tag_name":"v1.0.0","draft":false,"prerelease":false,"assets":[{"name":"telamon-bundle.json","size":10,"state":"uploaded","browser_download_url":"https://github.com/EternalCoder454/telamon-gates/releases/download/v1.0.0/telamon-bundle.json"}]}"#,
        ]),
    );
    o.put(
        "native_desktop",
        "gates.bin",
        &join(&[DESKTOP, PREFIX, b"telamon-gates"]),
    );
    o.put(
        "native_desktop",
        "spaces.bin",
        &join(&[
            b"[Desktop Entry]\nType=Application\nName=G\nExec=telamon-gates\n",
            b"/home/my user/it's/current",
            b"telamon-gates",
            b"other",
        ]),
    );
    o.put(
        "native_dbus",
        "gates.bin",
        &join(&[DBUS, PREFIX, b"telamon-gates"]),
    );
    let mut plan = Vec::new();
    plan.extend_from_slice(PREFIX);
    plan.push(SEP);
    let entries: Vec<Vec<u8>> = vec![
        join(&[b"x", b"bin/telamon-gates", b"#!/bin/sh\n"]),
        join(&[
            b"f",
            b"share/applications/net.eterneon.telamon.gates.desktop",
            DESKTOP,
        ]),
        join(&[
            b"f",
            b"share/dbus-1/services/net.eterneon.telamon.gates.service",
            DBUS,
        ]),
        join(&[
            b"f",
            b"share/icons/hicolor/scalable/apps/net.eterneon.telamon.gates.svg",
            b"<svg/>",
        ]),
        join(&[b"l", b"bin/gates", b"telamon-gates"]),
    ];
    plan.extend_from_slice(&entries.join(&0x1Eu8));
    o.put("native_plan", "gates.bin", &plan);
    o.put("native_unpack", "good.tar.zst", &good.archive);
    let hostile = BundleBuilder::new("net.eterneon.telamon.gates", "Telamon Gates", "1.0.0")
        .exe("telamon-gates")
        .raw(telamon_store_core::native::fake::Raw {
            name: b"../evil".to_vec(),
            kind: tar::EntryType::Regular,
            data: b"x".to_vec(),
            link: None,
        })
        .raw(telamon_store_core::native::fake::Raw {
            name: b"bin/lnk".to_vec(),
            kind: tar::EntryType::Symlink,
            data: vec![],
            link: Some(b"/etc/passwd".to_vec()),
        })
        .build();
    o.put("native_unpack", "hostile.tar.zst", &hostile.archive);
    let tar_of = |archive: &[u8]| zstd::stream::decode_all(archive).unwrap();
    o.put("native_unpack_tar", "good.tar", &tar_of(&good.archive));
    o.put(
        "native_unpack_tar",
        "hostile.tar",
        &tar_of(&hostile.archive),
    );

    // ---- key files ----
    for (name, bytes) in [
        ("desktop.ini", DESKTOP.to_vec()),
        ("hello.flatpakref", fixture("flatpakref/hello.flatpakref")),
        ("firefox.metadata", fixture("permissions/firefox.metadata")),
        ("override.ini", fixture("permissions/override-user.ini")),
    ] {
        let mut seed = vec![0u8];
        seed.extend_from_slice(&bytes);
        o.put("keyfile", name, &seed);
        let mut small = vec![1u8];
        small.extend_from_slice(&bytes);
        o.put("keyfile", &format!("small-{name}"), &small);
    }
    for name in [
        "hello.flatpakref",
        "test.flatpakrepo",
        "filter.flatpakrepo",
        "unsigned.flatpakrepo",
    ] {
        o.put("flatpakref", name, &fixture(&format!("flatpakref/{name}")));
    }

    // ---- text and URLs ----
    o.put(
        "urls",
        "plain",
        &join(&[b"dl.example.org/a/b?x=1#y", b"/c/../d"]),
    );
    o.put(
        "urls",
        "dots",
        &join(&[b"dl.example.org/a/%2e%2e/b", b"https://other.example.org/x"]),
    );
    o.put("urls", "version", &join(&[b"1.2.3-beta.1", b"1.2.10"]));
    o.put(
        "urls",
        "path",
        &join(&[b"/home/u/Downloads/x.AppImage", b"//evil.com/x"]),
    );
    o.put(
        "text",
        "nasty",
        b"\x50 a\xc2\xa0 b\xe2\x80\xae evil\xe2\x80\x8b\r\n\ttext \xf0\x9f\x98\x80",
    );
    o.put("text", "short", b"\x05hello world");
    let args = |a: &[&[u8]]| join(a);
    o.put(
        "launch_args",
        "flatpakref",
        &args(&[b"--", b"/home/u/x.flatpakref", b"appstream:org.test.Hello"]),
    );
    o.put(
        "launch_args",
        "options",
        &args(&[
            b"--page=updates",
            b"--app",
            b"org.test.Hello",
            b"--search",
            b"gimp",
        ]),
    );
    o.put(
        "launch_args",
        "url",
        &args(&[
            b"flatpak+https://dl.example.org/x.flatpakref",
            b"--appimage-install",
            b"/home/u/a.AppImage",
        ]),
    );

    // ---- AppImage ----
    let normal = image::normal_squash();
    o.put("squashfs", "gzip.sqfs", &normal.build());
    o.put(
        "squashfs",
        "zstd.sqfs",
        &image::Squash {
            compressor: backhand::compression::Compressor::Zstd,
            ..image::normal_squash()
        }
        .build(),
    );
    o.put(
        "squashfs",
        "xz.sqfs",
        &image::Squash {
            compressor: backhand::compression::Compressor::Xz,
            ..image::normal_squash()
        }
        .build(),
    );
    o.put(
        "squashfs",
        "links.sqfs",
        &image::Squash::default()
            .file("/a.desktop", image::DESKTOP)
            .link("/b.desktop", "a.desktop")
            .link("/c.desktop", "/etc/passwd")
            .link("/d.desktop", "../../../a.desktop")
            .link("/usr/share/applications/y.desktop", "../../../a.desktop")
            .build(),
    );
    o.put("appimage_file", "type2.AppImage", &image::normal());
    o.put("appimage_file", "type1.AppImage", &image::type1());
    o.put("icon", "png", &image::fake_png(256, 256));
    o.put("icon", "huge.png", &image::fake_png(5000, 5000));
    o.put("icon", "plain.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\"><linearGradient id=\"g\"/><rect fill=\"url(#g)\" width=\"8\" height=\"8\"/></svg>");
    o.put(
        "icon",
        "script.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>",
    );
    o.put(
        "icon",
        "href.svg",
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><a href=\"http://x\"/></svg>",
    );
    let mut answer = serde_json::to_vec(&serde_json::json!({
        "format": "type2", "size": 1234, "sha256": "ab".repeat(32), "file_name": "x.AppImage",
        "inspected": true, "note": "", "name": "X", "version": "1", "publisher": "P", "summary": "S",
        "app_id": "org.x.Y", "icon_kind": "png", "signature": {"state": "none"}, "origin": {"kind": "unknown"},
    }))
    .unwrap();
    answer.push(b'\n');
    answer.extend_from_slice(&image::fake_png(64, 64));
    o.put("inspection_decode", "answer.bin", &answer);
    o.put(
        "inspection_decode",
        "error.bin",
        b"ERROR could not look inside\n",
    );
    o.put(
        "appstream_metainfo",
        "sample.xml",
        image::APPSTREAM.as_bytes(),
    );
    o.put(
        "appstream_metainfo",
        "gates.xml",
        b"<?xml version=\"1.0\"?>\n<component type=\"desktop-application\"><id>net.eterneon.telamon.gates</id><name>Telamon Gates</name><summary>Chat</summary><description><p>Text &amp; <em>more</em></p><ul><li>one</li></ul></description></component>\n",
    );
}
