//! What an AppImage says about itself, read from inside its squashfs without
//! running it: the `.desktop` file at the top, the AppStream metainfo and the
//! icon. All of it is written by whoever made the file, so every value is
//! cleaned and capped here, and the icon must look like an image.

use super::squash::{Kind, Tree};
use crate::appstream::lang::langs_from_env;
use crate::appstream::{Limits as XmlLimits, ParseOptions, parse_metainfo};
use crate::keyfile::{KeyFile, Limits as KeyLimits};
use crate::text;

/// Longest name shown, in characters.
pub const MAX_NAME: usize = 100;
const MAX_VERSION: usize = 100;
const MAX_TEXT: usize = 300;
/// Largest icon read, in bytes.
pub const MAX_ICON: u64 = 1 << 20;
/// Largest icon side accepted, in pixels.
pub const MAX_ICON_SIDE: u32 = 2048;
/// Largest metainfo or desktop file read, in bytes.
const MAX_TEXT_FILE: u64 = 1 << 20;

/// An icon's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconKind {
    Png,
    Svg,
}

impl IconKind {
    pub fn ext(self) -> &'static str {
        match self {
            IconKind::Png => "png",
            IconKind::Svg => "svg",
        }
    }
}

/// An icon that passed `icon_kind`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub kind: IconKind,
    pub bytes: Vec<u8>,
}

/// The pixel size of a PNG, from its header.
pub fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || b[..8] != *b"\x89PNG\r\n\x1a\n" || b[12..16] != *b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
    Some((w, h))
}

/// The rest of a PNG's header (`IHDR`, which `png_size` found): 13 bytes of
/// data, a bit depth that the colour type allows, and the only compression
/// and filter methods PNG has. A decoder sizes its buffers from this.
fn png_header_ok(b: &[u8]) -> bool {
    if b.len() < 29 || b[8..12] != 13u32.to_be_bytes() {
        return false;
    }
    let (depth, color, compression, filter, interlace) = (b[24], b[25], b[26], b[27], b[28]);
    let depth_ok = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    depth_ok && compression == 0 && filter == 0 && interlace <= 1
}

/// Whether `bytes` is an icon the Store will keep: a PNG of a sane size, or a
/// small plain SVG without anything that pulls in another file or script.
pub fn icon_kind(bytes: &[u8]) -> Option<IconKind> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ICON {
        return None;
    }
    if let Some((w, h)) = png_size(bytes) {
        return ((1..=MAX_ICON_SIDE).contains(&w)
            && (1..=MAX_ICON_SIDE).contains(&h)
            && png_header_ok(bytes))
        .then_some(IconKind::Png);
    }
    if bytes.len() > 512 << 10 || bytes.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    svg_ok(text).then_some(IconKind::Svg)
}

/// Elements (by local name, whatever their prefix) an icon may not have:
/// they pull in another file, run code, or can retarget any attribute (the
/// animation elements can set an `href`).
const REFUSED_ELEMENTS: [&str; 30] = [
    "script",
    "style",
    "image",
    "use",
    "a",
    "feimage",
    "foreignobject",
    "iframe",
    "embed",
    "object",
    "applet",
    "link",
    "base",
    "meta",
    "animate",
    "set",
    "handler",
    "listener",
    "audio",
    "video",
    "canvas",
    "html",
    "head",
    "body",
    "form",
    "input",
    "cursor",
    "font-face-uri",
    "color-profile",
    "tref",
];
/// ... and `mpath` and `discard`, which refer to other elements by `href`.
const REFUSED_MORE: [&str; 2] = ["mpath", "discard"];
/// Deepest element nesting and most elements accepted: a renderer recurses
/// on the first and walks all of the second.
const SVG_MAX_DEPTH: usize = 64;
const SVG_MAX_ELEMENTS: usize = 20_000;

/// Reads the SVG as XML, tag by tag, and accepts it only when nothing in it
/// can load another file, run code or hide from this check. It is a
/// tokenizer, not a parser: anything it does not understand is refused, so
/// a document that a real XML parser would read differently is refused too.
///
/// - the only entities are the five predefined ones (no `&#..;` that could
///   spell a name, no DOCTYPE to define more);
/// - the only declaration is `<?xml ...?>` at the very start, and its
///   encoding, if it names one, is UTF-8 (a parser that honored UTF-7 would
///   read other tags than these);
/// - no element of `REFUSED_ELEMENTS`, with any prefix, in any case;
/// - no attribute that is an event handler (`on...`), none with a backslash
///   (a CSS escape), `javascript:`, `data:` or `vbscript:`; every `href`
///   (any prefix) is `#id` and every `url(...)` points at `#id`.
fn svg_ok(text: &str) -> bool {
    if !entities_ok(text) {
        return false;
    }
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut at = start;
    // The names of the elements still open: a closing tag must be the last
    // one's (a mismatch is not XML, and parsers differ in what they do with it).
    let mut open: Vec<&str> = Vec::new();
    let mut elements = 0usize;
    let mut root = false;
    while let Some(off) = text[at..].find('<') {
        at += off;
        let rest = &text[at..];
        if let Some(body) = rest.strip_prefix("<!--") {
            let Some(end) = body.find("-->") else {
                return false;
            };
            // `--` inside a comment is not XML.
            if body[..end].contains("--") || body[..end].ends_with('-') {
                return false;
            }
            at += 4 + end + 3;
        } else if rest.starts_with("<![CDATA[") {
            let Some(end) = rest.find("]]>") else {
                return false;
            };
            at += end + 3;
        } else if rest.starts_with("<?") {
            // Only the XML declaration, only first.
            let Some(len) = xml_declaration(rest, text.is_ascii()).filter(|_| at == start) else {
                return false;
            };
            at += len;
        } else if rest.starts_with("<!") {
            return false;
        } else if let Some(close) = rest.strip_prefix("</") {
            let end = close.find('>').unwrap_or(close.len());
            if end == close.len() || !close[..end].trim_end().bytes().all(name_byte) {
                return false;
            }
            if open.pop() != Some(close[..end].trim_end()) {
                return false;
            }
            at += 2 + end + 1;
        } else {
            let Some((len, name, empty)) = open_tag(rest) else {
                return false;
            };
            let local = name
                .rsplit(':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if local.is_empty()
                || REFUSED_ELEMENTS.contains(&local.as_str())
                || REFUSED_MORE.contains(&local.as_str())
            {
                return false;
            }
            // One root, and it is the svg.
            if open.is_empty() {
                if root || local != "svg" {
                    return false;
                }
                root = true;
            }
            elements += 1;
            if elements > SVG_MAX_ELEMENTS {
                return false;
            }
            if !empty {
                open.push(name);
                if open.len() > SVG_MAX_DEPTH {
                    return false;
                }
            }
            at += len;
        }
    }
    root && open.is_empty()
}

/// Only `&amp; &lt; &gt; &quot; &apos;`.
fn entities_ok(text: &str) -> bool {
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        rest = &rest[i + 1..];
        if !["amp;", "lt;", "gt;", "quot;", "apos;"]
            .iter()
            .any(|e| rest.starts_with(e))
        {
            return false;
        }
    }
    true
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'-' | b'.')
}

/// `<?xml version="1.0" encoding="UTF-8" standalone="no"?>`: its length in
/// bytes when it is well formed and the encoding, if it names one, reads these
/// bytes as the text they are. UTF-8 does. US-ASCII, ISO-8859-1 (`latin1`) and
/// windows-1252 give the same text as UTF-8 when the whole document is ASCII
/// (`ascii`), and only then: a file with other bytes would read differently.
/// Any other encoding is refused (a parser that honored UTF-7 or UTF-16 would
/// read other tags than these). Numeric character references stay refused
/// (`entities_ok`), whatever the encoding: they can spell a name.
fn xml_declaration(rest: &str, ascii: bool) -> Option<usize> {
    let end = rest.get(2..)?.find("?>")? + 2;
    let decl = rest[2..end].to_ascii_lowercase();
    if !decl.starts_with("xml") || decl.contains('<') || !decl[3..].starts_with(char::is_whitespace)
    {
        return None;
    }
    if let Some(i) = decl.find("encoding") {
        let value = decl[i + 8..].trim_start().strip_prefix('=')?.trim_start();
        let quote = value.chars().next().filter(|q| matches!(q, '"' | '\''))?;
        let name = value[1..].split(quote).next()?;
        let same_as_utf8 = match name {
            "utf-8" => true,
            "us-ascii" | "iso-8859-1" | "latin1" | "windows-1252" => ascii,
            _ => false,
        };
        if !same_as_utf8 {
            return None;
        }
    }
    Some(end + 2)
}

/// Reads `<name attr="v" ...>` or `.../>` at the start of `rest`: how many
/// bytes it takes, the name, whether it is empty (`/>`). `None` for anything
/// malformed or any attribute the icon may not have.
fn open_tag(rest: &str) -> Option<(usize, &str, bool)> {
    let b = rest.as_bytes();
    let mut i = 1;
    while i < b.len() && name_byte(b[i]) {
        i += 1;
    }
    let name = &rest[1..i];
    if name.is_empty() || !(b[1].is_ascii_alphabetic() || b[1] == b'_' || b[1] == b':') {
        return None;
    }
    loop {
        let ws = b[i..]
            .iter()
            .take_while(|c| c.is_ascii_whitespace())
            .count();
        i += ws;
        match b.get(i)? {
            b'>' => return Some((i + 1, name, false)),
            b'/' => {
                return (b.get(i + 1) == Some(&b'>')).then_some((i + 2, name, true));
            }
            _ => {}
        }
        // An attribute needs space before it.
        if ws == 0 {
            return None;
        }
        let start = i;
        while i < b.len() && name_byte(b[i]) {
            i += 1;
        }
        let attr = &rest[start..i];
        if attr.is_empty() || !(b[start].is_ascii_alphabetic() || matches!(b[start], b'_' | b':')) {
            return None;
        }
        i += b[i..]
            .iter()
            .take_while(|c| c.is_ascii_whitespace())
            .count();
        if b.get(i) != Some(&b'=') {
            return None;
        }
        i += 1;
        i += b[i..]
            .iter()
            .take_while(|c| c.is_ascii_whitespace())
            .count();
        let quote = *b.get(i)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value_start = i + 1;
        let len = b[value_start..].iter().position(|c| *c == quote)?;
        let value = &rest[value_start..value_start + len];
        if !attribute_ok(attr, value) {
            return None;
        }
        i = value_start + len + 1;
    }
}

/// Whether an attribute may be in an icon.
fn attribute_ok(name: &str, value: &str) -> bool {
    let local = name
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    // Event handlers (`onload`, `onclick`, `onbegin`...); the base of every
    // relative reference.
    if local.starts_with("on") || local == "base" {
        return false;
    }
    if value.contains('<') || value.contains('\\') {
        return false;
    }
    // Quotes written as entities read as quotes.
    let v = value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .to_ascii_lowercase();
    if local == "href" && !v.starts_with('#') {
        return false;
    }
    for scheme in ["javascript:", "vbscript:", "data:", "@import"] {
        if v.contains(scheme) {
            return false;
        }
    }
    // Every CSS reference points inside the document.
    for func in [
        "url(",
        "image(",
        "image-set(",
        "src(",
        "element(",
        "cross-fade(",
    ] {
        let mut rest = v.as_str();
        while let Some(i) = rest.find(func) {
            rest = rest[i + func.len()..].trim_start();
            if func != "url(" {
                return false;
            }
            let inner = rest.strip_prefix(['"', '\'']).unwrap_or(rest).trim_start();
            if !inner.starts_with('#') {
                return false;
            }
        }
    }
    true
}

/// A name that may be looked up as an icon inside the image.
pub fn icon_name_ok(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && !s.starts_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

/// What was found. Every text is cleaned and capped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub name: String,
    pub summary: String,
    pub version: String,
    pub publisher: String,
    /// A valid AppStream or desktop file ID, or "".
    pub app_id: String,
    pub icon: Option<Icon>,
    /// Plain notes about what was missing or refused.
    pub notes: Vec<&'static str>,
}

fn key_limits() -> KeyLimits {
    KeyLimits {
        max_bytes: 128 << 10,
        max_lines: 2000,
        max_groups: 64,
        max_keys: 1000,
        max_value: 8192,
    }
}

fn is_file(tree: &Tree<'_>, path: &str) -> bool {
    matches!(tree.kind(path), Some(Kind::File(_) | Kind::Symlink))
}

/// The desktop file's stem as an app ID, when it reads like one.
fn id_from_stem(path: &str) -> String {
    let stem = path.rsplit('/').next().unwrap_or("");
    let stem = stem.strip_suffix(".desktop").unwrap_or(stem);
    if text::valid_id(stem) && stem.split('.').count() >= 3 {
        stem.to_string()
    } else {
        String::new()
    }
}

fn first_file<'t>(tree: &'t Tree<'_>, prefix: &str, suffixes: &[&str]) -> Vec<&'t str> {
    tree.list(prefix)
        .into_iter()
        .filter(|p| {
            let rest = &p[prefix.len()..];
            !rest.contains('/') && suffixes.iter().any(|s| rest.ends_with(s)) && is_file(tree, p)
        })
        .collect()
}

fn read_icon(tree: &Tree<'_>, desktop_icon: Option<&str>) -> Option<Icon> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(name) = desktop_icon {
        // The largest PNG of the hicolor theme up to 512, nearest 256, first.
        let mut sized: Vec<(u32, String)> = Vec::new();
        for p in tree.list("usr/share/icons/hicolor/") {
            let mut parts = p.split('/');
            let size = parts.nth(4).unwrap_or("");
            let rest: Vec<&str> = parts.collect();
            if rest.len() == 2 && rest[0] == "apps" && rest[1] == format!("{name}.png") {
                let side: u32 = size
                    .split('x')
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
                if (16..=512).contains(&side) {
                    sized.push((side.abs_diff(256), p.to_string()));
                }
            }
        }
        sized.sort();
        candidates.extend(sized.into_iter().map(|(_, p)| p).take(3));
        candidates.push(format!("{name}.png"));
    }
    candidates.push(".DirIcon".to_string());
    if let Some(name) = desktop_icon {
        candidates.push(format!("usr/share/pixmaps/{name}.png"));
        candidates.push(format!("{name}.svg"));
        candidates.push(format!("usr/share/icons/hicolor/scalable/apps/{name}.svg"));
        candidates.push(format!("usr/share/pixmaps/{name}.svg"));
    }
    for path in candidates.iter().take(10) {
        if !is_file(tree, path) {
            continue;
        }
        let Ok(bytes) = tree.read(path, MAX_ICON) else {
            continue;
        };
        if let Some(kind) = icon_kind(&bytes) {
            return Some(Icon { kind, bytes });
        }
    }
    None
}

/// Reads the desktop file, metainfo and icon out of the tree.
pub fn extract(tree: &Tree<'_>) -> Meta {
    let mut meta = Meta::default();

    // The desktop file: the first one at the top, else the first under
    // usr/share/applications.
    let mut desktops = first_file(tree, "", &[".desktop"]);
    if desktops.is_empty() {
        desktops = first_file(tree, "usr/share/applications/", &[".desktop"]);
    }
    let mut desktop_name = String::new();
    let mut desktop_icon: Option<String> = None;
    let mut desktop_comment = String::new();
    let mut desktop_version = String::new();
    let mut desktop_vendor = String::new();
    let mut stem_id = String::new();
    let mut found_desktop = false;
    for path in desktops.iter().take(3) {
        let Ok(bytes) = tree.read(path, MAX_TEXT_FILE) else {
            continue;
        };
        let Ok(kf) = KeyFile::parse(&bytes, &key_limits()) else {
            continue;
        };
        let g = "Desktop Entry";
        if !kf.has_group(g) {
            continue;
        }
        let get = |k: &str| kf.string(g, k).ok().flatten().unwrap_or_default();
        if get("Type") != "Application" {
            continue;
        }
        found_desktop = true;
        desktop_name = text::clean(&get("Name"), MAX_NAME);
        desktop_comment = text::clean(&get("Comment"), MAX_TEXT);
        desktop_version = text::clean(&get("X-AppImage-Version"), MAX_VERSION);
        desktop_vendor = text::clean(&get("X-AppImage-Vendor"), MAX_NAME);
        let icon = get("Icon");
        desktop_icon = icon_name_ok(&icon).then_some(icon);
        stem_id = id_from_stem(path);
        break;
    }
    if !found_desktop {
        meta.notes.push("There is no usable desktop entry inside.");
    }

    // The metainfo: the first that parses to a valid component.
    let mut dirs = first_file(
        tree,
        "usr/share/metainfo/",
        &[".appdata.xml", ".metainfo.xml"],
    );
    dirs.extend(first_file(
        tree,
        "usr/share/appdata/",
        &[".appdata.xml", ".metainfo.xml"],
    ));
    let opts = ParseOptions {
        origin: String::new(),
        langs: langs_from_env(),
        limits: XmlLimits::default(),
    };
    let mut component = None;
    for path in dirs.iter().take(3) {
        let Ok(bytes) = tree.read(path, MAX_TEXT_FILE) else {
            continue;
        };
        if let Ok(Some(c)) = parse_metainfo(bytes.as_slice(), &opts) {
            component = Some(c);
            break;
        }
    }
    if component.is_none() {
        meta.notes
            .push("There is no usable AppStream metadata inside.");
    }

    if let Some(c) = &component {
        meta.name = text::clean(&c.name, MAX_NAME);
        meta.summary = text::clean(&c.summary, MAX_TEXT);
        meta.publisher = text::clean(&c.developer, MAX_NAME);
        meta.version = c
            .releases
            .first()
            .map(|r| text::clean(&r.version, MAX_VERSION))
            .unwrap_or_default();
        let id = c.id_bare();
        if text::valid_id(id) {
            meta.app_id = id.to_string();
        }
    }
    if meta.name.is_empty() {
        meta.name = desktop_name;
    }
    if meta.summary.is_empty() {
        meta.summary = desktop_comment;
    }
    if meta.version.is_empty() {
        meta.version = desktop_version;
    }
    if meta.publisher.is_empty() {
        meta.publisher = desktop_vendor;
    }
    if meta.app_id.is_empty() {
        meta.app_id = stem_id;
    }
    meta.icon = read_icon(tree, desktop_icon.as_deref());
    meta
}
