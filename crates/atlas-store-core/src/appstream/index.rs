//! The on-disk index: a parsed catalog in a compact binary file under the
//! cache directory, so the Store starts without parsing 50 MB of XML.
//!
//! The file is a cache, and it is read as untrusted: it is opened without
//! following symlinks, its size and checksum are checked, every length is
//! checked against the bytes that are left, and every string must be UTF-8.
//! Any mismatch is an [`IndexError`] and the caller rebuilds from the XML.
//!
//! The checksum only detects accidental damage: anyone who can write the file
//! can write a matching checksum. So the decoder never trusts the payload: it
//! applies the parser's own checks to every string (no control or bidi
//! characters, one line, within the parser's lengths), ID, URL, icon file,
//! bundle reference, runtime and SDK, requires the bundle to match the
//! component and to exist where the parser requires it, refuses duplicate
//! IDs, requires the catalog's origin to be the key's, and stops at a total budget of decoded
//! data, so a small hostile file can't expand without limit. The cache
//! directory must be the user's own and not writable by group or others, or
//! the index is neither read nor written.
//!
//! Layout, little-endian: the magic `ATLASIDX`, the format version, the key
//! (origin, commit, languages), the payload length, a checksum of everything
//! before it and of the payload, then the payload.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::parse::{Limits, bundle_matches, needs_bundle};
use super::{
    Block, Branding, Bundle, Catalog, Component, ContentRating, Icon, Image, Intensity, Kind,
    RatingScheme, Release, ReleaseKind, Screenshot, Span, Style, UrlKind, Verification,
};
use crate::text;

/// The layout version this code writes and reads.
pub const FORMAT: u32 = 1;

const MAGIC: &[u8; 8] = b"ATLASIDX";
/// Largest index file read, and largest written. The real Flathub index is
/// about 14 MB, so this leaves more than twice that.
const MAX_FILE: u64 = 32 << 20;
/// Most that decoding may account for: the bytes of every string plus 24 for
/// its header, 16 for every list item and 512 for every component. The real
/// Flathub index comes to about 30 MB by this count; 96 MiB is over three
/// times that, and bounds the memory a hostile file can make us use.
const MAX_DECODED: usize = 96 << 20;
/// Temp files of a crashed write older than this are removed.
const STALE_TEMP: Duration = Duration::from_secs(600);

// Caps on what a file may claim, matching the parser's.
const MAX_STR: usize = 64 << 10;
const MAX_COMPONENTS: usize = 100_000;
const MAX_BLOCKS: usize = 64;
const MAX_ITEMS: usize = 256;
const MAX_SPANS: usize = 8192;
const MAX_LIST: usize = 64;

/// What an index was built from. A file is only used for the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexKey {
    /// The remote's name: `[A-Za-z0-9_.-]`, 1 to 64 characters, no leading `.`.
    pub origin: String,
    /// The remote's OSTree commit: lowercase hex, 16 to 64 characters.
    pub commit: String,
    /// The language preference the text was chosen with.
    pub langs: Vec<String>,
    /// The layout version, [`FORMAT`].
    pub format: u32,
}

/// Why an index could not be written or used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// The key has an origin or commit that isn't allowed in a file name.
    InvalidKey(&'static str),
    /// The file could not be opened or read (including a symlink).
    Io(String),
    /// Bigger than the cap.
    TooLarge,
    /// Not an index file, or a different layout version.
    BadHeader(&'static str),
    /// Built from a different origin, commit, language list or version.
    KeyMismatch,
    /// The file is cut short or its checksum is wrong.
    Damaged(&'static str),
    /// A value is out of range or not valid.
    Corrupt(&'static str),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::InvalidKey(w) => write!(f, "invalid index key: {w}"),
            IndexError::Io(e) => write!(f, "can't read the index: {e}"),
            IndexError::TooLarge => write!(f, "the index file is too large"),
            IndexError::BadHeader(w) => write!(f, "not a usable index: {w}"),
            IndexError::KeyMismatch => write!(f, "the index was built for something else"),
            IndexError::Damaged(w) => write!(f, "the index is damaged: {w}"),
            IndexError::Corrupt(w) => write!(f, "the index holds a bad value: {w}"),
        }
    }
}

impl std::error::Error for IndexError {}

impl IndexKey {
    fn check(&self) -> Result<(), IndexError> {
        let o = &self.origin;
        if o.is_empty()
            || o.len() > 64
            || o.starts_with('.')
            || !o
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
        {
            return Err(IndexError::InvalidKey("origin"));
        }
        let c = &self.commit;
        if c.len() < 16
            || c.len() > 64
            || !c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(IndexError::InvalidKey("commit"));
        }
        if self.langs.len() > 32 || self.langs.iter().any(|l| l.len() > 64) {
            return Err(IndexError::InvalidKey("languages"));
        }
        Ok(())
    }

    fn langs_hash(&self) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for l in &self.langs {
            for b in l.bytes().chain(std::iter::once(0)) {
                h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
            }
        }
        h
    }
}

/// The index file for `key` in `cache_dir`:
/// `index-<origin>-<first 16 of commit>-<8 hex of the languages>.bin`.
/// Fails when the origin or commit could not be a safe file name.
pub fn cache_file(cache_dir: &Path, key: &IndexKey) -> Result<PathBuf, IndexError> {
    key.check()?;
    Ok(cache_dir.join(format!(
        "index-{}-{}-{:08x}.bin",
        key.origin,
        &key.commit[..16],
        key.langs_hash()
    )))
}

/// Whether `name` is an index file of `origin`: after `index-<origin>-` come
/// exactly 16 hex, `-`, 8 hex and `.bin`, so `foo` doesn't match `foo-bar`.
fn is_index_of(origin: &str, name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("index-")
        .and_then(|r| r.strip_prefix(origin))
        .and_then(|r| r.strip_prefix('-'))
    else {
        return false;
    };
    let Some(mid) = rest.strip_suffix(".bin") else {
        return false;
    };
    let hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
    match mid.split_once('-') {
        Some((a, b)) => a.len() == 16 && b.len() == 8 && hex(a) && hex(b),
        None => false,
    }
}

/// A fast 64-bit hash over 8-byte words: detects corruption, not tampering.
struct Sum(u64);

impl Sum {
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn new() -> Sum {
        Sum(0xcbf2_9ce4_8422_2325)
    }

    fn update(&mut self, data: &[u8]) {
        let (chunks, rest) = data.as_chunks::<8>();
        for c in chunks {
            self.0 = (self.0 ^ u64::from_le_bytes(*c)).wrapping_mul(Self::PRIME);
            self.0 ^= self.0 >> 29;
        }
        for &b in rest {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(Self::PRIME);
        }
        // The length stops a trailing zero byte going unnoticed.
        self.0 = (self.0 ^ data.len() as u64).wrapping_mul(Self::PRIME);
    }
}

// ---- encoding ----

struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        // Everything written is capped far below u32::MAX by the parser.
        self.u32(u32::try_from(n).unwrap_or(u32::MAX));
    }
    fn str(&mut self, s: &str) {
        self.len(s.len());
        self.0.extend_from_slice(s.as_bytes());
    }
    fn opt_str(&mut self, s: &Option<String>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn strs(&mut self, v: &[String]) {
        self.len(v.len());
        for s in v {
            self.str(s);
        }
    }
    fn spans(&mut self, v: &[Span]) {
        self.len(v.len());
        for s in v {
            self.u8(match s.style {
                Style::Plain => 0,
                Style::Emphasis => 1,
                Style::Code => 2,
            });
            self.str(&s.text);
        }
    }
    fn blocks(&mut self, v: &[Block]) {
        self.len(v.len());
        for b in v {
            match b {
                Block::Paragraph(s) => {
                    self.u8(0);
                    self.spans(s);
                }
                Block::List { ordered, items } => {
                    self.u8(if *ordered { 2 } else { 1 });
                    self.len(items.len());
                    for i in items {
                        self.spans(i);
                    }
                }
            }
        }
    }

    fn component(&mut self, c: &Component) {
        self.str(&c.id);
        self.u8(match c.kind {
            Kind::DesktopApp => 0,
            Kind::ConsoleApp => 1,
            Kind::Addon => 2,
            Kind::Runtime => 3,
            Kind::Other => 4,
        });
        self.str(&c.name);
        self.str(&c.summary);
        self.blocks(&c.description);
        self.str(&c.developer);
        self.str(&c.license);
        self.strs(&c.categories);
        self.strs(&c.keywords);
        match &c.icon {
            Some(i) => {
                self.u8(1);
                self.str(&i.file);
                self.len(i.sizes.len());
                for s in &i.sizes {
                    self.u16(*s);
                }
            }
            None => self.u8(0),
        }
        self.len(c.urls.len());
        for (k, u) in &c.urls {
            self.u8(url_kind_code(*k));
            self.str(u);
        }
        self.len(c.screenshots.len());
        for s in &c.screenshots {
            self.u8(u8::from(s.default));
            self.str(&s.caption);
            self.len(s.images.len());
            for i in &s.images {
                self.u8(u8::from(i.thumbnail));
                self.u32(i.width);
                self.u32(i.height);
                self.str(&i.url);
            }
        }
        self.len(c.releases.len());
        for r in &c.releases {
            self.str(&r.version);
            self.i64(r.timestamp);
            self.u8(match r.kind {
                ReleaseKind::Stable => 0,
                ReleaseKind::Development => 1,
                ReleaseKind::Snapshot => 2,
                ReleaseKind::Other => 3,
            });
            self.blocks(&r.description);
        }
        match &c.content_rating {
            Some(r) => {
                self.u8(1);
                self.u8(match r.scheme {
                    RatingScheme::Oars10 => 0,
                    RatingScheme::Oars11 => 1,
                    RatingScheme::Other => 2,
                });
                self.len(r.attrs.len());
                for (id, i) in &r.attrs {
                    self.str(id);
                    self.u8(match i {
                        Intensity::None => 0,
                        Intensity::Mild => 1,
                        Intensity::Moderate => 2,
                        Intensity::Intense => 3,
                    });
                }
            }
            None => self.u8(0),
        }
        match &c.bundle {
            Some(b) => {
                self.u8(1);
                self.str(&b.reference);
                self.opt_str(&b.runtime);
                self.opt_str(&b.sdk);
            }
            None => self.u8(0),
        }
        self.strs(&c.extends);
        self.opt_str(&c.launchable);
        match &c.verification {
            Some(v) => {
                self.u8(1);
                self.str(&v.method);
                self.str(&v.website);
                self.str(&v.login_name);
                self.str(&v.login_provider);
                self.u8(u8::from(v.organization));
                self.i64(v.timestamp);
            }
            None => self.u8(0),
        }
        match &c.branding {
            Some(b) => {
                self.u8(1);
                for col in [b.light, b.dark] {
                    match col {
                        Some(rgb) => {
                            self.u8(1);
                            self.0.extend_from_slice(&rgb);
                        }
                        None => self.u8(0),
                    }
                }
            }
            None => self.u8(0),
        }
    }
}

fn url_kind_code(k: UrlKind) -> u8 {
    match k {
        UrlKind::Homepage => 0,
        UrlKind::Bugtracker => 1,
        UrlKind::Help => 2,
        UrlKind::Donation => 3,
        UrlKind::Translate => 4,
        UrlKind::Contact => 5,
        UrlKind::Contribute => 6,
        UrlKind::VcsBrowser => 7,
        UrlKind::Faq => 8,
    }
}

fn url_kind_from(c: u8) -> Result<UrlKind, IndexError> {
    Ok(match c {
        0 => UrlKind::Homepage,
        1 => UrlKind::Bugtracker,
        2 => UrlKind::Help,
        3 => UrlKind::Donation,
        4 => UrlKind::Translate,
        5 => UrlKind::Contact,
        6 => UrlKind::Contribute,
        7 => UrlKind::VcsBrowser,
        8 => UrlKind::Faq,
        _ => return Err(IndexError::Corrupt("url kind")),
    })
}

fn encode(key: &IndexKey, cat: &Catalog) -> Vec<u8> {
    let mut p = Enc(Vec::with_capacity(1 << 20));
    p.str(&cat.origin);
    p.u32(cat.skipped);
    p.len(cat.components.len());
    for c in &cat.components {
        p.component(c);
    }
    let payload = p.0;

    let mut h = Enc(Vec::with_capacity(payload.len() + 256));
    h.0.extend_from_slice(MAGIC);
    h.u32(key.format);
    for s in [&key.origin, &key.commit] {
        h.u16(s.len() as u16);
        h.0.extend_from_slice(s.as_bytes());
    }
    h.u16(key.langs.len() as u16);
    for l in &key.langs {
        h.u16(l.len() as u16);
        h.0.extend_from_slice(l.as_bytes());
    }
    h.u64(payload.len() as u64);
    let mut sum = Sum::new();
    sum.update(&h.0);
    sum.update(&payload);
    h.u64(sum.0);
    h.0.extend_from_slice(&payload);
    h.0
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Removes the temp file unless it was renamed into place.
struct TempGuard<'a> {
    path: &'a Path,
    armed: bool,
}

impl Drop for TempGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(self.path);
        }
    }
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

/// Whether `name` is a temp file of a write of `origin`'s index:
/// `.<index file>.tmp.<pid>.<n>`.
fn is_temp_of(origin: &str, name: &str) -> bool {
    let Some((base, tail)) = name
        .strip_prefix('.')
        .and_then(|n| n.split_once(".bin.tmp."))
    else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    is_index_of(origin, &format!("{base}.bin"))
        && tail
            .split_once('.')
            .is_some_and(|(a, b)| digits(a) && digits(b))
}

/// Refuses a cache directory that is a symlink, not a directory or not the
/// user's own. One that only group or others can write to (Fedora's umask 002
/// does that to a directory something else created) is the user's to fix: it
/// is set to 0700 with a warning. A directory that doesn't exist yet is fine.
fn check_dir(dir: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(dir) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    // SAFETY: geteuid has no preconditions and can't fail.
    let me = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != me {
        log::warn!(
            "not using the cache directory {}: it must be a real directory of the current user, not a link",
            dir.display()
        );
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a private directory of the user", dir.display()),
        ));
    }
    if meta.mode() & 0o022 != 0 {
        log::warn!(
            "the cache directory {} is writable by group or others; setting it to 0700",
            dir.display()
        );
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes the index of `key` into `cache_dir` atomically and returns its path
/// (see [`cache_file`]): a temp file in the same directory (created
/// exclusively, without following links, mode 0600), fsynced, renamed into
/// place, then the directory fsynced (best effort). The directory is created
/// with mode 0700; an existing one is refused when it is a link or not the
/// user's, and set to 0700 when group or others can write to it.
/// The catalog must be of the key's origin and the key of this layout version.
/// Afterwards older index files and stale temp files of the same origin are
/// removed. A failure leaves no temp file and the previous index as it was.
pub fn write(cache_dir: &Path, key: &IndexKey, catalog: &Catalog) -> io::Result<PathBuf> {
    key.check().map_err(|e| invalid(e.to_string()))?;
    if key.format != FORMAT {
        return Err(invalid("the key is of another index layout version"));
    }
    if catalog.origin != key.origin {
        return Err(invalid("the catalog is of another origin than the key"));
    }
    if cache_dir.as_os_str().is_empty() {
        return Err(invalid("the cache directory is empty"));
    }
    let path = cache_file(cache_dir, key).map_err(|e| invalid(e.to_string()))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("the index path has no file name"))?
        .to_owned();
    let dir = cache_dir;
    let bytes = encode(key, catalog);
    if bytes.len() as u64 > MAX_FILE {
        return Err(invalid("the index would be larger than the cap"));
    }
    check_dir(dir)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    check_dir(dir)?;

    let mut attempts = 0;
    let (mut file, tmp) = loop {
        let tmp = dir.join(format!(
            ".{}.tmp.{}.{}",
            name.to_string_lossy(),
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
        {
            Ok(f) => break (f, tmp),
            // A leftover of a crashed run with the same pid and counter.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) => return Err(e),
        }
    };
    let mut guard = TempGuard {
        path: &tmp,
        armed: true,
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, &path)?;
    guard.armed = false;
    // The new index is in place; a failed directory sync only means it might
    // not survive a power cut, and the old files must still be cleaned up.
    if let Err(e) = File::open(dir).and_then(|d| d.sync_all()) {
        log::warn!("can't sync the cache directory {}: {e}", dir.display());
    }

    remove_older(dir, &key.origin, &name);
    Ok(path)
}

/// Best-effort removal of the other index files of `origin`, and of its temp
/// files older than ten minutes (a running write's is newer). Symlinks are
/// removed as links, never followed.
fn remove_older(dir: &Path, origin: &str, keep: &std::ffi::OsStr) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("can't list {} to remove old indexes: {e}", dir.display());
            return;
        }
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let fname = entry.file_name();
        if fname == keep {
            continue;
        }
        let Some(n) = fname.to_str() else { continue };
        let temp = is_temp_of(origin, n);
        if !temp && !is_index_of(origin, n) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if !(ft.is_file() || ft.is_symlink()) {
            continue;
        }
        if temp {
            // Not followed: the age of the entry itself.
            let age = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok());
            if !age.is_some_and(|a| a >= STALE_TEMP) {
                continue;
            }
        }
        if let Err(e) = fs::remove_file(entry.path()) {
            log::warn!("can't remove the old index file {n}: {e}");
        }
    }
}

// ---- decoding ----

struct Dec<'a> {
    b: &'a [u8],
    /// What may still be decoded, see [`MAX_DECODED`].
    budget: usize,
}

/// Whether `s` has none of the characters the parser drops: controls (other
/// than whitespace), bidi embeddings, overrides and isolates, BOM and
/// noncharacters.
fn clean_text(s: &str) -> bool {
    if s.is_ascii() {
        return s
            .bytes()
            .all(|b| b != 0x7f && (b >= 0x20 || (9..=13).contains(&b)));
    }
    s.chars().all(|c| text::class(c) != text::Class::Drop)
}

/// A verification website: the parser keeps what the remote says, which on
/// Flathub is a bare host name (`example.org`); a full URL is fine too.
fn valid_website(s: &str) -> bool {
    s.is_empty()
        || text::valid_url(s, false)
        || (!s.contains(['/', '?', '#', '@', ':', '\\'])
            && text::valid_url(&format!("https://{s}"), true))
}

impl<'a> Dec<'a> {
    fn new(b: &'a [u8]) -> Dec<'a> {
        Dec {
            b,
            budget: MAX_DECODED,
        }
    }
    fn charge(&mut self, n: usize) -> Result<(), IndexError> {
        self.budget = self
            .budget
            .checked_sub(n)
            .ok_or(IndexError::Corrupt("more data than any catalog holds"))?;
        Ok(())
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], IndexError> {
        if n > self.b.len() {
            return Err(IndexError::Damaged("cut short"));
        }
        let (a, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(a)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], IndexError> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, IndexError> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, IndexError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32, IndexError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> Result<u64, IndexError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn i64(&mut self) -> Result<i64, IndexError> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    fn flag(&mut self) -> Result<bool, IndexError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(IndexError::Corrupt("flag")),
        }
    }
    /// A count of items that each take at least one byte: it can't be more
    /// than `max` or than the bytes left.
    fn count(&mut self, max: usize) -> Result<usize, IndexError> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(IndexError::Corrupt("count over the cap"));
        }
        if n > self.b.len() {
            return Err(IndexError::Damaged("count over the bytes left"));
        }
        self.charge(n.saturating_mul(16))?;
        Ok(n)
    }
    fn str(&mut self) -> Result<String, IndexError> {
        let n = self.u32()? as usize;
        if n > MAX_STR {
            return Err(IndexError::Corrupt("string over the cap"));
        }
        self.charge(n + 24)?;
        let b = self.take(n)?;
        let s = String::from_utf8(b.to_vec()).map_err(|_| IndexError::Corrupt("not UTF-8"))?;
        if !clean_text(&s) {
            return Err(IndexError::Corrupt("control or bidi character"));
        }
        Ok(s)
    }
    /// A string the parser would have produced for a text field: one line,
    /// cleaned, of at most `max` characters.
    fn line(&mut self, max: usize) -> Result<String, IndexError> {
        let s = self.str()?;
        if s.len() > max.saturating_mul(4) || text::clean(&s, max) != s {
            return Err(IndexError::Corrupt("text the parser would not keep"));
        }
        Ok(s)
    }
    fn opt_str(&mut self) -> Result<Option<String>, IndexError> {
        Ok(if self.flag()? {
            Some(self.str()?)
        } else {
            None
        })
    }
    fn strs(&mut self, max: usize) -> Result<Vec<String>, IndexError> {
        let n = self.count(max)?;
        (0..n).map(|_| self.str()).collect()
    }
    fn spans(&mut self) -> Result<Vec<Span>, IndexError> {
        let n = self.count(MAX_SPANS)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let style = match self.u8()? {
                0 => Style::Plain,
                1 => Style::Emphasis,
                2 => Style::Code,
                _ => return Err(IndexError::Corrupt("style")),
            };
            v.push(Span {
                text: self.str()?,
                style,
            });
        }
        Ok(v)
    }
    fn blocks(&mut self) -> Result<Vec<Block>, IndexError> {
        let n = self.count(MAX_BLOCKS)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(match self.u8()? {
                0 => Block::Paragraph(self.spans()?),
                t @ (1 | 2) => {
                    let m = self.count(MAX_ITEMS)?;
                    let items = (0..m).map(|_| self.spans()).collect::<Result<_, _>>()?;
                    Block::List {
                        ordered: t == 2,
                        items,
                    }
                }
                _ => return Err(IndexError::Corrupt("block")),
            });
        }
        Ok(v)
    }

    fn component(&mut self) -> Result<Component, IndexError> {
        self.charge(512)?;
        let id = self.str()?;
        if !text::valid_id(&id) {
            return Err(IndexError::Corrupt("component id"));
        }
        let kind = match self.u8()? {
            0 => Kind::DesktopApp,
            1 => Kind::ConsoleApp,
            2 => Kind::Addon,
            3 => Kind::Runtime,
            4 => Kind::Other,
            _ => return Err(IndexError::Corrupt("kind")),
        };
        let lim = Limits::default();
        let name = self.line(lim.name)?;
        if name.is_empty() {
            return Err(IndexError::Corrupt("empty name"));
        }
        let summary = self.line(lim.summary)?;
        let description = self.blocks()?;
        let developer = self.line(lim.developer)?;
        let license = self.str()?;
        let categories = self.strs(MAX_LIST)?;
        let n = self.count(lim.keywords)?;
        let keywords = (0..n)
            .map(|_| self.line(lim.keyword))
            .collect::<Result<Vec<_>, _>>()?;
        let icon = if self.flag()? {
            let file = self.str()?;
            if !text::valid_icon_file(&file) {
                return Err(IndexError::Corrupt("icon file"));
            }
            let n = self.count(MAX_LIST)?;
            let sizes = (0..n).map(|_| self.u16()).collect::<Result<_, _>>()?;
            Some(Icon { file, sizes })
        } else {
            None
        };
        let n = self.count(MAX_LIST)?;
        let mut urls = Vec::with_capacity(n);
        for _ in 0..n {
            let k = url_kind_from(self.u8()?)?;
            let u = self.str()?;
            if !text::valid_url(&u, false) {
                return Err(IndexError::Corrupt("url"));
            }
            urls.push((k, u));
        }
        let n = self.count(MAX_LIST)?;
        let mut screenshots = Vec::with_capacity(n);
        for _ in 0..n {
            let default = self.flag()?;
            let caption = self.str()?;
            let m = self.count(MAX_LIST)?;
            let mut images = Vec::with_capacity(m);
            for _ in 0..m {
                let thumbnail = self.flag()?;
                let width = self.u32()?;
                let height = self.u32()?;
                let url = self.str()?;
                if !text::valid_url(&url, true) {
                    return Err(IndexError::Corrupt("image url"));
                }
                images.push(Image {
                    thumbnail,
                    width,
                    height,
                    url,
                });
            }
            screenshots.push(Screenshot {
                default,
                caption,
                images,
            });
        }
        let n = self.count(MAX_LIST)?;
        let mut releases = Vec::with_capacity(n);
        for _ in 0..n {
            let version = self.str()?;
            let timestamp = self.i64()?;
            let kind = match self.u8()? {
                0 => ReleaseKind::Stable,
                1 => ReleaseKind::Development,
                2 => ReleaseKind::Snapshot,
                3 => ReleaseKind::Other,
                _ => return Err(IndexError::Corrupt("release kind")),
            };
            releases.push(Release {
                version,
                timestamp,
                kind,
                description: self.blocks()?,
            });
        }
        let content_rating = if self.flag()? {
            let scheme = match self.u8()? {
                0 => RatingScheme::Oars10,
                1 => RatingScheme::Oars11,
                2 => RatingScheme::Other,
                _ => return Err(IndexError::Corrupt("rating scheme")),
            };
            let n = self.count(MAX_LIST)?;
            let mut attrs = Vec::with_capacity(n);
            for _ in 0..n {
                let id = self.str()?;
                let i = match self.u8()? {
                    0 => Intensity::None,
                    1 => Intensity::Mild,
                    2 => Intensity::Moderate,
                    3 => Intensity::Intense,
                    _ => return Err(IndexError::Corrupt("intensity")),
                };
                attrs.push((id, i));
            }
            Some(ContentRating { scheme, attrs })
        } else {
            None
        };
        let bundle = if self.flag()? {
            let reference = self.str()?;
            if !text::valid_bundle_ref(&reference) {
                return Err(IndexError::Corrupt("bundle reference"));
            }
            let runtime = self.opt_str()?;
            let sdk = self.opt_str()?;
            for t in [&runtime, &sdk].into_iter().flatten() {
                if !text::valid_flatpak_target(t) {
                    return Err(IndexError::Corrupt("runtime or sdk"));
                }
            }
            if !bundle_matches(&id, &reference) {
                return Err(IndexError::Corrupt("bundle of another component"));
            }
            Some(Bundle {
                reference,
                runtime,
                sdk,
            })
        } else {
            None
        };
        if bundle.is_none() && needs_bundle(kind) {
            return Err(IndexError::Corrupt("missing bundle"));
        }
        let extends = self.strs(MAX_LIST)?;
        if !extends.iter().all(|e| text::valid_id(e)) {
            return Err(IndexError::Corrupt("extends id"));
        }
        let launchable = self.opt_str()?;
        if launchable.as_deref().is_some_and(|l| !text::valid_id(l)) {
            return Err(IndexError::Corrupt("launchable id"));
        }
        let verification = if self.flag()? {
            let method = self.str()?;
            let website = self.str()?;
            if !valid_website(&website) {
                return Err(IndexError::Corrupt("verification website"));
            }
            Some(Verification {
                method,
                website,
                login_name: self.str()?,
                login_provider: self.str()?,
                organization: self.flag()?,
                timestamp: self.i64()?,
            })
        } else {
            None
        };
        let branding = if self.flag()? {
            let one = |d: &mut Dec<'a>| -> Result<Option<[u8; 3]>, IndexError> {
                Ok(if d.flag()? { Some(d.arr::<3>()?) } else { None })
            };
            let light = one(self)?;
            let dark = one(self)?;
            Some(Branding { light, dark })
        } else {
            None
        };
        Ok(Component {
            id,
            kind,
            name,
            summary,
            description,
            developer,
            license,
            categories,
            keywords,
            icon,
            urls,
            screenshots,
            releases,
            content_rating,
            bundle,
            extends,
            launchable,
            verification,
            branding,
        })
    }
}

/// Reads the index at `path` if it was built for `key`. The directory must be
/// the user's own and not a link; the file is opened without following a symlink, must
/// be a regular file of at most 32 MiB and must pass every check in the
/// module description.
pub fn read(path: &Path, key: &IndexKey) -> Result<Catalog, IndexError> {
    key.check()?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty() && *p != Path::new("."))
        .ok_or_else(|| IndexError::Io(format!("{} has no cache directory", path.display())))?;
    check_dir(dir).map_err(|e| IndexError::Io(e.to_string()))?;
    let io_err = |e: io::Error| IndexError::Io(format!("{}: {e}", path.display()));
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(io_err)?;
    let meta = file.metadata().map_err(io_err)?;
    if !meta.is_file() {
        return Err(IndexError::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > MAX_FILE {
        return Err(IndexError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(io_err)?;
    decode(&bytes, key)
}

fn decode(bytes: &[u8], key: &IndexKey) -> Result<Catalog, IndexError> {
    decode_with(bytes, key, MAX_DECODED)
}

fn decode_with(bytes: &[u8], key: &IndexKey, budget: usize) -> Result<Catalog, IndexError> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(IndexError::TooLarge);
    }
    let mut d = Dec::new(bytes);
    if d.take(8).map_err(|_| IndexError::BadHeader("too short"))? != MAGIC {
        return Err(IndexError::BadHeader("magic"));
    }
    if d.u32()? != key.format || key.format != FORMAT {
        return Err(IndexError::BadHeader("format version"));
    }
    let short = |d: &mut Dec<'_>| -> Result<String, IndexError> {
        let n = d.u16()? as usize;
        String::from_utf8(d.take(n)?.to_vec()).map_err(|_| IndexError::Corrupt("key"))
    };
    let origin = short(&mut d)?;
    let commit = short(&mut d)?;
    let n = d.u16()? as usize;
    if n > 32 {
        return Err(IndexError::KeyMismatch);
    }
    let langs = (0..n)
        .map(|_| short(&mut d))
        .collect::<Result<Vec<_>, _>>()?;
    if origin != key.origin || commit != key.commit || langs != key.langs {
        return Err(IndexError::KeyMismatch);
    }
    let payload_len = d.u64()?;
    let header_len = bytes.len() - d.b.len();
    let want = d.u64()?;
    if payload_len != d.b.len() as u64 {
        return Err(IndexError::Damaged("length"));
    }
    let mut sum = Sum::new();
    sum.update(&bytes[..header_len]);
    sum.update(d.b);
    if sum.0 != want {
        return Err(IndexError::Damaged("checksum"));
    }

    let mut d = Dec::new(d.b);
    d.budget = budget;
    let cat_origin = d.str()?;
    if cat_origin != key.origin {
        return Err(IndexError::KeyMismatch);
    }
    let skipped = d.u32()?;
    let n = d.count(MAX_COMPONENTS)?;
    let mut components = Vec::with_capacity(n);
    let mut seen = HashSet::with_capacity(n);
    for _ in 0..n {
        let c = d.component()?;
        if !seen.insert(c.id.clone()) {
            return Err(IndexError::Corrupt("duplicate component id"));
        }
        components.push(c);
    }
    if !d.b.is_empty() {
        return Err(IndexError::Damaged("trailing bytes"));
    }
    Ok(Catalog {
        origin: cat_origin,
        components,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstream::{ParseOptions, parse};

    fn key() -> IndexKey {
        IndexKey {
            origin: "flathub".into(),
            commit: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            langs: vec!["pl".into()],
            format: FORMAT,
        }
    }

    fn empty() -> Catalog {
        Catalog {
            origin: "flathub".into(),
            ..Catalog::default()
        }
    }

    fn catalog() -> Catalog {
        let xml = include_str!("../../tests/fixtures/flathub-sample.xml");
        let o = ParseOptions {
            origin: "flathub".into(),
            langs: vec!["pl".into()],
            ..ParseOptions::default()
        };
        parse(xml.as_bytes(), &o).expect("the sample parses")
    }

    /// Recomputes the checksum after `bytes` was edited, so the decoder's own
    /// bounds checks are what gets exercised.
    fn refix(bytes: &mut [u8], key: &IndexKey) {
        let head = encode(key, &empty());
        // The header is the same length for the same key: payload_len and
        // checksum are the last 16 bytes of it.
        let header_len = head.len() - payload_of(&head).len();
        let payload_len = (bytes.len() - header_len) as u64;
        bytes[header_len - 16..header_len - 8].copy_from_slice(&payload_len.to_le_bytes());
        let mut sum = Sum::new();
        sum.update(&bytes[..header_len - 8]);
        sum.update(&bytes[header_len..]);
        bytes[header_len - 8..header_len].copy_from_slice(&sum.0.to_le_bytes());
    }

    fn payload_of(b: &[u8]) -> &[u8] {
        // For the empty catalog: the origin string (4 + 7), skipped (4), count (4).
        &b[b.len() - 19..]
    }

    /// Decodes the sample catalog after `edit`, which must break one rule.
    fn bad(edit: impl FnOnce(&mut Catalog)) -> IndexError {
        let mut cat = catalog();
        edit(&mut cat);
        decode(&encode(&key(), &cat), &key()).expect_err("the edit must be refused")
    }

    fn first_with_bundle(cat: &mut Catalog) -> &mut Component {
        cat.components
            .iter_mut()
            .find(|c| c.bundle.is_some() && c.kind == Kind::DesktopApp)
            .expect("the sample has an app")
    }

    #[test]
    fn decoder_repeats_the_parsers_component_checks() {
        let c = |e: IndexError| matches!(e, IndexError::Corrupt(_));
        assert!(c(bad(|k| {
            first_with_bundle(k).bundle.as_mut().unwrap().runtime = Some("not a target".into());
        })));
        assert!(c(bad(|k| {
            first_with_bundle(k).bundle.as_mut().unwrap().sdk = Some("a.b/x86_64".into());
        })));
        assert!(c(bad(|k| {
            first_with_bundle(k).bundle.as_mut().unwrap().reference =
                "app/x.evil/x86_64/stable".into();
        })));
        assert!(c(bad(|k| {
            let m = k
                .components
                .iter_mut()
                .find(|c| c.kind == Kind::Runtime && c.bundle.is_some());
            m.expect("the sample has a runtime")
                .bundle
                .as_mut()
                .unwrap()
                .reference = "runtime/x.evil/x86_64/stable".into();
        })));
        assert!(c(bad(|k| first_with_bundle(k).bundle = None)));
        assert!(c(bad(|k| {
            let d = k.components[0].clone();
            k.components.push(d);
        })));
        assert!(c(bad(|k| k.components[0].name.clear())));
    }

    #[test]
    fn decoder_repeats_the_parsers_text_rules() {
        let c = |e: IndexError| matches!(e, IndexError::Corrupt(_));
        let long = "a".repeat(Limits::default().name + 1);
        assert!(c(bad(|k| k.components[0].name = long.clone())));
        assert!(c(bad(|k| k.components[0].summary = "a".repeat(401))));
        assert!(c(bad(|k| k.components[0].developer = "a".repeat(201))));
        assert!(c(bad(|k| k.components[0].keywords = vec!["a".repeat(65)])));
        assert!(c(bad(|k| k.components[0].name = "two  spaces".into())));
        assert!(c(bad(|k| k.components[0].summary = "line\nbreak".into())));
        assert!(c(bad(|k| k.components[0].developer = " padded ".into())));
    }

    #[test]
    fn roundtrip_in_memory() {
        let cat = catalog();
        let bytes = encode(&key(), &cat);
        assert_eq!(decode(&bytes, &key()).unwrap(), cat);
        assert_eq!(decode(&encode(&key(), &empty()), &key()).unwrap(), empty());
    }

    #[test]
    fn every_flipped_byte_and_every_cut_is_an_error() {
        let bytes = encode(&key(), &catalog());
        for i in 0..bytes.len() {
            let mut b = bytes.clone();
            b[i] ^= 0x01;
            assert!(decode(&b, &key()).is_err(), "flip at {i}");
        }
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n], &key()).is_err(), "cut at {n}");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer, &key()).is_err());
    }

    #[test]
    fn decoder_survives_edits_with_a_valid_checksum() {
        let k = key();
        let bytes = encode(&k, &catalog());
        let header_len = bytes.len() - {
            let mut d = Dec::new(&bytes);
            d.take(8).unwrap();
            d.u32().unwrap();
            for _ in 0..2 {
                let n = d.u16().unwrap() as usize;
                d.take(n).unwrap();
            }
            let n = d.u16().unwrap();
            for _ in 0..n {
                let n = d.u16().unwrap() as usize;
                d.take(n).unwrap();
            }
            d.take(16).unwrap();
            d.b.len()
        };
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut ok, mut err) = (0, 0);
        for _ in 0..6000 {
            let mut b = bytes.clone();
            for _ in 0..1 + next() % 4 {
                let at = header_len + (next() as usize) % (b.len() - header_len);
                b[at] = match next() % 3 {
                    0 => 0xFF,
                    1 => 0,
                    _ => next() as u8,
                };
            }
            if next() % 4 == 0 {
                let cut = header_len + (next() as usize) % (b.len() - header_len);
                b.truncate(cut);
            }
            refix(&mut b, &k);
            match decode(&b, &k) {
                Ok(_) => ok += 1,
                Err(_) => err += 1,
            }
        }
        assert!(err > 1000, "ok {ok}, err {err}");
    }

    #[test]
    fn huge_counts_and_lengths_are_refused_without_allocating() {
        let k = key();
        let mut bytes = encode(&k, &empty());
        // The component count is the last four bytes.
        let n = bytes.len();
        bytes[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        refix(&mut bytes, &k);
        assert!(matches!(decode(&bytes, &k), Err(IndexError::Corrupt(_))));
        bytes[n - 4..].copy_from_slice(&90_000u32.to_le_bytes());
        refix(&mut bytes, &k);
        assert!(matches!(decode(&bytes, &k), Err(IndexError::Damaged(_))));
        // A string longer than the bytes left.
        let mut bytes = encode(&k, &empty());
        let hl = bytes.len() - 19;
        bytes[hl..hl + 4].copy_from_slice(&60_000u32.to_le_bytes());
        refix(&mut bytes, &k);
        assert!(matches!(decode(&bytes, &k), Err(IndexError::Damaged(_))));
        bytes[hl..hl + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        refix(&mut bytes, &k);
        assert!(matches!(decode(&bytes, &k), Err(IndexError::Corrupt(_))));
    }

    #[test]
    fn index_file_name_matching() {
        let n = "index-foo-0123456789abcdef-0a1b2c3d.bin";
        assert!(is_index_of("foo", n));
        assert!(!is_index_of("fo", n));
        assert!(!is_index_of("foo-bar", n));
        assert!(!is_index_of(
            "foo",
            "index-foo-bar-0123456789abcdef-0a1b2c3d.bin"
        ));
        assert!(!is_index_of(
            "foo",
            "index-foo-0123456789abcdef-0a1b2c3d.bin.tmp"
        ));
        assert!(!is_index_of("foo", "notes.txt"));
    }

    /// The first component of the sample with `edit` applied, encoded.
    fn hostile(edit: impl FnOnce(&mut Component)) -> Vec<u8> {
        let mut cat = catalog();
        cat.components.truncate(1);
        edit(&mut cat.components[0]);
        encode(&key(), &cat)
    }

    fn verification(website: &str, method: &str) -> Verification {
        Verification {
            method: method.into(),
            website: website.into(),
            login_name: String::new(),
            login_provider: String::new(),
            organization: false,
            timestamp: 0,
        }
    }

    #[test]
    fn the_payload_origin_must_be_the_keys() {
        let mut cat = catalog();
        cat.origin = "fedora".into();
        let bytes = encode(&key(), &cat);
        assert_eq!(decode(&bytes, &key()), Err(IndexError::KeyMismatch));
    }

    #[test]
    fn hostile_text_in_a_valid_file_is_corrupt() {
        let bad = "a\u{202e}b";
        let ctl = "a\u{1}b";
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("name", hostile(|c| c.name = bad.into())),
            ("summary", hostile(|c| c.summary = ctl.into())),
            ("developer", hostile(|c| c.developer = "x\u{2066}".into())),
            ("license", hostile(|c| c.license = "\u{feff}".into())),
            ("keyword", hostile(|c| c.keywords = vec![ctl.into()])),
            (
                "span",
                hostile(|c| {
                    c.description = vec![Block::Paragraph(vec![Span {
                        text: bad.into(),
                        style: Style::Plain,
                    }])]
                }),
            ),
            (
                "caption",
                hostile(|c| {
                    c.screenshots = vec![Screenshot {
                        default: true,
                        caption: ctl.into(),
                        images: vec![],
                    }]
                }),
            ),
            (
                "version",
                hostile(|c| {
                    c.releases = vec![Release {
                        version: bad.into(),
                        timestamp: 0,
                        kind: ReleaseKind::Stable,
                        description: vec![],
                    }]
                }),
            ),
            (
                "rating id",
                hostile(|c| {
                    c.content_rating = Some(ContentRating {
                        scheme: RatingScheme::Oars11,
                        attrs: vec![(ctl.into(), Intensity::Mild)],
                    })
                }),
            ),
            (
                "method",
                hostile(|c| c.verification = Some(verification("example.org", bad))),
            ),
            (
                "website",
                hostile(|c| c.verification = Some(verification("javascript:alert(1)", ""))),
            ),
            (
                "website with a path",
                hostile(|c| c.verification = Some(verification("a.org/x y", ""))),
            ),
            ("extends", hostile(|c| c.extends = vec!["not an id".into()])),
            (
                "launchable",
                hostile(|c| c.launchable = Some("../x".into())),
            ),
        ];
        for (what, bytes) in cases {
            assert!(
                matches!(decode(&bytes, &key()), Err(IndexError::Corrupt(_))),
                "{what}"
            );
        }
        // The good forms pass.
        let ok = hostile(|c| {
            c.verification = Some(verification("example.org", "website"));
            c.extends = vec!["org.example.App".into()];
            c.launchable = Some("org.example.App.desktop".into());
        });
        assert!(decode(&ok, &key()).is_ok());
        let ok = hostile(|c| c.verification = Some(verification("https://example.org/x", "")));
        assert!(decode(&ok, &key()).is_ok());
    }

    #[test]
    fn decoding_stops_at_the_budget() {
        let bytes = encode(&key(), &catalog());
        assert!(decode_with(&bytes, &key(), MAX_DECODED).is_ok());
        assert!(matches!(
            decode_with(&bytes, &key(), 4096),
            Err(IndexError::Corrupt(_))
        ));
        // Many tiny strings cost more than their bytes: 4 bytes on disk, 24 charged.
        let mut cat = empty();
        let mut c = catalog().components.swap_remove(0);
        c.keywords = vec![String::new(); MAX_LIST];
        c.categories = vec![String::new(); MAX_LIST];
        cat.components = (0..200)
            .map(|i| {
                let mut c = c.clone();
                c.id = format!("org.example.App{i}");
                if let Some(b) = c.bundle.as_mut() {
                    b.reference = format!("app/{}/x86_64/stable", c.id);
                }
                c
            })
            .collect();
        let bytes = encode(&key(), &cat);
        // Each component costs at least 5120: 128 strings at 24 and 128 list items at 16.
        assert!(decode_with(&bytes, &key(), MAX_DECODED).is_ok());
        assert!(decode_with(&bytes, &key(), 200 * 5000).is_err());
    }
}
