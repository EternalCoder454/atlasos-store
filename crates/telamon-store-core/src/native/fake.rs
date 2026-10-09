//! A GitHub that answers from memory or from a folder of recorded answers, and
//! a builder of bundles, for tests and for screenshots of the window.
//!
//! Compiled for tests, for the integration tests (`test-hooks`) and for the
//! screenshot build (`fake-github`); never in a package.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;

use blake2::Blake2b512;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::archive::hex;
use super::fetch::Fetcher;
use super::manifest::{ArchiveInfo, FileEntry, Kind, LinkEntry, Manifest, archive_name};
use super::sign::SIGNATURE_NAME;
use crate::net::NetError;

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn b64(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

/// A throw-away minisign key for tests and screenshots, made from a one-byte
/// seed: its secret half is in this file on purpose, so what it signs proves
/// nothing outside those. Signatures are the format `minisign -S` writes
/// (hashed, `ED`), which the tests against files made by the real tool
/// (`tests/fixtures/native/signed`) keep honest.
pub struct TestKey {
    pair: ed25519_compact::KeyPair,
    id: [u8; 8],
}

impl TestKey {
    /// The key for `n`; a different `n` is a different key.
    pub fn new(n: u8) -> TestKey {
        TestKey {
            pair: ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([n; 32])),
            id: [0x54, 0x45, 0x53, 0x54, n, n, n, n],
        }
    }

    /// The `RW...` string a catalog entry lists.
    pub fn public(&self) -> String {
        let mut bytes = b"Ed".to_vec();
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(self.pair.pk.as_ref());
        b64(&bytes)
    }

    /// The key ID as `minisign` and the Store print it.
    pub fn key_id(&self) -> String {
        self.id.iter().rev().map(|b| format!("{b:02X}")).collect()
    }

    /// A signature file for `file`, with the given trusted comment.
    pub fn sign_with_comment(&self, file: &[u8], trusted: &str) -> Vec<u8> {
        let hash = Blake2b512::digest(file);
        let sig = self.pair.sk.sign(hash.as_slice(), None);
        let mut head = b"ED".to_vec();
        head.extend_from_slice(&self.id);
        head.extend_from_slice(sig.as_ref());
        let mut global = sig.as_ref().to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global = self.pair.sk.sign(&global, None);
        format!(
            "untrusted comment: signature from a Telamon test key\n{}\ntrusted comment: {trusted}\n{}\n",
            b64(&head),
            b64(global.as_ref())
        )
        .into_bytes()
    }

    /// A signature file for `file`.
    pub fn sign(&self, file: &[u8]) -> Vec<u8> {
        self.sign_with_comment(file, "timestamp:0\tfile:telamon-bundle.json\thashed")
    }
}

/// The key `Fake::catalog` lists and `Fake::publish` signs with.
pub fn default_key() -> TestKey {
    TestKey::new(1)
}

/// How a published release is signed.
pub enum Signing<'a> {
    /// A good signature by this key.
    With(&'a TestKey),
    /// The release has no signature file.
    Unsigned,
}

/// Answers by address. Unknown addresses are a 404.
#[derive(Default)]
pub struct Fake {
    answers: Mutex<BTreeMap<String, Result<Vec<u8>, NetError>>>,
    seen: Mutex<Vec<String>>,
}

impl Fake {
    pub fn new() -> Fake {
        Fake::default()
    }

    /// Every file under `root` is the answer for
    /// `https://<path below root>`: `root/api.github.com/repos/o/r/releases/latest`
    /// answers `https://api.github.com/repos/o/r/releases/latest`.
    pub fn from_dir(root: &Path) -> io::Result<Fake> {
        let fake = Fake::new();
        fn walk(fake: &Fake, root: &Path, dir: &Path) -> io::Result<()> {
            for e in std::fs::read_dir(dir)? {
                let e = e?;
                let p = e.path();
                if e.file_type()?.is_dir() {
                    walk(fake, root, &p)?;
                } else if let Ok(rel) = p.strip_prefix(root) {
                    let url = format!("https://{}", rel.to_string_lossy());
                    fake.answers
                        .lock()
                        .unwrap()
                        .insert(url, Ok(std::fs::read(&p)?));
                }
            }
            Ok(())
        }
        walk(&fake, root, root)?;
        Ok(fake)
    }

    pub fn set(&self, url: &str, body: Vec<u8>) {
        self.answers.lock().unwrap().insert(url.into(), Ok(body));
    }

    pub fn fail(&self, url: &str, error: NetError) {
        self.answers.lock().unwrap().insert(url.into(), Err(error));
    }

    pub fn remove(&self, url: &str) {
        self.answers.lock().unwrap().remove(url);
    }

    /// The addresses asked for so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// The catalog, each app listing [`default_key`] as its signer.
    pub fn catalog(&self, apps: &[(&str, &str)]) {
        let key = default_key();
        let apps: Vec<_> = apps
            .iter()
            .map(|(id, repo)| (*id, *repo, vec![&key]))
            .collect();
        self.catalog_with_keys(&apps);
    }

    /// The catalog, each app listing the given keys as its signers.
    pub fn catalog_with_keys(&self, apps: &[(&str, &str, Vec<&TestKey>)]) {
        let list: Vec<_> = apps
            .iter()
            .map(|(id, repo, keys)| {
                let signers: Vec<_> = keys
                    .iter()
                    .map(|k| json!({"type": "minisign", "key": k.public()}))
                    .collect();
                json!({"id": id, "repo": repo, "channel": "releases", "signers": signers})
            })
            .collect();
        self.set(
            super::CATALOG_URL,
            serde_json::to_vec(&json!({"schema": 1, "apps": list})).unwrap(),
        );
    }

    /// Publishes `bundle` as the latest release of `repo` under `tag`, its
    /// manifest signed with [`default_key`].
    pub fn publish(&self, repo: &str, tag: &str, bundle: &Built) {
        self.publish_with(repo, tag, bundle, Signing::With(&default_key()));
    }

    /// Publishes `bundle` as the latest release of `repo` under `tag`.
    pub fn publish_with(&self, repo: &str, tag: &str, bundle: &Built, signing: Signing<'_>) {
        let base = format!("https://github.com/{repo}/releases/download/{tag}");
        let archive_file = bundle.outer.archive.as_ref().unwrap().name.clone();
        let manifest_bytes = serde_json::to_vec_pretty(&bundle.outer).unwrap();
        let mut assets = vec![
            json!({"name": "telamon-bundle.json", "size": manifest_bytes.len(), "state": "uploaded",
             "browser_download_url": format!("{base}/telamon-bundle.json")}),
            json!({"name": archive_file, "size": bundle.archive.len(), "state": "uploaded",
             "digest": format!("sha256:{}", bundle.sha256),
             "browser_download_url": format!("{base}/{archive_file}")}),
        ];
        if let Signing::With(key) = signing {
            let sig = key.sign(&manifest_bytes);
            assets.push(
                json!({"name": SIGNATURE_NAME, "size": sig.len(), "state": "uploaded",
                 "browser_download_url": format!("{base}/{SIGNATURE_NAME}")}),
            );
            self.set(&format!("{base}/{SIGNATURE_NAME}"), sig);
        } else {
            self.remove(&format!("{base}/{SIGNATURE_NAME}"));
        }
        let release = json!({
        "tag_name": tag, "draft": false, "prerelease": false,
        "html_url": format!("https://github.com/{repo}/releases/tag/{tag}"),
        "assets": assets});
        self.set(
            &super::github::latest_url(repo),
            serde_json::to_vec(&release).unwrap(),
        );
        self.set(&format!("{base}/telamon-bundle.json"), manifest_bytes);
        self.set(&format!("{base}/{archive_file}"), bundle.archive.clone());
    }

    fn answer(&self, url: &str) -> Result<Vec<u8>, NetError> {
        self.seen.lock().unwrap().push(url.to_string());
        self.answers
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or(Err(NetError::Status(404)))
    }
}

impl Fetcher for Fake {
    fn get(&self, url: &str, _accept: &str, max_bytes: u64) -> Result<Vec<u8>, NetError> {
        let body = self.answer(url)?;
        if body.len() as u64 > max_bytes {
            return Err(NetError::TooLarge);
        }
        Ok(body)
    }

    fn download(
        &self,
        url: &str,
        max_bytes: u64,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<u64, NetError> {
        let body = self.answer(url)?;
        if body.len() as u64 > max_bytes {
            return Err(NetError::TooLarge);
        }
        for piece in body.chunks(8192) {
            sink(piece).map_err(|e| NetError::Failed(e.to_string()))?;
        }
        Ok(body.len() as u64)
    }
}

/// An entry written to the tar as it is, whatever it is (for hostile files).
pub struct Raw {
    pub name: Vec<u8>,
    pub kind: tar::EntryType,
    pub data: Vec<u8>,
    pub link: Option<Vec<u8>>,
}

type Edit = Box<dyn Fn(&mut Manifest)>;

/// A bundle under construction.
pub struct BundleBuilder {
    pub id: String,
    pub name: String,
    pub version: String,
    pub exe: String,
    files: BTreeMap<String, (Vec<u8>, bool)>,
    links: Vec<(String, String)>,
    raw: Vec<Raw>,
    edit_inner: Option<Edit>,
    edit_outer: Option<Edit>,
    skip_manifest: bool,
}

/// A finished bundle.
pub struct Built {
    pub archive: Vec<u8>,
    pub sha256: String,
    pub inner: Manifest,
    pub outer: Manifest,
}

impl BundleBuilder {
    /// A small but complete app: a program, a desktop entry, an icon, a
    /// metainfo file and a data file.
    pub fn new(id: &str, name: &str, version: &str) -> BundleBuilder {
        let exe = id.rsplit('.').next().unwrap_or("app").to_string();
        let mut b = BundleBuilder {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            exe: exe.clone(),
            files: BTreeMap::new(),
            links: Vec::new(),
            raw: Vec::new(),
            edit_inner: None,
            edit_outer: None,
            skip_manifest: false,
        };
        b.reset_defaults();
        b
    }

    fn reset_defaults(&mut self) {
        let (id, name, exe) = (self.id.clone(), self.name.clone(), self.exe.clone());
        self.files.clear();
        self.files.insert(
            format!("bin/{exe}"),
            (
                format!("#!/bin/sh\n# {name} {}\nexit 0\n", self.version).into_bytes(),
                true,
            ),
        );
        self.files.insert(
            format!("share/applications/{id}.desktop"),
            (
                format!("[Desktop Entry]\nType=Application\nName={name}\nComment=A test app\nExec={exe} %U\nIcon={id}\nTerminal=false\nCategories=Utility;\n").into_bytes(),
                false,
            ),
        );
        self.files.insert(
            format!("share/icons/hicolor/scalable/apps/{id}.svg"),
            (b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\"><rect width=\"64\" height=\"64\" rx=\"14\" fill=\"#6c5ce7\"/></svg>\n".to_vec(), false),
        );
        self.files.insert(
            format!("share/metainfo/{id}.metainfo.xml"),
            (format!("<?xml version=\"1.0\"?>\n<component type=\"desktop-application\"><id>{id}</id><name>{name}</name></component>\n").into_bytes(), false),
        );
        self.files.insert(
            format!("share/{id}/data.txt"),
            (
                format!("data of {} {}\n", self.name, self.version).into_bytes(),
                false,
            ),
        );
    }

    pub fn exe(mut self, exe: &str) -> BundleBuilder {
        self.exe = exe.into();
        self.reset_defaults();
        self
    }

    pub fn file(mut self, path: &str, bytes: &[u8], executable: bool) -> BundleBuilder {
        self.files.insert(path.into(), (bytes.to_vec(), executable));
        self
    }

    pub fn without(mut self, path: &str) -> BundleBuilder {
        self.files.remove(path);
        self
    }

    pub fn link(mut self, path: &str, target: &str) -> BundleBuilder {
        self.links.push((path.into(), target.into()));
        self
    }

    /// Adds a tar entry that the manifest does not list, written as given.
    pub fn raw(mut self, raw: Raw) -> BundleBuilder {
        self.raw.push(raw);
        self
    }

    pub fn edit_inner(mut self, f: impl Fn(&mut Manifest) + 'static) -> BundleBuilder {
        self.edit_inner = Some(Box::new(f));
        self
    }

    pub fn edit_outer(mut self, f: impl Fn(&mut Manifest) + 'static) -> BundleBuilder {
        self.edit_outer = Some(Box::new(f));
        self
    }

    pub fn no_manifest(mut self) -> BundleBuilder {
        self.skip_manifest = true;
        self
    }

    pub fn manifest(&self) -> Manifest {
        let mut files: Vec<FileEntry> = self
            .files
            .iter()
            .map(|(path, (bytes, exec))| FileEntry {
                path: path.clone(),
                size: bytes.len() as u64,
                sha256: hex(&Sha256::digest(bytes)),
                executable: *exec,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Manifest {
            schema: 1,
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            summary: format!("{} is a test app", self.name),
            homepage: "https://github.com/EternalCoder454/telamon-gates".into(),
            license: "MIT".into(),
            arch: "x86_64".into(),
            min_telamon_ui: "2.0.0".into(),
            min_os_version: "44".into(),
            files,
            links: self
                .links
                .iter()
                .map(|(p, t)| LinkEntry {
                    path: p.clone(),
                    target: t.clone(),
                })
                .collect(),
            archive: None,
        }
    }

    pub fn build(&self) -> Built {
        let mut inner = self.manifest();
        if let Some(f) = &self.edit_inner {
            f(&mut inner);
        }
        let mut tar_bytes = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut tar_bytes);
            tar.mode(tar::HeaderMode::Deterministic);
            for (path, (bytes, exec)) in &self.files {
                let mut h = tar::Header::new_gnu();
                h.set_size(bytes.len() as u64);
                h.set_mode(if *exec { 0o755 } else { 0o644 });
                h.set_mtime(0);
                h.set_entry_type(tar::EntryType::Regular);
                tar.append_data(&mut h, path, &bytes[..]).unwrap();
            }
            for (path, target) in &self.links {
                let mut h = tar::Header::new_gnu();
                h.set_size(0);
                h.set_mode(0o777);
                h.set_mtime(0);
                h.set_entry_type(tar::EntryType::Symlink);
                tar.append_link(&mut h, path, target).unwrap();
            }
            if !self.skip_manifest {
                let text = serde_json::to_vec_pretty(&inner).unwrap();
                let mut h = tar::Header::new_gnu();
                h.set_size(text.len() as u64);
                h.set_mode(0o644);
                h.set_mtime(0);
                h.set_entry_type(tar::EntryType::Regular);
                tar.append_data(&mut h, "telamon-bundle.json", &text[..])
                    .unwrap();
            }
            tar.finish().unwrap();
        }
        // Hostile entries are written by hand: the library refuses `..`.
        if !self.raw.is_empty() {
            // Drop the two zero blocks the builder ended with.
            tar_bytes.truncate(tar_bytes.len() - 1024);
            for r in &self.raw {
                let mut h = tar::Header::new_gnu();
                {
                    let gnu = h.as_gnu_mut().unwrap();
                    gnu.name[..r.name.len().min(100)]
                        .copy_from_slice(&r.name[..r.name.len().min(100)]);
                    if let Some(l) = &r.link {
                        gnu.linkname[..l.len().min(100)].copy_from_slice(&l[..l.len().min(100)]);
                    }
                }
                h.set_size(r.data.len() as u64);
                h.set_mode(0o644);
                h.set_entry_type(r.kind);
                h.set_cksum();
                tar_bytes.extend_from_slice(h.as_bytes());
                tar_bytes.extend_from_slice(&r.data);
                let pad = (512 - r.data.len() % 512) % 512;
                tar_bytes.extend(std::iter::repeat_n(0u8, pad));
            }
            tar_bytes.extend(std::iter::repeat_n(0u8, 1024));
        }
        let archive = zstd::stream::encode_all(&tar_bytes[..], 3).unwrap();
        let sha256 = hex(&Sha256::digest(&archive));
        let mut outer = inner.clone();
        outer.archive = Some(ArchiveInfo {
            name: archive_name(&self.id, &self.version),
            sha256: sha256.clone(),
            size: archive.len() as u64,
        });
        if let Some(f) = &self.edit_outer {
            f(&mut outer);
        }
        // `inner` as the Store reads it from the archive has no `archive`.
        debug_assert!(inner.archive.is_none());
        let _ = Kind::Inner;
        Built {
            archive,
            sha256,
            inner,
            outer,
        }
    }
}
