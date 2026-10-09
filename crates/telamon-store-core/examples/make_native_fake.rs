//! Builds what the window's screenshots of the Telamon apps need, without
//! GitHub: a folder of recorded-style answers (the catalog, each app's latest
//! release, its manifest and archive) and, in a fresh data folder, an older
//! version of one app already installed, so Updates has something to show.
//!
//!   cargo run --example make_native_fake --features fake-github -- <fake-root> <data-home> <home>
//!
//! The Store, built with its `fake-github` feature and run with
//! `TELAMON_STORE_FAKE_GITHUB=<fake-root>`, reads these instead of GitHub.
//! Also writes `<fake-root>/../local-bundle.tar.zst`, a bundle to open with
//! `--install-bundle`.

use std::path::Path;

use telamon_store_core::native::fake::{Built, BundleBuilder, default_key};
use telamon_store_core::native::install::{Dirs, Options, Origin, install_bundle};
use telamon_store_core::native::manifest::Host;

const GATES: &str = "net.eterneon.telamon.gates";
const GATES_REPO: &str = "EternalCoder454/telamon-gates";
const SCRATCH: &str = "net.eterneon.telamon.scratch";
const SCRATCH_REPO: &str = "EternalCoder454/telamon-scratch";

fn write(root: &Path, url: &str, bytes: &[u8]) {
    let rel = url.strip_prefix("https://").expect("https");
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn publish(root: &Path, repo: &str, tag: &str, b: &Built) {
    let fake = telamon_store_core::native::fake::Fake::new();
    fake.publish(repo, tag, b);
    // Re-use Fake's layout by asking it for what it would answer.
    use telamon_store_core::native::fetch::Fetcher;
    let base = format!("https://github.com/{repo}/releases/download/{tag}");
    let archive = &b.outer.archive.as_ref().unwrap().name;
    for url in [
        telamon_store_core::native::github::latest_url(repo),
        format!("{base}/telamon-bundle.json"),
        format!("{base}/telamon-bundle.json.minisig"),
        format!("{base}/{archive}"),
    ] {
        let body = fake.get(&url, "*/*", u64::MAX).unwrap();
        write(root, &url, &body);
    }
}

fn gates(version: &str) -> Built {
    BundleBuilder::new(GATES, "Telamon Gates", version)
        .exe("telamon-gates")
        .edit_inner(|m| {
            m.summary =
                "Chat with a local AI model, with every conversation kept as a plain file".into()
        })
        .edit_outer(|m| {
            m.summary =
                "Chat with a local AI model, with every conversation kept as a plain file".into()
        })
        .build()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (root, data, home) = (
        Path::new(&args[1]),
        Path::new(&args[2]),
        Path::new(&args[3]),
    );
    std::fs::create_dir_all(root).unwrap();
    // The catalog, at the address the Store asks for.
    // Each app lists the fake GitHub's test key as its signer; the releases
    // below are signed with it (`Fake::publish`).
    let signers = serde_json::json!([{"type": "minisign", "key": default_key().public()}]);
    let catalog = serde_json::json!({"schema": 1, "apps": [
        {"id": GATES, "repo": GATES_REPO, "channel": "releases", "signers": signers},
        {"id": SCRATCH, "repo": SCRATCH_REPO, "channel": "releases", "signers": signers},
    ]});
    write(
        root,
        telamon_store_core::native::CATALOG_URL,
        serde_json::to_string_pretty(&catalog).unwrap().as_bytes(),
    );
    // Gates has 0.2.0 out; 0.1.0 gets installed below. Scratch is new.
    publish(root, GATES_REPO, "v0.2.0", &gates("0.2.0"));
    let scratch = BundleBuilder::new(SCRATCH, "Telamon Scratch", "1.0.0")
        .exe("telamon-scratch")
        .edit_inner(|m| m.summary = "A scratch pad for quick notes".into())
        .edit_outer(|m| m.summary = "A scratch pad for quick notes".into())
        .build();
    publish(root, SCRATCH_REPO, "v1.0.0", &scratch);

    let dirs = Dirs {
        data: data.into(),
        home: home.into(),
        system: Vec::new(),
    };
    std::fs::create_dir_all(data).unwrap();
    std::fs::create_dir_all(home).unwrap();
    let old = gates("0.1.0");
    let archive = root.join("gates-0.1.0.tar.zst");
    std::fs::write(&archive, &old.archive).unwrap();
    let host = Host {
        os_version: Some(44),
        telamon_ui: None,
        arch: "x86_64".into(),
    };
    install_bundle(
        &dirs,
        &archive,
        &Options {
            expect_id: Some(GATES),
            outer: Some(&old.outer),
            origin: Origin::signed_release(GATES_REPO, "v0.1.0", &default_key().key_id()),
            host: &host,
        },
    )
    .unwrap();
    std::fs::remove_file(&archive).unwrap();

    // A bundle to open from a file.
    let local = BundleBuilder::new("net.eterneon.telamon.sketch", "Telamon Sketch", "0.3.0")
        .exe("telamon-sketch")
        .edit_inner(|m| m.summary = "A drawing app I am still working on".into())
        .build();
    std::fs::write(
        root.parent().unwrap_or(root).join("local-bundle.tar.zst"),
        &local.archive,
    )
    .unwrap();
}
