//! Reads files out of the squashfs inside an AppImage without mounting or
//! running anything. The image is untrusted, so the superblock is checked
//! before the reader (the `backhand` crate) gets it, and everything read
//! has a cap:
//!
//! - the block sizes, counts and table offsets must be sane, and the two
//!   metadata tables the reader decompresses whole (inodes and directories)
//!   are walked block by block first, so a table of thousands of tiny blocks
//!   that each expand to 8 KiB is refused before anything is decompressed;
//! - only the few paths an inspection needs are kept (the top folder, the
//!   metainfo, applications, pixmaps and hicolor icon folders);
//! - a file bigger than its cap is refused, not cut, and all reads together
//!   have a budget.
//!
//! The reader never follows a symlink out of the image: links are resolved
//! inside the image's own tree.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::os::unix::fs::FileExt;
use std::path::{Component, Path};

use backhand::{FilesystemReader, InnerNode, Node, SquashfsFileReader};

/// The caps on one squashfs. The defaults are far above a real AppImage
/// (Electron apps have about 50,000 inodes in 6,000 metadata blocks).
#[derive(Debug, Clone)]
pub struct Limits {
    /// Most inodes the superblock may claim.
    pub max_inodes: u32,
    /// Most fragment table entries.
    pub max_fragments: u32,
    /// Most 8 KiB metadata blocks in the inode table and in the directory
    /// table, each. (Decompressed memory is at most this times 8 KiB.)
    pub max_meta_blocks: u32,
    /// Largest file read, in bytes.
    pub max_file: u64,
    /// All files read together, in bytes.
    pub max_total: u64,
    /// Entries kept in the index of interesting paths.
    pub max_kept: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_inodes: 400_000,
            max_fragments: 400_000,
            max_meta_blocks: 8192,
            max_file: 2 << 20,
            max_total: 8 << 20,
            max_kept: 20_000,
        }
    }
}

/// Why the image was not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SquashError {
    /// Over one of the limits.
    TooLarge(&'static str),
    /// Not a squashfs this reader accepts, or one that disagrees with itself.
    Damaged(&'static str),
    /// A compression the reader does not have.
    Unsupported,
    /// The path is not in the image, is not a file, or is a link that leaves
    /// the image or loops.
    NoFile,
    Io,
}

impl std::fmt::Display for SquashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SquashError::TooLarge(what) => write!(f, "too large ({what})"),
            SquashError::Damaged(what) => write!(f, "damaged ({what})"),
            SquashError::Unsupported => f.write_str("compressed in a way the Store can't read"),
            SquashError::NoFile => f.write_str("no such file"),
            SquashError::Io => f.write_str("could not be read"),
        }
    }
}

const SUPERBLOCK: usize = 96;
const MAGIC: [u8; 4] = *b"hsqs";
/// Compressor ids in the superblock: gzip, xz and zstd are built in.
const COMPRESSORS: [u16; 3] = [1, 4, 6];
/// Metadata blocks are at most 8 KiB, compressed or not.
const META_MAX: u64 = 8192;

fn le16(b: &[u8]) -> u64 {
    u64::from(u16::from_le_bytes([b[0], b[1]]))
}
fn le32(b: &[u8]) -> u64 {
    u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
fn le64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Walks the metadata blocks from `start` to `end` by their 2-byte headers
/// and checks that they end exactly at `end`. Returns the block count.
fn walk_meta(
    file: &File,
    base: u64,
    start: u64,
    end: u64,
    max_blocks: u32,
) -> Result<u32, SquashError> {
    let mut at = start;
    let mut blocks = 0u32;
    while at < end {
        blocks += 1;
        if blocks > max_blocks {
            return Err(SquashError::TooLarge("metadata"));
        }
        let mut h = [0u8; 2];
        file.read_exact_at(&mut h, base + at)
            .map_err(|_| SquashError::Damaged("metadata block"))?;
        let size = u64::from(u16::from_le_bytes(h) & 0x7FFF);
        if size == 0 || size > META_MAX {
            return Err(SquashError::Damaged("metadata block size"));
        }
        at += 2 + size;
    }
    if at != end {
        return Err(SquashError::Damaged("metadata tables"));
    }
    Ok(blocks)
}

/// Checks the squashfs superblock at `base` in `file` (`len` bytes) against
/// `limits`. Nothing is decompressed.
pub fn check_superblock(
    file: &File,
    base: u64,
    len: u64,
    limits: &Limits,
) -> Result<(), SquashError> {
    if base.checked_add(SUPERBLOCK as u64).is_none_or(|e| e > len) {
        return Err(SquashError::Damaged("no room for a squashfs"));
    }
    let mut sb = [0u8; SUPERBLOCK];
    file.read_exact_at(&mut sb, base)
        .map_err(|_| SquashError::Io)?;
    if sb[..4] != MAGIC {
        return Err(SquashError::Damaged("not a little-endian squashfs"));
    }
    let inodes = le32(&sb[4..]);
    let block_size = le32(&sb[12..]);
    let frags = le32(&sb[16..]);
    let compressor = le16(&sb[20..]) as u16;
    let block_log = le16(&sb[22..]);
    let (major, minor) = (le16(&sb[28..]), le16(&sb[30..]));
    let used = le64(&sb[40..]);
    let id_table = le64(&sb[48..]);
    let xattr_table = le64(&sb[56..]);
    let inode_table = le64(&sb[64..]);
    let dir_table = le64(&sb[72..]);
    let frag_table = le64(&sb[80..]);
    let export_table = le64(&sb[88..]);
    if (major, minor) != (4, 0) {
        return Err(SquashError::Damaged("version"));
    }
    if !COMPRESSORS.contains(&compressor) {
        return Err(SquashError::Unsupported);
    }
    // `block_log` is a 16-bit field of the file: shifting by it unchecked
    // panics (or wraps) for anything over 63.
    if !(4096..=1 << 20).contains(&block_size)
        || !block_size.is_power_of_two()
        || block_log > 20
        || (1u64 << block_log) != block_size
    {
        return Err(SquashError::Damaged("block size"));
    }
    if inodes > u64::from(limits.max_inodes) {
        return Err(SquashError::TooLarge("files"));
    }
    if frags > u64::from(limits.max_fragments) {
        return Err(SquashError::TooLarge("fragments"));
    }
    if used > len - base || used < SUPERBLOCK as u64 {
        return Err(SquashError::Damaged("size"));
    }
    // The tables in the order mksquashfs writes them: data, inodes,
    // directories, fragments, (export), ids, (xattrs).
    let absent = u64::MAX;
    if !(SUPERBLOCK as u64..used).contains(&inode_table)
        || !(inode_table + 1..used).contains(&dir_table)
    {
        return Err(SquashError::Damaged("table offsets"));
    }
    let after_dirs = [frag_table, export_table, id_table, xattr_table]
        .into_iter()
        .filter(|t| *t != absent && *t > dir_table && *t < used)
        .min()
        .unwrap_or(used);
    walk_meta(file, base, inode_table, dir_table, limits.max_meta_blocks)?;
    walk_meta(file, base, dir_table, after_dirs, limits.max_meta_blocks)?;
    // The id and fragment tables are arrays of offsets of metadata blocks;
    // backhand seeks to each of them, adding the squashfs's start in the file
    // with a plain `+`, so an offset near u64::MAX panics (overflow checks on).
    check_lookup(file, base, id_table, le16(&sb[26..]), 4, used)?;
    check_lookup(file, base, frag_table, frags, 16, used)?;
    Ok(())
}

/// A lookup table of `count` entries of `entry` bytes: at `table` an array of
/// one little-endian u64 per 8 KiB metadata block, each pointing inside the
/// image (`used` bytes from `base`).
fn check_lookup(
    file: &File,
    base: u64,
    table: u64,
    count: u64,
    entry: u64,
    used: u64,
) -> Result<(), SquashError> {
    if count == 0 {
        return Ok(());
    }
    let blocks = count.div_ceil(META_MAX / entry);
    let bad = SquashError::Damaged("lookup table");
    if !(SUPERBLOCK as u64..used).contains(&table) || table + blocks * 8 > used {
        return Err(bad);
    }
    for i in 0..blocks {
        let mut p = [0u8; 8];
        file.read_exact_at(&mut p, base + table + i * 8)
            .map_err(|_| SquashError::Damaged("lookup table"))?;
        let at = u64::from_le_bytes(p);
        if !(SUPERBLOCK as u64..used).contains(&at) {
            return Err(bad);
        }
    }
    Ok(())
}

/// What kind of entry a kept path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File(u64),
    Dir,
    Symlink,
}

/// The kept entries of an open image, and the budget for reading them.
pub struct Tree<'a> {
    fs: &'a FilesystemReader<'static>,
    nodes: HashMap<String, &'a Node<SquashfsFileReader>>,
    limits: &'a Limits,
    total: Cell<u64>,
}

/// `a/b/../c` as `a/c`; `..` stops at the root; empty and `.` parts go.
fn normalize(path: &Path) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(n) => parts.push(n.to_str().unwrap_or("\u{fffd}")),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    parts.join("/")
}

/// Paths kept besides the top folder's.
const KEPT: [&str; 5] = [
    "usr/share/metainfo/",
    "usr/share/appdata/",
    "usr/share/applications/",
    "usr/share/pixmaps/",
    "usr/share/icons/hicolor/",
];

fn kept(path: &str) -> bool {
    path.matches('/').count() == 0 || KEPT.iter().any(|p| path.starts_with(p))
}

impl<'a> Tree<'a> {
    /// The kept paths that start with `prefix` (a folder, ending in `/`, or
    /// "" for the top folder's entries), sorted.
    pub fn list(&self, prefix: &str) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .nodes
            .keys()
            .map(String::as_str)
            .filter(|p| p.starts_with(prefix) && (!prefix.is_empty() || !p.contains('/')))
            .collect();
        out.sort_unstable();
        out
    }

    pub fn kind(&self, path: &str) -> Option<Kind> {
        Some(match &self.nodes.get(path)?.inner {
            InnerNode::File(f) => Kind::File(f.file_len() as u64),
            InnerNode::Dir(_) => Kind::Dir,
            InnerNode::Symlink(_) => Kind::Symlink,
            _ => return None,
        })
    }

    /// The path a link chain from `path` ends at, if it ends at a file.
    fn resolve(&self, path: &str) -> Option<&'a Node<SquashfsFileReader>> {
        let mut cur = normalize(Path::new(path));
        for _ in 0..8 {
            let node = *self.nodes.get(&cur)?;
            match &node.inner {
                InnerNode::File(_) => return Some(node),
                InnerNode::Symlink(link) => {
                    let target = if link.link.is_absolute() {
                        link.link.clone()
                    } else {
                        Path::new(&cur)
                            .parent()
                            .unwrap_or(Path::new(""))
                            .join(&link.link)
                    };
                    cur = normalize(&target);
                }
                _ => return None,
            }
        }
        None
    }

    /// The bytes of the file at `path` (following links inside the image), at
    /// most `max` of them: a bigger file is `TooLarge`, not cut.
    pub fn read(&self, path: &str, max: u64) -> Result<Vec<u8>, SquashError> {
        let node = self.resolve(path).ok_or(SquashError::NoFile)?;
        let InnerNode::File(file) = &node.inner else {
            return Err(SquashError::NoFile);
        };
        let size = file.file_len() as u64;
        let cap = max.min(self.limits.max_file);
        if size > cap {
            return Err(SquashError::TooLarge("file"));
        }
        // backhand allocates every block as big as its size field says before it
        // reads it, and that field is 31 bits of the file's own: a block is never
        // larger than the squashfs block size, so an image that says otherwise
        // (up to 4 GiB) is refused here.
        let block_max = u64::from(self.fs.block_size);
        let fragment = self
            .fs
            .fragments
            .as_ref()
            .and_then(|f| f.get(file.frag_index()));
        if file
            .block_sizes()
            .iter()
            .any(|b| u64::from(b.size()) > block_max)
            || fragment.is_some_and(|f| u64::from(f.size.size()) > block_max)
        {
            return Err(SquashError::Damaged("block size"));
        }
        // backhand slices the decompressed fragment with `block_offset ..
        // block_offset + (size - blocks * block_size)` without checking either
        // against the file or the fragment: both come from the image. A file
        // with a fragment must have its full blocks inside its size and its
        // tail inside one block.
        if file.frag_index() != u32::MAX as usize {
            if fragment.is_none() {
                return Err(SquashError::Damaged("fragment"));
            }
            let full = (file.block_sizes().len() as u64).checked_mul(block_max);
            let tail = full.and_then(|f| size.checked_sub(f));
            let end = tail.and_then(|t| t.checked_add(u64::from(file.block_offset())));
            if end.is_none_or(|e| e > block_max) {
                return Err(SquashError::Damaged("fragment"));
            }
        }
        let used = self.total.get().saturating_add(size);
        if used > self.limits.max_total {
            return Err(SquashError::TooLarge("all files read"));
        }
        self.total.set(used);
        let mut out = Vec::with_capacity(size as usize);
        // One byte more than the size would show a reader that lies.
        // backhand's reader slices and subtracts with numbers from the image
        // that the checks above cannot all know (the decompressed length of a
        // fragment, for one); a panic there is a damaged image, not a crash.
        let read = contained(|| {
            self.fs
                .file(file)
                .reader()
                .take(size + 1)
                .read_to_end(&mut out)
        });
        match read {
            Some(Ok(_)) => {}
            _ => return Err(SquashError::Damaged("file data")),
        }
        if out.len() as u64 != size {
            return Err(SquashError::Damaged("file size"));
        }
        Ok(out)
    }
}

fn map_error(e: &backhand::BackhandError) -> SquashError {
    let text = e.to_string().to_ascii_lowercase();
    if text.contains("unsupported") || text.contains("compress") {
        SquashError::Unsupported
    } else {
        SquashError::Damaged("squashfs")
    }
}

thread_local! {
    /// How many calls into backhand, whose panics are caught, are running on
    /// this thread.
    static IN_BACKHAND: Cell<u32> = const { Cell::new(0) };
}

/// Whether this thread is inside a call into backhand whose panic is caught
/// and turned into "damaged" (so a panic hook, such as a fuzzer's, can tell it
/// from a panic of the Store's own code).
pub fn in_contained_backhand_call() -> bool {
    IN_BACKHAND.with(|c| c.get() > 0)
}

/// Runs `f` (a call into backhand) and turns its panic into `None`.
fn contained<T>(f: impl FnOnce() -> T) -> Option<T> {
    IN_BACKHAND.with(|c| c.set(c.get() + 1));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    IN_BACKHAND.with(|c| c.set(c.get() - 1));
    r.ok()
}

/// The part of a file from `base` to its end, as a reader of its own: reads
/// and seeks never overflow, and a position past the end reads nothing.
struct Window {
    file: File,
    base: u64,
    len: u64,
    pos: u64,
}

impl std::io::Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let want =
            usize::try_from(self.len - self.pos).map_or(buf.len(), |left| left.min(buf.len()));
        // base + pos < base + len <= the file's length: no overflow.
        let n = self.file.read_at(&mut buf[..want], self.base + self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl std::io::Seek for Window {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        use std::io::{Error, ErrorKind, SeekFrom};
        let invalid = || Error::new(ErrorKind::InvalidInput, "seek outside the squashfs");
        let pos = match to {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => self.pos.checked_add_signed(d).ok_or_else(invalid)?,
            SeekFrom::End(d) => self.len.checked_add_signed(d).ok_or_else(invalid)?,
        };
        self.pos = pos;
        Ok(pos)
    }
}

/// Opens the squashfs at `base` in the file and hands its tree to `f`.
/// `len` is the file's length.
pub fn with_tree<T>(
    file: &File,
    base: u64,
    len: u64,
    limits: &Limits,
    f: impl FnOnce(&Tree<'_>) -> T,
) -> Result<T, SquashError> {
    check_superblock(file, base, len, limits)?;
    let dup = file.try_clone().map_err(|_| SquashError::Io)?;
    // backhand adds the squashfs's start (`base`) to every offset it reads from
    // the file with a plain `+`, which overflows for an offset near u64::MAX
    // (and panics now that the release profile checks overflow). It is given
    // a window that starts at 0 instead, whose arithmetic is checked here.
    let window = Window {
        file: dup,
        base,
        len: len.saturating_sub(base),
        pos: 0,
    };
    let reader = BufReader::with_capacity(64 << 10, window);
    let fs = contained(|| FilesystemReader::from_reader(reader))
        .ok_or(SquashError::Damaged("squashfs"))?
        .map_err(|e| map_error(&e))?;
    let mut nodes: HashMap<String, &Node<SquashfsFileReader>> = HashMap::new();
    for node in fs.files() {
        let path = normalize(&node.fullpath);
        if path.is_empty() || !kept(&path) {
            continue;
        }
        if nodes.len() >= limits.max_kept {
            break;
        }
        nodes.insert(path, node);
    }
    let tree = Tree {
        fs: &fs,
        nodes,
        limits,
        total: Cell::new(0),
    };
    Ok(f(&tree))
}
