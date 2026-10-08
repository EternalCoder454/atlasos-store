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

/// Whether `bytes` is an icon the Store will keep: a PNG of a sane size, or a
/// small plain SVG without anything that pulls in another file or script.
pub fn icon_kind(bytes: &[u8]) -> Option<IconKind> {
    if bytes.is_empty() || bytes.len() as u64 > MAX_ICON {
        return None;
    }
    if let Some((w, h)) = png_size(bytes) {
        return ((1..=MAX_ICON_SIDE).contains(&w) && (1..=MAX_ICON_SIDE).contains(&h))
            .then_some(IconKind::Png);
    }
    if bytes.len() > 512 << 10 || bytes.contains(&0) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let lower = text.to_ascii_lowercase();
    let head = lower.trim_start_matches('\u{feff}').trim_start();
    if !(head.starts_with("<?xml") || head.starts_with("<svg") || head.starts_with("<!--"))
        || !lower.contains("<svg")
    {
        return None;
    }
    // Anything that can pull in another file or run: scripts, entities,
    // external references (`href` in any form, `<use>`, `<image>`, with or
    // without a namespace prefix), styles that can import, embedded HTML.
    const REFUSED: [&str; 15] = [
        "<script",
        "<!entity",
        "<!doctype",
        "<image",
        ":image",
        "feimage",
        "<use",
        ":use",
        "foreignobject",
        "<style",
        "@import",
        "<a ",
        ":a ",
        "javascript:",
        "<iframe",
    ];
    if REFUSED.iter().any(|r| lower.contains(r)) || external_href(&lower) {
        return None;
    }
    Some(IconKind::Svg)
}

/// Whether some `href` points anywhere but inside the document (`#id`):
/// gradients and clips refer that way, a file or a URL is not allowed.
fn external_href(lower: &str) -> bool {
    let mut rest = lower;
    while let Some(i) = rest.find("href") {
        let after = rest[i + 4..].trim_start();
        let Some(value) = after.strip_prefix('=').map(str::trim_start) else {
            return true;
        };
        if !(value.starts_with("\"#") || value.starts_with("'#")) {
            return true;
        }
        rest = &rest[i + 4..];
    }
    false
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
