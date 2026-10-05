//! `.flatpakref` and `.flatpakrepo` files: untrusted input, parsed and checked
//! before anything is shown or added.
//!
//! The Store shows a confirmation dialog from what is parsed here, and flatpak
//! then acts on the same file, so the two must agree. The file is read with
//! [`KeyFile`], which follows GLib's rules (last key wins, groups merge). What
//! flatpak would accept but we cannot show faithfully makes the whole file
//! fail, never a half-shown one: an invalid value of a key that changes what
//! flatpak does is an error, and only the cosmetic `Icon` and `Homepage` are
//! dropped. Nothing is decided about trust here: a file without a key parses
//! with `key: None` and the dialog warns that it is unsigned.
//!
//! Only known keys are allowed in `[Flatpak Ref]` and `[Flatpak Repo]`; any
//! other key (a translation `key[locale]` too) refuses the file. `NoEnumerate`
//! is refused (it hides a remote). `NoDeps` (narrows dependency installs),
//! `Subset` (narrows the catalog) and `Version` are accepted, ignored and not
//! written back by `to_bytes`: they narrow and cannot widen trust. Other
//! groups are ignored, as flatpak ignores them. Every URL follows the launch policy
//! ([`crate::launch::https_url`]) and is kept in its normalized form.
//!
//! Names and shapes move to atlas-framework-flatpak later unchanged.

use std::fmt;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::keyfile::{ErrorKind, KeyFile, KeyFileError, Limits};
use crate::launch::https_url;
use crate::text::{clean, valid_id};

const REF_GROUP: &str = "Flatpak Ref";
const REPO_GROUP: &str = "Flatpak Repo";

/// Largest decoded GPG key: what fits one key file value once base64-encoded
/// (`keyfile::Limits::default().max_value / 4 * 3`), so any [`GpgKey`] can be
/// written by `to_bytes`.
pub const MAX_KEY_BYTES: usize = 48 * 1024;
/// The most a file may hold (`keyfile::Limits::default().max_bytes`). Callers
/// read at most this plus one byte and refuse a longer file.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TITLE: usize = 200;
const MAX_COMMENT: usize = 300;
const MAX_DESCRIPTION: usize = 2000;

/// A parsed `.flatpakref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakRef {
    pub name: String,
    /// `None` when the file has no `Branch` (flatpak then uses `master`).
    pub branch: Option<String>,
    /// The remote's URL, normalized by the launch policy.
    pub url: String,
    pub title: Option<String>,
    pub comment: Option<String>,
    pub description: Option<String>,
    /// Never fetched before the user confirms (a caller rule).
    pub icon: Option<String>,
    pub homepage: Option<String>,
    pub is_runtime: bool,
    /// `None` means unsigned, whatever the collection IDs: the dialog must warn.
    pub key: Option<GpgKey>,
    /// Where the runtime comes from. Kept, never followed here.
    pub runtime_repo: Option<String>,
    /// The remote name to use: the file's `SuggestRemoteName`, or `<Name>-origin`.
    pub suggest_remote_name: String,
    pub collection_id: Option<String>,
    pub deploy_collection_id: Option<String>,
}

/// A parsed `.flatpakrepo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakRepo {
    pub url: String,
    pub title: Option<String>,
    pub comment: Option<String>,
    pub description: Option<String>,
    /// Never fetched before the user confirms (a caller rule).
    pub icon: Option<String>,
    pub homepage: Option<String>,
    pub default_branch: Option<String>,
    pub key: Option<GpgKey>,
    pub collection_id: Option<String>,
    pub deploy_collection_id: Option<String>,
}

/// Why a file was refused. The text never holds file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefError {
    repo: bool,
    pub reason: Reason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// The file is not a readable key file, or one value in it could not be
    /// read; then the second field names its key.
    KeyFile(KeyFileError, Option<&'static str>),
    /// The required group is missing.
    NoGroup,
    /// A required key is missing.
    Missing(&'static str),
    /// A key has a value that is not allowed.
    Invalid(&'static str),
    /// The name for the remote is too long or not usable and the file doesn't suggest one.
    RemoteName,
    /// A struct that doesn't survive being written and read back.
    Unwritable,
    /// A key the Store doesn't know (cleaned, cut short).
    UnknownKey(String),
    /// A feature the Store doesn't support.
    Unsupported(Unsupported),
    /// The GPG key is unusable.
    Key(KeyError),
}

/// What the Store does not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// `Filter`: a local path.
    Filter,
    /// `Authenticator*`: remote authenticators.
    Authenticator,
    /// `NoEnumerate`: a remote hidden from app lists.
    NoEnumerate,
    /// `Title[..]`, `Comment[..]` or `Description[..]`.
    Translations,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unsupported::Filter => {
                "it names a filter file on this computer, which a downloaded file must not do"
            }
            Unsupported::Authenticator => {
                "it asks for a remote authenticator, which the Store does not support"
            }
            Unsupported::NoEnumerate => {
                "it hides the remote from app lists, which the Store does not support"
            }
            Unsupported::Translations => {
                "it has translated titles or descriptions, which flatpak could show differently"
            }
        })
    }
}

/// Why a GPG key was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    NotBase64,
    TooLarge,
    Empty,
    /// Not a well-formed run of OpenPGP packets.
    BadPacket,
    /// The first packet is not a public key.
    NotPublicKey,
    /// More than one primary key: all would be imported, one is shown.
    SeveralKeys,
    /// A key version other than 4 or 6 (v3 and v5 are obsolete).
    Version(u8),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::NotBase64 => f.write_str("the GPG key is not valid base64"),
            KeyError::TooLarge => f.write_str("the GPG key is too large"),
            KeyError::Empty => f.write_str("the GPG key is empty"),
            KeyError::BadPacket => f.write_str("the GPG key is not well-formed OpenPGP data"),
            KeyError::NotPublicKey => f.write_str("the GPG key data is not a public key"),
            KeyError::SeveralKeys => f.write_str("the GPG key data holds more than one key"),
            KeyError::Version(v) => write!(
                f,
                "the GPG key is a version {v} key, which is not supported"
            ),
        }
    }
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let what = if self.repo {
            "The file is not a Flatpak repository"
        } else {
            "The file is not a Flatpak reference"
        };
        match &self.reason {
            Reason::KeyFile(e, Some(k)) => match e.kind {
                ErrorKind::NotBool => write!(f, "{what}: {k} is not true or false"),
                ErrorKind::BadEscape => write!(f, "{what}: {k} has an invalid escape"),
                _ => write!(f, "{what}: {k}: {e}"),
            },
            Reason::KeyFile(e, None) => write!(f, "{what}: {e}"),
            Reason::NoGroup => write!(
                f,
                "{what}: it has no [{}] section",
                if self.repo { REPO_GROUP } else { REF_GROUP }
            ),
            Reason::Missing(k) => write!(f, "{what}: the {k} entry is missing"),
            Reason::Invalid(k) => write!(f, "{what}: the {k} entry is not valid"),
            Reason::RemoteName => write!(
                f,
                "{what}: its name is too long or not usable as a remote name (the file must suggest one)"
            ),
            Reason::Unwritable => {
                write!(f, "{what}: it could not be written back as given")
            }
            Reason::UnknownKey(k) => write!(f, "{what}: it has an unknown entry, \"{k}\""),
            Reason::Unsupported(why) => write!(f, "{what}: {why}"),
            Reason::Key(e) => write!(f, "{what}: {e}"),
        }
    }
}

impl RefError {
    /// True when the file was parsed as a `.flatpakrepo`.
    pub fn is_repo(&self) -> bool {
        self.repo
    }
}

impl std::error::Error for RefError {}

struct Ctx {
    repo: bool,
}

impl Ctx {
    fn e(&self, reason: Reason) -> RefError {
        RefError {
            repo: self.repo,
            reason,
        }
    }
}

fn limits() -> Limits {
    Limits::default()
}

/// Keys flatpak reads that only narrow what a remote offers.
const NARROWING: [&str; 3] = ["NoDeps", "Subset", "Version"];
const REF_KEYS: [&str; 14] = [
    "Name",
    "Branch",
    "Url",
    "Title",
    "Comment",
    "Description",
    "Icon",
    "Homepage",
    "IsRuntime",
    "GPGKey",
    "RuntimeRepo",
    "SuggestRemoteName",
    "CollectionID",
    "DeployCollectionID",
];
const REPO_KEYS: [&str; 10] = [
    "Url",
    "Title",
    "Comment",
    "Description",
    "Icon",
    "Homepage",
    "GPGKey",
    "CollectionID",
    "DeployCollectionID",
    "DefaultBranch",
];

/// Refuses any key of `group` that is not known.
fn check_keys(kf: &KeyFile, group: &str, known: &[&str], c: &Ctx) -> Result<(), RefError> {
    for k in kf.all_keys(group) {
        let base = k.split('[').next().unwrap_or(k);
        if c.repo && base == "Filter" {
            return Err(c.e(Reason::Unsupported(Unsupported::Filter)));
        }
        if c.repo && base.starts_with("Authenticator") {
            return Err(c.e(Reason::Unsupported(Unsupported::Authenticator)));
        }
        if k == "NoEnumerate" {
            return Err(c.e(Reason::Unsupported(Unsupported::NoEnumerate)));
        }
        if k.contains('[') {
            if matches!(base, "Title" | "Comment" | "Description") {
                return Err(c.e(Reason::Unsupported(Unsupported::Translations)));
            }
        } else if known.contains(&k) || NARROWING.contains(&k) {
            continue;
        }
        return Err(c.e(Reason::UnknownKey(plain_key(k))));
    }
    Ok(())
}

/// A key name for an error: only `[A-Za-z0-9_.-]` kept, the rest `?`, cut at 40.
fn plain_key(k: &str) -> String {
    k.chars()
        .take(40)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn open(bytes: &[u8], group: &'static str, known: &[&str], c: &Ctx) -> Result<KeyFile, RefError> {
    let kf = KeyFile::parse(bytes, &limits()).map_err(|e| c.e(Reason::KeyFile(e, None)))?;
    if !kf.has_group(group) {
        return Err(c.e(Reason::NoGroup));
    }
    check_keys(&kf, group, known, c)?;
    Ok(kf)
}

fn get(kf: &KeyFile, g: &str, k: &'static str, c: &Ctx) -> Result<Option<String>, RefError> {
    kf.string(g, k)
        .map_err(|e| c.e(Reason::KeyFile(e, Some(k))))
}

fn text(
    kf: &KeyFile,
    g: &str,
    k: &'static str,
    max: usize,
    c: &Ctx,
) -> Result<Option<String>, RefError> {
    Ok(get(kf, g, k, c)?
        .map(|s| clean(&s, max))
        .filter(|s| !s.is_empty()))
}

/// A cosmetic https URL: dropped when it is not valid.
fn soft_url(kf: &KeyFile, g: &str, k: &'static str) -> Option<String> {
    kf.string(g, k).ok().flatten().and_then(|u| https_url(&u))
}

fn hard_url(
    kf: &KeyFile,
    g: &str,
    k: &'static str,
    required: bool,
    c: &Ctx,
) -> Result<Option<String>, RefError> {
    match get(kf, g, k, c)? {
        None if required => Err(c.e(Reason::Missing(k))),
        None => Ok(None),
        Some(u) => https_url(&u)
            .map(Some)
            .ok_or_else(|| c.e(Reason::Invalid(k))),
    }
}

fn id_key(kf: &KeyFile, g: &str, k: &'static str, c: &Ctx) -> Result<Option<String>, RefError> {
    match get(kf, g, k, c)? {
        None => Ok(None),
        Some(v) if valid_id(&v) => Ok(Some(v)),
        Some(_) => Err(c.e(Reason::Invalid(k))),
    }
}

fn branch_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && !s.starts_with(['-', '.'])
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn branch_key(kf: &KeyFile, g: &str, k: &'static str, c: &Ctx) -> Result<Option<String>, RefError> {
    match get(kf, g, k, c)? {
        None => Ok(None),
        Some(v) if branch_ok(&v) => Ok(Some(v)),
        Some(_) => Err(c.e(Reason::Invalid(k))),
    }
}

fn key_of(kf: &KeyFile, g: &str, c: &Ctx) -> Result<Option<GpgKey>, RefError> {
    match get(kf, g, "GPGKey", c)? {
        None => Ok(None),
        Some(v) => GpgKey::from_base64(&v)
            .map(Some)
            .map_err(|e| c.e(Reason::Key(e))),
    }
}

/// Parses a `.flatpakref`.
///
/// Caller rules:
/// - libflatpak gets [`FlatpakRef::to_bytes`], never the original path or
///   bytes, so it acts on exactly what was parsed and shown.
/// - Read at most [`MAX_FILE_BYTES`] + 1 bytes and refuse a longer file.
/// - Before the dialog, look at the installed remotes for
///   [`suggested_remote_name`] and the URL: never overwrite an existing remote
///   from a file; if the URL already exists, the file's key is not used and
///   the dialog says so. Compare normalized URLs: pass an existing remote's
///   URL through [`crate::launch::https_url`] first, falling back to the raw
///   string.
/// - Before the dialog, if the effective name is an installed remote with a
///   different URL, refuse, or set a new unique `suggest_remote_name` and show
///   that name. Never overwrite.
/// - Show the full fingerprint and URL.
/// - On the transaction, connect the add-new-remote signal and return FALSE
///   unless the Store's own dialog confirmed that exact remote name, normalized
///   URL and fingerprint; the transaction's ready step compares them again. A fetched `RuntimeRepo` file goes through
///   [`parse_flatpakrepo`] with the same rules.
pub fn parse_flatpakref(bytes: &[u8]) -> Result<FlatpakRef, RefError> {
    let c = Ctx { repo: false };
    let kf = open(bytes, REF_GROUP, &REF_KEYS, &c)?;
    let g = REF_GROUP;
    let name = match get(&kf, g, "Name", &c)? {
        None => return Err(c.e(Reason::Missing("Name"))),
        Some(n) if valid_id(&n) => n,
        Some(_) => return Err(c.e(Reason::Invalid("Name"))),
    };
    let url = hard_url(&kf, g, "Url", true, &c)?.ok_or_else(|| c.e(Reason::Missing("Url")))?;
    let is_runtime = kf
        .bool(g, "IsRuntime")
        .map_err(|e| c.e(Reason::KeyFile(e, Some("IsRuntime"))))?
        .unwrap_or(false);
    let runtime_repo = hard_url(&kf, g, "RuntimeRepo", false, &c)?;
    // A name flatpak would use for the remote: wrong means a refusal, since
    // flatpak would otherwise pick a name the dialog never showed.
    let suggest_remote_name = match get(&kf, g, "SuggestRemoteName", &c)? {
        Some(n) if valid_remote_name(&n) => n,
        Some(_) => return Err(c.e(Reason::Invalid("SuggestRemoteName"))),
        None => {
            let n = format!("{name}-origin");
            if !valid_remote_name(&n) {
                return Err(c.e(Reason::RemoteName));
            }
            n
        }
    };
    Ok(FlatpakRef {
        name,
        branch: branch_key(&kf, g, "Branch", &c)?,
        url,
        title: text(&kf, g, "Title", MAX_TITLE, &c)?,
        comment: text(&kf, g, "Comment", MAX_COMMENT, &c)?,
        description: text(&kf, g, "Description", MAX_DESCRIPTION, &c)?,
        icon: soft_url(&kf, g, "Icon"),
        homepage: soft_url(&kf, g, "Homepage"),
        is_runtime,
        key: key_of(&kf, g, &c)?,
        runtime_repo,
        suggest_remote_name,
        collection_id: id_key(&kf, g, "CollectionID", &c)?,
        deploy_collection_id: id_key(&kf, g, "DeployCollectionID", &c)?,
    })
}

/// Parses a `.flatpakrepo`. Caller rules as for [`parse_flatpakref`]. Refuses `Filter` (a local path) and any
/// `Authenticator*` key (remote authenticators are not supported).
pub fn parse_flatpakrepo(bytes: &[u8]) -> Result<FlatpakRepo, RefError> {
    let c = Ctx { repo: true };
    let kf = open(bytes, REPO_GROUP, &REPO_KEYS, &c)?;
    let g = REPO_GROUP;
    let url = hard_url(&kf, g, "Url", true, &c)?.ok_or_else(|| c.e(Reason::Missing("Url")))?;
    Ok(FlatpakRepo {
        url,
        title: text(&kf, g, "Title", MAX_TITLE, &c)?,
        comment: text(&kf, g, "Comment", MAX_COMMENT, &c)?,
        description: text(&kf, g, "Description", MAX_DESCRIPTION, &c)?,
        icon: soft_url(&kf, g, "Icon"),
        homepage: soft_url(&kf, g, "Homepage"),
        default_branch: branch_key(&kf, g, "DefaultBranch", &c)?,
        key: key_of(&kf, g, &c)?,
        collection_id: id_key(&kf, g, "CollectionID", &c)?,
        deploy_collection_id: id_key(&kf, g, "DeployCollectionID", &c)?,
    })
}

/// Flatpak allows no `/`, no leading `-` or `.`, printable characters. Stricter
/// here: `[A-Za-z0-9_.-]`, 1 to 64 bytes, not starting with `-` or `.`.
pub fn valid_remote_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with(['-', '.'])
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The remote name flatpak would use for this file: `SuggestRemoteName` when
/// valid, else `<Name>-origin`, as `flatpak install` of a ref file names the
/// remote it creates. [`parse_flatpakref`] refuses a file where that is not a
/// valid remote name, and fills `suggest_remote_name` with it, so for a parsed
/// ref the shown name is exactly the one [`FlatpakRef::to_bytes`] hands to
/// libflatpak. Flatpak reuses an existing remote with the same URL and numbers
/// a taken name; the caller sees that when it checks installed remotes.
pub fn suggested_remote_name(r: &FlatpakRef) -> String {
    if valid_remote_name(&r.suggest_remote_name) {
        r.suggest_remote_name.clone()
    } else {
        format!("{}-origin", r.name)
    }
}

/// A GPG public key from a flatpakref or flatpakrepo, checked: OpenPGP packets,
/// one primary key (v4 or v6) first, at most 64 KiB.
#[derive(Clone, PartialEq, Eq)]
pub struct GpgKey {
    bytes: Vec<u8>,
    fingerprint: String,
}

impl fmt::Debug for GpgKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GpgKey({})", self.fingerprint)
    }
}

impl GpgKey {
    /// Decodes strict base64 (RFC 4648, padding required; only space, tab, CR
    /// and LF may be added between characters) and checks the key.
    pub fn from_base64(s: &str) -> Result<GpgKey, KeyError> {
        GpgKey::from_bytes(decode_base64(s)?)
    }

    /// Checks binary OpenPGP key data.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<GpgKey, KeyError> {
        if bytes.is_empty() {
            return Err(KeyError::Empty);
        }
        if bytes.len() > MAX_KEY_BYTES {
            return Err(KeyError::TooLarge);
        }
        let fingerprint = fingerprint_of(&bytes)?;
        Ok(GpgKey { bytes, fingerprint })
    }

    /// The primary key's fingerprint, uppercase hex (40 digits for v4, 64 for v6).
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The decoded key data, for libflatpak.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Standard-alphabet base64 with required padding. Rejects any other
/// character, data after padding, and non-zero unused bits.
fn decode_base64(s: &str) -> Result<Vec<u8>, KeyError> {
    let mut vals: Vec<u8> = Vec::with_capacity(s.len());
    let mut pad = 0usize;
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(KeyError::NotBase64),
        };
        if pad > 0 {
            return Err(KeyError::NotBase64);
        }
        vals.push(v);
        if vals.len() > MAX_KEY_BYTES / 3 * 4 + 4 {
            return Err(KeyError::TooLarge);
        }
    }
    if vals.is_empty() {
        return Err(KeyError::Empty);
    }
    // Whole quads, the last one padded to four with exactly the right count.
    let rem = vals.len() % 4;
    if rem == 1 || pad != (4 - rem) % 4 || pad > 2 {
        return Err(KeyError::NotBase64);
    }
    let mut out = Vec::with_capacity(vals.len() / 4 * 3 + 3);
    for q in vals.chunks(4) {
        let mut n = 0u32;
        for (i, v) in q.iter().enumerate() {
            n |= u32::from(*v) << (18 - 6 * i);
        }
        let take = q.len() * 6 / 8;
        let be = n.to_be_bytes();
        if be[1 + take..4].iter().any(|b| *b != 0) {
            return Err(KeyError::NotBase64);
        }
        out.extend_from_slice(&be[1..1 + take]);
    }
    Ok(out)
}

/// One packet: its tag and body.
struct Packet<'a> {
    tag: u8,
    body: &'a [u8],
}

/// Reads one packet header and body from the front of `buf` (RFC 9580 section
/// 4). Partial and indeterminate lengths are refused: key material doesn't use
/// them.
fn next_packet(buf: &[u8]) -> Result<(Packet<'_>, &[u8]), KeyError> {
    let bad = KeyError::BadPacket;
    let first = *buf.first().ok_or(bad)?;
    if first & 0x80 == 0 {
        return Err(bad);
    }
    let (tag, len, hdr): (u8, usize, usize) = if first & 0x40 == 0 {
        let tag = (first >> 2) & 0x0F;
        match first & 3 {
            0 => (tag, usize::from(*buf.get(1).ok_or(bad)?), 2),
            1 => {
                let b = buf.get(1..3).ok_or(bad)?;
                (tag, usize::from(u16::from_be_bytes([b[0], b[1]])), 3)
            }
            2 => {
                let b = buf.get(1..5).ok_or(bad)?;
                (
                    tag,
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize,
                    5,
                )
            }
            _ => return Err(bad),
        }
    } else {
        let tag = first & 0x3F;
        let l0 = *buf.get(1).ok_or(bad)?;
        match l0 {
            0..=191 => (tag, usize::from(l0), 2),
            192..=223 => {
                let l1 = *buf.get(2).ok_or(bad)?;
                (
                    tag,
                    ((usize::from(l0) - 192) << 8) + usize::from(l1) + 192,
                    3,
                )
            }
            224..=254 => return Err(bad),
            255 => {
                let b = buf.get(2..6).ok_or(bad)?;
                (
                    tag,
                    u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize,
                    6,
                )
            }
        }
    };
    let end = hdr.checked_add(len).ok_or(bad)?;
    if end > buf.len() {
        return Err(bad);
    }
    Ok((
        Packet {
            tag,
            body: &buf[hdr..end],
        },
        &buf[end..],
    ))
}

/// Walks every packet, checks the first is the only Public-Key packet and
/// returns its fingerprint.
fn fingerprint_of(data: &[u8]) -> Result<String, KeyError> {
    let (first, mut rest) = next_packet(data)?;
    if first.tag != 6 {
        return Err(KeyError::NotPublicKey);
    }
    while !rest.is_empty() {
        let (p, r) = next_packet(rest)?;
        if p.tag == 6 {
            return Err(KeyError::SeveralKeys);
        }
        // Signature, Trust, User ID, Public-Subkey, User Attribute only:
        // GnuPG unpacks compressed data and reads secret keys and the rest.
        if !matches!(p.tag, 2 | 12 | 13 | 14 | 17) {
            return Err(KeyError::BadPacket);
        }
        rest = r;
    }
    let body = first.body;
    let digest: Vec<u8> = match body.first().copied() {
        // version, 4-byte time, algorithm, then at least one MPI length.
        Some(4) if body.len() >= 8 && body.len() <= 0xFFFF => {
            let mut h = Sha1::new();
            h.update([0x99]);
            h.update((body.len() as u16).to_be_bytes());
            h.update(body);
            h.finalize().to_vec()
        }
        // version, time, algorithm, 4-byte key material length, material.
        Some(6) if body.len() >= 11 => {
            let mut h = Sha256::new();
            h.update([0x9B]);
            h.update((body.len() as u32).to_be_bytes());
            h.update(body);
            h.finalize().to_vec()
        }
        Some(4 | 6) => return Err(KeyError::BadPacket),
        Some(v) => return Err(KeyError::Version(v)),
        None => return Err(KeyError::BadPacket),
    };
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02X}"));
    }
    Ok(s)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(d: &[u8]) -> String {
    let mut o = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                o.push(char::from(B64[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                o.push('=');
            }
        }
    }
    o
}

/// A string value as GLib writes it: `\\`, newline, tab, CR escaped, and a
/// leading space as `\s`.
fn escape(v: &str) -> String {
    let mut o = String::with_capacity(v.len());
    for (i, c) in v.chars().enumerate() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            ' ' if i == 0 => o.push_str("\\s"),
            c => o.push(c),
        }
    }
    o
}

fn put(out: &mut String, k: &str, v: Option<&str>) {
    if let Some(v) = v {
        out.push_str(k);
        out.push('=');
        out.push_str(&escape(v));
        out.push('\n');
    }
}

fn put_common(
    out: &mut String,
    title: &Option<String>,
    comment: &Option<String>,
    description: &Option<String>,
    icon: &Option<String>,
    homepage: &Option<String>,
) {
    put(out, "Title", title.as_deref());
    put(out, "Comment", comment.as_deref());
    put(out, "Description", description.as_deref());
    put(out, "Icon", icon.as_deref().and_then(https_url).as_deref());
    put(
        out,
        "Homepage",
        homepage.as_deref().and_then(https_url).as_deref(),
    );
}

fn put_ids(out: &mut String, key: &Option<GpgKey>, coll: &Option<String>, deploy: &Option<String>) {
    if let Some(k) = key {
        put(out, "GPGKey", Some(&encode_base64(k.bytes())));
    }
    put(out, "CollectionID", coll.as_deref());
    put(out, "DeployCollectionID", deploy.as_deref());
}

impl FlatpakRef {
    fn verified(&self, out: Vec<u8>) -> Result<Vec<u8>, RefError> {
        match parse_flatpakref(&out) {
            Ok(back) if back == *self => Ok(out),
            _ => Err(RefError {
                repo: false,
                reason: Reason::Unwritable,
            }),
        }
    }
    /// A canonical `.flatpakref` of what was parsed: only modelled keys, in a
    /// fixed order, normalized URLs, the key re-encoded and
    /// `SuggestRemoteName` set to the name the dialog shows. Parsing it gives
    /// this value back. This is what libflatpak gets. The pub fields can be set
    /// to anything, so the result is parsed again and must equal `self`;
    /// otherwise this is an error and no bytes are returned.
    pub fn to_bytes(&self) -> Result<Vec<u8>, RefError> {
        let mut o = format!("[{REF_GROUP}]\n");
        put(&mut o, "Name", Some(&self.name));
        put(&mut o, "Branch", self.branch.as_deref());
        put(&mut o, "Url", Some(&self.url));
        put_common(
            &mut o,
            &self.title,
            &self.comment,
            &self.description,
            &self.icon,
            &self.homepage,
        );
        put(
            &mut o,
            "IsRuntime",
            Some(if self.is_runtime { "true" } else { "false" }),
        );
        put_ids(
            &mut o,
            &self.key,
            &self.collection_id,
            &self.deploy_collection_id,
        );
        put(&mut o, "RuntimeRepo", self.runtime_repo.as_deref());
        put(
            &mut o,
            "SuggestRemoteName",
            Some(&suggested_remote_name(self)),
        );
        self.verified(o.into_bytes())
    }
}

impl FlatpakRepo {
    fn verified(&self, out: Vec<u8>) -> Result<Vec<u8>, RefError> {
        match parse_flatpakrepo(&out) {
            Ok(back) if back == *self => Ok(out),
            _ => Err(RefError {
                repo: true,
                reason: Reason::Unwritable,
            }),
        }
    }
    /// A canonical `.flatpakrepo` of what was parsed, checked by parsing it
    /// again, as for
    /// [`FlatpakRef::to_bytes`].
    pub fn to_bytes(&self) -> Result<Vec<u8>, RefError> {
        let mut o = format!("[{REPO_GROUP}]\n");
        put(&mut o, "Url", Some(&self.url));
        put_common(
            &mut o,
            &self.title,
            &self.comment,
            &self.description,
            &self.icon,
            &self.homepage,
        );
        put(&mut o, "DefaultBranch", self.default_branch.as_deref());
        put_ids(
            &mut o,
            &self.key,
            &self.collection_id,
            &self.deploy_collection_id,
        );
        self.verified(o.into_bytes())
    }
}
