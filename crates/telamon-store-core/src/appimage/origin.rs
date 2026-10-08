//! Where a downloaded file says it came from. Browsers (Firefox, Chrome and
//! the others on Linux) record the address in the file's extended attributes,
//! `user.xdg.origin.url` and `user.xdg.referrer.url`. Anything that can write
//! the file can write them, so this is a hint to show, never proof; no
//! attribute at all is also a finding (the file did not come from a browser,
//! or the attribute was stripped on the way).

use std::ffi::CString;
use std::fs::File;
use std::os::fd::AsRawFd;

use crate::text;

/// Longest value read, in bytes.
const MAX_XATTR: usize = 2048;

/// How an address was classified.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Origin {
    /// The browser did not say.
    Unknown,
    /// An `https` address to a public site; only the host is kept.
    Https { host: String },
    /// A plain `http` address: the file may have been changed on the way.
    Http { host: String },
    /// Something else (`file:`, `ftp:`, `blob:`, a malformed address).
    Other,
}

impl Origin {
    /// Whether the file is known to have come over HTTPS.
    pub fn is_secure(&self) -> bool {
        matches!(self, Origin::Https { .. })
    }
}

fn fget(file: &File, name: &str) -> Option<String> {
    let c = CString::new(name).ok()?;
    let mut buf = vec![0u8; MAX_XATTR + 1];
    // SAFETY: the descriptor is open for the call, `c` is NUL-terminated, and
    // `buf` is writable for the length given.
    let n = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            c.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if n <= 0 || n as usize > MAX_XATTR {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

/// The host of an `http://` address: ASCII, lowercase, without user name,
/// port, path, query or fragment; `None` when it does not look like a host.
fn http_host(url: &str) -> Option<String> {
    let rest = url.get(..7).filter(|s| s.eq_ignore_ascii_case("http://"))?;
    let rest = &url[rest.len()..];
    if !rest.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(h, port)| {
            if port.bytes().all(|b| b.is_ascii_digit()) {
                h
            } else {
                authority
            }
        })
        .to_ascii_lowercase();
    let ok = !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'));
    ok.then_some(host)
}

/// Classifies a recorded address.
pub fn classify(url: &str) -> Origin {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_XATTR || url.chars().any(|c| c.is_control()) {
        return Origin::Other;
    }
    if let Some(https) = crate::launch::https_url(url) {
        let host = https
            .strip_prefix("https://")
            .and_then(|r| r.split(['/', '?', '#']).next())
            .unwrap_or_default();
        return Origin::Https {
            host: text::clean(host, 100),
        };
    }
    if url
        .get(..7)
        .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
    {
        return match http_host(url) {
            Some(host) => Origin::Http { host },
            None => Origin::Other,
        };
    }
    Origin::Other
}

/// The origin of an open file: its `user.xdg.origin.url`, and when that is
/// missing or is not a web address (a `blob:` link), its referrer.
pub fn read(file: &File) -> Origin {
    let origin = fget(file, "user.xdg.origin.url").map(|u| classify(&u));
    let referrer = fget(file, "user.xdg.referrer.url").map(|u| classify(&u));
    match (origin, referrer) {
        (Some(o), _) if o != Origin::Other => o,
        (_, Some(r)) => r,
        (Some(o), None) => o,
        (None, None) => Origin::Unknown,
    }
}
