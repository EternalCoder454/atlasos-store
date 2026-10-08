//! Reading a bundle's `.tar.zst`: every entry is checked before anything is
//! written, and the result is a private folder holding exactly what the
//! bundle's own manifest lists.
//!
//! The tar library only reads here; it never unpacks. Each entry is looked at
//! and written by this module:
//!
//! - names are relative, plain (no `..`, `.`, empty parts, hidden characters)
//!   and unique; a leading `./` and the `./` folder entry are tolerated;
//! - only folders, regular files and symbolic links exist: hard links,
//!   devices, FIFOs and anything else refuse the bundle;
//! - sizes, counts and the decompressed total are capped, the decompressor is
//!   cut off at the cap (a "zip bomb" ends there);
//! - files are made new (`O_EXCL`, `O_NOFOLLOW`) below folders this module made;
//!   archive modes are ignored (files are 0644 or, when the manifest says
//!   `executable`, 0755; folders 0755; never setuid);
//! - links are made last, relative, and each must resolve (on the real folder)
//!   to something inside the folder;
//! - afterwards the folder is compared with the inner manifest: the same
//!   files, sizes and SHA-256, the same links, and, when the release's outer
//!   manifest is known, the same content.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::manifest::{self, Kind, MAX_FILE, MAX_FILES, MAX_MANIFEST, MAX_UNPACKED, Manifest};
use super::{Error, err, io_err};

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(DIGITS[(b >> 4) as usize] as char);
        s.push(DIGITS[(b & 15) as usize] as char);
    }
    s
}

/// SHA-256 of a file, read through `open_regular`'s rules (no links).
pub fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    let mut f = crate::appimage::fsutil::open_regular(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf)?;
        if r == 0 {
            return Ok((hex(&h.finalize()), n));
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
}

/// A reader that fails after `left` more bytes.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            // Distinguish "ended" from "too much": try one byte.
            let mut one = [0u8; 1];
            return match self.inner.read(&mut one)? {
                0 => Ok(0),
                _ => Err(io::Error::other(TOO_BIG)),
            };
        }
        let max = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        Ok(n)
    }
}

const TOO_BIG: &str = "telamon-store: bundle is too large";

fn read_error(e: &io::Error) -> Error {
    if e.to_string().contains(TOO_BIG) {
        err("The bundle is larger than the Store accepts once unpacked.")
    } else {
        err("The bundle's archive is damaged.")
    }
}

/// Creates the folders of `rel` (a relative path) below `root`, one at a time,
/// each of them a real folder.
fn ensure_dirs(root: &Path, rel: &Path) -> Result<PathBuf, Error> {
    let mut cur = root.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        match DirBuilder::new().mode(0o755).create(&cur) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err("make a folder", &e)),
        }
        let md = fs::symlink_metadata(&cur).map_err(|e| io_err("check a folder", &e))?;
        if !md.is_dir() {
            return Err(err("The bundle puts a file where it also needs a folder."));
        }
    }
    Ok(cur)
}

/// The entry's path as a plain relative string, or `None` for the root `./`.
fn entry_path(raw: &[u8], is_dir: bool) -> Result<Option<String>, Error> {
    let s = std::str::from_utf8(raw)
        .map_err(|_| err("The bundle has a file name that is not text."))?;
    let s = s.strip_prefix("./").unwrap_or(s);
    let s = if is_dir {
        s.strip_suffix('/').unwrap_or(s)
    } else {
        s
    };
    if s.is_empty() || s == "." {
        return if is_dir {
            Ok(None)
        } else {
            Err(err("The bundle has an unusable file name."))
        };
    }
    if !manifest::valid_rel_path(s) {
        return Err(err(
            "The bundle has a file name that leads outside its folder.",
        ));
    }
    Ok(Some(s.to_string()))
}

/// What was found in the archive.
struct Found {
    files: BTreeMap<String, (u64, String)>,
    links: BTreeMap<String, String>,
    manifest: Option<Vec<u8>>,
}

/// Unpacks the archive at `archive` into `dest`, which must exist and be
/// empty, and checks it against its own manifest (and `outer`, the release's
/// manifest, when given). On an error `dest` may hold a part of the bundle;
/// the caller removes it.
pub fn unpack(archive: &Path, dest: &Path, outer: Option<&Manifest>) -> Result<Manifest, Error> {
    let file = crate::appimage::fsutil::open_regular(archive)
        .map_err(|e| io_err("open the downloaded bundle", &e))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|_| err("The bundle is not a zstd archive."))?;
    // Headers add to the content; 512 bytes an entry, and long names.
    let limit = MAX_UNPACKED + (MAX_FILES as u64 * 2 + 16) * 1536;
    let reader = Capped {
        inner: decoder,
        left: limit,
    };
    let mut tar = tar::Archive::new(reader);
    tar.set_ignore_zeros(false);

    let mut found = Found {
        files: BTreeMap::new(),
        links: BTreeMap::new(),
        manifest: None,
    };
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    let mut entries = 0usize;
    let mut total = 0u64;
    let entries_iter = tar.entries().map_err(|e| read_error(&e))?;
    for entry in entries_iter {
        let mut entry = entry.map_err(|e| read_error(&e))?;
        entries += 1;
        if entries > MAX_FILES * 2 {
            return Err(err("The bundle holds too many files."));
        }
        let kind = entry.header().entry_type();
        let is_dir = kind.is_dir();
        let raw = entry.path_bytes().into_owned();
        let Some(path) = entry_path(&raw, is_dir)? else {
            continue;
        };
        if found.files.contains_key(&path)
            || found.links.contains_key(&path)
            || (dirs.contains(&path) && !is_dir)
        {
            return Err(err("The bundle holds the same name twice."));
        }
        let rel = Path::new(&path);
        if is_dir {
            ensure_dirs(dest, rel)?;
            dirs.insert(path);
            continue;
        }
        if kind.is_symlink() {
            let target = entry
                .link_name_bytes()
                .ok_or_else(|| err("The bundle has a link with no target."))?;
            let target = std::str::from_utf8(&target)
                .map_err(|_| err("The bundle has a link target that is not text."))?
                .to_string();
            if !manifest::link_target_ok(&path, &target) {
                return Err(err("The bundle has a link that leaves its folder."));
            }
            if let Some(parent) = rel.parent() {
                ensure_dirs(dest, parent)?;
            }
            found.links.insert(path, target);
            continue;
        }
        if !(kind.is_file() || kind == tar::EntryType::Continuous) {
            return Err(err(
                "The bundle holds something that is not a file, folder or link.",
            ));
        }
        let size = entry
            .header()
            .size()
            .map_err(|_| err("The bundle's archive is damaged."))?;
        if size > MAX_FILE {
            return Err(err("The bundle holds a file that is too large."));
        }
        total = total.saturating_add(size);
        if total > MAX_UNPACKED {
            return Err(err(
                "The bundle is larger than the Store accepts once unpacked.",
            ));
        }
        if let Some(parent) = rel.parent() {
            ensure_dirs(dest, parent)?;
        }
        let target = dest.join(rel);
        let mut out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&target)
            .map_err(|e| io_err("write the bundle's files", &e))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut written = 0u64;
        let mut keep = (path == manifest::NAME).then(Vec::new);
        loop {
            let n = entry.read(&mut buf).map_err(|e| read_error(&e))?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > size {
                return Err(err("The bundle's archive is damaged."));
            }
            hasher.update(&buf[..n]);
            if let Some(k) = keep.as_mut() {
                if k.len() as u64 + n as u64 > MAX_MANIFEST {
                    return Err(err("The bundle's manifest is too large."));
                }
                k.extend_from_slice(&buf[..n]);
            }
            out.write_all(&buf[..n])
                .map_err(|e| io_err("write the bundle's files", &e))?;
        }
        if written != size {
            return Err(err("The bundle's archive is damaged."));
        }
        out.flush()
            .map_err(|e| io_err("write the bundle's files", &e))?;
        if let Some(k) = keep {
            found.manifest = Some(k);
        } else {
            found.files.insert(path, (size, hex(&hasher.finalize())));
        }
    }

    let manifest_bytes = found
        .manifest
        .take()
        .ok_or_else(|| err("The bundle has no manifest inside."))?;
    let inner = Manifest::parse(&manifest_bytes, Kind::Inner)?;
    compare(&inner, &found)?;
    if let Some(outer) = outer
        && !outer.same_content(&inner)
    {
        return Err(err(
            "The manifest inside the bundle is not the one the release published.",
        ));
    }

    // Links last, below folders that exist, then each checked on the real tree.
    let root = fs::canonicalize(dest).map_err(|e| io_err("check the bundle's folder", &e))?;
    for (path, target) in &found.links {
        let at = dest.join(path);
        std::os::unix::fs::symlink(target, &at).map_err(|e| io_err("make a link", &e))?;
    }
    for path in found.links.keys() {
        let real = fs::canonicalize(dest.join(path))
            .map_err(|_| err("The bundle has a link that points nowhere."))?;
        if !real.starts_with(&root) {
            return Err(err("The bundle has a link that leaves its folder."));
        }
    }

    // Modes: the manifest's word, not the archive's.
    for f in &inner.files {
        let mode = if f.executable { 0o755 } else { 0o644 };
        fs::set_permissions(dest.join(&f.path), fs::Permissions::from_mode(mode))
            .map_err(|e| io_err("set a file's mode", &e))?;
    }
    fs::set_permissions(dest.join(manifest::NAME), fs::Permissions::from_mode(0o644))
        .map_err(|e| io_err("set a file's mode", &e))?;
    Ok(inner)
}

/// The unpacked tree against the manifest: the same files, the same links.
fn compare(m: &Manifest, found: &Found) -> Result<(), Error> {
    if m.files.len() != found.files.len() {
        return Err(err(
            "The bundle's files are not the ones its manifest lists.",
        ));
    }
    for f in &m.files {
        match found.files.get(&f.path) {
            Some((size, sha)) if *size == f.size && *sha == f.sha256 => {}
            Some(_) => {
                return Err(err(
                    "A file in the bundle is not what its manifest says (the checksum differs).",
                ));
            }
            None => return Err(err("The bundle is missing a file its manifest lists.")),
        }
    }
    if m.links.len() != found.links.len()
        || !m
            .links
            .iter()
            .all(|l| found.links.get(&l.path) == Some(&l.target))
    {
        return Err(err(
            "The bundle's links are not the ones its manifest lists.",
        ));
    }
    Ok(())
}
