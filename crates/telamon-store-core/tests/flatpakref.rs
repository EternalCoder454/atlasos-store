//! `.flatpakref` and `.flatpakrepo` parsing: invariants on real-shaped files
//! and on hostile ones.

use telamon_store_core::flatpakref::*;
use telamon_store_core::keyfile::{KeyFile, Limits};

const REF: &str = include_str!("fixtures/flatpakref/hello.flatpakref");
const REPO: &str = include_str!("fixtures/flatpakref/test.flatpakrepo");
const ED_B64: &str = include_str!("fixtures/flatpakref/ed.b64");
const RSA_B64: &str = include_str!("fixtures/flatpakref/rsa.b64");
const BOTH_B64: &str = include_str!("fixtures/flatpakref/both.b64");
/// `gpg --with-colons --fingerprint` of the fixture keys.
const ED_FPR: &str = "FD8E785820C3277F7DB38BF466EC72E7F89640E0";
const RSA_FPR: &str = "CA961B4672FA745BA1494E1A0171513B12A6E0B7";

fn r(s: &str) -> Result<FlatpakRef, RefError> {
    parse_flatpakref(s.as_bytes())
}
fn rp(s: &str) -> Result<FlatpakRepo, RefError> {
    parse_flatpakrepo(s.as_bytes())
}
fn reason(e: RefError) -> Reason {
    e.reason
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
fn b64(d: &[u8]) -> String {
    let mut o = String::new();
    for c in d.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                o.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

fn ref_with_key(key: &str) -> String {
    format!("[Flatpak Ref]\nName=org.a.B\nUrl=https://h.example.org/r\nGPGKey={key}\n")
}
fn key_err(data: &[u8]) -> KeyError {
    match r(&ref_with_key(&b64(data))) {
        Err(RefError { .. }) => {}
        Ok(_) => panic!("accepted"),
    }
    GpgKey::from_bytes(data.to_vec()).unwrap_err()
}
/// An old-format packet with a one-byte length.
fn old(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0x80 | (tag << 2), body.len() as u8];
    v.extend_from_slice(body);
    v
}
fn v4_body() -> Vec<u8> {
    // version 4, time, Ed25519 (22), OID length and OID, point MPI (stub).
    let mut b = vec![
        4, 0, 0, 0, 1, 22, 9, 0x2B, 6, 1, 4, 1, 0xDA, 0x47, 15, 1, 1, 0x01, 0x07,
    ];
    b.extend_from_slice(&[0x40; 32]);
    b
}

#[test]
fn ref_fixture() {
    let f = r(REF).unwrap();
    assert_eq!(f.name, "org.test.Hello");
    assert_eq!(f.branch.as_deref(), Some("stable"));
    assert_eq!(f.url, "https://dl.example.org/test/repo/");
    assert_eq!(f.title.as_deref(), Some("Hello"));
    assert!(!f.is_runtime);
    assert_eq!(
        f.runtime_repo.as_deref(),
        Some("https://dl.example.org/test/test.flatpakrepo")
    );
    assert_eq!(f.key.as_ref().unwrap().fingerprint(), ED_FPR);
    assert_eq!(suggested_remote_name(&f), "org.test.Hello-origin");
}

#[test]
fn repo_fixture() {
    let f = rp(REPO).unwrap();
    assert_eq!(f.url, "https://dl.example.org/test/repo/");
    assert_eq!(f.title.as_deref(), Some("Telamon Store Test"));
    let k = f.key.unwrap();
    assert_eq!(k.fingerprint(), ED_FPR);
    assert!(!k.bytes().is_empty());
}

#[test]
fn rsa_fingerprint_matches_gpg() {
    let k = GpgKey::from_base64(RSA_B64.trim()).unwrap();
    assert_eq!(k.fingerprint(), RSA_FPR);
}

#[test]
fn bytes_round_trip() {
    let k = GpgKey::from_base64(ED_B64.trim()).unwrap();
    assert_eq!(b64(k.bytes()), ED_B64.trim());
}

#[test]
fn two_primary_keys_refused() {
    assert_eq!(
        GpgKey::from_base64(BOTH_B64.trim()).unwrap_err(),
        KeyError::SeveralKeys
    );
}

#[test]
fn unsigned_parses() {
    let f = r("[Flatpak Ref]\nName=org.a.B\nUrl=https://h.example.org/r\n").unwrap();
    assert!(f.key.is_none());
    assert_eq!(f.branch, None);
    assert!(f.collection_id.is_none());
}

#[test]
fn all_ref_keys() {
    let f = r(&format!(
        "[Flatpak Ref]\nName=org.a.B\nBranch=1.2_x-y\nUrl=https://h.example.org/r\nTitle=T\nComment=C\n\
         Description=D\nIcon=https://h.example.org/i.png\nHomepage=https://h.example.org\nIsRuntime=true\n\
         GPGKey={}\nRuntimeRepo=https://h.example.org/x.flatpakrepo\nSuggestRemoteName=mine\n\
         CollectionID=org.c.D\nDeployCollectionID=org.e.F\nNoDeps=true\n[Other]\nName=bad\n",
        ED_B64.trim()
    ))
    .unwrap();
    assert!(f.is_runtime);
    assert_eq!(f.icon.as_deref(), Some("https://h.example.org/i.png"));
    assert_eq!(f.homepage.as_deref(), Some("https://h.example.org"));
    assert_eq!(suggested_remote_name(&f), "mine");
    assert_eq!(f.collection_id.as_deref(), Some("org.c.D"));
    assert_eq!(f.deploy_collection_id.as_deref(), Some("org.e.F"));
    assert_eq!(f.comment.as_deref(), Some("C"));
    assert_eq!(f.description.as_deref(), Some("D"));
}

#[test]
fn missing_required() {
    assert_eq!(reason(r("").unwrap_err()), Reason::NoGroup);
    assert_eq!(
        reason(r("[Other]\nName=a.b\n").unwrap_err()),
        Reason::NoGroup
    );
    assert_eq!(
        reason(r("[Flatpak Ref]\nUrl=https://h.example.org\n").unwrap_err()),
        Reason::Missing("Name")
    );
    assert_eq!(
        reason(r("[Flatpak Ref]\nName=a.b\n").unwrap_err()),
        Reason::Missing("Url")
    );
    assert_eq!(
        reason(rp("[Flatpak Repo]\nTitle=x\n").unwrap_err()),
        Reason::Missing("Url")
    );
    // A repo file is not a ref file and the other way round.
    assert_eq!(reason(r(REPO).unwrap_err()), Reason::NoGroup);
    assert_eq!(reason(rp(REF).unwrap_err()), Reason::NoGroup);
}

#[test]
fn urls_must_be_https() {
    for u in [
        "http://h.example.org/r",
        "file:///work/repo",
        "ftp://h.example.org",
        "https://u:p@h.example.org/",
        "https://h.example.org/ x",
        "javascript:alert(1)",
        "",
    ] {
        let f = format!("[Flatpak Ref]\nName=a.b\nUrl={u}\n");
        assert!(r(&f).is_err(), "{u}");
        let f = format!("[Flatpak Repo]\nUrl={u}\n");
        assert!(rp(&f).is_err(), "{u}");
    }
    let f = "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nRuntimeRepo=file:///x\n";
    assert_eq!(reason(r(f).unwrap_err()), Reason::Invalid("RuntimeRepo"));
}

#[test]
fn bad_values() {
    let base = "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n";
    for bad in [
        "Name=nodots",
        "Name=a.b/../c",
        "Name=a b.c",
        "Branch=",
        "Branch=a/b",
        "Branch=a b",
        "SuggestRemoteName=-x",
        "SuggestRemoteName=a/b",
        "SuggestRemoteName=.x",
        "CollectionID=bad",
        "DeployCollectionID=a..b",
        "IsRuntime=yes",
        "Title=a\\q",
    ] {
        assert!(r(&format!("{base}{bad}\n")).is_err(), "{bad}");
    }
    let long = "a".repeat(256);
    assert!(r(&format!("{base}Branch={long}\n")).is_err());
    let long = "a".repeat(65);
    assert!(r(&format!("{base}SuggestRemoteName={long}\n")).is_err());
}

#[test]
fn invalid_icon_and_homepage_are_dropped() {
    let f = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nIcon=http://x.example.org/i.png\nHomepage=file:///etc\n")
        .unwrap();
    assert_eq!(f.icon, None);
    assert_eq!(f.homepage, None);
    let f =
        rp("[Flatpak Repo]\nUrl=https://h.example.org\nIcon=\\q\nHomepage=javascript:1\n").unwrap();
    assert_eq!((f.icon, f.homepage), (None, None));
}

#[test]
fn repo_extras() {
    let f = rp(
        "[Flatpak Repo]\nUrl=https://h.example.org\nDefaultBranch=stable\nCollectionID=org.c.D\n",
    )
    .unwrap();
    assert_eq!(f.default_branch.as_deref(), Some("stable"));
    assert_eq!(f.collection_id.as_deref(), Some("org.c.D"));
    assert!(rp("[Flatpak Repo]\nUrl=https://h.example.org\nDefaultBranch=a/b\n").is_err());
}

#[test]
fn filter_and_authenticator_refused() {
    let e = rp("[Flatpak Repo]\nUrl=https://h.example.org\nFilter=/etc/passwd\n").unwrap_err();
    assert_eq!(e.reason, Reason::Unsupported(Unsupported::Filter));
    for k in [
        "AuthenticatorName=org.x.Auth",
        "AuthenticatorInstall=true",
        "AuthenticatorOption.token=1",
    ] {
        let e = rp(&format!("[Flatpak Repo]\nUrl=https://h.example.org\n{k}\n")).unwrap_err();
        assert_eq!(
            e.reason,
            Reason::Unsupported(Unsupported::Authenticator),
            "{k}"
        );
    }
    // Merged groups cannot hide it.
    let f = "[Flatpak Repo]\nUrl=https://h.example.org\n[X]\na=b\n[Flatpak Repo]\nFilter=/x\n";
    assert!(rp(f).is_err());
}

#[test]
fn base64_strictness() {
    let key = ED_B64.trim();
    let wrap = |s: &str| ref_with_key(s);
    // Whitespace of GLib's kind inside the value (after a `\n` escape the value
    // is on one line, so use escapes).
    let spaced = format!("{} {}", &key[..8], &key[8..]);
    assert_eq!(
        r(&wrap(&spaced)).unwrap().key.unwrap().fingerprint(),
        ED_FPR
    );
    let tabbed = format!("{}\\t{}\\n", &key[..8], &key[8..]);
    assert_eq!(
        r(&wrap(&tabbed)).unwrap().key.unwrap().fingerprint(),
        ED_FPR
    );
    // Not accepted: other characters, URL-safe alphabet, missing or extra
    // padding, data after padding, an empty or lone-char value.
    let unpadded = key.trim_end_matches('=');
    for bad in [
        "!!!!".to_string(),
        "ab-_".to_string(),
        format!("{key}!"),
        format!("{unpadded}={key}"),
        format!("{key}AAAA"),
        "A".to_string(),
        "AA".to_string(),
        "AAA".to_string(),
        "AA==AA==".to_string(),
        String::new(),
    ] {
        assert!(r(&wrap(&bad)).is_err(), "{bad}");
    }
    if key.contains('=') {
        assert!(r(&wrap(unpadded)).is_err());
    }
    // Non-zero trailing bits.
    assert_eq!(
        GpgKey::from_base64("mR==").unwrap_err(),
        KeyError::NotBase64
    );
    // Other alphabet use: "+/" decode.
    assert!(GpgKey::from_base64("+/+/").is_err()); // decodes, not OpenPGP
}

#[test]
fn oversized_key() {
    let mut p = vec![0x80 | (6 << 2) | 2];
    let body = 49_200u32;
    p.extend_from_slice(&body.to_be_bytes());
    p.extend(std::iter::repeat_n(4u8, body as usize));
    assert_eq!(
        GpgKey::from_bytes(p.clone()).unwrap_err(),
        KeyError::TooLarge
    );
    assert!(r(&ref_with_key(&b64(&p))).is_err());
}

#[test]
fn non_key_packets_and_junk() {
    // A user ID packet first, a signature, literal data, text.
    assert_eq!(key_err(&old(13, b"someone")), KeyError::NotPublicKey);
    assert_eq!(key_err(&old(2, b"sig")), KeyError::NotPublicKey);
    assert_eq!(key_err(&old(5, &v4_body())), KeyError::NotPublicKey); // secret key
    assert_eq!(key_err(b"hello world, not a key"), KeyError::BadPacket);
    assert_eq!(key_err(&[0x00, 0x01]), KeyError::BadPacket);
}

#[test]
fn truncated_packets() {
    let k = old(6, &v4_body());
    for cut in 0..k.len() {
        assert!(GpgKey::from_bytes(k[..cut].to_vec()).is_err(), "cut {cut}");
    }
    assert!(GpgKey::from_bytes(k.clone()).is_ok());
    // Trailing garbage and a truncated second packet.
    let mut t = k.clone();
    t.extend_from_slice(&[0x80 | (13 << 2), 5, b'a']);
    assert_eq!(GpgKey::from_bytes(t).unwrap_err(), KeyError::BadPacket);
    let mut t = k.clone();
    t.push(0xFF);
    assert!(GpgKey::from_bytes(t).is_err());
    // Real key cut at every length that matters.
    let real = GpgKey::from_base64(ED_B64.trim()).unwrap();
    for cut in [1, 2, 3, 20, real.bytes().len() - 1] {
        assert!(
            GpgKey::from_bytes(real.bytes()[..cut].to_vec()).is_err(),
            "{cut}"
        );
    }
    // Header length claims more than the buffer holds; 4-byte length overflow.
    assert!(GpgKey::from_bytes(vec![0x80 | (6 << 2) | 2, 0xFF, 0xFF, 0xFF, 0xFF, 4]).is_err());
    // Indeterminate and partial lengths.
    assert!(GpgKey::from_bytes(vec![0x80 | (6 << 2) | 3, 4, 0, 0]).is_err());
    assert!(GpgKey::from_bytes(vec![0xC0 | 6, 224, 4, 0, 0]).is_err());
}

#[test]
fn old_and_new_packet_formats_agree() {
    let body = v4_body();
    let a = GpgKey::from_bytes(old(6, &body)).unwrap();
    let mut n = vec![0xC0 | 6, body.len() as u8];
    n.extend_from_slice(&body);
    let b = GpgKey::from_bytes(n).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    // Two-byte new-format length (192..=8383) and five-byte one.
    let mut big = body.clone();
    big.resize(300, 0);
    let l = big.len() - 192;
    let mut p2 = vec![0xC0 | 6, (l >> 8) as u8 + 192, (l & 255) as u8];
    p2.extend_from_slice(&big);
    let mut p5 = vec![0xC0 | 6, 255];
    p5.extend_from_slice(&(big.len() as u32).to_be_bytes());
    p5.extend_from_slice(&big);
    let mut p_old2 = vec![0x80 | (6 << 2) | 1];
    p_old2.extend_from_slice(&(big.len() as u16).to_be_bytes());
    p_old2.extend_from_slice(&big);
    let f2 = GpgKey::from_bytes(p2).unwrap();
    assert_eq!(
        f2.fingerprint(),
        GpgKey::from_bytes(p5).unwrap().fingerprint()
    );
    assert_eq!(
        f2.fingerprint(),
        GpgKey::from_bytes(p_old2).unwrap().fingerprint()
    );
    assert_eq!(f2.fingerprint().len(), 40);
}

#[test]
fn versions() {
    let mut v3 = v4_body();
    v3[0] = 3;
    assert_eq!(key_err(&old(6, &v3)), KeyError::Version(3));
    let mut v5 = v4_body();
    v5[0] = 5;
    assert_eq!(key_err(&old(6, &v5)), KeyError::Version(5));
    // v6: SHA-256 over 0x9B, 4-byte length, body.
    let mut v6 = vec![6, 0, 0, 0, 1, 27, 0, 0, 0, 32];
    v6.extend_from_slice(&[7u8; 32]);
    let k = GpgKey::from_bytes(old(6, &v6)).unwrap();
    assert_eq!(k.fingerprint().len(), 64);
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update([0x9B]);
    h.update((v6.len() as u32).to_be_bytes());
    h.update(&v6);
    let want: String = h.finalize().iter().map(|b| format!("{b:02X}")).collect();
    assert_eq!(k.fingerprint(), want);
    assert!(
        k.fingerprint()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
    );
}

#[test]
fn text_is_cleaned() {
    let f = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nTitle=Hel\u{202E}lo\u{7}\\n  W\u{200B}orld\u{2066}\n\
               Comment=\\n\\t \nDescription=a\\nb\n")
        .unwrap();
    assert_eq!(f.title.as_deref(), Some("Hello World"));
    assert_eq!(f.comment, None);
    assert_eq!(f.description.as_deref(), Some("a b"));
    let long = "x".repeat(10_000);
    let f = r(&format!(
        "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nTitle={long}\nDescription={long}\n"
    ))
    .unwrap();
    assert_eq!(f.title.unwrap().chars().count(), 200);
    assert_eq!(f.description.unwrap().chars().count(), 2000);
}

#[test]
fn last_key_wins_like_glib() {
    let f = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nName=c.d\n[X]\nName=z\n[Flatpak Ref]\nUrl=https://i.example.org\n")
        .unwrap();
    assert_eq!(f.name, "c.d");
    assert_eq!(f.url, "https://i.example.org");
    // A bad earlier value is overridden, as GLib would; a bad last one is not.
    assert!(r("[Flatpak Ref]\nName=bad\nName=a.b\nUrl=https://h.example.org\n").is_ok());
    assert!(r("[Flatpak Ref]\nName=a.b\nName=bad\nUrl=https://h.example.org\n").is_err());
    // The same file, read by the shared key file reader.
    let src = "[Flatpak Ref]\nTitle=1\nTitle=2\n";
    let kf = KeyFile::parse(src.as_bytes(), &Limits::default()).unwrap();
    let f = r(&format!("{src}Name=a.b\nUrl=https://h.example.org\n")).unwrap();
    assert_eq!(f.title, kf.string("Flatpak Ref", "Title").unwrap());
    assert_eq!(f.title.as_deref(), Some("2"));
}

#[test]
fn crlf_and_comments() {
    let f = REF.replace('\n', "\r\n");
    let a = r(&f).unwrap();
    assert_eq!(a, r(REF).unwrap());
    let f = format!("# comment\r\n\r\n{}", REPO.replace('\n', "\r\n"));
    assert_eq!(rp(&f).unwrap(), rp(REPO).unwrap());
}

#[test]
fn hostile_files() {
    assert!(r("").is_err());
    assert!(rp("").is_err());
    assert!(r("\n\n").is_err());
    assert!(parse_flatpakref(b"[Flatpak Ref]\nName=a.b\0\n").is_err());
    assert!(parse_flatpakref(b"[Flatpak Ref]\nName=a.\xFF\n").is_err());
    assert!(parse_flatpakref(&[0u8; 100]).is_err());
    // 300 KiB: too large whatever it holds.
    let big = format!(
        "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nTitle={}\n",
        "x".repeat(300 * 1024)
    );
    let e = r(&big).unwrap_err();
    assert!(matches!(e.reason, Reason::KeyFile(..)));
    let many = "# c\n".repeat(75_000);
    assert!(
        r(&format!(
            "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n{many}"
        ))
        .is_err()
    );
    let big_key = format!(
        "[Flatpak Repo]\nUrl=https://h.example.org\nGPGKey={}\n",
        "A".repeat(100_000)
    );
    assert!(rp(&big_key).is_err());
}

#[test]
fn errors_are_plain_and_leak_nothing() {
    let e = r("[Flatpak Ref]\nName=SECRET-VALUE\nUrl=https://h.example.org\n").unwrap_err();
    let s = e.to_string();
    assert!(s.starts_with("The file is not a Flatpak reference: "));
    assert!(!s.contains("SECRET"));
    let e = rp("[Flatpak Repo]\nUrl=http://SECRET\n").unwrap_err();
    assert!(
        e.to_string()
            .starts_with("The file is not a Flatpak repository: ")
    );
    assert!(!e.to_string().contains("SECRET"));
    let e = r(&ref_with_key("SECRET!")).unwrap_err();
    assert!(!e.to_string().contains("SECRET"));
}

#[test]
fn remote_names() {
    for ok in ["a", "flathub", "a_b.c-d", "A1"] {
        assert!(valid_remote_name(ok), "{ok}");
    }
    for bad in ["", "-a", ".a", "a/b", "a b", "a\u{e9}", "a\n"] {
        assert!(!valid_remote_name(bad), "{bad:?}");
    }
    assert!(valid_remote_name(&"a".repeat(64)));
    assert!(!valid_remote_name(&"a".repeat(65)));
    let f = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n").unwrap();
    assert_eq!(suggested_remote_name(&f), "a.b-origin");
    assert_eq!(f.suggest_remote_name, "a.b-origin");
    // No valid remote name from `<Name>-origin`: refused, never cut or replaced.
    let base = "[Flatpak Ref]\nUrl=https://h.example.org\n";
    let n = format!("org.{}", "x".repeat(60));
    let e = r(&format!("{base}Name={n}\n")).unwrap_err();
    assert_eq!(e.reason, Reason::RemoteName);
    assert!(e.to_string().contains("too long"));
    // ...unless the file suggests a valid one.
    assert!(r(&format!("{base}Name={n}\nSuggestRemoteName=mine\n")).is_ok());
    let n = format!("org.{}", "x".repeat(50));
    assert!(r(&format!("{base}Name={n}\n")).is_ok());
    // An ID never starts with a dash (it would be an option on a command
    // line), whatever remote name the file suggests.
    for n in ["-a.b", "--help.x"] {
        for extra in ["", "SuggestRemoteName=mine\n"] {
            let e = r(&format!("{base}Name={n}\n{extra}")).unwrap_err();
            assert_eq!(e.reason, Reason::Invalid("Name"), "{n}");
        }
    }
}

#[test]
fn only_allowlisted_packets_follow_the_key() {
    let k = old(6, &v4_body());
    let new = |tag: u8| vec![0xC0 | tag, 3, 1, 2, 3];
    for tag in [2u8, 12, 13, 14, 17] {
        let mut t = k.clone();
        t.extend(new(tag));
        assert!(GpgKey::from_bytes(t).is_ok(), "tag {tag}");
    }
    for tag in [
        1u8, 3, 4, 5, 7, 8, 9, 10, 11, 15, 16, 18, 19, 20, 60, 61, 62, 63,
    ] {
        let mut t = k.clone();
        t.extend(new(tag));
        assert_eq!(
            GpgKey::from_bytes(t).unwrap_err(),
            KeyError::BadPacket,
            "tag {tag}"
        );
    }
    // Old format: tags 5, 7 and 8.
    for tag in [5u8, 7, 8] {
        let mut t = k.clone();
        t.extend(old(tag, b"xyz"));
        assert_eq!(
            GpgKey::from_bytes(t).unwrap_err(),
            KeyError::BadPacket,
            "old {tag}"
        );
    }
}

#[test]
fn only_known_keys() {
    let base = "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n";
    for extra in [
        "Evil=1",
        "name=x",
        "Foo[de]=1",
        "Name[de]=a.c",
        "Url[x]=https://h.example.org",
    ] {
        let e = r(&format!("{base}{extra}\n")).unwrap_err();
        assert!(matches!(e.reason, Reason::UnknownKey(_)), "{extra}");
    }
    let e = r(&format!("{base}Ev\u{202E}il=1\n")).unwrap_err();
    assert!(e.to_string().contains("\"Ev?il\""));
    for ok in ["NoDeps=true", "Subset=x", "Version=1"] {
        assert!(r(&format!("{base}{ok}\n")).is_ok(), "{ok}");
    }
    // Other groups are ignored.
    assert!(r(&format!("{base}[Other]\nAnything=1\n")).is_ok());
    assert!(rp("[Flatpak Repo]\nUrl=https://h.example.org\nNoGpgVerify=true\n").is_err());
    assert!(rp("[Flatpak Repo]\nUrl=https://h.example.org\nFilter[de]=/x\n").is_err());
    assert!(rp("[Flatpak Repo]\nUrl=https://h.example.org\nAuthenticatorName[de]=x\n").is_err());
}

#[test]
fn translated_text_refused() {
    for k in ["Title[de]=Hallo", "Comment[fr]=x", "Description[xx_YY]=x"] {
        let e = r(&format!(
            "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n{k}\n"
        ))
        .unwrap_err();
        assert_eq!(
            e.reason,
            Reason::Unsupported(Unsupported::Translations),
            "{k}"
        );
        let e = rp(&format!("[Flatpak Repo]\nUrl=https://h.example.org\n{k}\n")).unwrap_err();
        assert_eq!(
            e.reason,
            Reason::Unsupported(Unsupported::Translations),
            "{k}"
        );
    }
}

#[test]
fn launch_url_policy() {
    for u in [
        "https://127.0.0.1/r",
        "https://[::1]/r",
        "https://h.example.org:8443/r",
        "https://localhost/r",
        "https://printer.local/r",
        "https://h.example.org/a/../b",
        "https://h.example.org/a/%2e%2E/b",
        "https://h.lan/r",
    ] {
        assert!(
            r(&format!("[Flatpak Ref]\nName=a.b\nUrl={u}\n")).is_err(),
            "{u}"
        );
        assert!(rp(&format!("[Flatpak Repo]\nUrl={u}\n")).is_err(), "{u}");
        let f = format!(
            "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nRuntimeRepo={u}\nIcon={u}\nHomepage={u}\n"
        );
        assert_eq!(
            reason(r(&f).unwrap_err()),
            Reason::Invalid("RuntimeRepo"),
            "{u}"
        );
        let f =
            format!("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nIcon={u}\nHomepage={u}\n");
        let ok = r(&f).unwrap();
        assert_eq!((ok.icon, ok.homepage), (None, None), "{u}");
    }
    // Normalized: host lowercased, :443 dropped.
    let f = r("[Flatpak Ref]\nName=a.b\nUrl=https://H.Example.ORG:443/Repo\n").unwrap();
    assert_eq!(f.url, "https://h.example.org/Repo");
}

#[test]
fn branches_may_not_start_with_dash_or_dot() {
    let base = "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n";
    for b in ["-x", ".x", "..", "-"] {
        assert!(r(&format!("{base}Branch={b}\n")).is_err(), "{b}");
        assert!(
            rp(&format!(
                "[Flatpak Repo]\nUrl=https://h.example.org\nDefaultBranch={b}\n"
            ))
            .is_err(),
            "{b}"
        );
    }
}

#[test]
fn suggested_name_survives_non_ascii() {
    let mut f = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n").unwrap();
    f.name = "\u{e9}".repeat(40);
    f.suggest_remote_name = String::new();
    assert_eq!(suggested_remote_name(&f), format!("{}-origin", f.name));
    f.suggest_remote_name = "\u{e9}".into();
    assert!(suggested_remote_name(&f).ends_with("-origin"));
}

#[test]
fn no_enumerate_refused() {
    let e =
        r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nNoEnumerate=true\n").unwrap_err();
    assert_eq!(e.reason, Reason::Unsupported(Unsupported::NoEnumerate));
    assert!(rp("[Flatpak Repo]\nUrl=https://h.example.org\nNoEnumerate=false\n").is_err());
}

#[test]
fn unknown_key_text_is_plain() {
    let e = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nE\u{202E}v\u{e9}il_1.x-y=1\n")
        .unwrap_err();
    assert_eq!(e.reason, Reason::UnknownKey("E?v?il_1.x-y".into()));
    let k = "k".repeat(100);
    let e = r(&format!(
        "[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n{k}=1\n"
    ))
    .unwrap_err();
    assert_eq!(e.reason, Reason::UnknownKey("k".repeat(40)));
}

#[test]
fn to_bytes_round_trips() {
    let a = r(REF).unwrap();
    assert_eq!(
        r(std::str::from_utf8(&a.to_bytes().unwrap()).unwrap()).unwrap(),
        a
    );
    let b = rp(REPO).unwrap();
    assert_eq!(
        rp(std::str::from_utf8(&b.to_bytes().unwrap()).unwrap()).unwrap(),
        b
    );
    // Everything set, plus keys that must not survive.
    let full = format!(
        "[Flatpak Ref]\nName=org.a.B\nBranch=1.2\nUrl=https://H.example.org:443/r\nTitle=T\nComment=C\n\
         Description=D\nIcon=https://h.example.org/i.png\nHomepage=https://h.example.org\nIsRuntime=true\n\
         GPGKey={}\nRuntimeRepo=https://h.example.org/x.flatpakrepo\nCollectionID=org.c.D\n\
         DeployCollectionID=org.e.F\nNoDeps=true\nSubset=x\nVersion=1\n[Other]\nx=y\n",
        ED_B64.trim()
    );
    let f = r(&full).unwrap();
    let out = f.to_bytes().unwrap();
    let text = std::str::from_utf8(&out).unwrap();
    assert!(!text.contains("NoDeps") && !text.contains("Subset") && !text.contains("[Other]"));
    assert!(text.contains("Url=https://h.example.org/r\n"));
    assert!(text.contains("SuggestRemoteName=org.a.B-origin\n"));
    assert_eq!(r(text).unwrap(), f);
    assert_eq!(f.to_bytes().unwrap(), r(text).unwrap().to_bytes().unwrap());
    let repo = rp(&format!(
        "[Flatpak Repo]\nUrl=https://h.example.org\nTitle=T\nDefaultBranch=stable\nGPGKey={}\nCollectionID=org.c.D\nIcon=https://h.example.org/i.svg\n",
        ED_B64.trim()
    ))
    .unwrap();
    assert_eq!(
        rp(std::str::from_utf8(&repo.to_bytes().unwrap()).unwrap()).unwrap(),
        repo
    );
    // An unsigned, minimal one.
    let m = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n").unwrap();
    assert_eq!(
        r(std::str::from_utf8(&m.to_bytes().unwrap()).unwrap()).unwrap(),
        m
    );
}

#[test]
fn to_bytes_escapes_and_checks_itself() {
    let base = || r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n").unwrap();
    // A backslash survives.
    let mut f = base();
    f.title = Some("a\\b\\n".into());
    f.comment = Some("a\\".into());
    let out = f.to_bytes().unwrap();
    let kf = KeyFile::parse(&out, &Limits::default()).unwrap();
    assert_eq!(
        kf.string("Flatpak Ref", "Title").unwrap().as_deref(),
        Some("a\\b\\n")
    );
    assert_eq!(r(std::str::from_utf8(&out).unwrap()).unwrap(), f);
    // Anything that would not read back the same is an error, never bad bytes.
    let bad_titles = [
        " lead",
        "\x0Bx",
        "x\0y",
        "a\tb",
        "a\nb",
        "a\rb",
        "a\x01b",
        "a\x1Fb",
        "a\u{202E}b",
        "a  b",
        "x ",
    ];
    for t in bad_titles {
        let mut f = base();
        f.title = Some(t.into());
        let e = f.to_bytes().unwrap_err();
        assert_eq!(e.reason, Reason::Unwritable, "{t:?}");
    }
    let mut f = base();
    f.url = "https://H.example.org:443/r".into();
    assert!(f.to_bytes().is_err());
    let mut f = base();
    f.name = format!("org.{}", "x".repeat(300));
    assert!(f.to_bytes().is_err());
    let mut f = base();
    f.name = "nodots".into();
    assert!(f.to_bytes().is_err());
    let mut f = base();
    f.icon = Some("http://x.example.org/i.png".into());
    assert!(f.to_bytes().is_err());
    f.icon = Some("https://X.example.org:443/i.png".into());
    assert!(f.to_bytes().is_err()); // not normalized
    let mut f = base();
    f.branch = Some("-x".into());
    assert!(f.to_bytes().is_err());
    // The same for a repo.
    let mut g = rp("[Flatpak Repo]\nUrl=https://h.example.org\n").unwrap();
    assert!(g.to_bytes().is_ok());
    g.url = "http://h.example.org".into();
    let e = g.to_bytes().unwrap_err();
    assert!(
        e.to_string()
            .starts_with("The file is not a Flatpak repository")
    );
    g.url = "https://h.example.org".into();
    g.title = Some("x\0".into());
    assert!(g.to_bytes().is_err());
}

#[test]
fn key_limit_matches_one_value() {
    assert_eq!(MAX_KEY_BYTES, Limits::default().max_value / 4 * 3);
    assert_eq!(MAX_FILE_BYTES, Limits::default().max_bytes);
    // Exactly at the limit: accepted, and written and read back.
    let mut body = v4_body();
    body.resize(MAX_KEY_BYTES - 3, 0);
    let mut p = vec![0x80 | (6 << 2) | 1];
    p.extend_from_slice(&(body.len() as u16).to_be_bytes());
    p.extend_from_slice(&body);
    assert_eq!(p.len(), MAX_KEY_BYTES);
    let key = GpgKey::from_bytes(p.clone()).unwrap();
    let mut f = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n").unwrap();
    f.key = Some(key.clone());
    let out = f.to_bytes().unwrap();
    assert_eq!(r(std::str::from_utf8(&out).unwrap()).unwrap(), f);
    let mut g = rp("[Flatpak Repo]\nUrl=https://h.example.org\n").unwrap();
    g.key = Some(key);
    assert_eq!(
        rp(std::str::from_utf8(&g.to_bytes().unwrap()).unwrap()).unwrap(),
        g
    );
    // One byte more is refused.
    p.push(0);
    p[1..3].copy_from_slice(&((body.len() + 1) as u16).to_be_bytes());
    p.truncate(MAX_KEY_BYTES + 1);
    assert_eq!(GpgKey::from_bytes(p).unwrap_err(), KeyError::TooLarge);
}

#[test]
fn rfc9580_v6_sample_fingerprint() {
    // The primary key packet of RFC 9580 Appendix A.3 (v6, Ed25519); the
    // fingerprint is the one the RFC publishes.
    let k = GpgKey::from_base64("xioGY4d/4xsAAAAg+U2nu0jWCmHlZ3BqZYfQMxmZu52JGggkLq2EVD34laM=")
        .unwrap();
    assert_eq!(
        k.fingerprint(),
        "CB186C4F0609A697E4D52DFA6C722B0C1F1E27C18A56708F6525EC27BAD9ACC9"
    );
}

#[test]
fn hostile_text_round_trips() {
    let long = format!("{}\u{e9}yy", "x".repeat(199));
    let cases = [
        "Title=a\u{a0}b".to_string(),
        "Title=a\u{85}b".to_string(),
        "Title=a\u{2028}b".to_string(),
        "Title=\x0Bx".to_string(),
        "Title=a\\\\\\\\\\\\b\\\\".to_string(),
        "Title=a\u{200F}b\u{200E}".to_string(),
        format!("Title={long}"),
        format!("Description={}", long.repeat(20)),
        "Title=\\s x".to_string(),
        "Title=\\s\\sx\\t".to_string(),
        "Comment=\u{3000}x\u{3000}".to_string(),
    ];
    for c in &cases {
        let src = format!("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\n{c}\n");
        let first = r(&src).unwrap_or_else(|e| panic!("{c:?}: {e}"));
        let out = first.to_bytes().unwrap_or_else(|e| panic!("{c:?}: {e}"));
        let second = parse_flatpakref(&out).unwrap();
        assert_eq!(first, second, "{c:?}");
        let repo = format!("[Flatpak Repo]\nUrl=https://h.example.org\n{c}\n");
        let first = rp(&repo).unwrap();
        let second = parse_flatpakrepo(&first.to_bytes().unwrap()).unwrap();
        assert_eq!(first, second, "{c:?}");
    }
    let full = format!(
        "[Flatpak Repo]\nUrl=https://H.example.org:443/r\nTitle=T \\\\ x\nComment=C\nDescription=D\n\
         Icon=https://h.example.org/i.png\nHomepage=https://h.example.org\nDefaultBranch=stable\n\
         GPGKey={}\nCollectionID=org.c.D\nDeployCollectionID=org.e.F\n",
        ED_B64.trim()
    );
    let a = rp(&full).unwrap();
    assert_eq!(parse_flatpakrepo(&a.to_bytes().unwrap()).unwrap(), a);
    assert!(a.icon.is_some() && a.key.is_some() && a.default_branch.is_some());
    assert!(
        !r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nIsRuntime=maybe\n")
            .unwrap_err()
            .is_repo()
    );
}

#[test]
fn value_errors_name_the_key() {
    let e = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nIsRuntime=maybe\n").unwrap_err();
    assert!(
        e.to_string().ends_with("IsRuntime is not true or false"),
        "{e}"
    );
    let e = r("[Flatpak Ref]\nName=a.b\nUrl=https://h.example.org\nTitle=a\\q\n").unwrap_err();
    assert!(
        e.to_string().ends_with("Title has an invalid escape"),
        "{e}"
    );
    assert!(
        rp("[Flatpak Repo]\nUrl=http://x.example.org\n")
            .unwrap_err()
            .is_repo()
    );
}
