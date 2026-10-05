//! The AppStream parser: a streaming pass over the XML that keeps only what
//! the Store shows, in one language, within fixed caps. The input is untrusted
//! network data, so a DOCTYPE, any entity beyond the five predefined ones,
//! deep nesting, a huge download and a huge single node are all refused, and
//! what is merely too long is cut. Nothing here panics on input.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use super::index;
use super::lang::LangPrefs;
use super::{
    Block, Branding, Bundle, Catalog, Component, ContentRating, Icon, Image, Intensity, Kind,
    RatingScheme, Release, ReleaseKind, Screenshot, Span, Style, UrlKind, Verification,
};
use crate::text::{self, Class, LineBuf};

/// The caps. Text over a cap is cut at a character boundary and lists stop
/// growing; neither is an error. Sizes, depth and component count are errors,
/// because cutting them would silently drop data.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Largest compressed file, in bytes.
    pub max_compressed: u64,
    /// Largest XML after decompression, in bytes.
    pub max_decompressed: u64,
    /// Longest single node, in bytes: one text node, comment, CDATA section or
    /// processing instruction. Counted by a small scanner over the raw bytes,
    /// so a `<` inside a comment or a quoted value doesn't restart the count.
    pub max_token: usize,
    /// Longest start or empty-element tag, in bytes, attributes included.
    pub max_tag: usize,
    /// Most attributes on one element.
    pub max_attrs: usize,
    /// Bytes of text kept in all components of a catalog.
    pub max_retained_bytes: usize,
    /// Strings, spans, list items and other objects kept in all components.
    pub max_retained_objects: usize,
    /// Deepest element nesting, counting the root.
    pub max_depth: usize,
    /// Most `<component>` elements.
    pub max_components: usize,
    pub name: usize,
    pub summary: usize,
    pub caption: usize,
    pub developer: usize,
    pub license: usize,
    pub version: usize,
    /// Longest keyword, in characters.
    pub keyword: usize,
    pub keywords: usize,
    /// Longest category, in characters.
    pub category: usize,
    pub categories: usize,
    pub desc_blocks: usize,
    /// Characters in one paragraph or list item.
    pub desc_para: usize,
    pub desc_items: usize,
    /// Bytes of description text in all.
    pub desc_total: usize,
    /// Bytes of text in one release description.
    pub release_desc_total: usize,
    pub urls: usize,
    pub screenshots: usize,
    pub images: usize,
    /// Longest attribute value, in bytes; longer ones are ignored.
    pub attr: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_compressed: 64 << 20,
            max_decompressed: MAX_DECOMPRESSED,
            max_token: 4 << 20,
            max_tag: 64 << 10,
            max_attrs: 32,
            max_retained_bytes: MAX_RETAINED_BYTES,
            max_retained_objects: MAX_RETAINED_OBJECTS,
            max_depth: 32,
            max_components: MAX_COMPONENTS,
            name: 200,
            summary: 400,
            caption: 300,
            developer: 200,
            license: 300,
            version: 100,
            keyword: 64,
            keywords: 64,
            category: 64,
            categories: 16,
            desc_blocks: 64,
            desc_para: 4096,
            desc_items: 256,
            desc_total: 32 << 10,
            release_desc_total: 8 << 10,
            urls: 16,
            screenshots: 16,
            images: 8,
            attr: 2048,
        }
    }
}

/// The real Flathub catalog is 49.9 MB decompressed (2026-10); about 3x that.
const MAX_DECOMPRESSED: u64 = 150_000_000;
/// Retained text and objects of the real Flathub catalog (11.6 MB of text in
/// 234,000 objects, 2026-10), with about 4x headroom.
const MAX_RETAINED_BYTES: usize = 48_000_000;
const MAX_RETAINED_OBJECTS: usize = 1_000_000;
const MAX_COMPONENTS: usize = 100_000;

/// The revision of what the parser produces. Bump it whenever a change makes
/// the parser return something different for the same XML (a fix, a new rule,
/// a changed cap): the on-disk index stores it, so a cache built by an older
/// parser is rebuilt instead of served. A change to the shape of
/// [`Catalog`] or [`Component`] bumps [`super::index::FORMAT`] instead.
///
/// 2: empty translations never win, icons keyed by file and width, exactly
/// one default screenshot, best duplicate kept and counted apart, block
/// elements inside inline content separated, unknown inline elements keep the
/// style, `xml:lang="C"` is untagged, one content rating.
///
/// 3: the duplicate that wins is the one with a bundle, then the newest
/// release, then the newest branch (numbers compared as numbers, one too
/// long for 64 bits as the largest), then the most fields.
pub const PARSER_REV: u32 = 3;

/// Most spans in one paragraph or list item; text past it is cut.
pub(crate) const MAX_SPANS: usize = 256;
/// How many releases a component keeps.
pub(crate) const RELEASES: usize = 10;
/// Most sizes kept of an icon.
pub(crate) const MAX_ICON_SIZES: usize = 16;
/// Most distinct icon files read before the best is chosen.
const MAX_ICONS: usize = 8;
/// Most `<extends>` kept.
pub(crate) const MAX_EXTENDS: usize = 16;
/// Most attributes of a content rating kept.
pub(crate) const MAX_RATING_ATTRS: usize = 64;
/// Longest component ID, icon file, launchable or extends, in characters.
const MAX_ID_CHARS: usize = 300;
/// Longest URL, in characters.
const MAX_URL_CHARS: usize = 2048;
/// Longest bundle reference, in characters.
const MAX_BUNDLE_CHARS: usize = 600;
/// Longest verification value, in characters.
const MAX_CUSTOM_CHARS: usize = 256;

impl Limits {
    /// The longest string the parser can keep, in bytes (4 per character at
    /// most). The index decoder's string cap, so it can't drift from the
    /// parser's.
    pub(crate) fn max_string_bytes(&self) -> usize {
        [
            self.name,
            self.summary,
            self.caption,
            self.developer,
            self.license,
            self.version,
            self.keyword,
            self.category,
            self.desc_para,
            MAX_ID_CHARS,
            MAX_URL_CHARS,
            MAX_BUNDLE_CHARS,
            MAX_CUSTOM_CHARS,
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
        .saturating_mul(4)
    }
}

/// What to parse for.
#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// The remote's name, copied to [`Catalog::origin`].
    pub origin: String,
    /// Languages in order of preference, as [`super::lang::langs_from_env`]
    /// gives them. Only the best match of each text is kept.
    pub langs: Vec<String>,
    pub limits: Limits,
}

/// Why a parse failed. The caller keeps its old index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The file could not be opened or read, or the gzip is damaged.
    Io(String),
    /// Not well-formed XML: a syntax error, a mismatched tag, a truncated
    /// file or an invalid character reference. `position` is a byte offset.
    Xml {
        position: u64,
        message: String,
        /// The ID of the component being read, when it was known.
        component: Option<String>,
    },
    /// A DOCTYPE is never accepted.
    DocType,
    /// An entity other than `&amp; &lt; &gt; &quot; &apos;`.
    Entity {
        name: String,
        /// The ID of the component being read, when it was known.
        component: Option<String>,
    },
    /// Elements nested deeper than [`Limits::max_depth`].
    TooDeep {
        /// The ID of the component being read, when it was known.
        component: Option<String>,
    },
    /// A size or count cap was passed; names which.
    Limit {
        what: &'static str,
        /// The ID of the component being read, when it was known.
        component: Option<String>,
    },
    /// Well-formed XML that is not an AppStream catalog.
    NotCatalog,
}

impl ParseError {
    /// A cap hit, with no component known yet ([`parse`] adds it).
    pub(crate) fn limit(what: &'static str) -> ParseError {
        ParseError::Limit {
            what,
            component: None,
        }
    }

    /// The component ID the error carries, if any.
    pub fn component(&self) -> Option<&str> {
        match self {
            ParseError::Xml { component, .. }
            | ParseError::Entity { component, .. }
            | ParseError::TooDeep { component }
            | ParseError::Limit { component, .. } => component.as_deref(),
            _ => None,
        }
    }

    /// Fills in the component of the variants that carry one, if still empty.
    fn in_component(mut self, id: Option<String>) -> ParseError {
        if let ParseError::Xml { component, .. }
        | ParseError::Entity { component, .. }
        | ParseError::TooDeep { component }
        | ParseError::Limit { component, .. } = &mut self
            && component.is_none()
        {
            *component = id;
        }
        self
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Io(e) => write!(f, "can't read the AppStream data: {e}")?,
            ParseError::Xml {
                position, message, ..
            } => {
                write!(f, "AppStream XML is damaged at byte {position}: {message}")?;
            }
            ParseError::DocType => write!(f, "AppStream XML has a DOCTYPE, which is refused")?,
            ParseError::Entity { name, .. } => {
                write!(
                    f,
                    "AppStream XML uses the entity &{name};, which is refused"
                )?;
            }
            ParseError::TooDeep { .. } => write!(f, "AppStream XML is nested too deeply")?,
            ParseError::Limit { what, .. } => write!(f, "AppStream data is too large: {what}")?,
            ParseError::NotCatalog => write!(f, "the file is not an AppStream catalog")?,
        }
        if let Some(id) = self.component() {
            write!(f, " (in component {id})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

/// A cap hit inside the reader; carried through `io::Error` and mapped back.
#[derive(Debug)]
struct LimitHit(&'static str);

impl fmt::Display for LimitHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} over the limit", self.0)
    }
}

impl std::error::Error for LimitHit {}

/// Where the raw-byte scanner is. Only the bytes that end a node matter, so
/// this does no XML checking: quick-xml does that on the same bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// Character data.
    Text,
    /// Just after a `<`, matching `!--` or `![CDATA[`; `buf[..n]` are the
    /// bytes seen so far.
    Open { buf: [u8; 8], n: u8 },
    /// Inside a start, end or other tag; `quote` is the open quote, or 0.
    Tag { quote: u8 },
    /// A comment, ended by `-->`; `dashes` is the run of `-` just seen.
    Comment { dashes: u8 },
    /// A CDATA section, ended by `]]>`; `brackets` is the run of `]` just seen.
    CData { brackets: u8 },
    /// A processing instruction or the XML declaration, ended by `?>`.
    Pi { question: bool },
}

/// How a tag byte moves the scanner: `>` outside quotes ends it.
fn tag_byte(quote: u8, b: u8) -> Scan {
    match (quote, b) {
        (0, b'>') => Scan::Text,
        (0, b'"' | b'\'') => Scan::Tag { quote: b },
        (q, c) if q != 0 && q == c => Scan::Tag { quote: 0 },
        (q, _) => Scan::Tag { quote: q },
    }
}

/// Counts what passes through and fails past the caps, so a gzip bomb or a
/// single endless text node, comment, CDATA section or tag stops early
/// instead of filling memory. Each kind of node is counted from its own
/// start, so a `<` inside a comment or a quoted value restarts nothing.
struct Guard<R> {
    inner: R,
    total: u64,
    max_total: u64,
    state: Scan,
    /// Bytes of the current node so far.
    run: usize,
    max_run: usize,
    max_tag: usize,
}

impl<R> Guard<R> {
    fn new(inner: R, lim: &Limits) -> Guard<R> {
        Guard {
            inner,
            total: 0,
            max_total: lim.max_decompressed,
            state: Scan::Text,
            run: 0,
            max_run: lim.max_token,
            max_tag: lim.max_tag.min(lim.max_token),
        }
    }

    /// Advances the scanner over one byte.
    fn step(&mut self, b: u8) -> Result<(), LimitHit> {
        self.run += 1;
        let (cap, what) = match self.state {
            Scan::Tag { .. } | Scan::Open { .. } => (self.max_tag, "one tag"),
            Scan::Comment { .. } => (self.max_run, "one comment"),
            Scan::CData { .. } => (self.max_run, "one CDATA section"),
            Scan::Pi { .. } => (self.max_run, "one processing instruction"),
            Scan::Text => (self.max_run, "one text node"),
        };
        if self.run > cap {
            return Err(LimitHit(what));
        }
        let next = match self.state {
            Scan::Text => {
                if b == b'<' {
                    self.run = 1;
                    Scan::Open { buf: [0; 8], n: 0 }
                } else {
                    Scan::Text
                }
            }
            Scan::Open { mut buf, n } => {
                if n == 0 && b == b'?' {
                    // quick-xml ends a PI at the first `>` after the `?` of
                    // `<?`, so `<?>` is complete (it then fails as malformed).
                    Scan::Pi { question: true }
                } else {
                    let i = usize::from(n);
                    buf[i] = b;
                    let seen = &buf[..=i];
                    if seen == b"!--" {
                        Scan::Comment { dashes: 0 }
                    } else if seen == b"![CDATA[" {
                        Scan::CData { brackets: 0 }
                    } else if b"!--".starts_with(seen) || b"![CDATA[".starts_with(seen) {
                        Scan::Open { buf, n: n + 1 }
                    } else if seen[0] == b'!' {
                        // quick-xml reads any `<!d` as a DOCTYPE (skipping
                        // `<>` pairs inside `[..]`) and any `<![` as CDATA, so
                        // this scanner cannot follow them. Real catalogs have
                        // no markup declarations and DOCTYPE is refused anyway.
                        return Err(LimitHit("markup declaration"));
                    } else {
                        // Not a comment or CDATA: an ordinary tag. The bytes
                        // matched so far hold no quote or `>`.
                        tag_byte(0, b)
                    }
                }
            }
            Scan::Tag { quote } => tag_byte(quote, b),
            Scan::Comment { dashes } => match b {
                b'>' if dashes >= 2 => Scan::Text,
                b'-' => Scan::Comment {
                    dashes: dashes.saturating_add(1),
                },
                _ => Scan::Comment { dashes: 0 },
            },
            Scan::CData { brackets } => match b {
                b'>' if brackets >= 2 => Scan::Text,
                b']' => Scan::CData {
                    brackets: brackets.saturating_add(1),
                },
                _ => Scan::CData { brackets: 0 },
            },
            Scan::Pi { question } => match b {
                b'>' if question => Scan::Text,
                _ => Scan::Pi {
                    question: b == b'?',
                },
            },
        };
        if next == Scan::Text && self.state != Scan::Text {
            self.run = 0;
        }
        self.state = next;
        Ok(())
    }
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.total = self.total.saturating_add(n as u64);
        if self.total > self.max_total {
            return Err(io::Error::other(LimitHit("decompressed size")));
        }
        let chunk = out.get(..n).unwrap_or(&[]);
        for &b in chunk {
            if self.state == Scan::Text && b != b'<' {
                // Fast path: plain text only counts.
                self.run += 1;
                if self.run > self.max_run {
                    return Err(io::Error::other(LimitHit("one text node")));
                }
                continue;
            }
            self.step(b).map_err(io::Error::other)?;
        }
        Ok(n)
    }
}

/// `O_NOFOLLOW` plus `O_NONBLOCK`, so a symlink is refused and a FIFO can't
/// block the open.
fn open_nofollow(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

/// Parses a gzip-compressed catalog from a file. The path is opened without
/// following a symlink, must be a regular file, and the compressed size is
/// checked before reading.
pub fn parse_gz_file(path: &Path, opts: &ParseOptions) -> Result<Catalog, ParseError> {
    let file =
        open_nofollow(path).map_err(|e| ParseError::Io(format!("{}: {e}", path.display())))?;
    let meta = file
        .metadata()
        .map_err(|e| ParseError::Io(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(ParseError::Io(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if meta.len() > opts.limits.max_compressed {
        return Err(ParseError::limit("compressed size"));
    }
    let raw = BufReader::with_capacity(64 << 10, file.take(opts.limits.max_compressed));
    parse(flate2::bufread::GzDecoder::new(raw), opts)
}

/// Parses an uncompressed catalog. An error names the component being read
/// when its ID was already known.
pub fn parse<R: Read>(reader: R, opts: &ParseOptions) -> Result<Catalog, ParseError> {
    let lim = &opts.limits;
    let guard = Guard::new(reader, lim);
    let mut rd = Reader::from_reader(BufReader::with_capacity(64 << 10, guard));
    let mut st = State::new(opts);
    match drive(&mut rd, &mut st, lim) {
        Ok(()) => {}
        Err(e) => {
            let id = st.cur_id();
            return Err(e.in_component(id));
        }
    }
    let id = st.cur_id();
    st.finish().map_err(|e| e.in_component(id))
}

fn drive<R: io::BufRead>(
    rd: &mut Reader<R>,
    st: &mut State<'_>,
    lim: &Limits,
) -> Result<(), ParseError> {
    let mut buf = Vec::with_capacity(8 << 10);
    loop {
        // The previous event's buffer: whatever the scanner let through, no
        // event may be larger than a node.
        if buf.len() > lim.max_token {
            return Err(ParseError::limit("one text node or tag"));
        }
        buf.clear();
        let ev = match rd.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => return Err(map_error(e, rd.error_position())),
        };
        st.pos = rd.buffer_position();
        match ev {
            Event::Eof => return Ok(()),
            Event::Start(e) => st.start(&e)?,
            Event::Empty(e) => {
                st.start(&e)?;
                st.end()?;
            }
            Event::End(_) => st.end()?,
            Event::Text(t) => st.text(&t),
            Event::CData(t) => st.text(&t),
            Event::GeneralRef(r) => st.reference(&r)?,
            Event::DocType(_) => return Err(ParseError::DocType),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
        }
    }
}

fn map_error(e: quick_xml::Error, position: u64) -> ParseError {
    if let quick_xml::Error::Io(io) = &e {
        if let Some(hit) = io.get_ref().and_then(|i| i.downcast_ref::<LimitHit>()) {
            return ParseError::limit(hit.0);
        }
        return ParseError::Io(io.to_string());
    }
    ParseError::Xml {
        position,
        message: e.to_string(),
        component: None,
    }
}

/// What an open element is, as far as the parser cares.
#[derive(Clone, Copy, PartialEq, Eq)]
enum El {
    Components,
    Component,
    /// An element whose text is collected into `State::leaf`.
    Leaf,
    Developer,
    Categories,
    Keywords,
    Screenshots,
    Screenshot,
    Releases,
    Release,
    Rating,
    Custom,
    Branding,
    Desc,
    /// `<p>`
    Block,
    /// `<ul>` or `<ol>`
    List,
    /// `<li>`
    Item,
    /// An inline element inside a paragraph or item.
    Inline,
    /// A `<p>`, `<ul>`, `<ol>` or `<li>` inside a paragraph or item: read as
    /// inline text, set apart by a space.
    InlineBlock,
    /// Unknown, or in a language not wanted: the whole subtree is ignored.
    Skip,
}

#[derive(Clone, Copy)]
enum Key {
    Verified,
    Method,
    Timestamp,
    Website,
    LoginName,
    LoginProvider,
    LoginIsOrg,
}

#[derive(Clone, Copy)]
enum Pref {
    Light,
    Dark,
    Both,
}

enum Target {
    Id,
    Name,
    Summary,
    License,
    DevName,
    DevLegacy,
    Category,
    Keyword,
    Caption,
    Extends,
    Launchable,
    Bundle {
        runtime: Option<String>,
        sdk: Option<String>,
    },
    Icon {
        width: u16,
    },
    Url(UrlKind),
    Image {
        thumbnail: bool,
        width: u32,
        height: u32,
    },
    Custom(Key),
    Rating(String),
    Color(Pref),
}

impl Target {
    /// Free text that is shown, as opposed to an ID, URL or file name.
    fn is_text(&self) -> bool {
        matches!(
            self,
            Target::Name
                | Target::Summary
                | Target::License
                | Target::DevName
                | Target::DevLegacy
                | Target::Category
                | Target::Keyword
                | Target::Caption
                | Target::Custom(_)
        )
    }
}

/// The text of the leaf element being read.
struct Leaf {
    target: Target,
    buf: LineBuf,
    rank: usize,
}

/// A localized string: the best rank seen so far.
struct Loc {
    rank: usize,
    s: String,
}

impl Loc {
    fn new() -> Loc {
        Loc {
            rank: usize::MAX,
            s: String::new(),
        }
    }
}

/// A localized list (keywords): one rank wins, its elements add up.
struct LocList {
    rank: usize,
    v: Vec<String>,
}

#[derive(Default)]
struct Verif {
    verified: bool,
    method: String,
    website: String,
    login_name: String,
    login_provider: String,
    organization: bool,
    timestamp: i64,
}

struct ShotB {
    default: bool,
    caption: Loc,
    images: Vec<Image>,
}

struct RelB {
    version: String,
    timestamp: i64,
    kind: ReleaseKind,
    desc: Option<(usize, Vec<Block>)>,
}

/// The component being read.
struct Cur {
    kind: Kind,
    id: String,
    name: Loc,
    summary: Loc,
    desc: Option<(usize, Vec<Block>)>,
    dev: Loc,
    dev_legacy: Loc,
    license: String,
    categories: Vec<String>,
    keywords: LocList,
    /// Every cached icon file with its sizes; the best one is kept.
    icons: Vec<Icon>,
    urls: Vec<(UrlKind, String)>,
    shots: Vec<Screenshot>,
    shot: Option<ShotB>,
    releases: Vec<Release>,
    rel: Option<RelB>,
    rating: Option<ContentRating>,
    bundle: Option<Bundle>,
    extends: Vec<String>,
    launchable: Option<String>,
    verif: Verif,
    light: Option<[u8; 3]>,
    dark: Option<[u8; 3]>,
}

impl Cur {
    fn new(kind: Kind) -> Cur {
        Cur {
            kind,
            id: String::new(),
            name: Loc::new(),
            summary: Loc::new(),
            desc: None,
            dev: Loc::new(),
            dev_legacy: Loc::new(),
            license: String::new(),
            categories: Vec::new(),
            keywords: LocList {
                rank: usize::MAX,
                v: Vec::new(),
            },
            icons: Vec::new(),
            urls: Vec::new(),
            shots: Vec::new(),
            shot: None,
            releases: Vec::new(),
            rel: None,
            rating: None,
            bundle: None,
            extends: Vec::new(),
            launchable: None,
            verif: Verif::default(),
            light: None,
            dark: None,
        }
    }
}

/// Builds the spans of one paragraph or list item, with the same cleaning as
/// [`LineBuf`], plus a style stack for `<em>` and `<code>`.
#[derive(Default)]
struct Inline {
    spans: Vec<Span>,
    cur: String,
    styles: Vec<Style>,
    pending: bool,
    chars: usize,
    bytes: usize,
    max_chars: usize,
    max_bytes: usize,
    over: bool,
}

impl Inline {
    fn reset(&mut self, max_chars: usize, max_bytes: usize) {
        *self = Inline {
            max_chars,
            max_bytes,
            ..Inline::default()
        };
    }

    fn style(&self) -> Style {
        self.styles.last().copied().unwrap_or(Style::Plain)
    }

    fn flush(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let style = self.style();
        // A plain run split by an element that adds no style (or by a
        // separator) is one span, even at the cap.
        if let Some(last) = self.spans.last_mut()
            && style == Style::Plain
            && last.style == Style::Plain
        {
            last.text.push_str(&self.cur);
            self.cur.clear();
            return;
        }
        if self.spans.len() >= MAX_SPANS {
            // Past the span cap the rest of the paragraph is dropped.
            self.cur.clear();
            self.over = true;
            return;
        }
        self.spans.push(Span {
            text: std::mem::take(&mut self.cur),
            style,
        });
    }

    /// Ends a run of text: what follows starts after a space.
    fn separate(&mut self) {
        self.pending = self.chars > 0;
    }

    fn enter(&mut self, style: Style) {
        self.flush();
        self.styles.push(style);
    }

    fn exit(&mut self) {
        self.flush();
        self.styles.pop();
    }

    fn put(&mut self, c: char) {
        self.chars += 1;
        self.bytes += c.len_utf8();
        // A space between a plain run and a styled one belongs to the plain
        // run, so a styled span never starts or ends with a space. Between two
        // styled spans it is a plain span of its own.
        if c == ' ' && self.cur.is_empty() && self.style() != Style::Plain {
            let room = self.spans.len() < MAX_SPANS;
            match self.spans.last_mut() {
                Some(last) if last.style == Style::Plain => last.text.push(' '),
                Some(_) if room => self.spans.push(Span {
                    text: " ".into(),
                    style: Style::Plain,
                }),
                // At the span cap the space is dropped with the rest.
                Some(_) | None => {}
            }
            return;
        }
        self.cur.push(c);
    }

    fn push_str(&mut self, s: &str) {
        for c in s.chars() {
            if self.over {
                return;
            }
            match text::class(c) {
                Class::Drop => {}
                Class::Space => self.pending = self.chars > 0,
                Class::Keep => {
                    let extra = usize::from(self.pending) + 1;
                    if self.chars + extra > self.max_chars
                        || self.bytes + extra - 1 + c.len_utf8() > self.max_bytes
                    {
                        self.over = true;
                        return;
                    }
                    if self.pending {
                        self.put(' ');
                        self.pending = false;
                    }
                    self.put(c);
                }
            }
        }
    }

    /// The finished spans; empty when there was no text.
    fn take(&mut self) -> (Vec<Span>, usize) {
        self.flush();
        (std::mem::take(&mut self.spans), self.bytes)
    }
}

struct ListB {
    ordered: bool,
    items: Vec<Vec<Span>>,
}

/// The description being read.
struct DescB {
    rank: usize,
    release: bool,
    blocks: Vec<Block>,
    total: usize,
    cap: usize,
    inline: Inline,
    list: Option<ListB>,
}

struct State<'o> {
    opts: &'o ParseOptions,
    langs: LangPrefs,
    stack: Vec<El>,
    pos: u64,
    saw_root: bool,
    /// The index in `comps` of each ID kept.
    seen: HashMap<String, usize>,
    comps: Vec<Component>,
    skipped: u32,
    duplicates: u32,
    /// What the components kept cost in the index file and when decoded,
    /// see [`index::cost`].
    index_file: usize,
    index_charge: usize,
    n_components: usize,
    retained_bytes: usize,
    retained_objects: usize,
    cur: Option<Cur>,
    leaf: Option<Leaf>,
    desc: Option<DescB>,
}

/// The attributes of a tag, without quick-xml's quadratic duplicate check.
fn attributes<'a>(e: &'a BytesStart<'_>) -> quick_xml::events::attributes::Attributes<'a> {
    let mut it = e.attributes();
    it.with_checks(false);
    it
}

fn kind_of(t: Option<&str>) -> Kind {
    match t {
        Some("desktop-application" | "desktop") => Kind::DesktopApp,
        Some("console-application") => Kind::ConsoleApp,
        Some("addon") => Kind::Addon,
        Some("runtime") => Kind::Runtime,
        _ => Kind::Other,
    }
}

fn url_kind(t: &str) -> Option<UrlKind> {
    Some(match t {
        "homepage" => UrlKind::Homepage,
        "bugtracker" => UrlKind::Bugtracker,
        "help" => UrlKind::Help,
        "donation" => UrlKind::Donation,
        "translate" => UrlKind::Translate,
        "contact" => UrlKind::Contact,
        "contribute" => UrlKind::Contribute,
        "vcs-browser" => UrlKind::VcsBrowser,
        "faq" => UrlKind::Faq,
        _ => return None,
    })
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` (or a longer ISO date-time, of which the date is read) as
/// seconds since the epoch.
fn parse_date(s: &str) -> Option<i64> {
    let b = s.as_bytes().get(..10)?;
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = b.get(r)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}

fn intensity(s: &str) -> Option<Intensity> {
    Some(match s {
        "none" => Intensity::None,
        "mild" => Intensity::Mild,
        "moderate" => Intensity::Moderate,
        "intense" => Intensity::Intense,
        _ => return None,
    })
}

impl<'o> State<'o> {
    fn new(opts: &'o ParseOptions) -> State<'o> {
        State {
            opts,
            langs: LangPrefs::new(&opts.langs),
            stack: Vec::with_capacity(16),
            pos: 0,
            saw_root: false,
            seen: HashMap::new(),
            comps: Vec::new(),
            skipped: 0,
            duplicates: 0,
            index_file: 0,
            index_charge: 0,
            n_components: 0,
            retained_bytes: 0,
            retained_objects: 0,
            cur: None,
            leaf: None,
            desc: None,
        }
    }

    fn xml_err(&self, message: impl Into<String>) -> ParseError {
        ParseError::Xml {
            position: self.pos,
            message: message.into(),
            component: None,
        }
    }

    /// Reads the attributes named in `keys` in one pass. A value over the
    /// attribute cap counts as missing.
    fn attrs<'a, const N: usize>(
        &self,
        e: &'a BytesStart<'_>,
        keys: [&str; N],
    ) -> Result<[Option<Cow<'a, str>>; N], ParseError> {
        let mut out: [Option<Cow<'a, str>>; N] = std::array::from_fn(|_| None);
        for a in attributes(e) {
            let a = a.map_err(|er| self.xml_err(er.to_string()))?;
            let Some(i) = keys.iter().position(|k| *k == a.key.as_ref()) else {
                continue;
            };
            let v = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|er| self.xml_err(er.to_string()))?;
            if v.len() <= self.opts.limits.attr {
                out[i] = Some(v);
            }
        }
        Ok(out)
    }

    /// The rank of an element by its `xml:lang`, or `None` when it is for a
    /// language not wanted or can't beat what is already kept.
    fn rank_of(&self, e: &BytesStart<'_>, best: usize) -> Result<Option<usize>, ParseError> {
        // A value over the cap means a language nobody asked for, not "no
        // language": the element is unwanted.
        for a in attributes(e) {
            let a = a.map_err(|er| self.xml_err(er.to_string()))?;
            if a.key.as_ref() != "xml:lang" {
                continue;
            }
            let v = a
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|er| self.xml_err(er.to_string()))?;
            if v.len() > self.opts.limits.attr {
                return Ok(None);
            }
            return Ok(self.langs.rank(Some(&v)).filter(|r| *r < best));
        }
        Ok(self.langs.rank(None).filter(|r| *r < best))
    }

    fn start(&mut self, e: &BytesStart<'_>) -> Result<(), ParseError> {
        if self.stack.len() >= self.opts.limits.max_depth {
            return Err(ParseError::TooDeep { component: None });
        }
        if e.len() > self.opts.limits.max_tag {
            return Err(ParseError::limit("one tag"));
        }
        // Every attribute of every element is checked, read or not: a
        // malformed one or an entity beyond the predefined five fails the
        // parse. quick-xml's own duplicate check is quadratic and unbounded,
        // so it is off; the count is capped first and duplicates are found
        // here, over at most `max_attrs` names.
        let mut keys: Vec<&str> = Vec::new();
        for a in attributes(e) {
            let a = a.map_err(|er| self.xml_err(er.to_string()))?;
            if keys.len() >= self.opts.limits.max_attrs {
                return Err(ParseError::limit("attributes on one element"));
            }
            if keys.contains(&a.key.as_ref()) {
                return Err(self.xml_err("duplicate attribute"));
            }
            keys.push(a.key.into_inner());
            a.normalized_value(XmlVersion::Implicit1_0)
                .map_err(|er| self.xml_err(er.to_string()))?;
        }
        let el = self.classify(e)?;
        self.stack.push(el);
        Ok(())
    }

    fn classify(&mut self, e: &BytesStart<'_>) -> Result<El, ParseError> {
        let name = e.name();
        let name = name.as_ref();
        let Some(&parent) = self.stack.last() else {
            if self.saw_root || name != "components" {
                return Err(ParseError::NotCatalog);
            }
            self.saw_root = true;
            return Ok(El::Components);
        };
        match parent {
            El::Components if name == "component" => self.begin_component(e),
            El::Component => self.component_child(name, e),
            El::Developer if name == "name" => {
                let best = self.cur.as_ref().map_or(0, |c| c.dev.rank);
                self.leaf_ranked(e, Target::DevName, best, self.opts.limits.developer)
            }
            El::Categories if name == "category" => {
                Ok(self.leaf(Target::Category, 0, self.opts.limits.category))
            }
            El::Keywords if name == "keyword" => {
                let best = self.cur.as_ref().map_or(0, |c| c.keywords.rank);
                // Keywords of the same rank add up, so equal ranks are kept.
                self.leaf_ranked(
                    e,
                    Target::Keyword,
                    best.saturating_add(1),
                    self.opts.limits.keyword,
                )
            }
            El::Screenshots if name == "screenshot" => {
                let [ty] = self.attrs(e, ["type"])?;
                if let Some(c) = self.cur.as_mut() {
                    c.shot = Some(ShotB {
                        default: ty.as_deref() == Some("default"),
                        caption: Loc::new(),
                        images: Vec::new(),
                    });
                }
                Ok(El::Screenshot)
            }
            El::Screenshot if name == "caption" => {
                let best = self
                    .cur
                    .as_ref()
                    .and_then(|c| c.shot.as_ref())
                    .map_or(0, |s| s.caption.rank);
                self.leaf_ranked(e, Target::Caption, best, self.opts.limits.caption)
            }
            El::Screenshot if name == "image" => {
                let [ty, w, h] = self.attrs(e, ["type", "width", "height"])?;
                let n =
                    |v: Option<Cow<'_, str>>| v.and_then(|v| v.trim().parse().ok()).unwrap_or(0);
                Ok(self.leaf(
                    Target::Image {
                        thumbnail: ty.as_deref() == Some("thumbnail"),
                        width: n(w),
                        height: n(h),
                    },
                    0,
                    MAX_URL_CHARS,
                ))
            }
            El::Releases if name == "release" => self.begin_release(e),
            El::Release if name == "description" => self.begin_desc(e, true),
            El::Rating if name == "content_attribute" => {
                let [id] = self.attrs(e, ["id"])?;
                let id = id.map(|i| text::clean(&i, 64)).unwrap_or_default();
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
                    return Ok(El::Skip);
                }
                Ok(self.leaf(Target::Rating(id), 0, 16))
            }
            El::Custom if name == "value" => {
                let [key] = self.attrs(e, ["key"])?;
                let k = match key.as_deref() {
                    Some("flathub::verification::verified") => Key::Verified,
                    Some("flathub::verification::method") => Key::Method,
                    Some("flathub::verification::timestamp") => Key::Timestamp,
                    Some("flathub::verification::website") => Key::Website,
                    Some("flathub::verification::login_name") => Key::LoginName,
                    Some("flathub::verification::login_provider") => Key::LoginProvider,
                    Some("flathub::verification::login_is_organization") => Key::LoginIsOrg,
                    _ => return Ok(El::Skip),
                };
                Ok(self.leaf(Target::Custom(k), 0, MAX_CUSTOM_CHARS))
            }
            El::Branding if name == "color" => {
                let [ty, pref] = self.attrs(e, ["type", "scheme_preference"])?;
                if ty.as_deref() != Some("primary") {
                    return Ok(El::Skip);
                }
                let p = match pref.as_deref() {
                    None => Pref::Both,
                    Some("light") => Pref::Light,
                    Some("dark") => Pref::Dark,
                    Some(_) => return Ok(El::Skip),
                };
                Ok(self.leaf(Target::Color(p), 0, 7))
            }
            El::Desc if name == "p" => {
                self.begin_inline(self.opts.limits.desc_para);
                Ok(El::Block)
            }
            El::Desc if name == "ul" || name == "ol" => {
                if let Some(d) = self.desc.as_mut() {
                    d.list = Some(ListB {
                        ordered: name == "ol",
                        items: Vec::new(),
                    });
                }
                Ok(El::List)
            }
            El::List if name == "li" => {
                self.begin_inline(self.opts.limits.desc_para);
                Ok(El::Item)
            }
            El::Block | El::Item | El::Inline | El::InlineBlock => {
                if let Some(d) = self.desc.as_mut() {
                    // An unknown element keeps the style it is in. A block
                    // element inside inline content ends the run of text, so
                    // its text doesn't run into what is around it.
                    let style = match name {
                        "em" => Style::Emphasis,
                        "code" => Style::Code,
                        _ => d.inline.style(),
                    };
                    let block = matches!(name, "p" | "ul" | "ol" | "li");
                    if block {
                        d.inline.separate();
                    }
                    d.inline.enter(style);
                    return Ok(if block { El::InlineBlock } else { El::Inline });
                }
                Ok(El::Inline)
            }
            _ => Ok(El::Skip),
        }
    }

    fn leaf(&mut self, target: Target, rank: usize, max_chars: usize) -> El {
        self.leaf = Some(Leaf {
            target,
            buf: LineBuf::new(max_chars),
            rank,
        });
        El::Leaf
    }

    /// A leaf with an `xml:lang` that must beat `best` to be read at all.
    fn leaf_ranked(
        &mut self,
        e: &BytesStart<'_>,
        target: Target,
        best: usize,
        max_chars: usize,
    ) -> Result<El, ParseError> {
        match self.rank_of(e, best)? {
            Some(rank) => Ok(self.leaf(target, rank, max_chars)),
            None => Ok(El::Skip),
        }
    }

    fn begin_component(&mut self, e: &BytesStart<'_>) -> Result<El, ParseError> {
        self.n_components += 1;
        if self.n_components > self.opts.limits.max_components {
            return Err(ParseError::limit("components"));
        }
        let [ty] = self.attrs(e, ["type"])?;
        self.cur = Some(Cur::new(kind_of(ty.as_deref())));
        Ok(El::Component)
    }

    fn component_child(&mut self, name: &str, e: &BytesStart<'_>) -> Result<El, ParseError> {
        let lim = &self.opts.limits;
        let Some(c) = self.cur.as_ref() else {
            return Ok(El::Skip);
        };
        match name {
            "id" => Ok(self.leaf(Target::Id, 0, MAX_ID_CHARS)),
            "name" => self.leaf_ranked(e, Target::Name, c.name.rank, lim.name),
            "summary" => self.leaf_ranked(e, Target::Summary, c.summary.rank, lim.summary),
            "project_license" => Ok(self.leaf(Target::License, 0, lim.license)),
            "developer" => Ok(El::Developer),
            "developer_name" => {
                self.leaf_ranked(e, Target::DevLegacy, c.dev_legacy.rank, lim.developer)
            }
            "categories" => Ok(El::Categories),
            "keywords" => Ok(El::Keywords),
            "description" => self.begin_desc(e, false),
            "screenshots" => Ok(El::Screenshots),
            "releases" => Ok(El::Releases),
            "content_rating" => {
                // Only the first rating is read; a second one is skipped whole.
                if c.rating.is_some() {
                    return Ok(El::Skip);
                }
                let [ty] = self.attrs(e, ["type"])?;
                let scheme = match ty.as_deref() {
                    Some("oars-1.0") => RatingScheme::Oars10,
                    Some("oars-1.1") => RatingScheme::Oars11,
                    _ => RatingScheme::Other,
                };
                if let Some(c) = self.cur.as_mut() {
                    c.rating = Some(ContentRating {
                        scheme,
                        attrs: Vec::new(),
                    });
                }
                Ok(El::Rating)
            }
            "custom" => Ok(El::Custom),
            "branding" => Ok(El::Branding),
            "extends" => Ok(self.leaf(Target::Extends, 0, MAX_ID_CHARS)),
            "launchable" => {
                let [ty] = self.attrs(e, ["type"])?;
                if ty.as_deref() != Some("desktop-id") {
                    return Ok(El::Skip);
                }
                Ok(self.leaf(Target::Launchable, 0, MAX_ID_CHARS))
            }
            "bundle" => {
                let [ty, rt, sdk] = self.attrs(e, ["type", "runtime", "sdk"])?;
                if ty.as_deref() != Some("flatpak") || c.bundle.is_some() {
                    return Ok(El::Skip);
                }
                let ok = |v: Option<Cow<'_, str>>| {
                    v.map(|v| text::clean(&v, MAX_ID_CHARS))
                        .filter(|v| text::valid_flatpak_target(v))
                };
                Ok(self.leaf(
                    Target::Bundle {
                        runtime: ok(rt),
                        sdk: ok(sdk),
                    },
                    0,
                    MAX_BUNDLE_CHARS,
                ))
            }
            "icon" => {
                let [ty, scale, w] = self.attrs(e, ["type", "scale", "width"])?;
                if ty.as_deref() != Some("cached") || !matches!(scale.as_deref(), None | Some("1"))
                {
                    return Ok(El::Skip);
                }
                let width = w.and_then(|w| w.trim().parse().ok()).unwrap_or(0);
                Ok(self.leaf(Target::Icon { width }, 0, MAX_ID_CHARS))
            }
            "url" => {
                let [ty] = self.attrs(e, ["type"])?;
                match ty.as_deref().and_then(url_kind) {
                    Some(k) if c.urls.len() < lim.urls => {
                        Ok(self.leaf(Target::Url(k), 0, MAX_URL_CHARS))
                    }
                    _ => Ok(El::Skip),
                }
            }
            _ => Ok(El::Skip),
        }
    }

    fn begin_release(&mut self, e: &BytesStart<'_>) -> Result<El, ParseError> {
        let [version, ts, date, ty] = self.attrs(e, ["version", "timestamp", "date", "type"])?;
        let version = version
            .map(|v| text::clean(&v, self.opts.limits.version))
            .unwrap_or_default();
        let timestamp = ts
            .and_then(|t| t.trim().parse::<i64>().ok())
            .or_else(|| date.and_then(|d| parse_date(&d)))
            .unwrap_or(0);
        let kind = match ty.as_deref() {
            None | Some("stable") => ReleaseKind::Stable,
            Some("development") => ReleaseKind::Development,
            Some("snapshot") => ReleaseKind::Snapshot,
            Some(_) => ReleaseKind::Other,
        };
        if let Some(c) = self.cur.as_mut() {
            c.rel = (!version.is_empty()).then_some(RelB {
                version,
                timestamp,
                kind,
                desc: None,
            });
        }
        Ok(El::Release)
    }

    fn begin_desc(&mut self, e: &BytesStart<'_>, release: bool) -> Result<El, ParseError> {
        let Some(c) = self.cur.as_ref() else {
            return Ok(El::Skip);
        };
        let best = if release {
            match c.rel.as_ref() {
                Some(r) => r.desc.as_ref().map_or(usize::MAX, |d| d.0),
                None => return Ok(El::Skip),
            }
        } else {
            c.desc.as_ref().map_or(usize::MAX, |d| d.0)
        };
        let Some(rank) = self.rank_of(e, best)? else {
            return Ok(El::Skip);
        };
        let cap = if release {
            self.opts.limits.release_desc_total
        } else {
            self.opts.limits.desc_total
        };
        self.desc = Some(DescB {
            rank,
            release,
            blocks: Vec::new(),
            total: 0,
            cap,
            inline: Inline::default(),
            list: None,
        });
        Ok(El::Desc)
    }

    fn begin_inline(&mut self, max_chars: usize) {
        if let Some(d) = self.desc.as_mut() {
            let left = d.cap.saturating_sub(d.total);
            let full = d.blocks.len() >= self.opts.limits.desc_blocks;
            d.inline.reset(if full { 0 } else { max_chars }, left);
        }
    }

    fn text(&mut self, t: &str) {
        match self.stack.last() {
            Some(El::Leaf) => {
                if let Some(l) = self.leaf.as_mut() {
                    l.buf.push_str(t);
                }
            }
            Some(El::Block | El::Item | El::Inline | El::InlineBlock) => {
                if let Some(d) = self.desc.as_mut() {
                    d.inline.push_str(t);
                }
            }
            _ => {}
        }
    }

    /// `&amp;` and its four siblings and numeric references are text; any
    /// other entity fails the parse, wherever it is.
    fn reference(&mut self, r: &quick_xml::events::BytesRef<'_>) -> Result<(), ParseError> {
        if r.is_char_ref() {
            match r.resolve_char_ref() {
                Ok(Some(c)) => {
                    let mut b = [0u8; 4];
                    self.text(c.encode_utf8(&mut b));
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(e) => Err(self.xml_err(e.to_string())),
            }
        } else if let Some(s) = resolve_predefined_entity(r) {
            self.text(s);
            Ok(())
        } else {
            Err(ParseError::Entity {
                name: text::clean(r, 64),
                component: None,
            })
        }
    }

    fn end(&mut self) -> Result<(), ParseError> {
        let Some(el) = self.stack.pop() else {
            return Err(self.xml_err("unexpected closing tag"));
        };
        match el {
            El::Leaf => self.end_leaf(),
            El::Component => self.end_component()?,
            El::Screenshot => self.end_screenshot(),
            El::Release => self.end_release(),
            El::Desc => self.end_desc(),
            El::Block => self.end_block(),
            El::List => self.end_list(),
            El::Item => self.end_item(),
            El::Inline | El::InlineBlock => {
                if let Some(d) = self.desc.as_mut() {
                    d.inline.exit();
                    if el == El::InlineBlock {
                        d.inline.separate();
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn end_leaf(&mut self) {
        let (Some(leaf), Some(c)) = (self.leaf.take(), self.cur.as_mut()) else {
            return;
        };
        let lim = &self.opts.limits;
        // Names that are looked up or opened later are never repaired: a
        // control or bidi character makes them invalid.
        let truncated = leaf.buf.truncated() || (leaf.buf.altered() && !leaf.target.is_text());
        let s = leaf.buf.finish();
        match leaf.target {
            Target::Id => c.id = if truncated { String::new() } else { s },
            Target::Name => keep_loc(&mut c.name, leaf.rank, s),
            Target::Summary => keep_loc(&mut c.summary, leaf.rank, s),
            Target::DevName => keep_loc(&mut c.dev, leaf.rank, s),
            Target::DevLegacy => keep_loc(&mut c.dev_legacy, leaf.rank, s),
            Target::License => {
                if c.license.is_empty() {
                    c.license = s;
                }
            }
            Target::Category => {
                if !s.is_empty() && c.categories.len() < lim.categories {
                    c.categories.push(s);
                }
            }
            Target::Keyword => {
                // An empty keyword never takes the slot from a better-ranked one.
                if s.is_empty() {
                    return;
                }
                if leaf.rank < c.keywords.rank {
                    c.keywords.rank = leaf.rank;
                    c.keywords.v.clear();
                }
                if c.keywords.v.len() < lim.keywords {
                    c.keywords.v.push(s);
                }
            }
            Target::Caption => {
                if let Some(sh) = c.shot.as_mut() {
                    keep_loc(&mut sh.caption, leaf.rank, s);
                }
            }
            Target::Extends => {
                if !truncated && text::valid_id(&s) && c.extends.len() < MAX_EXTENDS {
                    c.extends.push(s);
                }
            }
            Target::Launchable => {
                if c.launchable.is_none() && !truncated && text::valid_id(&s) {
                    c.launchable = Some(s);
                }
            }
            Target::Bundle { runtime, sdk } => {
                if !truncated && text::valid_bundle_ref(&s) {
                    c.bundle = Some(Bundle {
                        reference: s,
                        runtime,
                        sdk,
                    });
                }
            }
            Target::Icon { width } => {
                if truncated || !text::valid_icon_file(&s) {
                    return;
                }
                // Keyed by file and width: a later size of another file is
                // kept apart, and the best file is chosen at the end.
                let at = match c.icons.iter().position(|i| i.file == s) {
                    Some(i) => i,
                    None if c.icons.len() < MAX_ICONS => {
                        c.icons.push(Icon {
                            file: s,
                            sizes: Vec::new(),
                        });
                        c.icons.len() - 1
                    }
                    None => return,
                };
                let icon = &mut c.icons[at];
                if width > 0 && !icon.sizes.contains(&width) && icon.sizes.len() < MAX_ICON_SIZES {
                    icon.sizes.push(width);
                    icon.sizes.sort_unstable();
                }
            }
            Target::Url(kind) => {
                if !truncated && text::valid_url(&s, false) && c.urls.len() < lim.urls {
                    c.urls.push((kind, s));
                }
            }
            Target::Image {
                thumbnail,
                width,
                height,
            } => {
                if let Some(sh) = c.shot.as_mut()
                    && !truncated
                    && text::valid_url(&s, true)
                    && sh.images.len() < lim.images
                {
                    sh.images.push(Image {
                        thumbnail,
                        width,
                        height,
                        url: s,
                    });
                }
            }
            Target::Custom(k) => {
                let v = &mut c.verif;
                match k {
                    Key::Verified => v.verified = s == "true",
                    Key::Method => v.method = text::clean(&s, 64),
                    Key::Timestamp => v.timestamp = s.parse().unwrap_or(0),
                    Key::Website => v.website = text::clean(&s, 253),
                    Key::LoginName => v.login_name = text::clean(&s, 100),
                    Key::LoginProvider => v.login_provider = text::clean(&s, 64),
                    Key::LoginIsOrg => v.organization = s == "true",
                }
            }
            Target::Rating(id) => {
                if let (Some(r), Some(i)) = (c.rating.as_mut(), intensity(&s))
                    && r.attrs.len() < MAX_RATING_ATTRS
                {
                    r.attrs.push((id, i));
                }
            }
            Target::Color(p) => {
                if let Some(rgb) = parse_color(&s) {
                    if matches!(p, Pref::Light | Pref::Both) && c.light.is_none() {
                        c.light = Some(rgb);
                    }
                    if matches!(p, Pref::Dark | Pref::Both) && c.dark.is_none() {
                        c.dark = Some(rgb);
                    }
                }
            }
        }
    }

    fn end_block(&mut self) {
        let max = self.opts.limits.desc_blocks;
        if let Some(d) = self.desc.as_mut() {
            let (spans, bytes) = d.inline.take();
            d.total += bytes;
            if !spans.is_empty() && d.blocks.len() < max {
                d.blocks.push(Block::Paragraph(spans));
            }
        }
    }

    fn end_item(&mut self) {
        let max = self.opts.limits.desc_items;
        if let Some(d) = self.desc.as_mut() {
            let (spans, bytes) = d.inline.take();
            d.total += bytes;
            if let Some(l) = d.list.as_mut()
                && !spans.is_empty()
                && l.items.len() < max
            {
                l.items.push(spans);
            }
        }
    }

    fn end_list(&mut self) {
        let max = self.opts.limits.desc_blocks;
        if let Some(d) = self.desc.as_mut()
            && let Some(l) = d.list.take()
            && !l.items.is_empty()
            && d.blocks.len() < max
        {
            d.blocks.push(Block::List {
                ordered: l.ordered,
                items: l.items,
            });
        }
    }

    fn end_desc(&mut self) {
        let (Some(d), Some(c)) = (self.desc.take(), self.cur.as_mut()) else {
            return;
        };
        let slot = if d.release {
            c.rel.as_mut().map(|r| &mut r.desc)
        } else {
            Some(&mut c.desc)
        };
        if let Some(slot) = slot
            && !d.blocks.is_empty()
            && slot.as_ref().is_none_or(|(r, _)| d.rank < *r)
        {
            *slot = Some((d.rank, d.blocks));
        }
    }

    fn end_screenshot(&mut self) {
        let max = self.opts.limits.screenshots;
        let Some(c) = self.cur.as_mut() else { return };
        if let Some(s) = c.shot.take()
            && !s.images.is_empty()
        {
            let shot = Screenshot {
                default: s.default,
                caption: s.caption.s,
                images: s.images,
            };
            if c.shots.len() < max {
                c.shots.push(shot);
            } else if shot.default
                && !c.shots.iter().any(|x| x.default)
                && let Some(last) = c.shots.last_mut()
            {
                // The marked one past the cap takes the last place.
                *last = shot;
            }
        }
    }

    fn end_release(&mut self) {
        let Some(c) = self.cur.as_mut() else { return };
        if let Some(r) = c.rel.take() {
            c.releases.push(Release {
                version: r.version,
                timestamp: r.timestamp,
                kind: r.kind,
                description: r.desc.map(|d| d.1).unwrap_or_default(),
            });
            // Keep the list short however many releases there are.
            if c.releases.len() >= 64 {
                c.releases.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
                c.releases.truncate(RELEASES);
            }
        }
    }

    fn end_component(&mut self) -> Result<(), ParseError> {
        let Some(c) = self.cur.take() else {
            return Ok(());
        };
        self.desc = None;
        self.leaf = None;
        let valid_id = text::valid_id(&c.id);
        let needs_bundle = needs_bundle(c.kind);
        if !valid_id || c.name.s.is_empty() || (needs_bundle && c.bundle.is_none()) {
            self.skipped = self.skipped.saturating_add(1);
            return Ok(());
        }
        // The bundle is what gets installed, so it must be the app shown.
        if let Some(b) = c.bundle.as_ref()
            && !bundle_matches(&c.id, &b.reference)
        {
            self.skipped = self.skipped.saturating_add(1);
            return Ok(());
        }
        let mut releases = c.releases;
        releases.sort_by_key(|r| std::cmp::Reverse(r.timestamp));
        releases.truncate(RELEASES);
        let v = c.verif;
        let verification = v.verified.then_some(Verification {
            method: v.method,
            website: v.website,
            login_name: v.login_name,
            login_provider: v.login_provider,
            organization: v.organization,
            timestamp: v.timestamp,
        });
        let branding = (c.light.is_some() || c.dark.is_some()).then_some(Branding {
            light: c.light,
            dark: c.dark,
        });
        let developer = if c.dev.s.is_empty() {
            c.dev_legacy.s
        } else {
            c.dev.s
        };
        let comp = Component {
            id: c.id,
            kind: c.kind,
            name: c.name.s,
            summary: c.summary.s,
            description: c.desc.map(|d| d.1).unwrap_or_default(),
            developer,
            license: c.license,
            categories: c.categories,
            keywords: c.keywords.v,
            icon: best_icon(c.icons),
            urls: c.urls,
            screenshots: one_default(c.shots),
            releases,
            content_rating: c.rating,
            bundle: c.bundle,
            extends: c.extends,
            launchable: c.launchable,
            verification,
            branding,
        };
        // A second component of the same ID (the real catalogs have them: one
        // per branch of a runtime or add-on) replaces the first only when it
        // is the better one, see `better`. Either way it is counted.
        let at = self.seen.get(&comp.id).copied();
        if let Some(i) = at {
            self.duplicates = self.duplicates.saturating_add(1);
            if !better(&comp, &self.comps[i]) {
                return Ok(());
            }
        }
        let (bytes, objects) = weigh(&comp);
        let (file, charge) = index::cost(&comp);
        // A replaced component leaves the totals, the new one enters them.
        let (ob, oo, of, oc) = at.map_or((0, 0, 0, 0), |i| {
            let (b, n) = weigh(&self.comps[i]);
            let (f, g) = index::cost(&self.comps[i]);
            (b, n, f, g)
        });
        let retained_bytes = self.retained_bytes.saturating_sub(ob).saturating_add(bytes);
        let retained_objects = self
            .retained_objects
            .saturating_sub(oo)
            .saturating_add(objects);
        let index_file = self.index_file.saturating_sub(of).saturating_add(file);
        let index_charge = self.index_charge.saturating_sub(oc).saturating_add(charge);
        let lim = &self.opts.limits;
        let id = Some(comp.id.clone());
        let over = |what: &'static str| ParseError::Limit {
            what,
            component: id.clone(),
        };
        if retained_bytes > lim.max_retained_bytes {
            return Err(over("text kept for the catalog"));
        }
        if retained_objects > lim.max_retained_objects {
            return Err(over("objects kept for the catalog"));
        }
        // What is kept must also fit the index: the same catalog is written
        // to it and read back, and a file over its caps would be refused
        // every time and the XML parsed again at each start.
        if index_file > index::MAX_PAYLOAD || index_charge > index::MAX_CHARGE {
            return Err(over("catalog too large for the index"));
        }
        self.retained_bytes = retained_bytes;
        self.retained_objects = retained_objects;
        self.index_file = index_file;
        self.index_charge = index_charge;
        match at {
            Some(i) => self.comps[i] = comp,
            None => {
                self.seen.insert(comp.id.clone(), self.comps.len());
                self.comps.push(comp);
            }
        }
        Ok(())
    }

    /// The ID of the component being read, once its `<id>` has been seen.
    fn cur_id(&self) -> Option<String> {
        self.cur
            .as_ref()
            .map(|c| &c.id)
            .filter(|i| text::valid_id(i))
            .cloned()
    }

    fn finish(self) -> Result<Catalog, ParseError> {
        if !self.stack.is_empty() {
            return Err(ParseError::Xml {
                position: self.pos,
                message: "the file ends inside an element (truncated?)".into(),
                component: self.cur_id(),
            });
        }
        if !self.saw_root {
            return Err(ParseError::NotCatalog);
        }
        Ok(Catalog {
            origin: self.opts.origin.clone(),
            components: self.comps,
            skipped: self.skipped,
            duplicates: self.duplicates,
        })
    }
}

/// Whether `a` is a better copy of a component than `b`, the same ID. One with
/// a bundle beats one without; then the one with the newer release; then the
/// newer branch; then the one with more of its fields filled. Equal ones keep
/// the first in the file.
fn better(a: &Component, b: &Component) -> bool {
    fn key(c: &Component) -> (bool, i64, BranchKey, usize) {
        let filled = [
            !c.summary.is_empty(),
            !c.description.is_empty(),
            !c.developer.is_empty(),
            !c.license.is_empty(),
            !c.categories.is_empty(),
            !c.keywords.is_empty(),
            c.icon.is_some(),
            !c.urls.is_empty(),
            !c.screenshots.is_empty(),
            !c.releases.is_empty(),
            c.content_rating.is_some(),
            c.launchable.is_some(),
            c.verification.is_some(),
            c.branding.is_some(),
            c.kind != Kind::Other,
        ];
        let newest = c.releases.iter().map(|r| r.timestamp).max().unwrap_or(0);
        (
            c.bundle.is_some(),
            newest,
            branch_key(c),
            filled.into_iter().filter(|f| *f).count(),
        )
    }
    key(a) > key(b)
}

/// A branch for ordering: a dotted numeric version (compared number by
/// number) beats any other name, and other names compare as strings.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum BranchKey {
    None,
    Name(String),
    Version(Vec<u64>),
}

/// The ordering key of the last `/` segment of the bundle reference.
fn branch_key(c: &Component) -> BranchKey {
    let Some(b) = c.bundle.as_ref() else {
        return BranchKey::None;
    };
    let branch = b.reference.rsplit('/').next().unwrap_or("");
    let nums: Option<Vec<u64>> = branch
        .split('.')
        .map(|p| {
            if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
                // Too long for a u64: still a number, and the largest.
                Some(p.parse().unwrap_or(u64::MAX))
            } else {
                None
            }
        })
        .collect();
    match nums {
        Some(n) if !n.is_empty() => BranchKey::Version(n),
        _ if branch.is_empty() => BranchKey::None,
        _ => BranchKey::Name(branch.to_string()),
    }
}

/// Whether a component of this kind is only listed with a bundle.
pub(crate) fn needs_bundle(kind: Kind) -> bool {
    kind != Kind::Other
}

/// Whether a bundle reference installs the component it is listed under: its
/// ID has to be the component's own, or that with `.desktop` stripped (a few
/// real apps, such as org.telegram.desktop, have it in the bundle too).
/// Known gap: component `X.desktop` with bundle `X` passes, so the shown name
/// may belong to a different ID than the one installed; the install dialog
/// shows the real ref.
pub(crate) fn bundle_matches(component_id: &str, reference: &str) -> bool {
    bundle_id(reference).is_some_and(|i| i == component_id || i == super::bare_id(component_id))
}

/// The ID of an `app/ID/arch/branch` or `runtime/ID/arch/branch` reference.
fn bundle_id(r: &str) -> Option<&str> {
    r.split('/').nth(1)
}

/// The text bytes and the objects (strings and spans) a component keeps.
fn weigh(c: &Component) -> (usize, usize) {
    let mut bytes = 0;
    let mut objects = 1;
    let mut s = |t: &str| {
        bytes += t.len();
        objects += 1;
    };
    s(&c.id);
    s(&c.name);
    s(&c.summary);
    s(&c.developer);
    s(&c.license);
    c.categories.iter().for_each(|t| s(t));
    c.keywords.iter().for_each(|t| s(t));
    c.extends.iter().for_each(|t| s(t));
    if let Some(l) = &c.launchable {
        s(l);
    }
    if let Some(i) = &c.icon {
        s(&i.file);
    }
    c.urls.iter().for_each(|(_, t)| s(t));
    let blocks = |bl: &[Block], s: &mut dyn FnMut(&str)| {
        for b in bl {
            match b {
                Block::Paragraph(spans) => spans.iter().for_each(|sp| s(&sp.text)),
                Block::List { items, .. } => items.iter().flatten().for_each(|sp| s(&sp.text)),
            }
        }
    };
    blocks(&c.description, &mut s);
    for sh in &c.screenshots {
        s(&sh.caption);
        sh.images.iter().for_each(|i| s(&i.url));
    }
    for r in &c.releases {
        s(&r.version);
        blocks(&r.description, &mut s);
    }
    if let Some(r) = &c.content_rating {
        r.attrs.iter().for_each(|(t, _)| s(t));
    }
    if let Some(b) = &c.bundle {
        s(&b.reference);
        b.runtime.iter().for_each(|t| s(t));
        b.sdk.iter().for_each(|t| s(t));
    }
    if let Some(v) = &c.verification {
        s(&v.method);
        s(&v.website);
        s(&v.login_name);
        s(&v.login_provider);
    }
    (bytes, objects)
}

/// The icon with the largest size, then the most sizes; the first of equals. A
/// file with no sizes (an SVG) counts as having none.
fn best_icon(icons: Vec<Icon>) -> Option<Icon> {
    let key = |i: &Icon| (i.sizes.last().copied().unwrap_or(0), i.sizes.len());
    let mut best: Option<Icon> = None;
    for i in icons {
        if best.as_ref().is_none_or(|b| key(&i) > key(b)) {
            best = Some(i);
        }
    }
    best
}

/// Exactly one default screenshot, when there are any: the first marked one,
/// moved to the front, or else the first.
fn one_default(mut shots: Vec<Screenshot>) -> Vec<Screenshot> {
    let at = shots.iter().position(|s| s.default).unwrap_or(0);
    for s in &mut shots {
        s.default = false;
    }
    if !shots.is_empty() {
        let mut first = shots.remove(at);
        first.default = true;
        shots.insert(0, first);
    }
    shots
}

/// Takes `s` into `slot` if it ranks better. An empty string never does: a
/// blank translation must not wipe the fallback.
fn keep_loc(slot: &mut Loc, rank: usize, s: String) {
    if rank < slot.rank && !s.is_empty() {
        slot.rank = rank;
        slot.s = s;
    }
}
