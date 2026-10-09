//! A repository's latest GitHub release, as the Store reads it.
//!
//! `GET https://api.github.com/repos/<repo>/releases/latest` names the latest
//! published release that is neither a draft nor a prerelease. Only its tag
//! and its files matter, and a file counts only when its download address is
//! exactly `https://github.com/<repo>/releases/download/<tag>/<name>`: the
//! same repository, the same release. Anything else in the answer is ignored.

use serde::Deserialize;

use super::manifest::{self, Manifest};
use super::sign;
use super::{Error, err};

/// Largest API answer read.
pub const MAX_RELEASE_JSON: u64 = 1024 * 1024;
/// Largest `telamon-bundle.json` downloaded.
pub const MAX_MANIFEST_DOWNLOAD: u64 = manifest::MAX_MANIFEST;
/// Largest `telamon-bundle.json.minisig` downloaded.
pub const MAX_SIGNATURE_DOWNLOAD: u64 = sign::MAX_SIGNATURE;
/// Assets looked at in one release.
const MAX_ASSETS: usize = 300;

/// The API address for a repository's latest release.
pub fn latest_url(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// One file of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
    /// GitHub's own SHA-256 of the file (`digest: "sha256:..."`), when it
    /// gives one.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// A tag the Store will put in an address: letters, digits, `.`, `_`, `-`.
fn tag_ok(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A release asset's name: the same characters as a tag, plus none other
/// (the two names the Store looks for are plain).
fn asset_name_ok(name: &str) -> bool {
    tag_ok(name) && name.len() <= 200
}

impl Release {
    /// Reads the API's answer for `repo`. Fails for a draft or prerelease
    /// (the API does not return those for `latest`; this is a second check)
    /// and for a tag that cannot go in an address. Assets whose address is
    /// not in this repository's release for this tag are dropped.
    pub fn parse(bytes: &[u8], repo: &str) -> Result<Release, Error> {
        if bytes.len() as u64 > MAX_RELEASE_JSON {
            return Err(err("GitHub's answer is larger than the Store accepts."));
        }
        let raw: RawRelease = serde_json::from_slice(bytes)
            .map_err(|_| err("GitHub's answer about the release is not valid."))?;
        if raw.draft || raw.prerelease {
            return Err(err("The latest release is not a final release."));
        }
        if !tag_ok(&raw.tag_name) {
            return Err(err("The release's tag is not usable."));
        }
        let prefix = format!(
            "https://github.com/{repo}/releases/download/{}/",
            raw.tag_name
        );
        let mut assets = Vec::new();
        for a in raw.assets.into_iter().take(MAX_ASSETS) {
            if !asset_name_ok(&a.name) || a.state.as_deref().is_some_and(|s| s != "uploaded") {
                continue;
            }
            let Some(head) = a.browser_download_url.get(..prefix.len()) else {
                continue;
            };
            // The repository's letter case is GitHub's; the catalog's may differ.
            if !head.eq_ignore_ascii_case(&prefix)
                || a.browser_download_url[prefix.len()..] != a.name
            {
                continue;
            }
            let sha256 = a
                .digest
                .as_deref()
                .and_then(|d| d.strip_prefix("sha256:"))
                .filter(|h| {
                    h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                })
                .map(str::to_string);
            assets.push(Asset {
                name: a.name,
                size: a.size,
                url: a.browser_download_url,
                sha256,
            });
        }
        Ok(Release {
            tag: raw.tag_name,
            assets,
        })
    }

    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// The release's `telamon-bundle.json`, or why there is none.
    pub fn manifest_asset(&self) -> Result<&Asset, Error> {
        let a = self
            .asset(manifest::NAME)
            .ok_or_else(|| err("The latest release has no Telamon bundle."))?;
        if a.size == 0 || a.size > MAX_MANIFEST_DOWNLOAD {
            return Err(err(
                "The release's manifest is larger than the Store accepts.",
            ));
        }
        Ok(a)
    }

    /// The release's `telamon-bundle.json.minisig`, or why there is none. A
    /// release without one is not used: see [`super::sign`].
    pub fn signature_asset(&self) -> Result<&Asset, Error> {
        let a = self
            .asset(sign::SIGNATURE_NAME)
            .ok_or_else(|| err("The latest release is not signed."))?;
        if a.size == 0 || a.size > MAX_SIGNATURE_DOWNLOAD {
            return Err(err(
                "The release's signature is larger than the Store accepts.",
            ));
        }
        Ok(a)
    }

    /// The archive `manifest` names, checked against what GitHub says about
    /// the file (size, and its SHA-256 when it gives one) and against the
    /// tag: `v<version>` or `<version>`.
    pub fn archive_asset(&self, m: &Manifest) -> Result<&Asset, Error> {
        let info = m
            .archive
            .as_ref()
            .ok_or_else(|| err("The manifest names no archive."))?;
        let tag_version = self.tag.strip_prefix('v').unwrap_or(&self.tag);
        if tag_version != m.version {
            return Err(err(format!(
                "The release is tagged {} but the bundle is version {}.",
                crate::text::clean(&self.tag, 40),
                m.version
            )));
        }
        let a = self
            .asset(&info.name)
            .ok_or_else(|| err("The release does not hold the archive its manifest names."))?;
        if a.size != info.size {
            return Err(err("The archive's size is not what its manifest says."));
        }
        if let Some(h) = &a.sha256
            && *h != info.sha256
        {
            return Err(err("The archive's checksum is not what GitHub has for it."));
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REPO: &str = "EternalCoder454/telamon-gates";

    fn release(tag: &str, assets: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(
            &json!({"tag_name": tag, "draft": false, "prerelease": false, "assets": assets}),
        )
        .unwrap()
    }

    fn asset(tag: &str, name: &str, size: u64) -> serde_json::Value {
        json!({"name": name, "size": size, "state": "uploaded",
               "browser_download_url": format!("https://github.com/{REPO}/releases/download/{tag}/{name}")})
    }

    /// An answer recorded from the real API (this repository's own latest
    /// release, once): the shape the parser has to read, with GitHub's digest.
    #[test]
    fn a_recorded_answer_is_read() {
        let recorded = include_str!("../../tests/fixtures/native/github-release-latest.json");
        let r = Release::parse(recorded.as_bytes(), "EternalCoder454/atlasos-store").unwrap();
        assert_eq!(r.tag, "v0.3.1");
        assert_eq!(r.assets.len(), 1);
        let rpm = r.asset("telamon-store-0.3.1-1.fc44.x86_64.rpm").unwrap();
        assert!(rpm.size > 1_000_000);
        assert_eq!(rpm.sha256.as_ref().map(String::len), Some(64));
        // It is not a Telamon app release: no bundle in it.
        assert!(r.manifest_asset().is_err());
        // Asked about another repository, none of its files count.
        let other = Release::parse(recorded.as_bytes(), "EternalCoder454/telamon-gates").unwrap();
        assert!(other.assets.is_empty());
    }

    #[test]
    fn assets_of_this_release_are_kept() {
        let bytes = release(
            "v0.2.0",
            json!([
                asset("v0.2.0", "telamon-bundle.json", 900),
                asset("v0.2.0", "x-0.2.0-x86_64.tar.zst", 5000)
            ]),
        );
        let r = Release::parse(&bytes, REPO).unwrap();
        assert_eq!(r.tag, "v0.2.0");
        assert_eq!(r.assets.len(), 2);
        assert_eq!(r.manifest_asset().unwrap().size, 900);
    }

    #[test]
    fn the_catalogs_letter_case_may_differ() {
        let bytes = release("v1", json!([asset("v1", "telamon-bundle.json", 9)]));
        assert!(
            Release::parse(&bytes, "eternalcoder454/TELAMON-GATES")
                .unwrap()
                .manifest_asset()
                .is_ok()
        );
    }

    #[test]
    fn assets_from_elsewhere_are_dropped() {
        let other = |url: &str| json!({"name": "telamon-bundle.json", "size": 9, "browser_download_url": url});
        for url in [
            "https://github.com/Other/repo/releases/download/v1/telamon-bundle.json",
            "https://github.com/EternalCoder454/telamon-gates/releases/download/v0/telamon-bundle.json",
            "https://evil.example.net/EternalCoder454/telamon-gates/releases/download/v1/telamon-bundle.json",
            "http://github.com/EternalCoder454/telamon-gates/releases/download/v1/telamon-bundle.json",
            "https://github.com/EternalCoder454/telamon-gates/releases/download/v1/other.json",
            "https://github.com/EternalCoder454/telamon-gates/releases/download/v1/telamon-bundle.json?x=1",
            "https://github.com/EternalCoder454/telamon-gates/releases/download/v1/../v2/telamon-bundle.json",
        ] {
            let r = Release::parse(&release("v1", json!([other(url)])), REPO).unwrap();
            assert!(r.manifest_asset().is_err(), "{url}");
        }
    }

    #[test]
    fn the_signature_is_the_releases_own_small_file() {
        let sig = "telamon-bundle.json.minisig";
        let r = Release::parse(&release("v1", json!([asset("v1", sig, 330)])), REPO).unwrap();
        assert_eq!(r.signature_asset().unwrap().size, 330);
        // Missing, empty or too large.
        let none = Release::parse(&release("v1", json!([])), REPO).unwrap();
        assert!(none.signature_asset().unwrap_err().0.contains("not signed"));
        for size in [0, 4097, 1 << 20] {
            let r = Release::parse(&release("v1", json!([asset("v1", sig, size)])), REPO).unwrap();
            assert!(r.signature_asset().is_err(), "{size}");
        }
        // From another release or repository it is not there at all.
        let other = json!({"name": sig, "size": 330, "browser_download_url":
            format!("https://github.com/Other/repo/releases/download/v1/{sig}")});
        let r = Release::parse(&release("v1", json!([other])), REPO).unwrap();
        assert!(r.signature_asset().is_err());
    }

    #[test]
    fn drafts_prereleases_and_odd_tags_are_refused() {
        let base = |extra: serde_json::Value| {
            let mut v = json!({"tag_name": "v1", "assets": []});
            for (k, val) in extra.as_object().unwrap() {
                v[k] = val.clone();
            }
            serde_json::to_vec(&v).unwrap()
        };
        assert!(Release::parse(&base(json!({"draft": true})), REPO).is_err());
        assert!(Release::parse(&base(json!({"prerelease": true})), REPO).is_err());
        assert!(Release::parse(&base(json!({"tag_name": "v1/../x"})), REPO).is_err());
        assert!(Release::parse(&base(json!({"tag_name": ""})), REPO).is_err());
        assert!(Release::parse(b"[]", REPO).is_err());
        assert!(Release::parse(&vec![b' '; 2 << 20], REPO).is_err());
    }

    #[test]
    fn the_archive_must_match_its_manifest_and_the_tag() {
        use crate::native::manifest::{ArchiveInfo, FileEntry, Kind};
        let m = Manifest {
            schema: 1,
            id: "net.eterneon.telamon.gates".into(),
            name: "Gates".into(),
            version: "0.2.0".into(),
            summary: String::new(),
            homepage: String::new(),
            license: String::new(),
            arch: "x86_64".into(),
            min_telamon_ui: "2.0.2".into(),
            min_os_version: "44".into(),
            files: vec![FileEntry {
                path: "bin/x".into(),
                size: 1,
                sha256: "a".repeat(64),
                executable: true,
            }],
            links: vec![],
            archive: Some(ArchiveInfo {
                name: "net.eterneon.telamon.gates-0.2.0-x86_64.tar.zst".into(),
                sha256: "b".repeat(64),
                size: 5000,
            }),
        };
        let name = "net.eterneon.telamon.gates-0.2.0-x86_64.tar.zst";
        let ok = Release::parse(
            &release("v0.2.0", json!([asset("v0.2.0", name, 5000)])),
            REPO,
        )
        .unwrap();
        assert!(ok.archive_asset(&m).is_ok());
        // Wrong tag for the version.
        let old = Release::parse(
            &release("v0.1.0", json!([asset("v0.1.0", name, 5000)])),
            REPO,
        )
        .unwrap();
        assert!(old.archive_asset(&m).is_err());
        // Wrong size.
        let big = Release::parse(
            &release("v0.2.0", json!([asset("v0.2.0", name, 5001)])),
            REPO,
        )
        .unwrap();
        assert!(big.archive_asset(&m).is_err());
        // Missing.
        let none = Release::parse(&release("v0.2.0", json!([])), REPO).unwrap();
        assert!(none.archive_asset(&m).is_err());
        // GitHub's own digest disagrees.
        let mut a = asset("v0.2.0", name, 5000);
        a["digest"] = json!(format!("sha256:{}", "c".repeat(64)));
        let bad = Release::parse(&release("v0.2.0", json!([a])), REPO).unwrap();
        assert!(bad.archive_asset(&m).is_err());
        let mut a = asset("v0.2.0", name, 5000);
        a["digest"] = json!(format!("sha256:{}", "b".repeat(64)));
        let good = Release::parse(&release("v0.2.0", json!([a])), REPO).unwrap();
        assert!(good.archive_asset(&m).is_ok());
        let _ = Kind::Outer;
    }
}
