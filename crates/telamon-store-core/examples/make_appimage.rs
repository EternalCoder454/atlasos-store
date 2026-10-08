//! Makes an AppImage for smoke runs, without running anything:
//!
//!   make_appimage <out.AppImage> <Name> <app-id> <version> [icon.png] [<gnupg home> <fingerprint>]
//!
//! With a GnuPG home and a key's fingerprint in it, the file is signed the way
//! appimagetool does (the hex SHA-256 of the file, with the signature and key
//! sections empty, signed with a detached armored signature).
//! The file's body is the same builder the tests use.
#[path = "../tests/common/appimage.rs"]
mod build;

use sha2::{Digest, Sha256};
use std::process::{Command, Stdio};

fn gpg(home: &str, args: &[&str], stdin: Option<&[u8]>) -> Vec<u8> {
    use std::io::Write;
    let mut c = Command::new("gpg")
        .args([
            "--homedir",
            home,
            "--batch",
            "--yes",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
        ])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("gpg");
    if let Some(d) = stdin {
        c.stdin.take().unwrap().write_all(d).unwrap();
    }
    let out = c.wait_with_output().unwrap();
    assert!(out.status.success(), "gpg failed");
    out.stdout
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!(
            "usage: make_appimage <out> <Name> <app-id> <version> [icon.png] [<gnupg home> <fingerprint>]"
        );
        std::process::exit(2);
    }
    let (out, name, id, version) = (&a[1], &a[2], &a[3], &a[4]);
    let icon = a.get(5).map(|p| std::fs::read(p).expect("icon"));
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<component type=\"desktop-application\"><id>{id}</id><name>{name}</name><summary>A sample app for trying the AppImage dialog</summary><developer><name>Example Studio</name></developer><releases><release version=\"{version}\" date=\"2026-09-01\"/></releases></component>\n"
    );
    let desktop =
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec=sample %U\nIcon=sample\n");
    let mut sq = build::Squash::default()
        .file("/AppRun", b"#!/bin/sh\necho sample\n".to_vec())
        .file("/sample.desktop", desktop)
        .file(&format!("/usr/share/metainfo/{id}.appdata.xml"), xml);
    if let Some(png) = icon {
        sq = sq
            .file("/sample.png", png.clone())
            .link("/.DirIcon", "sample.png");
    }
    let squash = sq.build();
    let bytes = if let (Some(home), Some(fpr)) = (a.get(6), a.get(7)) {
        let unsigned = build::type2(&squash, &[], &[]);
        let digest: String = Sha256::digest(&unsigned)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let sig = gpg(
            home,
            &[
                "--armor",
                "--detach-sign",
                "--local-user",
                fpr,
                "--output",
                "-",
            ],
            Some(digest.as_bytes()),
        );
        let key = gpg(home, &["--armor", "--export", fpr], None);
        build::type2(&squash, &sig, &key)
    } else {
        build::type2(&squash, &[], &[])
    };
    std::fs::write(out, bytes).expect("write");
}
