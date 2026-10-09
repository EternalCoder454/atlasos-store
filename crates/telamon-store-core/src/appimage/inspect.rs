//! Looking inside an AppImage without running it. [`inspect`] reads the ELF
//! headers, the squashfs (desktop entry, metainfo, icon), the signature
//! sections, the download address and the SHA-256 of the whole file.
//!
//! Because everything inside the file is the work of whoever made it, the
//! Store runs this in a helper process of its own (`telamon-store
//! --appimage-inspect <file>`, see [`super::helper`]) with a memory and time
//! limit, and treats what comes back as untrusted again: [`Inspection::sanitize`]
//! cleans every text and drops an icon that is not an image.

use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::format::{self, Format};
use super::meta::{self, Icon, IconKind};
use super::origin::{self, Origin};
use super::sign::{self, Signature};
use super::squash::{self, Limits};
use crate::text;

/// Smallest file that can be an AppImage.
const MIN_SIZE: u64 = 4096;
/// Largest file looked at (it is hashed whole).
pub const MAX_SIZE: u64 = 4 << 30;

/// What was found. Texts are cleaned and capped; `icon` is not part of the
/// serialized form (the helper sends it after the header).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub format: Format,
    pub size: u64,
    /// SHA-256 of the whole file, lowercase hex.
    pub sha256: String,
    /// The file's name, cleaned.
    pub file_name: String,
    /// The contents were read. False for a type 1 AppImage and for one whose
    /// squashfs could not be read; `note` says why.
    pub inspected: bool,
    pub note: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub summary: String,
    /// A valid AppStream or desktop ID, or "".
    pub app_id: String,
    pub icon_kind: Option<IconKind>,
    pub signature: Signature,
    pub origin: Origin,
    #[serde(skip)]
    pub icon: Option<Icon>,
}

/// Why a file was not inspected at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectError {
    /// Could not be opened or read.
    Io(String),
    NotAFile,
    /// Too small, or no AppImage marker.
    NotAppImage,
    TooLarge,
    /// The helper process failed.
    Helper(String),
}

impl std::fmt::Display for InspectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InspectError::Io(e) => write!(f, "The file could not be read ({e})."),
            InspectError::NotAFile => f.write_str("This is not a regular file."),
            InspectError::NotAppImage => f.write_str("This file isn't an AppImage."),
            InspectError::TooLarge => f.write_str("This file is too large to look at (over 4 GB)."),
            InspectError::Helper(e) => write!(f, "Telamon couldn't look at this file ({e})."),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The app's name from its file name: no `.AppImage`, no architecture or
/// separators at the end.
pub fn name_from_file(file_name: &str) -> String {
    let mut s = file_name;
    if s.len() >= 9
        && s.is_char_boundary(s.len() - 9)
        && s[s.len() - 9..].eq_ignore_ascii_case(".appimage")
    {
        s = &s[..s.len() - 9];
    }
    let mut s = s.to_string();
    for suffix in [
        "-x86_64", "-x86-64", "_x86_64", "-amd64", "_amd64", "-aarch64", "-arm64",
    ] {
        if let Some(t) = s.strip_suffix(suffix) {
            s = t.to_string();
        }
    }
    text::clean(&s.replace(['_', '-'], " "), meta::MAX_NAME)
}

/// SHA-256 of the file and, when `zero` has ranges, of the file with those
/// ranges (start, end) read as zeros: what a signature covers.
fn hash_file(file: &File, len: u64, zero: &[(u64, u64)]) -> io::Result<(String, Option<String>)> {
    let mut full = Sha256::new();
    let mut signed = (!zero.is_empty()).then(Sha256::new);
    let mut buf = vec![0u8; 1 << 20];
    let mut at = 0u64;
    while at < len {
        let n = ((len - at) as usize).min(buf.len());
        file.read_exact_at(&mut buf[..n], at)?;
        full.update(&buf[..n]);
        if let Some(s) = signed.as_mut() {
            for &(a, b) in zero {
                let (lo, hi) = (a.max(at), b.min(at + n as u64));
                if lo < hi {
                    buf[(lo - at) as usize..(hi - at) as usize].fill(0);
                }
            }
            s.update(&buf[..n]);
        }
        at += n as u64;
    }
    Ok((hex(&full.finalize()), signed.map(|s| hex(&s.finalize()))))
}

/// An AppImage that has been opened, hashed and checked for a signature, and
/// whose squashfs has not been read yet. The inspection is in two stages so
/// that the helper process can close itself in between (see
/// [`super::sandbox`]): [`prepare`] does everything that needs more than the
/// open file (`gpgv` is started in it), [`Prepared::finish`] does the complex
/// parsing of the file's contents and needs nothing but the open file and
/// memory.
pub struct Prepared {
    file: File,
    len: u64,
    out: Inspection,
    /// Where the squashfs starts, for a type 2 file whose header was read.
    squash_at: Option<u64>,
}

/// Opens `path`, reads the ELF headers and the signature sections, hashes the
/// whole file (and the variant with the signature sections zeroed) and checks
/// the signature. Never runs the file.
pub fn prepare(path: &Path) -> Result<Prepared, InspectError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| InspectError::Io(e.kind().to_string()))?;
    let md = file
        .metadata()
        .map_err(|e| InspectError::Io(e.kind().to_string()))?;
    if !md.is_file() {
        return Err(InspectError::NotAFile);
    }
    let len = md.size();
    if len < MIN_SIZE {
        return Err(InspectError::NotAppImage);
    }
    if len > MAX_SIZE {
        return Err(InspectError::TooLarge);
    }
    let kind = format::sniff_file(&file).ok_or(InspectError::NotAppImage)?;
    let file_name = text::clean(
        &path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        200,
    );
    let mut out = Inspection {
        format: kind,
        size: len,
        sha256: String::new(),
        file_name: file_name.clone(),
        inspected: false,
        note: String::new(),
        name: name_from_file(&file_name),
        version: String::new(),
        publisher: String::new(),
        summary: String::new(),
        app_id: String::new(),
        icon_kind: None,
        signature: Signature::None,
        origin: origin::read(&file),
        icon: None,
    };
    let io_err = |e: io::Error| InspectError::Io(e.kind().to_string());

    if kind == Format::Type1 {
        out.note =
            "This AppImage uses an old format (type 1) that Telamon can't look inside.".into();
        out.sha256 = hash_file(&file, len, &[]).map_err(io_err)?.0;
        return Ok(Prepared {
            file,
            len,
            out,
            squash_at: None,
        });
    }

    // Type 2: the ELF runtime, then the squashfs.
    let mut zero: Vec<(u64, u64)> = Vec::new();
    let mut sig_bytes = None;
    let mut key_bytes = None;
    let mut sig_unreadable = false;
    let mut squash_at = None;
    match format::read_elf(&file, len) {
        Ok(elf) => {
            for name in [".sha256_sig", ".sig_key"] {
                if let Some(s) = elf.section(name)
                    && s.offset.saturating_add(s.size) <= len
                {
                    zero.push((s.offset, s.offset + s.size));
                }
            }
            sig_bytes = elf.read_section(&file, len, ".sha256_sig", sign::MAX_SIG);
            key_bytes = elf.read_section(&file, len, ".sig_key", sign::MAX_KEY);
            sig_unreadable = (elf.section(".sha256_sig").is_some() && sig_bytes.is_none())
                || (elf.section(".sig_key").is_some() && key_bytes.is_none());
            squash_at = Some(elf.end);
        }
        Err(_) => out.note = "Telamon couldn't read this file's header.".into(),
    }

    let signed_present = sig_bytes
        .as_deref()
        .is_some_and(|b| !sign::trimmed(b).is_empty());
    zero.sort_unstable();
    let ranges = if signed_present { zero.as_slice() } else { &[] };
    let (full, signed) = hash_file(&file, len, ranges).map_err(io_err)?;
    out.sha256 = full;
    out.signature = if sig_unreadable {
        Signature::Unchecked
    } else if !signed_present {
        Signature::None
    } else if let (Some(gpgv), Some(digest), Some(sig)) =
        (sign::find_gpgv(), signed, sig_bytes.as_deref())
    {
        sign::verify(
            &gpgv,
            sig,
            key_bytes.as_deref().unwrap_or_default(),
            &digest,
        )
    } else {
        Signature::Unchecked
    };
    Ok(Prepared {
        file,
        len,
        out,
        squash_at,
    })
}

impl Prepared {
    /// Reads the squashfs: desktop entry, metainfo and icon.
    pub fn finish(self, limits: &Limits) -> Inspection {
        let Prepared {
            file,
            len,
            mut out,
            squash_at,
        } = self;
        let Some(base) = squash_at else {
            return out;
        };
        match squash::with_tree(&file, base, len, limits, meta::extract) {
            Ok(m) => {
                out.inspected = true;
                out.name = if m.name.is_empty() { out.name } else { m.name };
                out.version = m.version;
                out.publisher = m.publisher;
                out.summary = m.summary;
                out.app_id = m.app_id;
                out.icon_kind = m.icon.as_ref().map(|i| i.kind);
                out.icon = m.icon;
                if let Some(n) = m.notes.first() {
                    out.note = (*n).to_string();
                }
            }
            Err(e) => out.note = format!("Telamon couldn't look inside this file: {e}."),
        }
        out
    }
}

/// Looks inside the AppImage at `path`. Never runs it. (In-process: the
/// helper process uses [`prepare`] and [`Prepared::finish`] with its sandbox
/// in between.)
pub fn inspect(path: &Path, limits: &Limits) -> Result<Inspection, InspectError> {
    Ok(prepare(path)?.finish(limits))
}

fn host_ok(h: &str) -> bool {
    (1..=253).contains(&h.len())
        && h.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
}

impl Inspection {
    /// Cleans every text and checks every field, whatever produced this (the
    /// helper process is untrusted too): the result can be shown and used.
    pub fn sanitize(&mut self) {
        let clean = |s: &str, n: usize| text::clean(s, n);
        self.file_name = clean(&self.file_name, 200);
        self.note = clean(&self.note, 300);
        self.name = clean(&self.name, meta::MAX_NAME);
        if self.name.is_empty() {
            self.name = name_from_file(&self.file_name);
        }
        self.version = clean(&self.version, 100);
        self.publisher = clean(&self.publisher, meta::MAX_NAME);
        self.summary = clean(&self.summary, 300);
        if !self.app_id.is_empty() && !text::valid_id(&self.app_id) {
            self.app_id.clear();
        }
        if !(self.sha256.is_empty()
            || (self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))))
        {
            self.sha256.clear();
        }
        self.origin = match std::mem::replace(&mut self.origin, Origin::Unknown) {
            Origin::Https { host } if host_ok(&host) => Origin::Https { host },
            Origin::Http { host } if host_ok(&host) => Origin::Http { host },
            Origin::Unknown => Origin::Unknown,
            _ => Origin::Other,
        };
        if let Signature::Signed { fingerprint } = &self.signature {
            let ok = (40..=64).contains(&fingerprint.len())
                && fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
            if !ok {
                self.signature = Signature::Unchecked;
            }
        }
        let kind = self.icon.as_ref().and_then(|i| meta::icon_kind(&i.bytes));
        match (kind, self.icon_kind) {
            (Some(k), Some(d)) if k == d => {}
            _ => self.icon = None,
        }
        self.icon_kind = self.icon.as_ref().map(|i| i.kind);
    }

    /// The helper's output: one line of JSON, then the icon's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = serde_json::to_vec(self).unwrap_or_default();
        out.push(b'\n');
        if let Some(i) = &self.icon {
            out.extend_from_slice(&i.bytes);
        }
        out
    }

    /// Reads [`Inspection::encode`]'s output, then sanitizes it.
    pub fn decode(bytes: &[u8]) -> Result<Inspection, InspectError> {
        let end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| InspectError::Helper("no answer".into()))?;
        if let Some(rest) = bytes[..end].strip_prefix(b"ERROR ") {
            let msg = String::from_utf8_lossy(rest);
            return Err(InspectError::Helper(text::clean(&msg, 200)));
        }
        let mut insp: Inspection = serde_json::from_slice(&bytes[..end])
            .map_err(|_| InspectError::Helper("unreadable answer".into()))?;
        let tail = &bytes[end + 1..];
        if let Some(kind) = insp.icon_kind
            && !tail.is_empty()
        {
            insp.icon = Some(Icon {
                kind,
                bytes: tail.to_vec(),
            });
        }
        insp.sanitize();
        Ok(insp)
    }
}
