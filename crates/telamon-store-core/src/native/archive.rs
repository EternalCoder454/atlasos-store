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
//!   cut off at the cap (a "zip bomb" ends there) and may use a window of at
//!   most 128 MiB; what the tar library keeps in memory for an entry (its
//!   long-name and PAX headers) is capped at 1 MiB;
//! - everything is made through an open folder (`Dir`): every file and folder
//!   is created by `openat`/`mkdirat` from its parent's descriptor, files new
//!   (`O_EXCL`, `O_NOFOLLOW`), folders 0700, so no name in the archive and no
//!   link another process plants meanwhile can lead a write out of the tree;
//!   archive modes are ignored (files are 0644 or, when the manifest says
//!   `executable`, 0755; never setuid; the folders above are 0700);
//! - links are made last, relative, and each is followed by hand, one name at
//!   a time, to something that exists inside the folder;
//! - nothing but zero padding may follow the end of the tar;
//! - afterwards the folder is compared with the inner manifest: the same
//!   files, sizes and SHA-256, the same links, and, when the release's outer
//!   manifest is known, the same content.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Read, Write};
use std::path::Path;
use std::rc::Rc;

use sha2::{Digest, Sha256};

use super::dirfd::Dir;
use super::manifest::{self, Kind, MAX_FILE, MAX_FILES, MAX_MANIFEST, MAX_UNPACKED, Manifest};
use super::{Error, err, io_err};

/// Tests change the folders under an unpack to see that it does not follow.
#[cfg(feature = "test-hooks")]
pub mod test_hooks {
    use std::cell::RefCell;
    /// What a test runs at a step.
    pub type Callback = Box<dyn Fn(&str)>;
    thread_local! {
        /// Called with the path of each entry before it is written.
        pub static ON_ENTRY: RefCell<Option<Callback>> = const { RefCell::new(None) };
    }
}

/// The largest window the zstd decoder may use: 2^27 bytes, 128 MiB. A frame
/// that asks for more is refused, so a bundle cannot make the Store allocate
/// what it likes.
const WINDOW_LOG_MAX: u32 = 27;
/// What the tar library may read while it looks for the next entry: the
/// padding of the last one, the header, and the long-name and PAX headers that
/// come with it. They are read into memory by the library, so a header that
/// claims a gigabyte stops here instead.
const HEADERS_MAX: u64 = 1024 * 1024;
/// Zeros after the end of the tar that are tolerated (the block padding).
const TRAILING_ZEROS_MAX: u64 = 1024 * 1024;

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

/// While armed, the reads of the tar library (looking for the next entry) are
/// limited; the reads of an entry's content, which this module makes, are not.
#[derive(Default)]
struct Gate {
    armed: Cell<bool>,
    left: Cell<u64>,
}

impl Gate {
    fn arm(&self) {
        self.left.set(HEADERS_MAX);
        self.armed.set(true);
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

/// A reader that fails after `left` more bytes, and, while the gate is armed,
/// after the header allowance.
struct Capped<R> {
    inner: R,
    left: u64,
    gate: Rc<Gate>,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            // Distinguish "ended" from "too much": try one byte.
            let mut one = [0u8; 1];
            return match self.inner.read(&mut one)? {
                0 => Ok(0),
                _ => Err(io::Error::other(TOO_BIG)),
            };
        }
        let mut max = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        if self.gate.armed.get() {
            let allow = self.gate.left.get();
            if allow == 0 {
                // Headers, not content: the library would keep it in memory.
                return Err(io::Error::other(HEADERS_TOO_BIG));
            }
            max = max.min(allow.min(usize::MAX as u64) as usize);
        }
        let n = self.inner.read(&mut buf[..max])?;
        self.left -= n as u64;
        if self.gate.armed.get() {
            self.gate.left.set(self.gate.left.get() - n as u64);
        }
        Ok(n)
    }
}

const TOO_BIG: &str = "telamon-store: bundle is too large";
const HEADERS_TOO_BIG: &str = "telamon-store: bundle headers are too large";

fn read_error(e: &io::Error) -> Error {
    if e.to_string().contains(TOO_BIG) {
        err("The bundle is larger than the Store accepts once unpacked.")
    } else {
        err("The bundle's archive is damaged.")
    }
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

/// Opens (making them when missing, 0700) the folders of `rel` below `root`,
/// one at a time, each from the one above with `O_NOFOLLOW`: a link where a
/// folder is needed, or a file, is an error. `rel` may be empty.
fn ensure_dirs(root: &Dir, rel: &str) -> Result<Dir, Error> {
    let mut cur: Option<Dir> = None;
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        let base = cur.as_ref().unwrap_or(root);
        let next = base.ensure_sub(part, 0o700, false).map_err(|e| {
            if matches!(e.raw_os_error(), Some(libc::ENOTDIR) | Some(libc::ELOOP)) {
                err("The bundle puts a file where it also needs a folder.")
            } else {
                io_err("make a folder", &e)
            }
        })?;
        cur = Some(next);
    }
    match cur {
        Some(d) => Ok(d),
        None => root.try_clone().map_err(|e| io_err("make a folder", &e)),
    }
}

/// What was found in the archive.
struct Found {
    files: BTreeMap<String, (u64, String)>,
    links: BTreeMap<String, String>,
    manifest: Option<Vec<u8>>,
}

/// Unpacks the archive at `archive` into `dest`, a folder that exists and is
/// empty (it is opened here, a link as `dest` itself is refused), and checks
/// it against its own manifest (and `outer`, the release's manifest, when
/// given). On an error `dest` may hold a part of the bundle; the caller
/// removes it.
pub fn unpack(archive: &Path, dest: &Path, outer: Option<&Manifest>) -> Result<Manifest, Error> {
    let dest = Dir::open_last_nofollow(dest).map_err(|e| io_err("open the bundle's folder", &e))?;
    unpack_into(archive, &dest, outer)
}

/// [`unpack`] into a folder that is already open. Every file and folder is
/// made from `dest` downward by `openat` with `O_NOFOLLOW`, so nothing in the
/// archive, and nothing another process puts in the tree meanwhile, can send
/// a write outside it.
pub fn unpack_into(
    archive: &Path,
    dest: &Dir,
    outer: Option<&Manifest>,
) -> Result<Manifest, Error> {
    let file = crate::appimage::fsutil::open_regular(archive)
        .map_err(|e| io_err("open the downloaded bundle", &e))?;
    let mut decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|_| err("The bundle is not a zstd archive."))?;
    decoder
        .window_log_max(WINDOW_LOG_MAX)
        .map_err(|_| err("The bundle is not a zstd archive."))?;
    // Headers add to the content; 512 bytes an entry, and long names.
    let limit = MAX_UNPACKED + (MAX_FILES as u64 * 2 + 16) * 1536;
    let gate = Rc::new(Gate::default());
    let reader = Capped {
        inner: decoder,
        left: limit,
        gate: gate.clone(),
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
    // The folder of the last file, for the next one in the same folder.
    let mut last_parent: Option<(String, Dir)> = None;
    {
        let mut iter = tar.entries().map_err(|e| read_error(&e))?;
        loop {
            gate.arm();
            let next = iter.next();
            gate.disarm();
            let Some(entry) = next else { break };
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
                || (path == manifest::NAME && found.manifest.is_some())
                || (dirs.contains(&path) && !is_dir)
            {
                return Err(err("The bundle holds the same name twice."));
            }
            #[cfg(feature = "test-hooks")]
            test_hooks::ON_ENTRY.with(|f| {
                if let Some(f) = f.borrow().as_ref() {
                    f(&path)
                }
            });
            let (parent_rel, name) = match path.rsplit_once('/') {
                Some((p, n)) => (p, n),
                None => ("", path.as_str()),
            };
            if is_dir {
                ensure_dirs(dest, &path)?;
                dirs.insert(path);
                last_parent = None;
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
                ensure_dirs(dest, parent_rel)?;
                found.links.insert(path, target);
                continue;
            }
            // Regular files, and the "contiguous" type, which is one. Sparse
            // files (GNU `S`), hard links, devices, FIFOs and the global PAX
            // header are not accepted.
            if !(kind.is_file() || kind == tar::EntryType::Continuous) {
                return Err(err(
                    "The bundle holds something that is not a file, folder or link.",
                ));
            }
            // The size the library will read: a PAX `size` overrides the
            // header's, and this is the one that counts.
            let size = entry.size();
            if size > MAX_FILE {
                return Err(err("The bundle holds a file that is too large."));
            }
            total = total.saturating_add(size);
            if total > MAX_UNPACKED {
                return Err(err(
                    "The bundle is larger than the Store accepts once unpacked.",
                ));
            }
            let parent = if let Some((p, d)) = &last_parent
                && p == parent_rel
            {
                d.try_clone().map_err(|e| io_err("make a folder", &e))?
            } else {
                let d = ensure_dirs(dest, parent_rel)?;
                let c = d.try_clone().map_err(|e| io_err("make a folder", &e))?;
                last_parent = Some((parent_rel.to_string(), c));
                d
            };
            let mut out = parent
                .create_new(name, 0o600)
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
    }
    // After the end of the tar only the block padding (zeros) may follow.
    let mut rest = tar.into_inner();
    let mut seen = 0u64;
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = rest.read(&mut buf).map_err(|e| read_error(&e))?;
        if n == 0 {
            break;
        }
        seen += n as u64;
        if seen > TRAILING_ZEROS_MAX || buf[..n].iter().any(|b| *b != 0) {
            return Err(err("The bundle's archive is damaged."));
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

    // Links last, in folders that exist, then each followed by hand through
    // the tree (a link that is not inside, or leads nowhere, refuses the
    // bundle).
    for (path, target) in &found.links {
        let (parent_rel, name) = path.rsplit_once('/').unwrap_or(("", path.as_str()));
        let parent = ensure_dirs(dest, parent_rel)?;
        parent.symlink(target, name).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                err("The bundle holds the same name twice.")
            } else {
                io_err("make a link", &e)
            }
        })?;
    }
    for path in found.links.keys() {
        match resolve_inside(dest, path) {
            Resolved::Inside => {}
            Resolved::Nowhere => return Err(err("The bundle has a link that points nowhere.")),
            Resolved::Outside => {
                return Err(err("The bundle has a link that leaves its folder."));
            }
        }
    }

    // Modes: the manifest's word, not the archive's.
    for f in &inner.files {
        let mode = if f.executable { 0o755 } else { 0o644 };
        set_mode(dest, &f.path, mode)?;
    }
    set_mode(dest, manifest::NAME, 0o644)?;
    Ok(inner)
}

fn set_mode(root: &Dir, rel: &str, mode: u32) -> Result<(), Error> {
    let bad = |e: io::Error| io_err("set a file's mode", &e);
    let (dir, name) = root.walk_to_parent(rel).map_err(bad)?;
    dir.as_ref()
        .unwrap_or(root)
        .chmod_file(name, mode)
        .map_err(bad)
}

/// Where a link in the unpacked tree ends.
#[derive(Debug, PartialEq, Eq)]
enum Resolved {
    Inside,
    Nowhere,
    Outside,
}

/// Follows `path` (and every link on the way) by hand from `root`, one name at
/// a time, so that nothing is resolved by the kernel behind the folder's back:
/// a `..` that would leave `root`, an absolute target or a loop is not
/// inside; a name that is missing is nowhere. The folders on the way are kept
/// open in a stack (a step is one `fstatat`, and one `openat` when it is a
/// folder), so the work is in step with the length of the path, not its
/// square.
fn resolve_inside(root: &Dir, path: &str) -> Resolved {
    let mut queue: VecDeque<String> = path.split('/').map(str::to_string).collect();
    // The real names so far; the folder behind each is open, a file has none.
    let mut stack: Vec<Option<Dir>> = Vec::new();
    let mut hops = 0;
    while let Some(part) = queue.pop_front() {
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                match stack.pop() {
                    None => return Resolved::Outside,
                    // `file/..` is not a path.
                    Some(None) => return Resolved::Nowhere,
                    Some(Some(_)) => {}
                }
                continue;
            }
            _ => {}
        }
        if stack.len() >= manifest::MAX_PATH_PARTS * 2 {
            return Resolved::Nowhere;
        }
        // The folder the name is in: the last one on the stack.
        let dir = match stack.last() {
            None => root,
            Some(Some(d)) => d,
            // Below a file.
            Some(None) => return Resolved::Nowhere,
        };
        match dir.stat(&part) {
            Err(_) => return Resolved::Nowhere,
            Ok(m) if m.kind == super::dirfd::Kind::Link => {
                hops += 1;
                if hops > 40 {
                    return Resolved::Nowhere;
                }
                let Ok(target) = dir.read_link(&part) else {
                    return Resolved::Nowhere;
                };
                if target.starts_with('/') {
                    return Resolved::Outside;
                }
                for piece in target.split('/').rev() {
                    queue.push_front(piece.to_string());
                }
            }
            Ok(m) if m.kind == super::dirfd::Kind::Dir => match dir.sub(&part) {
                Ok(d) => stack.push(Some(d)),
                Err(_) => return Resolved::Nowhere,
            },
            Ok(_) => stack.push(None),
        }
    }
    Resolved::Inside
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
