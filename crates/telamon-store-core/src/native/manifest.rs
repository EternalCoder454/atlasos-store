//! `telamon-bundle.json`: what a bundle says about itself.
//!
//! There are two. The **outer** one is a file of the GitHub release; it is
//! the inner one plus an `archive` object (name, SHA-256 and size of the
//! `.tar.zst`; an archive cannot hold its own hash). The **inner** one is at
//! the archive's root, lists every file with its size and SHA-256, and has no
//! `archive`. The Store checks the download against the outer one, then the
//! unpacked tree against the inner one, then that both say the same.
//!
//! Format (schema 1) in the framework's docs/BUNDLES.md. Unknown keys are
//! ignored, so a later tool can add fields; every key read is checked here.

use serde::{Deserialize, Serialize};

use super::version::Version;
use super::{Error, err, valid_app_id};
use crate::launch::{hidden, https_url};
use crate::text;

/// Largest manifest read, from the network or from the archive.
pub const MAX_MANIFEST: u64 = 1024 * 1024;
/// Most files (and links) a bundle may hold.
pub const MAX_FILES: usize = 20_000;
/// Largest archive the Store downloads.
pub const MAX_ARCHIVE: u64 = 256 * 1024 * 1024;
/// Largest file inside a bundle.
pub const MAX_FILE: u64 = 512 * 1024 * 1024;
/// Largest bundle once unpacked.
pub const MAX_UNPACKED: u64 = 1024 * 1024 * 1024;
/// The one CPU architecture bundles are built for.
pub const ARCH: &str = "x86_64";
/// The manifest's name inside the archive.
pub const NAME: &str = "telamon-bundle.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEntry {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveInfo {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub summary: String,
    pub homepage: String,
    pub license: String,
    pub arch: String,
    pub min_telamon_ui: String,
    pub min_os_version: String,
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub links: Vec<LinkEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<ArchiveInfo>,
}

/// Which of the two manifests is being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The release file: has `archive`.
    Outer,
    /// The one inside the archive: has none.
    Inner,
}

/// What the Store is running on, for the minimums a bundle names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `VERSION_ID` of `/etc/os-release` (Fedora's number, such as 44).
    pub os_version: Option<u32>,
    /// The installed telamon-ui.
    pub telamon_ui: Option<Version>,
    pub arch: String,
}

impl Host {
    /// This computer: `/etc/os-release` and `rpm -q telamon-ui`. What cannot
    /// be found out is `None` and not checked.
    pub fn detect() -> Host {
        Host {
            os_version: std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|t| os_version_id(&t)),
            telamon_ui: telamon_ui_version(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }
}

/// `VERSION_ID=44` (quoted or not) of an os-release file.
pub fn os_version_id(text: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix("VERSION_ID=")?.trim().trim_matches('"');
        if v.is_empty() || v.len() > 4 || !v.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        v.parse().ok()
    })
}

fn telamon_ui_version() -> Option<Version> {
    // Fixed path and an empty environment: a PATH in a user session holds
    // folders the user can write to.
    let out = std::process::Command::new("/usr/bin/rpm")
        .args(["-q", "--qf", "%{VERSION}", "telamon-ui"])
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.len() > 64 {
        return None;
    }
    Version::parse(std::str::from_utf8(&out.stdout).ok()?.trim())
}

fn is_hex_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The most parts a path in a bundle may have (`share/a/b/file` is four). One
/// number for everything that walks a bundle's tree: the unpacker, the link
/// resolver and the recursive remove (whose own limit is well above it), so a
/// tree the Store unpacked can always be walked and removed again.
pub const MAX_PATH_PARTS: usize = 32;

/// A relative path inside a bundle: `/`-separated plain names, none empty,
/// `.` or `..`, at most 1024 bytes in all, 255 in a name and
/// [`MAX_PATH_PARTS`] parts, no control or hidden characters.
pub fn valid_rel_path(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 1024
        && p.split('/').count() <= MAX_PATH_PARTS
        && !p.starts_with('/')
        && p.split('/').all(|n| {
            !n.is_empty() && n != "." && n != ".." && n.len() <= 255 && !n.chars().any(hidden)
        })
}

/// `<id>-<version>-x86_64.tar.zst`: the archive's one allowed name.
pub fn archive_name(id: &str, version: &str) -> String {
    format!("{id}-{version}-{ARCH}.tar.zst")
}

fn plain(field: &str, s: &str, max: usize, required: bool) -> Result<String, Error> {
    let cleaned = text::clean(s, max);
    if required && cleaned.is_empty() {
        return Err(err(format!("The bundle's {field} is empty.")));
    }
    Ok(cleaned)
}

impl Manifest {
    /// Parses and checks a manifest. The texts shown (name, summary, license)
    /// are cleaned and cut to length; every other field is refused when it
    /// is not exactly right.
    pub fn parse(bytes: &[u8], kind: Kind) -> Result<Manifest, Error> {
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err(err("The bundle's manifest is too large."));
        }
        let mut m: Manifest = serde_json::from_slice(bytes)
            .map_err(|_| err("The bundle's manifest is not valid."))?;
        m.check(kind)?;
        Ok(m)
    }

    fn check(&mut self, kind: Kind) -> Result<(), Error> {
        if self.schema != 1 {
            return Err(err(
                "The bundle is in a newer format than this Store understands. Update the Store.",
            ));
        }
        if !valid_app_id(&self.id) {
            return Err(err("The bundle's app ID is not valid."));
        }
        self.name = plain("name", &self.name, 80, true)?;
        self.summary = plain("summary", &self.summary, 300, false)?;
        self.license = plain("license", &self.license, 100, false)?;
        if Version::parse(&self.version).is_none() {
            return Err(err("The bundle's version is not valid."));
        }
        if self.arch != ARCH {
            return Err(err(format!(
                "The bundle is built for {}, not {ARCH}.",
                text::clean(&self.arch, 20)
            )));
        }
        if !self.homepage.is_empty() && https_url(&self.homepage).as_deref() != Some(&self.homepage)
        {
            return Err(err("The bundle's home page is not a plain https address."));
        }
        if Version::parse(&self.min_telamon_ui).is_none() {
            return Err(err("The bundle's minimum Telamon.Ui version is not valid."));
        }
        if self.min_os_version.is_empty()
            || self.min_os_version.len() > 4
            || !self.min_os_version.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(err("The bundle's minimum OS version is not valid."));
        }
        if self.files.is_empty() {
            return Err(err("The bundle lists no files."));
        }
        if self.files.len() > MAX_FILES || self.links.len() > MAX_FILES {
            return Err(err("The bundle lists too many files."));
        }
        let mut seen = std::collections::HashSet::new();
        let mut total = 0u64;
        for f in &self.files {
            if !valid_rel_path(&f.path) || f.path == NAME {
                return Err(err("The bundle lists a file with an unusable path."));
            }
            if !is_hex_sha256(&f.sha256) {
                return Err(err("The bundle lists a file with a bad checksum."));
            }
            if f.size > MAX_FILE {
                return Err(err("The bundle lists a file that is too large."));
            }
            total = total.saturating_add(f.size);
            if !seen.insert(f.path.as_str()) {
                return Err(err("The bundle lists a file twice."));
            }
        }
        if total > MAX_UNPACKED {
            return Err(err("The bundle is too large once unpacked."));
        }
        for l in &self.links {
            if !valid_rel_path(&l.path) || !seen.insert(l.path.as_str()) {
                return Err(err("The bundle lists a link with an unusable path."));
            }
            if !link_target_ok(&l.path, &l.target) {
                return Err(err("The bundle has a link that leaves its folder."));
            }
        }
        match (kind, &self.archive) {
            (Kind::Outer, None) => return Err(err("The release's manifest has no archive.")),
            (Kind::Inner, Some(_)) => {
                return Err(err("The manifest inside the archive names an archive."));
            }
            (Kind::Outer, Some(a)) => {
                if a.name != archive_name(&self.id, &self.version) {
                    return Err(err(
                        "The archive's name does not match the app and version.",
                    ));
                }
                if !is_hex_sha256(&a.sha256) {
                    return Err(err("The archive's checksum is not valid."));
                }
                if a.size == 0 || a.size > MAX_ARCHIVE {
                    return Err(err("The archive is larger than the Store accepts."));
                }
            }
            (Kind::Inner, None) => {}
        }
        Ok(())
    }

    /// Whether `host` can run this bundle; the reason when not.
    pub fn compatible(&self, host: &Host) -> Result<(), Error> {
        if host.arch != self.arch {
            return Err(err(format!(
                "It is built for {}, not this computer.",
                self.arch
            )));
        }
        if let (Some(have), Ok(need)) = (host.os_version, self.min_os_version.parse::<u32>())
            && have < need
        {
            return Err(err(format!(
                "It needs Telamon OS based on Fedora {need} or newer."
            )));
        }
        if let (Some(have), Some(need)) = (&host.telamon_ui, Version::parse(&self.min_telamon_ui))
            && *have < need
        {
            return Err(err(format!(
                "It needs Telamon.Ui {need} or newer; update Telamon OS first."
            )));
        }
        Ok(())
    }

    /// The version, parsed (checked when the manifest was read).
    pub fn parsed_version(&self) -> Version {
        Version::parse(&self.version).expect("checked by Manifest::parse")
    }

    /// Whether `other` (the inner manifest) says what this (the outer) says:
    /// everything but `archive`.
    pub fn same_content(&self, other: &Manifest) -> bool {
        let mut a = self.clone();
        let mut b = other.clone();
        a.archive = None;
        b.archive = None;
        a.files.sort_by(|x, y| x.path.cmp(&y.path));
        b.files.sort_by(|x, y| x.path.cmp(&y.path));
        a.links.sort_by(|x, y| x.path.cmp(&y.path));
        b.links.sort_by(|x, y| x.path.cmp(&y.path));
        a == b
    }
}

/// Whether a link at `path` pointing at `target` stays inside the bundle,
/// read lexically (the unpacker checks again against the real folders): a
/// relative target of plain names and `..`, which never climbs above the
/// bundle's root.
pub fn link_target_ok(path: &str, target: &str) -> bool {
    if target.is_empty()
        || target.len() > 1024
        || target.starts_with('/')
        || target.chars().any(hidden)
    {
        return false;
    }
    let mut depth = path.split('/').count() as i64 - 1;
    for part in target.split('/') {
        match part {
            "" | "." => return false,
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {
                depth += 1;
                // What the link leads to is a path of the bundle too.
                if depth > MAX_PATH_PARTS as i64 {
                    return false;
                }
            }
        }
    }
    true
}

/// A good outer manifest, for other modules' tests.
#[cfg(test)]
pub(crate) fn tests_sample() -> Manifest {
    tests::sample(Kind::Outer)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample(kind: Kind) -> Manifest {
        Manifest {
            schema: 1,
            id: "net.eterneon.telamon.gates".into(),
            name: "Telamon Gates".into(),
            version: "0.2.0".into(),
            summary: "Chat with a local AI model".into(),
            homepage: "https://github.com/EternalCoder454/telamon-gates".into(),
            license: "MIT".into(),
            arch: "x86_64".into(),
            min_telamon_ui: "2.0.2".into(),
            min_os_version: "44".into(),
            files: vec![FileEntry {
                path: "bin/telamon-gates".into(),
                size: 10,
                sha256: "a".repeat(64),
                executable: true,
            }],
            links: vec![],
            archive: (kind == Kind::Outer).then(|| ArchiveInfo {
                name: "net.eterneon.telamon.gates-0.2.0-x86_64.tar.zst".into(),
                sha256: "b".repeat(64),
                size: 1000,
            }),
        }
    }

    fn json(m: &Manifest) -> Vec<u8> {
        serde_json::to_vec_pretty(m).unwrap()
    }

    #[test]
    fn a_good_manifest_round_trips() {
        for kind in [Kind::Outer, Kind::Inner] {
            let m = sample(kind);
            assert_eq!(Manifest::parse(&json(&m), kind), Ok(m));
        }
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let mut v: serde_json::Value = serde_json::to_value(sample(Kind::Outer)).unwrap();
        v["future"] = serde_json::json!({"x": 1});
        let bytes = serde_json::to_vec(&v).unwrap();
        assert!(Manifest::parse(&bytes, Kind::Outer).is_ok());
    }

    fn rejects(edit: impl FnOnce(&mut Manifest), kind: Kind) {
        let mut m = sample(kind);
        edit(&mut m);
        assert!(Manifest::parse(&json(&m), kind).is_err(), "{m:?}");
    }

    #[test]
    fn bad_manifests_are_refused() {
        use Kind::{Inner, Outer};
        rejects(|m| m.schema = 2, Outer);
        rejects(|m| m.id = "gates".into(), Outer);
        rejects(|m| m.id = "../x.y".into(), Outer);
        rejects(|m| m.name = String::new(), Outer);
        rejects(|m| m.name = "\u{202e}".into(), Outer);
        rejects(|m| m.version = "v1".into(), Outer);
        rejects(|m| m.arch = "aarch64".into(), Outer);
        rejects(|m| m.homepage = "http://example.org".into(), Outer);
        rejects(|m| m.homepage = "https://localhost/".into(), Outer);
        rejects(|m| m.min_telamon_ui = "x".into(), Outer);
        rejects(|m| m.min_os_version = "44.1".into(), Outer);
        rejects(|m| m.files.clear(), Outer);
        rejects(|m| m.files[0].path = "/etc/passwd".into(), Outer);
        rejects(|m| m.files[0].path = "bin/../../x".into(), Outer);
        rejects(|m| m.files[0].path = NAME.into(), Outer);
        rejects(|m| m.files[0].sha256 = "A".repeat(64), Outer);
        rejects(|m| m.files[0].sha256 = "a".repeat(63), Outer);
        rejects(|m| m.files[0].size = MAX_FILE + 1, Outer);
        rejects(|m| m.files.push(m.files[0].clone()), Outer);
        rejects(
            |m| {
                m.links.push(LinkEntry {
                    path: "bin/x".into(),
                    target: "/etc/passwd".into(),
                })
            },
            Outer,
        );
        rejects(
            |m| {
                m.links.push(LinkEntry {
                    path: "bin/x".into(),
                    target: "../../etc".into(),
                })
            },
            Outer,
        );
        rejects(|m| m.archive = None, Outer);
        rejects(
            |m| m.archive.as_mut().unwrap().name = "x.tar.zst".into(),
            Outer,
        );
        rejects(|m| m.archive.as_mut().unwrap().sha256 = "zz".into(), Outer);
        rejects(
            |m| m.archive.as_mut().unwrap().size = MAX_ARCHIVE + 1,
            Outer,
        );
        rejects(|m| m.archive.as_mut().unwrap().size = 0, Outer);
        rejects(
            |m| {
                m.archive = Some(ArchiveInfo {
                    name: "n".into(),
                    sha256: "a".repeat(64),
                    size: 1,
                })
            },
            Inner,
        );
        assert!(Manifest::parse(b"{", Kind::Outer).is_err());
        assert!(Manifest::parse(&vec![b' '; 2 * 1024 * 1024], Kind::Outer).is_err());
    }

    #[test]
    fn link_targets_stay_inside() {
        assert!(link_target_ok("share/a/link", "../b"));
        assert!(link_target_ok("bin/x", "y"));
        assert!(!link_target_ok("bin/x", "../../y"));
        assert!(!link_target_ok("x", "../y"));
        assert!(!link_target_ok("bin/x", "/usr/bin/y"));
        assert!(!link_target_ok("bin/x", ""));
        assert!(!link_target_ok("bin/x", "a//b"));
        assert!(!link_target_ok("bin/x", "./a"));
    }

    #[test]
    fn paths_and_link_targets_have_a_depth_cap() {
        let path = |n: usize| vec!["d"; n].join("/");
        assert!(valid_rel_path(&path(MAX_PATH_PARTS)));
        assert!(!valid_rel_path(&path(MAX_PATH_PARTS + 1)));
        assert!(!valid_rel_path(&path(300)));
        // A link whose target resolves to a path over the cap.
        assert!(link_target_ok("l", &path(MAX_PATH_PARTS)));
        assert!(!link_target_ok("l", &path(MAX_PATH_PARTS + 1)));
        assert!(link_target_ok("a/l", &path(MAX_PATH_PARTS - 1)));
        assert!(!link_target_ok("a/l", &path(MAX_PATH_PARTS)));
    }

    #[test]
    fn host_minimums() {
        let m = sample(Kind::Outer);
        let host = |os: Option<u32>, ui: Option<&str>| Host {
            os_version: os,
            telamon_ui: ui.and_then(Version::parse),
            arch: "x86_64".into(),
        };
        assert!(m.compatible(&host(Some(44), Some("2.0.2"))).is_ok());
        assert!(m.compatible(&host(Some(45), Some("2.1.0"))).is_ok());
        assert!(m.compatible(&host(Some(43), Some("2.0.2"))).is_err());
        assert!(m.compatible(&host(Some(44), Some("2.0.1"))).is_err());
        // What cannot be found out is not held against the bundle.
        assert!(m.compatible(&host(None, None)).is_ok());
        let mut other = host(Some(44), Some("2.0.2"));
        other.arch = "aarch64".into();
        assert!(m.compatible(&other).is_err());
    }

    #[test]
    fn os_release_is_read() {
        assert_eq!(os_version_id("NAME=x\nVERSION_ID=44\n"), Some(44));
        assert_eq!(os_version_id("VERSION_ID=\"44\"\n"), Some(44));
        assert_eq!(os_version_id("VERSION_ID=rawhide\n"), None);
        assert_eq!(os_version_id(""), None);
    }

    #[test]
    fn outer_and_inner_compare_without_the_archive() {
        let outer = sample(Kind::Outer);
        let mut inner = sample(Kind::Inner);
        assert!(outer.same_content(&inner));
        inner.version = "0.3.0".into();
        assert!(!outer.same_content(&inner));
    }
}
