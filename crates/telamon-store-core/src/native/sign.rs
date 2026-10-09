//! Release signatures: who may vouch for an app's `telamon-bundle.json`.
//!
//! A release carries `telamon-bundle.json.minisig`, a [minisign] signature
//! (Ed25519 over the BLAKE2b-512 hash of the file, the format the real
//! `minisign -S` writes) over exactly the manifest's bytes. The catalog entry
//! of the app lists the public keys that may sign it ([`Signer`]); a release
//! is used only when one of them verifies it. The manifest names the archive's
//! SHA-256, so the signature covers the archive too.
//!
//! What counts:
//!
//! - the signature must verify with a listed key; a good signature by any
//!   other key is a failure;
//! - the signature must be the current, hashed kind (`ED`); the legacy kind
//!   (`Ed`, the file itself signed) is refused;
//! - the trusted comment is part of the signature file but is not used for
//!   anything: not the version, not the file name, not the time;
//! - the signature file is at most [`MAX_SIGNATURE`] bytes of UTF-8.
//!
//! Only minisign exists today. A catalog entry's `signers` list is typed so a
//! later kind (Sigstore / GitHub artifact attestations) can be added beside it
//! without a new catalog schema: a kind this Store does not know is ignored
//! when the entry is read (see [`super::catalog`]).
//!
//! [minisign]: https://jedisct1.github.io/minisign/

use minisign_verify::{Error as VerifyError, PublicKey, Signature};

use super::{Error, err};

/// The release file holding the manifest's signature.
pub const SIGNATURE_NAME: &str = "telamon-bundle.json.minisig";
/// Largest signature file read (a real one is about 330 bytes).
pub const MAX_SIGNATURE: u64 = 4096;
/// Most signers one catalog entry may list.
pub const MAX_SIGNERS: usize = 4;
/// The one signer type the Store knows.
pub const MINISIGN: &str = "minisign";

/// A key that may sign an app's releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    /// The minisign public key as written in the catalog (`RW...`).
    pub key: String,
    /// Its key ID as `minisign` prints it: 16 upper-case hex digits.
    pub key_id: String,
}

/// Whether `s` has the shape of a key ID shown to the user (and recorded).
pub fn valid_key_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F'))
}

fn b64_value(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The 42 bytes of a minisign public key (`Ed`, key ID, key) from its 56
/// characters of base64, with nothing before, after or between.
fn key_bytes(b64: &str) -> Option<[u8; 42]> {
    let s = b64.as_bytes();
    if s.len() != 56 {
        return None;
    }
    let mut out = [0u8; 42];
    for (i, chunk) in s.chunks(4).enumerate() {
        let mut n = 0u32;
        for &c in chunk {
            n = (n << 6) | u32::from(b64_value(c)?);
        }
        out[i * 3] = (n >> 16) as u8;
        out[i * 3 + 1] = (n >> 8) as u8;
        out[i * 3 + 2] = n as u8;
    }
    Some(out)
}

impl Signer {
    /// A minisign signer from the `RW...` string of a public key, or `None`
    /// when it is not one the Store can verify with.
    pub fn minisign(key: &str) -> Option<Signer> {
        let bytes = key_bytes(key)?;
        // Keys made by `minisign -G` say `Ed`; the library's parsing agrees.
        if &bytes[..2] != b"Ed" || PublicKey::from_base64(key).is_err() {
            return None;
        }
        // minisign prints the ID with the last byte first.
        let key_id = bytes[2..10].iter().rev().fold(String::new(), |mut s, b| {
            s.push_str(&format!("{b:02X}"));
            s
        });
        Some(Signer {
            key: key.to_string(),
            key_id,
        })
    }
}

/// Checks that `signature` (a minisign signature file) is a good signature of
/// exactly `manifest` by one of `signers`, and returns that signer's key ID.
/// Nothing in `manifest` is looked at.
pub fn verify(signers: &[Signer], manifest: &[u8], signature: &[u8]) -> Result<String, Error> {
    if signers.is_empty() {
        return Err(err("No key is listed for this app's releases."));
    }
    if signature.len() as u64 > MAX_SIGNATURE {
        return Err(err(
            "The release's signature is larger than the Store accepts.",
        ));
    }
    let text =
        std::str::from_utf8(signature).map_err(|_| err("The release's signature is not valid."))?;
    let sig = Signature::decode(text).map_err(|e| match e {
        VerifyError::UnsupportedAlgorithm => {
            err("The release's signature is of a kind the Store does not accept.")
        }
        _ => err("The release's signature is not valid."),
    })?;
    // Why no listed key verified it, from the most to the least telling.
    let mut legacy = false;
    let mut key_matched = false;
    for signer in signers {
        let Ok(key) = PublicKey::from_base64(&signer.key) else {
            continue;
        };
        match key.verify(manifest, &sig, false) {
            Ok(()) => return Ok(signer.key_id.clone()),
            Err(VerifyError::UnexpectedKeyId) => {}
            Err(VerifyError::UnexpectedAlgorithm) => legacy = true,
            Err(_) => key_matched = true,
        }
    }
    Err(err(if legacy {
        "The release is signed in an old format the Store does not accept."
    } else if key_matched {
        "The release's signature does not match its manifest."
    } else {
        "The release is not signed by a key that Telamon's list names for this app."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made with the real `minisign` 0.12 (see tests/fixtures/native/signed).
    const KEY_A: &str = include_str!("../../tests/fixtures/native/signed/a.pub");

    fn key(text: &str) -> &str {
        text.lines().nth(1).unwrap().trim()
    }

    #[test]
    fn a_key_has_the_id_minisign_prints() {
        let pub_file = KEY_A;
        let comment = pub_file.lines().next().unwrap();
        let id = comment.rsplit(' ').next().unwrap();
        let s = Signer::minisign(key(pub_file)).unwrap();
        assert_eq!(s.key_id, id);
        assert!(valid_key_id(&s.key_id));
    }

    #[test]
    fn things_that_are_not_keys_are_not_signers() {
        let good = key(KEY_A);
        assert!(Signer::minisign(good).is_some());
        for bad in [
            "",
            "RW",
            &good[1..],
            &format!("{good}A"),
            &format!(" {good}"),
            &good.replace('R', "!"),
            // The secret key's or a signature's prefix.
            "RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=",
        ] {
            assert!(Signer::minisign(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn key_ids_are_upper_hex() {
        assert!(valid_key_id("0123456789ABCDEF"));
        for bad in [
            "",
            "0123456789abcdef",
            "0123456789ABCDE",
            "0123456789ABCDEFF",
            "0123456789ABCDEG",
        ] {
            assert!(!valid_key_id(bad), "{bad:?}");
        }
    }

    // ---- verification, against files made by the real `minisign` 0.12 ----

    const KEY_B: &str = include_str!("../../tests/fixtures/native/signed/b.pub");
    const KEY_C: &str = include_str!("../../tests/fixtures/native/signed/c.pub");
    const M3: &[u8] = include_bytes!("../../tests/fixtures/native/signed/gates-0.3.0.json");
    const M2: &[u8] = include_bytes!("../../tests/fixtures/native/signed/gates-0.2.0.json");
    const SIG3_A: &[u8] =
        include_bytes!("../../tests/fixtures/native/signed/gates-0.3.0.by-a.minisig");
    const SIG3_B: &[u8] =
        include_bytes!("../../tests/fixtures/native/signed/gates-0.3.0.by-b.minisig");
    const SIG3_C: &[u8] =
        include_bytes!("../../tests/fixtures/native/signed/gates-0.3.0.by-c.minisig");
    const SIG2_A: &[u8] =
        include_bytes!("../../tests/fixtures/native/signed/gates-0.2.0.by-a.minisig");
    const SIG3_LEGACY: &[u8] =
        include_bytes!("../../tests/fixtures/native/signed/gates-0.3.0.legacy-by-a.minisig");

    fn signer(pub_file: &str) -> Signer {
        Signer::minisign(key(pub_file)).unwrap()
    }

    #[test]
    fn a_signature_made_by_the_real_tool_verifies() {
        let a = signer(KEY_A);
        assert_eq!(
            verify(std::slice::from_ref(&a), M3, SIG3_A).unwrap(),
            a.key_id
        );
        // Hashed, as the real tool makes it by default (not the legacy kind).
        assert!(std::str::from_utf8(SIG3_A).unwrap().contains("hashed"));
    }

    #[test]
    fn a_signature_by_a_key_that_is_not_listed_is_refused() {
        let (a, b) = (signer(KEY_A), signer(KEY_B));
        // A perfectly good signature, by a key the entry does not list.
        let e = verify(&[a.clone(), b.clone()], M3, SIG3_C).unwrap_err();
        assert!(
            e.0.contains("not signed by a key that Telamon's list names"),
            "{e}"
        );
        // The right file, the wrong listed key.
        assert!(verify(&[b], M3, SIG3_A).is_err());
        // No keys at all.
        assert!(verify(&[], M3, SIG3_A).is_err());
        let _ = a;
    }

    #[test]
    fn a_manifest_that_changed_by_one_byte_is_refused() {
        let a = signer(KEY_A);
        for i in [0, 1, M3.len() / 2, M3.len() - 2, M3.len() - 1] {
            let mut m = M3.to_vec();
            m[i] ^= 0x01;
            let e = verify(std::slice::from_ref(&a), &m, SIG3_A).unwrap_err();
            assert!(e.0.contains("does not match its manifest"), "{i}: {e}");
        }
        // Extra or missing bytes at the end are a different file too.
        let mut longer = M3.to_vec();
        longer.push(b'\n');
        assert!(verify(std::slice::from_ref(&a), &longer, SIG3_A).is_err());
        assert!(verify(&[a], &M3[..M3.len() - 1], SIG3_A).is_err());
    }

    #[test]
    fn a_signature_of_another_file_is_refused() {
        let a = signer(KEY_A);
        // The signature of the 0.2.0 manifest does not do for 0.3.0, and the
        // other way round: an old signed release cannot vouch for a new one.
        assert!(verify(std::slice::from_ref(&a), M3, SIG2_A).is_err());
        assert!(verify(std::slice::from_ref(&a), M2, SIG3_A).is_err());
        assert_eq!(
            verify(std::slice::from_ref(&a), M2, SIG2_A).unwrap(),
            a.key_id
        );
    }

    #[test]
    fn a_damaged_signature_file_is_refused() {
        let a = [signer(KEY_A)];
        let text = std::str::from_utf8(SIG3_A).unwrap();
        // Cut anywhere, it does not verify (and does not panic); only the final
        // newline may be missing.
        assert!(verify(&a, M3, &SIG3_A[..SIG3_A.len() - 1]).is_ok());
        for n in 0..SIG3_A.len() - 1 {
            assert!(verify(&a, M3, &SIG3_A[..n]).is_err(), "cut at {n}");
        }
        // Lines missing or reordered, garbage, the empty file, not UTF-8.
        let lines: Vec<&str> = text.lines().collect();
        assert!(verify(&a, M3, lines[..3].join("\n").as_bytes()).is_err());
        assert!(
            verify(
                &a,
                M3,
                [lines[0], lines[2], lines[1], lines[3]]
                    .join("\n")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(verify(&a, M3, b"").is_err());
        assert!(verify(&a, M3, b"hello").is_err());
        assert!(verify(&a, M3, &[0xff, 0xfe, 0x00, 0x80]).is_err());
        // A byte flipped in the signature or in the global signature.
        for line in [1usize, 3] {
            let mut ls: Vec<String> = lines.iter().map(|l| (*l).to_string()).collect();
            let mut b = ls[line].clone().into_bytes();
            b[20] = if b[20] == b'A' { b'B' } else { b'A' };
            ls[line] = String::from_utf8(b).unwrap();
            assert!(
                verify(&a, M3, ls.join("\n").as_bytes()).is_err(),
                "line {line}"
            );
        }
    }

    #[test]
    fn the_trusted_comment_is_signed_but_not_trusted() {
        let a = [signer(KEY_A)];
        let text = std::str::from_utf8(SIG3_A).unwrap();
        // Changing it breaks the global signature...
        let changed = text.replace("gates-0.3.0.json", "gates-9.9.9.json");
        assert!(verify(&a, M3, changed.as_bytes()).is_err());
        // ...and what it says is never read: a signature whose comment names
        // another file, another time or another version still verifies the
        // bytes it covers, and a comment that names the right ones adds nothing.
        // (SIG2_A's comment says gates-0.2.0.json and a different time.)
        assert_eq!(verify(&a, M2, SIG2_A).unwrap(), a[0].key_id);
        assert!(
            std::str::from_utf8(SIG3_B)
                .unwrap()
                .contains("gates-0.3.0.json")
        );
    }

    #[test]
    fn a_legacy_signature_is_refused() {
        // `minisign -S -l`: the file itself signed, not its hash.
        let a = signer(KEY_A);
        let e = verify(&[a], M3, SIG3_LEGACY).unwrap_err();
        assert!(e.0.contains("old format"), "{e}");
    }

    #[test]
    fn a_signature_file_has_a_size_limit() {
        let a = [signer(KEY_A)];
        let mut big = SIG3_A.to_vec();
        big.resize(MAX_SIGNATURE as usize + 1, b'\n');
        let e = verify(&a, M3, &big).unwrap_err();
        assert!(e.0.contains("larger"), "{e}");
        // At the limit, trailing blank lines are harmless.
        let mut at_limit = SIG3_A.to_vec();
        at_limit.resize(MAX_SIGNATURE as usize, b'\n');
        assert!(verify(&a, M3, &at_limit).is_ok());
    }

    #[test]
    fn a_key_can_be_rotated_to_a_second_listed_key() {
        let (a, b, c) = (signer(KEY_A), signer(KEY_B), signer(KEY_C));
        let both = [a.clone(), b.clone()];
        // While both are listed, a release signed by either verifies, and the
        // key that did is the one reported.
        assert_eq!(verify(&both, M3, SIG3_A).unwrap(), a.key_id);
        assert_eq!(verify(&both, M3, SIG3_B).unwrap(), b.key_id);
        // Once the old key is dropped from the entry, its releases stop.
        assert!(verify(std::slice::from_ref(&b), M3, SIG3_A).is_err());
        assert_eq!(
            verify(std::slice::from_ref(&b), M3, SIG3_B).unwrap(),
            b.key_id
        );
        // A key that was never listed is no use, however many are.
        assert!(verify(&[a, b], M3, SIG3_C).is_err());
        assert!(verify(std::slice::from_ref(&c), M3, SIG3_C).is_ok());
    }

    #[test]
    fn what_the_fake_signs_the_real_verifier_accepts() {
        use crate::native::fake::TestKey;
        let k = TestKey::new(7);
        let s = Signer::minisign(&k.public()).unwrap();
        assert_eq!(s.key_id, k.key_id());
        let sig = k.sign(M3);
        assert_eq!(
            verify(std::slice::from_ref(&s), M3, &sig).unwrap(),
            s.key_id
        );
        assert!(verify(&[s], M2, &sig).is_err());
    }
}
