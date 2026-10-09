//! The signature an AppImage can carry. `appimagetool --sign` hashes the
//! file with SHA-256, signs the 64-character lowercase hex of that hash with
//! GnuPG (a detached, ASCII-armored signature) and writes the signature into
//! the ELF section `.sha256_sig` and the signer's armored public key into
//! `.sig_key`. Both sections are zero bytes in an unsigned file, and the hash
//! is taken with both zeroed.
//!
//! The embedded key proves nothing about who made the file (whoever changed
//! the file can embed their own key), so a good signature is shown as
//! "signed by <fingerprint>, but this is not a key Telamon knows"; Telamon
//! has no list of keys it trusts. The check runs `gpgv` with a keyring made
//! of the embedded key alone, in a private temporary folder: the user's own
//! keyring is never read or changed, and the key is never trusted anywhere.

use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Longest signature and key read from the file, in bytes.
pub const MAX_SIG: u64 = 16 * 1024;
pub const MAX_KEY: u64 = 64 * 1024;
/// How long `gpgv` may run.
const GPGV_TIMEOUT: Duration = Duration::from_secs(10);
/// Most of `gpgv`'s status output read.
const MAX_STATUS: u64 = 64 * 1024;

/// What is known about the file's signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum Signature {
    /// No signature in the file.
    None,
    /// The file has a signature that this computer could not check (no
    /// `gpgv`, or the key or signature cannot be read).
    Unchecked,
    /// A signature that does not match the file and the key it carries.
    Wrong,
    /// The signature matches the file and the key it carries; the key is not
    /// one Telamon knows. `fingerprint` is uppercase hex.
    Signed { fingerprint: String },
}

impl Signature {
    /// A fingerprint as groups of four, `ABCD 1234 ...`.
    pub fn grouped(fingerprint: &str) -> String {
        let mut out = String::new();
        for (i, c) in fingerprint.chars().enumerate() {
            if i > 0 && i % 4 == 0 {
                out.push(' ');
            }
            out.push(c);
        }
        out
    }
}

/// Bytes of a section without the zero padding after the data (and any
/// whitespace); empty when it is all padding.
pub fn trimmed(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|b| *b != 0 && !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    &bytes[..end]
}

fn b64_value(c: u8) -> Option<u8> {
    Some(match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// The binary OpenPGP data of an ASCII-armored block, or the input itself if
/// it is not armored but looks like OpenPGP packets. `None` when it is
/// neither. The CRC line is not checked (`gpgv` checks the packets).
pub fn dearmor(input: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(input).ok();
    let Some(text) = text.filter(|t| t.trim_start().starts_with("-----BEGIN PGP")) else {
        // Binary packets start with a tag byte with the top bit set.
        return (input.first().is_some_and(|b| b & 0x80 != 0)).then(|| input.to_vec());
    };
    let mut body = String::new();
    let mut in_body = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN PGP") {
            in_body = false;
            body.clear();
            // The header lines come first, then an empty line.
            continue;
        }
        if line.starts_with("-----END PGP") {
            break;
        }
        if !in_body {
            if line.is_empty() {
                in_body = true;
            }
            continue;
        }
        if line.starts_with('=') {
            continue;
        }
        body.push_str(line);
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in body.bytes() {
        if c == b'=' {
            break;
        }
        acc = (acc << 6) | u32::from(b64_value(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// `gpgv` from the system's folders only: the `PATH` of a user session holds
/// folders the user (and any program running as them) can write to, and a
/// planted `gpgv` there could say "signed" about anything.
pub fn find_gpgv() -> Option<PathBuf> {
    ["/usr/bin/gpgv", "/bin/gpgv"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

fn temp_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    for n in 0..16u32 {
        let mut r = [0u8; 8];
        // SAFETY: fills the buffer; a short or failed read only makes the name less random.
        let _ = unsafe { libc::getrandom(r.as_mut_ptr().cast(), r.len(), 0) };
        let name = format!(
            "telamon-store-gpgv-{}-{n}-{:016x}",
            std::process::id(),
            u64::from_le_bytes(r)
        );
        let dir = base.join(name);
        if std::fs::DirBuilder::new().mode(0o700).create(&dir).is_ok() {
            return Some(dir);
        }
    }
    None
}

/// Checks `signature` (armored or binary) over `message` with `key` (armored
/// or binary) using `gpgv`. `digest_hex` is the message: the hex digest.
pub fn verify(gpgv: &Path, signature: &[u8], key: &[u8], message: &str) -> Signature {
    verify_within(gpgv, signature, key, message, GPGV_TIMEOUT)
}

/// [`verify`] with a time limit of the caller's choosing.
pub fn verify_within(
    gpgv: &Path,
    signature: &[u8],
    key: &[u8],
    message: &str,
    timeout: Duration,
) -> Signature {
    let (Some(sig), Some(key)) = (trimmed_nonempty(signature), dearmor(trimmed(key))) else {
        return Signature::Unchecked;
    };
    let Some(dir) = temp_dir() else {
        return Signature::Unchecked;
    };
    let result = run_gpgv(gpgv, &dir, sig, &key, message, timeout);
    // On every path, a timeout included: the folder holds the file's key.
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn trimmed_nonempty(b: &[u8]) -> Option<&[u8]> {
    let t = trimmed(b);
    (!t.is_empty()).then_some(t)
}

/// Limits for the `gpgv` child, set between fork and exec (only system calls
/// that are safe there): its input is a stranger's key and signature.
fn limit_gpgv() {
    let set = |resource: libc::__rlimit_resource_t, value: libc::rlim_t| {
        let lim = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: setrlimit reads the struct; a failure only leaves the
        // limit as it was.
        unsafe { libc::setrlimit(resource, &lim) };
    };
    set(libc::RLIMIT_CORE, 0);
    // It writes nothing bigger than a lock file.
    set(libc::RLIMIT_FSIZE, 1 << 20);
    set(libc::RLIMIT_CPU, 20);
    set(libc::RLIMIT_AS, 1 << 30);
    set(libc::RLIMIT_NOFILE, 64);
    // SAFETY: prctl with these arguments only sets a flag on this process:
    // it dies with the helper that started it, whatever happens to that.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
    }
}

fn run_gpgv(
    gpgv: &Path,
    dir: &Path,
    sig: &[u8],
    key: &[u8],
    message: &str,
    timeout: Duration,
) -> Signature {
    let (kr, sf, df, home) = (
        dir.join("keyring.gpg"),
        dir.join("sig"),
        dir.join("data"),
        dir.join("home"),
    );
    let wrote = std::fs::write(&kr, key)
        .and_then(|()| std::fs::write(&sf, sig))
        .and_then(|()| std::fs::write(&df, message.as_bytes()))
        .and_then(|()| std::fs::DirBuilder::new().mode(0o700).create(&home));
    if wrote.is_err() {
        return Signature::Unchecked;
    }
    // By argv, an empty environment (no agent socket, no proxy, no config
    // folder of the user's), a private home, no input, its own process group
    // and resource limits. `gpgv` has no config file and never talks to an
    // agent or the network; the keyring is the file given.
    let mut cmd = Command::new(gpgv);
    cmd.env_clear()
        .env("LC_ALL", "C")
        .env("GNUPGHOME", &home)
        .arg("--homedir")
        .arg(&home)
        .args(["--status-fd", "1", "--keyring"])
        .arg(&kr)
        .arg(&sf)
        .arg(&df)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    // SAFETY: the closure only calls setrlimit and prctl, which are safe
    // between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            limit_gpgv();
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else {
        return Signature::Unchecked;
    };
    // Read as it writes: a full pipe must not stall it, and what is read is
    // capped (the rest of a talkative `gpgv` gets SIGPIPE).
    let mut stdout = child.stdout.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_STATUS).read_to_end(&mut out);
        out
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                // The whole group, not only the child: whatever it started.
                super::helper::kill_group(child.id());
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Signature::Unchecked;
            }
        }
    }
    let out = reader.join().unwrap_or_default();
    parse_status(&String::from_utf8_lossy(&out))
}

/// Reads gpgv's `--status-fd` lines.
pub fn parse_status(status: &str) -> Signature {
    let mut fingerprint = None;
    let mut valid = false;
    let mut bad = false;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("[GNUPG:] ") else {
            continue;
        };
        let mut f = rest.split_whitespace();
        match f.next() {
            Some("VALIDSIG") => {
                let fields: Vec<&str> = f.collect();
                // fields: fpr date ts expire version reserved pkalgo hashalgo sigclass primary-fpr
                let fpr = fields.get(9).or(fields.first()).copied().unwrap_or("");
                if (40..=64).contains(&fpr.len()) && fpr.bytes().all(|b| b.is_ascii_hexdigit()) {
                    fingerprint = Some(fpr.to_ascii_uppercase());
                    valid = true;
                }
            }
            Some("EXPKEYSIG" | "EXPSIG") => valid = true,
            Some("BADSIG" | "ERRSIG" | "NO_PUBKEY" | "REVKEYSIG") => bad = true,
            _ => {}
        }
    }
    match (valid, bad, fingerprint) {
        (true, false, Some(fingerprint)) => Signature::Signed { fingerprint },
        (true, false, None) => Signature::Unchecked,
        _ => Signature::Wrong,
    }
}
