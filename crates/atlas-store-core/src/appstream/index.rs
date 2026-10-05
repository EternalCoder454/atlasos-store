//! The on-disk index: a parsed catalog in a compact binary file under the
//! cache directory, so the Store starts without parsing 50 MB of XML.
//!
//! The file is a cache, and it is read as untrusted: it is opened without
//! following symlinks, its size and checksum are checked, every length is
//! checked against the bytes that are left, and every string must be UTF-8.
//! Any mismatch is an [`IndexError`] and the caller rebuilds from the XML.
//!
//! Layout, little-endian: the magic `ATLASIDX`, the format version, the key
//! (origin, commit, languages), the payload length, a checksum of everything
//! before it and of the payload, then the payload.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{
    Block, Branding, Bundle, Catalog, Component, ContentRating, Icon, Image, Intensity, Kind,
    RatingScheme, Release, ReleaseKind, Screenshot, Span, Style, UrlKind, Verification,
};
use crate::text;

/// The layout version this code writes and reads.
pub const FORMAT: u32 = 1;

const MAGIC: &[u8; 8] = b"ATLASIDX";
/// Largest index file read, and largest written.
const MAX_FILE: u64 = 64 << 20;

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

/// Writes the index atomically: a temp file in the same directory (created
/// exclusively, without following links, mode 0600), fsynced, renamed over
/// `path`, then the directory fsynced. The directory is created with mode
/// 0700. Afterwards older index files of the same origin are removed. A
/// failure leaves no temp file and the previous index as it was.
pub fn write(path: &Path, key: &IndexKey, catalog: &Catalog) -> io::Result<()> {
    key.check().map_err(|e| invalid(e.to_string()))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("the index path has no file name"))?;
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let bytes = encode(key, catalog);
    if bytes.len() as u64 > MAX_FILE {
        return Err(invalid("the index would be larger than the cap"));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;

    let tmp = dir.join(format!(
        ".{}.tmp.{}.{}",
        name.to_string_lossy(),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    let mut guard = TempGuard {
        path: &tmp,
        armed: true,
    };
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)?;
    guard.armed = false;
    File::open(dir)?.sync_all()?;

    remove_older(dir, &key.origin, name);
    Ok(())
}

/// Best-effort removal of the other index files of `origin`. Symlinks are
/// removed as links, never followed.
fn remove_older(dir: &Path, origin: &str, keep: &std::ffi::OsStr) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("can't list {} to remove old indexes: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        if fname == keep {
            continue;
        }
        let Some(n) = fname.to_str() else { continue };
        if !is_index_of(origin, n) {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if (ft.is_file() || ft.is_symlink())
            && let Err(e) = fs::remove_file(entry.path())
        {
            log::warn!("can't remove the old index {n}: {e}");
        }
    }
}

// ---- decoding ----

struct Dec<'a> {
    b: &'a [u8],
}

impl<'a> Dec<'a> {
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
        Ok(n)
    }
    fn str(&mut self) -> Result<String, IndexError> {
        let n = self.u32()? as usize;
        if n > MAX_STR {
            return Err(IndexError::Corrupt("string over the cap"));
        }
        let b = self.take(n)?;
        String::from_utf8(b.to_vec()).map_err(|_| IndexError::Corrupt("not UTF-8"))
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
        let name = self.str()?;
        let summary = self.str()?;
        let description = self.blocks()?;
        let developer = self.str()?;
        let license = self.str()?;
        let categories = self.strs(MAX_LIST)?;
        let keywords = self.strs(MAX_LIST)?;
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
            Some(Bundle {
                reference,
                runtime: self.opt_str()?,
                sdk: self.opt_str()?,
            })
        } else {
            None
        };
        let extends = self.strs(MAX_LIST)?;
        let launchable = self.opt_str()?;
        let verification = if self.flag()? {
            Some(Verification {
                method: self.str()?,
                website: self.str()?,
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

/// Reads the index at `path` if it was built for `key`. The file is opened
/// without following a symlink, must be a regular file of at most 64 MiB and
/// must pass every check in the module description.
pub fn read(path: &Path, key: &IndexKey) -> Result<Catalog, IndexError> {
    key.check()?;
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
    if bytes.len() as u64 > MAX_FILE {
        return Err(IndexError::TooLarge);
    }
    let mut d = Dec { b: bytes };
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

    let mut d = Dec { b: d.b };
    let cat_origin = d.str()?;
    let skipped = d.u32()?;
    let n = d.count(MAX_COMPONENTS)?;
    let mut components = Vec::with_capacity(n);
    for _ in 0..n {
        components.push(d.component()?);
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
        let head = encode(key, &Catalog::default());
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
        // For the empty catalog: the origin string (4 + 0), skipped (4), count (4).
        &b[b.len() - 12..]
    }

    #[test]
    fn roundtrip_in_memory() {
        let cat = catalog();
        let bytes = encode(&key(), &cat);
        assert_eq!(decode(&bytes, &key()).unwrap(), cat);
        assert_eq!(
            decode(&encode(&key(), &Catalog::default()), &key()).unwrap(),
            Catalog::default()
        );
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
            let mut d = Dec { b: &bytes };
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
        let mut bytes = encode(&k, &Catalog::default());
        // The component count is the last four bytes.
        let n = bytes.len();
        bytes[n - 4..].copy_from_slice(&u32::MAX.to_le_bytes());
        refix(&mut bytes, &k);
        assert!(matches!(decode(&bytes, &k), Err(IndexError::Corrupt(_))));
        bytes[n - 4..].copy_from_slice(&90_000u32.to_le_bytes());
        refix(&mut bytes, &k);
        assert!(matches!(decode(&bytes, &k), Err(IndexError::Damaged(_))));
        // A string longer than the bytes left.
        let mut bytes = encode(&k, &Catalog::default());
        let hl = bytes.len() - 12;
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
}
