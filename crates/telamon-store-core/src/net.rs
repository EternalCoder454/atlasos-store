//! The Store's one HTTPS client, for the few things it fetches itself: the
//! Flathub API, a `.flatpakrepo` from a link, and the Telamon apps' catalog,
//! releases and bundles from GitHub (`native`; bundles stream through
//! [`download`]). Everything it returns is
//! untrusted bytes; the caller parses them with limits.
//!
//! Rules, from docs/DESIGN.md (Trust):
//!
//! - the URL passes [`crate::launch::https_url`] (https, port 443, a public
//!   DNS name), and so does every redirect target (at most [`MAX_REDIRECTS`],
//!   followed here, not by the HTTP library);
//! - the name is resolved by [`PublicOnly`], which drops every loopback,
//!   private, link-local, shared (CGNAT), multicast, documentation and
//!   otherwise non-global address, so a name that points at the local
//!   network is refused when the connection is made, not only when the URL is
//!   read (a DNS answer can change between the two);
//! - a timeout for the whole request and a cap on the body;
//! - no proxy from the environment (a proxy is not subject to the address
//!   rule above), no cookies, no credentials, a User-Agent that names the
//!   Store and its version and nothing else.
//!
//! Blocking: run on a worker thread, never the GUI thread.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// Redirects followed before giving up.
pub const MAX_REDIRECTS: u32 = 3;

/// What to ask for and how much to accept.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// The `Accept` header, such as `application/json`.
    pub accept: &'a str,
    /// The most body bytes read; more is [`NetError::TooLarge`].
    pub max_bytes: u64,
    /// The whole request, connecting and reading.
    pub timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetError {
    /// The URL (or a redirect's) is not an accepted https address.
    BadUrl,
    /// The server answered with this HTTP status.
    Status(u16),
    /// The body is larger than the request allows.
    TooLarge,
    /// A redirect without a usable target, or too many.
    Redirect,
    /// The name resolves only to addresses the Store does not connect to.
    NotPublic,
    /// No answer in time.
    TimedOut,
    /// Anything else (no network, TLS failure...). Plain words.
    Failed(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::BadUrl => f.write_str("The address is not a secure (https) web address."),
            NetError::Status(code) => write!(f, "The server answered with an error ({code})."),
            NetError::TooLarge => f.write_str("The answer is larger than the Store accepts."),
            NetError::Redirect => f.write_str("The server sent the Store somewhere it won't go."),
            NetError::NotPublic => f.write_str(
                "The address leads to this computer or its local network, not the internet.",
            ),
            NetError::TimedOut => f.write_str("The server did not answer in time."),
            NetError::Failed(why) => write!(f, "Could not connect: {why}"),
        }
    }
}

impl std::error::Error for NetError {}

/// A resolver that keeps only addresses on the public internet.
#[derive(Debug, Default)]
pub struct PublicOnly {
    inner: DefaultResolver,
}

impl Resolver for PublicOnly {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let all = self.inner.resolve(uri, config, timeout)?;
        let mut kept = self.empty();
        for addr in all.iter().filter(|a| is_public(a.ip())) {
            kept.push(*addr);
        }
        if kept.is_empty() {
            // The message is how `get` recognises this case.
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                NOT_PUBLIC,
            )));
        }
        Ok(kept)
    }
}

const NOT_PUBLIC: &str = "telamon-store: address is not public";

/// Whether the Store may connect to `ip`: a global unicast address.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 0.0.0.0/8, "this network"
        || a == 0
        // 100.64.0.0/10, shared address space (carrier-grade NAT)
        || (a == 100 && (64..=127).contains(&b))
        // 192.0.0.0/24, IETF protocol assignments
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15, benchmarking
        || (a == 198 && (b == 18 || b == 19))
        // 240.0.0.0/4, reserved
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    // An IPv4 address carried in an IPv6 one is judged as the IPv4 address.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let s = ip.segments();
    // 64:ff9b::/96 (NAT64) carries one in its last 32 bits.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6].iter().all(|&x| x == 0) {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return public_v4(v4);
    }
    // 2002::/16 (6to4) carries one right after the prefix.
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        return public_v4(v4);
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // fc00::/7 unique local
        || (s[0] & 0xfe00) == 0xfc00
        // fe80::/10 link-local, fec0::/10 site-local (deprecated)
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        // 2001::/32 Teredo (a tunnel), 2001:10::/28 ORCHID and 2001:20::/28
        // ORCHIDv2 (hashes, not hosts)
        || (s[0] == 0x2001 && (s[1] == 0 || (s[1] & 0xfff0) == 0x0010 || (s[1] & 0xfff0) == 0x0020))
        // ::/96, the deprecated IPv4-compatible form
        || s[..6].iter().all(|&x| x == 0)
        // 64:ff9b:1::/48, local-use NAT64
        || (s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1)
        // 100::/64 discard-only
        || (s[0] == 0x0100 && s[1..4].iter().all(|&x| x == 0)))
}

fn agent() -> ureq::Agent {
    let config = Config::builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .user_agent(concat!("telamon-store/", env!("CARGO_PKG_VERSION")))
        .build();
    ureq::Agent::with_parts(config, DefaultConnector::default(), PublicOnly::default())
}

/// Sends the request and follows redirects (each one through the same
/// checks); returns the final successful response, body unread.
fn open(url: &str, request: &Request<'_>) -> Result<ureq::http::Response<ureq::Body>, NetError> {
    let agent = agent();
    let deadline = std::time::Instant::now() + request.timeout;
    let mut url = crate::launch::https_url(url).ok_or(NetError::BadUrl)?;
    for _ in 0..=MAX_REDIRECTS {
        let left = deadline
            .checked_duration_since(std::time::Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(NetError::TimedOut)?;
        let response = agent
            .get(&url)
            .config()
            .timeout_global(Some(left))
            .build()
            .header("Accept", request.accept)
            .call()
            .map_err(map_error)?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or(NetError::Redirect)?;
            url = redirect_target(&url, location).ok_or(NetError::Redirect)?;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(NetError::Status(status));
        }
        return Ok(response);
    }
    Err(NetError::Redirect)
}

/// Fetches `url` and returns the body.
///
/// Blocking, up to `request.timeout` plus a little for the redirects (each
/// one gets what is left of the time).
pub fn get(url: &str, request: &Request<'_>) -> Result<Vec<u8>, NetError> {
    let mut response = open(url, request)?;
    response
        .body_mut()
        .with_config()
        .limit(request.max_bytes)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => NetError::TooLarge,
            other => map_error(other),
        })
}

/// Fetches `url` and hands the body to `sink` in pieces, never more than
/// `request.max_bytes` in all (more is [`NetError::TooLarge`], and the
/// pieces already given stay given). A `sink` error stops the download.
/// Returns the number of bytes delivered. For bodies too large to hold in
/// memory; `request.timeout` covers the whole transfer.
pub fn download(
    url: &str,
    request: &Request<'_>,
    sink: &mut dyn FnMut(&[u8]) -> std::io::Result<()>,
) -> Result<u64, NetError> {
    use std::io::Read;
    let mut response = open(url, request)?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(request.max_bytes.saturating_add(1))
        .reader();
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(NetError::TimedOut),
            Err(e) => {
                return Err(NetError::Failed(crate::text::clean(&e.to_string(), 200)));
            }
        };
        total += n as u64;
        if total > request.max_bytes {
            return Err(NetError::TooLarge);
        }
        sink(&buf[..n]).map_err(|e| NetError::Failed(crate::text::clean(&e.to_string(), 200)))?;
    }
}

fn map_error(e: ureq::Error) -> NetError {
    match e {
        ureq::Error::Timeout(_) => NetError::TimedOut,
        ureq::Error::BodyExceedsLimit(_) => NetError::TooLarge,
        ureq::Error::Io(ref io) if io.to_string().contains(NOT_PUBLIC) => NetError::NotPublic,
        ureq::Error::Io(ref io) if io.kind() == std::io::ErrorKind::TimedOut => NetError::TimedOut,
        other => NetError::Failed(crate::text::clean(&other.to_string(), 200)),
    }
}

/// The URL a redirect from `base` leads to, if it is acceptable: an absolute
/// https address, or a path on the same host. Anything else (another
/// scheme, a `//host` reference, a path with `..`) is refused, and so is
/// whatever [`crate::launch::https_url`] refuses.
pub fn redirect_target(base: &str, location: &str) -> Option<String> {
    if location.starts_with("//") {
        return None;
    }
    let target = if location.starts_with('/') {
        let host_end = base[8..]
            .find(['/', '?', '#'])
            .map_or(base.len(), |i| i + 8);
        format!("{}{}", &base[..host_end], location)
    } else {
        location.to_string()
    };
    crate::launch::https_url(&target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_global_addresses_are_public() {
        for ok in [
            "8.8.8.8",
            "151.101.1.69",
            "2606:4700:4700::1111",
            "2a00:1450:4001:81b::200e",
        ] {
            assert!(is_public(ip(ok)), "{ok}");
        }
        for bad in [
            "0.0.0.0",
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::7f00:1",
            "64:ff9b::a00:1",
            "2002:7f00:1::1",
            "2002:c0a8:101::1",
            "::10.0.0.1",
            "::8.8.8.8",
            "64:ff9b:1::1",
            "2001:10::1",
            "2001:2f::1",
            "2001::1",
        ] {
            assert!(!is_public(ip(bad)), "{bad}");
        }
        // The edges of the shared range are not caught by mistake.
        assert!(is_public(ip("100.63.255.255")));
        assert!(is_public(ip("100.128.0.1")));
        assert!(is_public(ip("172.15.0.1")));
        assert!(is_public(ip("172.32.0.1")));
        assert!(is_public(ip("::ffff:8.8.8.8")));
    }

    #[test]
    fn redirects_stay_on_https_and_public_names() {
        let base = "https://example.org/a/b?x=1";
        assert_eq!(
            redirect_target(base, "https://dl.example.org/repo").as_deref(),
            Some("https://dl.example.org/repo")
        );
        assert_eq!(
            redirect_target(base, "/other").as_deref(),
            Some("https://example.org/other")
        );
        for bad in [
            "http://example.org/",
            "//evil.example.net/",
            "https://localhost/",
            "https://127.0.0.1/",
            "https://[::1]/",
            "https://example.org:8443/",
            "https://example.org/../x",
            "https://user@example.org/",
            "file:///etc/passwd",
            "ftp://example.org/",
            "relative",
            "",
            "/a b",
        ] {
            assert_eq!(redirect_target(base, bad), None, "{bad}");
        }
    }

    #[test]
    fn urls_are_checked_before_any_connection() {
        let request = Request {
            accept: "*/*",
            max_bytes: 10,
            timeout: Duration::from_secs(1),
        };
        for bad in [
            "http://example.org/",
            "https://localhost/",
            "https://10.0.0.1/",
            "https://example.org:8443/",
            "ftp://example.org/",
        ] {
            assert_eq!(get(bad, &request), Err(NetError::BadUrl), "{bad}");
        }
    }

    #[test]
    fn errors_read_as_plain_words() {
        for e in [
            NetError::BadUrl,
            NetError::Status(404),
            NetError::TooLarge,
            NetError::Redirect,
            NetError::NotPublic,
            NetError::TimedOut,
            NetError::Failed("no route".into()),
        ] {
            let text = e.to_string();
            assert!(text.ends_with('.') || text.contains("no route"), "{text}");
        }
    }
}
