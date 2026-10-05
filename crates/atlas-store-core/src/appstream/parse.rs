//! The AppStream parser: a streaming pass over the XML that keeps only what
//! the Store shows, in one language, within fixed caps. The input is untrusted
//! network data, so a DOCTYPE, any entity beyond the five predefined ones,
//! deep nesting, a huge download and a huge single node are all refused, and
//! what is merely too long is cut. Nothing here panics on input.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

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
    /// Longest stretch of bytes without a `<`: one text node, comment or tag.
    pub max_token: usize,
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
            max_decompressed: 512 << 20,
            max_token: 16 << 20,
            max_depth: 32,
            max_components: 100_000,
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
    Xml { position: u64, message: String },
    /// A DOCTYPE is never accepted.
    DocType,
    /// An entity other than `&amp; &lt; &gt; &quot; &apos;`.
    Entity(String),
    /// Elements nested deeper than [`Limits::max_depth`].
    TooDeep,
    /// A size or count cap was passed; names which.
    Limit(&'static str),
    /// Well-formed XML that is not an AppStream catalog.
    NotCatalog,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Io(e) => write!(f, "can't read the AppStream data: {e}"),
            ParseError::Xml { position, message } => {
                write!(f, "AppStream XML is damaged at byte {position}: {message}")
            }
            ParseError::DocType => write!(f, "AppStream XML has a DOCTYPE, which is refused"),
            ParseError::Entity(e) => {
                write!(f, "AppStream XML uses the entity &{e};, which is refused")
            }
            ParseError::TooDeep => write!(f, "AppStream XML is nested too deeply"),
            ParseError::Limit(what) => write!(f, "AppStream data is too large: {what}"),
            ParseError::NotCatalog => write!(f, "the file is not an AppStream catalog"),
        }
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

/// Counts what passes through and fails past the caps, so a gzip bomb or a
/// single endless text node stops early instead of filling memory.
struct Guard<R> {
    inner: R,
    total: u64,
    max_total: u64,
    run: usize,
    max_run: usize,
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.total = self.total.saturating_add(n as u64);
        if self.total > self.max_total {
            return Err(io::Error::other(LimitHit("decompressed size")));
        }
        let chunk = out.get(..n).unwrap_or(&[]);
        match chunk.iter().rposition(|&b| b == b'<') {
            Some(i) => self.run = n - 1 - i,
            None => self.run = self.run.saturating_add(n),
        }
        if self.run > self.max_run {
            return Err(io::Error::other(LimitHit("one text node or tag")));
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
        return Err(ParseError::Limit("compressed size"));
    }
    let raw = BufReader::with_capacity(64 << 10, file.take(opts.limits.max_compressed));
    parse(flate2::bufread::GzDecoder::new(raw), opts)
}

/// Parses an uncompressed catalog.
pub fn parse<R: Read>(reader: R, opts: &ParseOptions) -> Result<Catalog, ParseError> {
    let lim = &opts.limits;
    let guard = Guard {
        inner: reader,
        total: 0,
        max_total: lim.max_decompressed,
        run: 0,
        max_run: lim.max_token,
    };
    let mut rd = Reader::from_reader(BufReader::with_capacity(64 << 10, guard));
    let mut st = State::new(opts);
    let mut buf = Vec::with_capacity(8 << 10);
    loop {
        buf.clear();
        let ev = match rd.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(e) => return Err(map_error(e, rd.error_position())),
        };
        st.pos = rd.buffer_position();
        match ev {
            Event::Eof => break,
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
    st.finish()
}

fn map_error(e: quick_xml::Error, position: u64) -> ParseError {
    if let quick_xml::Error::Io(io) = &e {
        if let Some(hit) = io.get_ref().and_then(|i| i.downcast_ref::<LimitHit>()) {
            return ParseError::Limit(hit.0);
        }
        return ParseError::Io(io.to_string());
    }
    ParseError::Xml {
        position,
        message: e.to_string(),
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
    icon: Option<Icon>,
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
            icon: None,
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
        if !self.cur.is_empty() {
            let style = self.style();
            self.spans.push(Span {
                text: std::mem::take(&mut self.cur),
                style,
            });
        }
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
        // run, so a styled span never starts or ends with a space.
        if c == ' '
            && self.cur.is_empty()
            && self.style() != Style::Plain
            && let Some(last) = self.spans.last_mut()
        {
            last.text.push(' ');
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
    seen: HashSet<String>,
    comps: Vec<Component>,
    skipped: u32,
    n_components: usize,
    cur: Option<Cur>,
    leaf: Option<Leaf>,
    desc: Option<DescB>,
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
            seen: HashSet::new(),
            comps: Vec::new(),
            skipped: 0,
            n_components: 0,
            cur: None,
            leaf: None,
            desc: None,
        }
    }

    fn xml_err(&self, message: impl Into<String>) -> ParseError {
        ParseError::Xml {
            position: self.pos,
            message: message.into(),
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
        for a in e.attributes() {
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
        let [lang] = self.attrs(e, ["xml:lang"])?;
        Ok(self.langs.rank(lang.as_deref()).filter(|r| *r < best))
    }

    fn start(&mut self, e: &BytesStart<'_>) -> Result<(), ParseError> {
        if self.stack.len() >= self.opts.limits.max_depth {
            return Err(ParseError::TooDeep);
        }
        // Every attribute of every element is checked, read or not: a
        // malformed one or an entity beyond the predefined five fails the parse.
        for a in e.attributes() {
            let a = a.map_err(|er| self.xml_err(er.to_string()))?;
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
                    2048,
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
                Ok(self.leaf(Target::Custom(k), 0, 256))
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
            El::Block | El::Item | El::Inline => {
                let style = match name {
                    "em" => Style::Emphasis,
                    "code" => Style::Code,
                    _ => Style::Plain,
                };
                if let Some(d) = self.desc.as_mut() {
                    d.inline.enter(style);
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
            return Err(ParseError::Limit("components"));
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
            "id" => Ok(self.leaf(Target::Id, 0, 300)),
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
                let [ty] = self.attrs(e, ["type"])?;
                let scheme = match ty.as_deref() {
                    Some("oars-1.0") => RatingScheme::Oars10,
                    Some("oars-1.1") => RatingScheme::Oars11,
                    _ => RatingScheme::Other,
                };
                if let Some(c) = self.cur.as_mut()
                    && c.rating.is_none()
                {
                    c.rating = Some(ContentRating {
                        scheme,
                        attrs: Vec::new(),
                    });
                }
                Ok(El::Rating)
            }
            "custom" => Ok(El::Custom),
            "branding" => Ok(El::Branding),
            "extends" => Ok(self.leaf(Target::Extends, 0, 300)),
            "launchable" => {
                let [ty] = self.attrs(e, ["type"])?;
                if ty.as_deref() != Some("desktop-id") {
                    return Ok(El::Skip);
                }
                Ok(self.leaf(Target::Launchable, 0, 300))
            }
            "bundle" => {
                let [ty, rt, sdk] = self.attrs(e, ["type", "runtime", "sdk"])?;
                if ty.as_deref() != Some("flatpak") || c.bundle.is_some() {
                    return Ok(El::Skip);
                }
                let ok = |v: Option<Cow<'_, str>>| {
                    v.map(|v| text::clean(&v, 300))
                        .filter(|v| text::valid_flatpak_target(v))
                };
                Ok(self.leaf(
                    Target::Bundle {
                        runtime: ok(rt),
                        sdk: ok(sdk),
                    },
                    0,
                    600,
                ))
            }
            "icon" => {
                let [ty, scale, w] = self.attrs(e, ["type", "scale", "width"])?;
                if ty.as_deref() != Some("cached") || !matches!(scale.as_deref(), None | Some("1"))
                {
                    return Ok(El::Skip);
                }
                let width = w.and_then(|w| w.trim().parse().ok()).unwrap_or(0);
                Ok(self.leaf(Target::Icon { width }, 0, 300))
            }
            "url" => {
                let [ty] = self.attrs(e, ["type"])?;
                match ty.as_deref().and_then(url_kind) {
                    Some(k) if c.urls.len() < lim.urls => Ok(self.leaf(Target::Url(k), 0, 2048)),
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
            Some(El::Block | El::Item | El::Inline) => {
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
            Err(ParseError::Entity(text::clean(r, 64)))
        }
    }

    fn end(&mut self) -> Result<(), ParseError> {
        let Some(el) = self.stack.pop() else {
            return Err(self.xml_err("unexpected closing tag"));
        };
        match el {
            El::Leaf => self.end_leaf(),
            El::Component => self.end_component(),
            El::Screenshot => self.end_screenshot(),
            El::Release => self.end_release(),
            El::Desc => self.end_desc(),
            El::Block => self.end_block(),
            El::List => self.end_list(),
            El::Item => self.end_item(),
            El::Inline => {
                if let Some(d) = self.desc.as_mut() {
                    d.inline.exit();
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
                if leaf.rank < c.keywords.rank {
                    c.keywords.rank = leaf.rank;
                    c.keywords.v.clear();
                }
                if !s.is_empty() && c.keywords.v.len() < lim.keywords {
                    c.keywords.v.push(s);
                }
            }
            Target::Caption => {
                if let Some(sh) = c.shot.as_mut() {
                    keep_loc(&mut sh.caption, leaf.rank, s);
                }
            }
            Target::Extends => {
                if !truncated && text::valid_id(&s) && c.extends.len() < 16 {
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
                let icon = c.icon.get_or_insert_with(|| Icon {
                    file: s.clone(),
                    sizes: Vec::new(),
                });
                if icon.file == s
                    && width > 0
                    && !icon.sizes.contains(&width)
                    && icon.sizes.len() < 16
                {
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
                    && r.attrs.len() < 64
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
            && c.shots.len() < max
        {
            c.shots.push(Screenshot {
                default: s.default,
                caption: s.caption.s,
                images: s.images,
            });
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

    fn end_component(&mut self) {
        let Some(c) = self.cur.take() else { return };
        self.desc = None;
        self.leaf = None;
        let valid_id = text::valid_id(&c.id);
        let needs_bundle = c.kind != Kind::Other;
        if !valid_id || c.name.s.is_empty() || (needs_bundle && c.bundle.is_none()) {
            self.skipped = self.skipped.saturating_add(1);
            return;
        }
        if !self.seen.insert(c.id.clone()) {
            self.skipped = self.skipped.saturating_add(1);
            return;
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
        self.comps.push(Component {
            id: c.id,
            kind: c.kind,
            name: c.name.s,
            summary: c.summary.s,
            description: c.desc.map(|d| d.1).unwrap_or_default(),
            developer,
            license: c.license,
            categories: c.categories,
            keywords: c.keywords.v,
            icon: c.icon,
            urls: c.urls,
            screenshots: c.shots,
            releases,
            content_rating: c.rating,
            bundle: c.bundle,
            extends: c.extends,
            launchable: c.launchable,
            verification,
            branding,
        });
    }

    fn finish(self) -> Result<Catalog, ParseError> {
        if !self.stack.is_empty() {
            return Err(ParseError::Xml {
                position: self.pos,
                message: "the file ends inside an element (truncated?)".into(),
            });
        }
        if !self.saw_root {
            return Err(ParseError::NotCatalog);
        }
        Ok(Catalog {
            origin: self.opts.origin.clone(),
            components: self.comps,
            skipped: self.skipped,
        })
    }
}

/// How many releases a component keeps.
const RELEASES: usize = 10;

fn keep_loc(slot: &mut Loc, rank: usize, s: String) {
    if rank < slot.rank {
        slot.rank = rank;
        slot.s = s;
    }
}
