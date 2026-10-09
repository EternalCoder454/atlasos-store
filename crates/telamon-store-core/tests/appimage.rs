//! AppImage inspection: fixtures are built in memory (see common/appimage.rs).
#[path = "common/appimage.rs"]
mod build;

use std::path::PathBuf;

use telamon_store_core::appimage::Format;
use telamon_store_core::appimage::inspect::{InspectError, inspect};
use telamon_store_core::appimage::meta::IconKind;
use telamon_store_core::appimage::sign::Signature;
use telamon_store_core::appimage::squash::Limits;

use build::scratch;

#[test]
fn a_normal_type_2_appimage_is_read_without_running_it() {
    let dir = scratch("normal");
    let p = build::write(&dir, "Sample-x86_64.AppImage", &build::normal());
    let i = inspect(&p, &Limits::default()).unwrap();
    assert_eq!(i.format, Format::Type2);
    assert!(i.inspected, "{}", i.note);
    assert_eq!(i.name, "Sample Draw");
    assert_eq!(i.version, "2.1.0");
    assert_eq!(i.publisher, "Example Studio");
    assert_eq!(i.summary, "Draw things");
    assert_eq!(i.app_id, "org.example.Sample");
    assert_eq!(i.icon_kind, Some(IconKind::Png));
    assert_eq!(i.signature, Signature::None);
    assert_eq!(i.sha256.len(), 64);
}

fn inspect_bytes(
    name: &str,
    bytes: &[u8],
) -> Result<telamon_store_core::appimage::Inspection, InspectError> {
    let dir = scratch("bytes");
    let p = build::write(&dir, name, bytes);
    inspect(&p, &Limits::default())
}

fn runtime_len() -> usize {
    build::runtime(&[], &[], 2).len()
}

#[test]
fn a_zstd_squashfs_is_read_too() {
    let bytes = build::type2(&build::normal_squash().zstd().build(), &[], &[]);
    let i = inspect_bytes("x.AppImage", &bytes).unwrap();
    assert!(i.inspected, "{}", i.note);
    assert_eq!(i.name, "Sample Draw");
}

#[test]
fn the_icon_is_found_through_dir_icon_and_in_the_hicolor_theme() {
    // Only .DirIcon (a link to a file elsewhere in the image) names it.
    let sq = build::Squash::default()
        .file(
            "/app.desktop",
            "[Desktop Entry]\nType=Application\nName=Linked\nExec=x\n",
        )
        .file(
            "/usr/share/icons/hicolor/128x128/apps/real.png",
            build::fake_png(128, 128),
        )
        .link("/.DirIcon", "usr/share/icons/hicolor/128x128/apps/real.png");
    let i = inspect_bytes("x.AppImage", &build::type2(&sq.build(), &[], &[])).unwrap();
    assert_eq!(
        i.icon.as_ref().map(|i| i.bytes.clone()),
        Some(build::fake_png(128, 128))
    );
    // Hicolor icons are found by the desktop file's Icon=.
    let sq = build::Squash::default()
        .file(
            "/app.desktop",
            "[Desktop Entry]\nType=Application\nName=Themed\nIcon=themed\nExec=x\n",
        )
        .file(
            "/usr/share/icons/hicolor/48x48/apps/themed.png",
            build::fake_png(48, 48),
        )
        .file(
            "/usr/share/icons/hicolor/256x256/apps/themed.png",
            build::fake_png(256, 256),
        );
    let i = inspect_bytes("x.AppImage", &build::type2(&sq.build(), &[], &[])).unwrap();
    assert_eq!(i.icon.unwrap().bytes, build::fake_png(256, 256));
}

#[test]
fn without_metainfo_the_desktop_entry_names_the_app() {
    let sq = build::Squash::default()
        .file("/Tool.desktop", "[Desktop Entry]\nType=Application\nName=Plain Tool\nComment=Does a thing\nX-AppImage-Version=1.4\nX-AppImage-Vendor=Somebody\nExec=tool\n");
    let i = inspect_bytes("Tool.AppImage", &build::type2(&sq.build(), &[], &[])).unwrap();
    assert!(i.inspected);
    assert_eq!(i.name, "Plain Tool");
    assert_eq!(i.summary, "Does a thing");
    assert_eq!(i.version, "1.4");
    assert_eq!(i.publisher, "Somebody");
    assert_eq!(i.app_id, "");
    assert!(i.note.contains("AppStream"), "{}", i.note);
}

#[test]
fn with_nothing_inside_the_file_name_is_used() {
    let sq = build::Squash::default().file("/AppRun", "x");
    let i = inspect_bytes(
        "My_Cool-App-x86_64.AppImage",
        &build::type2(&sq.build(), &[], &[]),
    )
    .unwrap();
    assert!(i.inspected);
    assert_eq!(i.name, "My Cool App");
}

#[test]
fn hostile_texts_are_cleaned_and_names_refused() {
    let desktop = "[Desktop Entry]\nType=Application\nName=Evil\\nApp\u{202e}gnp.exe\nComment=<b>bold</b>\\n\\nline\nExec=sh -c 'rm -rf ~' %f \"quote\nIcon=../../../../etc/passwd\n";
    let appdata = r#"<component type="desktop-application"><id>org.evil.App</id><name>Evil&#x7;App</name><summary>s</summary>
        <developer><name>Dev&#x202e;Corp</name></developer></component>"#;
    let sq = build::Squash::default()
        .file("/evil.desktop", desktop)
        .file("/usr/share/metainfo/x.appdata.xml", appdata)
        .file("/etc/passwd", "root:x:0:0")
        .link("/.DirIcon", "../../../../etc/passwd");
    let i = inspect_bytes("e.AppImage", &build::type2(&sq.build(), &[], &[])).unwrap();
    // An invalid control character in the XML refuses the metainfo; the
    // desktop entry's name is cleaned (newline and bidi override gone).
    assert!(
        !i.name.contains('\n') && !i.name.contains('\u{202e}'),
        "{:?}",
        i.name
    );
    assert!(!i.publisher.contains('\u{202e}'));
    assert!(
        i.icon.is_none(),
        "an icon outside the image must not be read"
    );
    assert!(i.name.starts_with("Evil"), "{:?}", i.name);
}

#[test]
fn huge_fields_are_capped() {
    let long = "A".repeat(7000);
    let desktop =
        format!("[Desktop Entry]\nType=Application\nName={long}\nComment={long}\nExec=x\n");
    let sq = build::Squash::default().file("/a.desktop", desktop);
    let i = inspect_bytes("huge.AppImage", &build::type2(&sq.build(), &[], &[])).unwrap();
    assert!(i.name.chars().count() <= 100);
    assert!(i.summary.chars().count() <= 300);
    // A value over the key-file limit refuses the whole desktop entry.
    let way_too_long = "B".repeat(20_000);
    let desktop = format!("[Desktop Entry]\nType=Application\nName={way_too_long}\nExec=x\n");
    let sq = build::Squash::default().file("/a.desktop", desktop);
    let i = inspect_bytes("huge2.AppImage", &build::type2(&sq.build(), &[], &[])).unwrap();
    assert_eq!(i.name, "huge2");
}

#[test]
fn a_file_that_expands_hugely_is_not_read() {
    let bomb = vec![0u8; 16 << 20];
    let sq = build::Squash::default()
        .file(
            "/a.desktop",
            "[Desktop Entry]\nType=Application\nName=Bomb\nExec=x\n",
        )
        .file("/usr/share/metainfo/b.appdata.xml", bomb);
    let bytes = build::type2(&sq.build(), &[], &[]);
    assert!(
        bytes.len() < 1 << 20,
        "the bomb compresses small: {}",
        bytes.len()
    );
    let i = inspect_bytes("bomb.AppImage", &bytes).unwrap();
    assert!(i.inspected);
    assert_eq!(i.name, "Bomb");
}

#[test]
fn a_truncated_file_is_not_inspected_but_still_hashed() {
    let full = build::normal();
    let cut = &full[..runtime_len() + 700];
    assert!(cut.len() < full.len());
    let i = inspect_bytes("cut.AppImage", cut).unwrap();
    assert!(!i.inspected);
    assert!(!i.note.is_empty());
    assert_eq!(i.sha256.len(), 64);
    // Cut inside the runtime too.
    let i = inspect_bytes("cut2.AppImage", &full[..6000]).unwrap();
    assert!(!i.inspected);
}

#[test]
fn a_type_1_appimage_is_recognized_and_not_parsed() {
    let i = inspect_bytes("old.AppImage", &build::type1()).unwrap();
    assert_eq!(i.format, Format::Type1);
    assert!(!i.inspected);
    assert!(i.note.contains("old format"), "{}", i.note);
    assert_eq!(i.name, "old");
}

#[test]
fn files_that_are_not_appimages_are_refused() {
    assert_eq!(
        inspect_bytes("a.AppImage", &vec![b'x'; 20_000]).unwrap_err(),
        InspectError::NotAppImage
    );
    assert_eq!(
        inspect_bytes("a.AppImage", b"tiny").unwrap_err(),
        InspectError::NotAppImage
    );
    // An ELF without the marker.
    let mut elf = build::runtime(&[], &[], 2);
    elf[8] = 0;
    elf.resize(20_000, 0);
    assert_eq!(
        inspect_bytes("elf.AppImage", &elf).unwrap_err(),
        InspectError::NotAppImage
    );
    // A folder.
    let dir = scratch("folder");
    assert_eq!(
        inspect(&dir, &Limits::default()).unwrap_err(),
        InspectError::NotAFile
    );
}

#[test]
fn the_marker_decides_not_the_extension() {
    let i = inspect_bytes("download", &build::normal()).unwrap();
    assert_eq!(i.name, "Sample Draw");
    let i = inspect_bytes("report.pdf", &build::normal()).unwrap();
    assert!(i.inspected);
}

fn patch(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn a_superblock_over_the_limits_is_refused_before_anything_is_decompressed() {
    let base = runtime_len();
    let ok = build::normal();
    // inode_count (offset 4), block_size (12) and fragment count (16).
    for (field, value) in [
        (4usize, 10_000_000u32),
        (12, 3000),
        (12, 1 << 24),
        (16, 9_000_000),
    ] {
        let mut b = ok.clone();
        patch(&mut b, base + field, value);
        let i = inspect_bytes("sb.AppImage", &b).unwrap();
        assert!(!i.inspected, "field {field}={value}: {}", i.note);
    }
    // A wrong version.
    let mut b = ok.clone();
    b[base + 28] = 3;
    assert!(!inspect_bytes("v3.AppImage", &b).unwrap().inspected);
    // Not squashfs at all.
    let mut b = ok.clone();
    b[base] = b'x';
    assert!(!inspect_bytes("nosq.AppImage", &b).unwrap().inspected);
    // Offsets pointing past the image.
    let mut b = ok.clone();
    b[base + 64..base + 72].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(!inspect_bytes("off.AppImage", &b).unwrap().inspected);
}

#[test]
fn no_byte_of_the_headers_can_make_the_reader_panic() {
    // Every byte of the ELF header, the section headers, the squashfs
    // superblock and the first metadata block, set to values that sit on the
    // edges of integer sizes. A panic here is a test failure (debug builds
    // check overflow); the answer itself does not matter.
    let ok = build::normal();
    let base = runtime_len();
    let shoff = u64::from_le_bytes(ok[0x28..0x30].try_into().unwrap()) as usize;
    let regions = [
        (0, 64),
        (shoff, shoff + 4 * 64),
        (base, base + 96),
        (base + 96, base + 104),
    ];
    let dir = scratch("flipped");
    for (from, to) in regions {
        for at in from..to {
            for value in [0x00u8, 0x01, 0x7f, 0x80, 0xff] {
                let mut b = ok.clone();
                b[at] = value;
                let p = build::write(&dir, "f.AppImage", &b);
                let _ = inspect(&p, &Limits::default());
            }
        }
    }
}

#[test]
fn a_block_size_exponent_that_overflows_the_shift_is_refused() {
    let base = runtime_len();
    for log in [21u16, 32, 63, 64, 65, 255, 0xffff] {
        let mut b = build::normal();
        b[base + 22..base + 24].copy_from_slice(&log.to_le_bytes());
        let i = inspect_bytes("log.AppImage", &b).unwrap();
        assert!(!i.inspected, "{log}");
        assert!(i.note.contains("damaged"), "{log}: {}", i.note);
    }
}

#[test]
fn thousands_of_tiny_metadata_blocks_are_refused() {
    // A superblock whose inode table is 20,000 two-byte-payload blocks: each
    // would be a legitimate block, together far more than any real image.
    let base = runtime_len();
    let mut sq = vec![0u8; 96];
    sq[..4].copy_from_slice(b"hsqs");
    sq[4..8].copy_from_slice(&10u32.to_le_bytes());
    sq[12..16].copy_from_slice(&131072u32.to_le_bytes());
    sq[20..22].copy_from_slice(&1u16.to_le_bytes());
    sq[22..24].copy_from_slice(&17u16.to_le_bytes());
    sq[28..30].copy_from_slice(&4u16.to_le_bytes());
    let blocks = 20_000usize;
    let inode_table = 96u64;
    let dir_table = inode_table + (blocks as u64) * 4;
    let used = dir_table + 6;
    sq[40..48].copy_from_slice(&used.to_le_bytes());
    sq[48..56].copy_from_slice(&used.to_le_bytes());
    sq[56..64].copy_from_slice(&u64::MAX.to_le_bytes());
    sq[64..72].copy_from_slice(&inode_table.to_le_bytes());
    sq[72..80].copy_from_slice(&dir_table.to_le_bytes());
    sq[80..88].copy_from_slice(&used.to_le_bytes());
    sq[88..96].copy_from_slice(&u64::MAX.to_le_bytes());
    for _ in 0..blocks {
        sq.extend_from_slice(&[2, 0x80, 0, 0]);
    }
    sq.extend_from_slice(&[2, 0x80, 0, 0, 0, 0]);
    let bytes = build::type2(&sq, &[], &[]);
    assert!(base < bytes.len());
    let i = inspect_bytes("blocks.AppImage", &bytes).unwrap();
    assert!(!i.inspected);
    assert!(i.note.contains("too large"), "{}", i.note);
}

#[test]
fn the_runtime_may_not_lie_about_where_it_ends() {
    let mut b = build::normal();
    // e_shnum = 0
    b[0x3C] = 0;
    b[0x3D] = 0;
    assert!(!inspect_bytes("n0.AppImage", &b).unwrap().inspected);
    // e_shoff beyond the file
    let mut b = build::normal();
    b[0x28..0x30].copy_from_slice(&(1u64 << 40).to_le_bytes());
    assert!(!inspect_bytes("far.AppImage", &b).unwrap().inspected);
}

// ---- signatures ----

mod signing {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::process::{Command, Stdio};

    pub struct Key {
        pub home: PathBuf,
        pub fingerprint: String,
    }

    impl Drop for Key {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .args(["--homedir"])
                .arg(&self.home)
                .args(["--kill", "gpg-agent"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    pub fn gpg(home: &std::path::Path, args: &[&str], stdin: Option<&[u8]>) -> Option<Vec<u8>> {
        use std::io::Write;
        let mut child = Command::new("gpg")
            .arg("--homedir")
            .arg(home)
            .args([
                "--batch",
                "--yes",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
            ])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        if let Some(data) = stdin {
            child.stdin.take().unwrap().write_all(data).ok()?;
        } else {
            drop(child.stdin.take());
        }
        let out = child.wait_with_output().ok()?;
        out.status.success().then_some(out.stdout)
    }

    /// A throwaway key in a private folder; `None` (with a message) where
    /// gpg is not installed.
    pub fn make_key(tag: &str) -> Option<Key> {
        let home = scratch(tag);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).ok()?;
        if gpg(
            &home,
            &[
                "--quick-generate-key",
                "Test Signer <test@example.org>",
                "ed25519",
                "sign",
                "never",
            ],
            None,
        )
        .is_none()
        {
            eprintln!("skipped: gpg is not available to make a test key");
            return None;
        }
        let colons = gpg(&home, &["--list-keys", "--with-colons"], None)?;
        let fingerprint = String::from_utf8(colons).ok()?.lines().find_map(|l| {
            l.strip_prefix("fpr:::::::::")
                .map(|r| r.trim_end_matches(':').to_string())
        })?;
        Some(Key { home, fingerprint })
    }

    pub fn public(k: &Key) -> Vec<u8> {
        gpg(&k.home, &["--armor", "--export", &k.fingerprint], None).unwrap()
    }

    pub fn sign(k: &Key, message: &str) -> Vec<u8> {
        gpg(
            &k.home,
            &[
                "--armor",
                "--detach-sign",
                "--local-user",
                &k.fingerprint,
                "--output",
                "-",
            ],
            Some(message.as_bytes()),
        )
        .unwrap()
    }

    /// What appimagetool does: hash the file with empty sections, sign the
    /// hex digest, put signature and key in the sections.
    pub fn signed(squash: &[u8], k: &Key) -> Vec<u8> {
        let unsigned = build::type2(squash, &[], &[]);
        let digest: String = Sha256::digest(&unsigned)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        build::type2(squash, &sign(k, &digest), &public(k))
    }
}

fn has_gpgv() -> bool {
    telamon_store_core::appimage::sign::find_gpgv().is_some()
}

#[test]
fn an_unsigned_appimage_says_so() {
    let i = inspect_bytes("u.AppImage", &build::normal()).unwrap();
    assert_eq!(i.signature, Signature::None);
}

#[test]
fn a_good_signature_names_the_key_but_nothing_is_trusted() {
    if !has_gpgv() {
        eprintln!("skipped: no gpgv");
        return;
    }
    let Some(key) = signing::make_key("sig-good") else {
        return;
    };
    let bytes = signing::signed(&build::normal_squash().build(), &key);
    let i = inspect_bytes("signed.AppImage", &bytes).unwrap();
    assert_eq!(
        i.signature,
        Signature::Signed {
            fingerprint: key.fingerprint.to_ascii_uppercase()
        }
    );
    assert!(i.inspected);
}

#[test]
fn a_changed_file_has_a_wrong_signature() {
    if !has_gpgv() {
        return;
    }
    let Some(key) = signing::make_key("sig-bad") else {
        return;
    };
    let mut bytes = signing::signed(&build::normal_squash().build(), &key);
    // Change a byte near the end of the file, outside the signature sections.
    let n = bytes.len();
    bytes[n - 3] ^= 1;
    let i = inspect_bytes("changed.AppImage", &bytes).unwrap();
    assert_eq!(i.signature, Signature::Wrong);
}

#[test]
fn a_signature_by_another_key_than_the_embedded_one_is_wrong() {
    if !has_gpgv() {
        return;
    }
    let (Some(a), Some(b)) = (signing::make_key("sig-a"), signing::make_key("sig-b")) else {
        return;
    };
    let squash = build::normal_squash().build();
    let unsigned = build::type2(&squash, &[], &[]);
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(&unsigned)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let bytes = build::type2(&squash, &signing::sign(&a, &digest), &signing::public(&b));
    let i = inspect_bytes("mixed.AppImage", &bytes).unwrap();
    assert_eq!(i.signature, Signature::Wrong);
}

#[test]
fn a_signature_that_cannot_be_read_is_unchecked() {
    let squash = build::normal_squash().build();
    // A signature with no key, and one with garbage for a key.
    let bytes = build::type2(
        &squash,
        b"-----BEGIN PGP SIGNATURE-----\n\nAAAA\n-----END PGP SIGNATURE-----\n",
        &[],
    );
    assert_eq!(
        inspect_bytes("a.AppImage", &bytes).unwrap().signature,
        Signature::Unchecked
    );
    let bytes = build::type2(
        &squash,
        b"-----BEGIN PGP SIGNATURE-----\n\nAAAA\n-----END PGP SIGNATURE-----\n",
        b"not a key",
    );
    assert_eq!(
        inspect_bytes("b.AppImage", &bytes).unwrap().signature,
        Signature::Unchecked
    );
}

#[test]
fn gpgv_status_lines_are_read_strictly() {
    use telamon_store_core::appimage::sign::parse_status;
    let fpr = "A".repeat(40);
    let good = format!(
        "[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG 1234 Someone\n[GNUPG:] VALIDSIG {fpr} 2026-01-01 1 0 4 0 22 8 00 {fpr}\n"
    );
    assert_eq!(
        parse_status(&good),
        Signature::Signed {
            fingerprint: fpr.clone()
        }
    );
    assert_eq!(parse_status("[GNUPG:] BADSIG 1234 x\n"), Signature::Wrong);
    assert_eq!(
        parse_status("[GNUPG:] ERRSIG 1 22 8 00 1 9 01 NO_PUBKEY\n[GNUPG:] NO_PUBKEY 1\n"),
        Signature::Wrong
    );
    assert_eq!(parse_status(""), Signature::Wrong);
    // A valid line next to a bad one is not good.
    assert_eq!(
        parse_status(&format!("{good}[GNUPG:] BADSIG 1 x\n")),
        Signature::Wrong
    );
    // A malformed fingerprint is not shown.
    assert_ne!(
        parse_status("[GNUPG:] VALIDSIG zzzz 1 1 0 4 0 22 8 00 zzzz\n"),
        Signature::Signed {
            fingerprint: "ZZZZ".into()
        }
    );
}

#[test]
fn armor_is_decoded_to_packets() {
    use telamon_store_core::appimage::sign::dearmor;
    let armored = b"-----BEGIN PGP PUBLIC KEY BLOCK-----\nVersion: x\n\nmQEN\n=abcd\n-----END PGP PUBLIC KEY BLOCK-----\n";
    assert_eq!(dearmor(armored).unwrap(), vec![0x99, 0x01, 0x0d]);
    assert_eq!(dearmor(b"not armor"), None);
    assert_eq!(dearmor(&[0x99, 1, 2]).unwrap(), vec![0x99, 1, 2]);
    assert_eq!(
        dearmor(
            b"-----BEGIN PGP PUBLIC KEY BLOCK-----\n\n!!!!\n-----END PGP PUBLIC KEY BLOCK-----"
        ),
        None
    );
}

// ---- the ELF part ----

/// A 32-bit ELF with `.shstrtab` and `.sha256_sig`, in either byte order.
fn elf32(big: bool) -> Vec<u8> {
    let w16 = |v: u16| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let w32 = |v: u32| {
        if big {
            v.to_be_bytes()
        } else {
            v.to_le_bytes()
        }
    };
    let names = b"\0.shstrtab\0.sha256_sig\0";
    let sig_off = 52u32;
    let names_off = sig_off + 16;
    let shoff = (names_off + names.len() as u32).div_ceil(4) * 4;
    let mut b = vec![0u8; 52];
    b[..4].copy_from_slice(b"\x7fELF");
    b[4] = 1;
    b[5] = if big { 2 } else { 1 };
    b[8..11].copy_from_slice(b"AI\x02");
    b[0x20..0x24].copy_from_slice(&w32(shoff));
    b[0x2E..0x30].copy_from_slice(&w16(40));
    b[0x30..0x32].copy_from_slice(&w16(3));
    b[0x32..0x34].copy_from_slice(&w16(1));
    b.extend_from_slice(b"SIGSIGSIGSIGSIG\0");
    b.extend_from_slice(names);
    b.resize(shoff as usize, 0);
    let hdr = |name: u32, off: u32, size: u32| {
        let mut h = vec![0u8; 40];
        h[0..4].copy_from_slice(&w32(name));
        h[16..20].copy_from_slice(&w32(off));
        h[20..24].copy_from_slice(&w32(size));
        h
    };
    b.extend_from_slice(&[0u8; 40]);
    b.extend(hdr(1, names_off, names.len() as u32));
    b.extend(hdr(11, sig_off, 16));
    b
}

#[test]
fn elf_sections_are_read_in_both_widths_and_byte_orders() {
    use telamon_store_core::appimage::format::{ElfError, read_elf, sniff};
    for big in [false, true] {
        let dir = scratch("elf32");
        let bytes = elf32(big);
        let p = build::write(&dir, "e", &bytes);
        let f = std::fs::File::open(&p).unwrap();
        let elf = read_elf(&f, bytes.len() as u64).unwrap();
        assert_eq!(elf.end, bytes.len() as u64);
        let s = elf.section(".sha256_sig").unwrap();
        assert_eq!((s.offset, s.size), (52, 16));
        assert_eq!(
            elf.read_section(&f, bytes.len() as u64, ".sha256_sig", 64)
                .unwrap(),
            b"SIGSIGSIGSIGSIG\0"
        );
        assert!(
            elf.read_section(&f, bytes.len() as u64, ".sha256_sig", 8)
                .is_none(),
            "over the cap"
        );
        assert!(
            elf.read_section(&f, bytes.len() as u64, ".sig_key", 64)
                .is_none()
        );
        assert!(sniff(&bytes).is_some());
    }
    // Not ELF at all, and a bad class.
    let dir = scratch("elf-bad");
    let p = build::write(&dir, "x", &[b'x'; 200]);
    let f = std::fs::File::open(&p).unwrap();
    assert_eq!(read_elf(&f, 200).unwrap_err(), ElfError::NotElf);
    let mut b = elf32(false);
    b[4] = 9;
    let p = build::write(&dir, "y", &b);
    let f = std::fs::File::open(&p).unwrap();
    assert_eq!(read_elf(&f, b.len() as u64).unwrap_err(), ElfError::NotElf);
}

// ---- gpgv is run as locked down as it can be ----

mod gpgv_run {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    use telamon_store_core::appimage::sign::{self, verify_within};

    /// A stand-in for `gpgv`: a script that records how it was started in
    /// `out`, then does `then`.
    fn fake_gpgv(dir: &std::path::Path, out: &std::path::Path, then: &str) -> PathBuf {
        let script = dir.join("gpgv");
        std::fs::write(
            &script,
            format!(
                "#!/bin/bash\n\
                 {{ echo \"home=$GNUPGHOME\"; echo \"cpu=$(ulimit -t)\"; echo \"fsize=$(ulimit -f)\"; \
                 echo \"core=$(ulimit -c)\"; echo \"files=$(ulimit -n)\"; echo \"nonewprivs=$(grep NoNewPrivs /proc/self/status)\"; \
                 echo \"pgid=$(cut -d' ' -f5 /proc/$$/stat)\"; echo \"args=$*\"; echo \"env:\"; env; }} > '{}'\n{then}\n",
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    fn pgid_of_me() -> String {
        // SAFETY: getpgrp has no arguments and cannot fail.
        unsafe { libc::getpgrp() }.to_string()
    }

    fn key_and_sig() -> (Vec<u8>, Vec<u8>) {
        // Any binary OpenPGP-looking bytes pass the reading; a fake gpgv
        // does not check them.
        (vec![0x99, 1, 2, 3], vec![0x89, 1, 2, 3])
    }

    #[test]
    fn gpgv_gets_an_empty_environment_limits_and_a_group_of_its_own() {
        let dir = scratch("gpgv-env");
        let out = dir.join("seen");
        let gpgv = fake_gpgv(&dir, &out, "exit 0");
        let (key, sig) = key_and_sig();
        let _ = verify_within(&gpgv, &sig, &key, "abc", Duration::from_secs(20));
        let seen = std::fs::read_to_string(&out).unwrap();
        let (head, env) = seen.split_once("env:\n").unwrap();
        // Nothing of the caller's environment: no agent socket, no proxy,
        // no HOME. (The shell adds its own PWD, SHLVL and _.)
        for line in env.lines() {
            let name = line.split('=').next().unwrap();
            assert!(
                matches!(
                    name,
                    "LC_ALL" | "GNUPGHOME" | "PWD" | "SHLVL" | "_" | "OLDPWD"
                ),
                "unexpected variable: {line}"
            );
        }
        assert!(head.contains("cpu=20\n"), "{head}");
        assert!(head.contains("core=0\n"), "{head}");
        assert!(head.contains("files=64\n"), "{head}");
        assert!(head.contains("NoNewPrivs:\t1"), "{head}");
        // 1 MiB, in the shell's 1024-byte units.
        assert!(head.contains("fsize=1024\n"), "{head}");
        let pgid = head
            .lines()
            .find_map(|l| l.strip_prefix("pgid="))
            .unwrap()
            .trim();
        assert_ne!(pgid, pgid_of_me(), "gpgv is in the caller's group");
        // The private home is inside the private folder, which is gone now.
        let home = head.lines().find_map(|l| l.strip_prefix("home=")).unwrap();
        assert!(
            !std::path::Path::new(home).parent().unwrap().exists(),
            "the temporary folder was left behind"
        );
        assert!(head.contains("--status-fd 1 --keyring"), "{head}");
    }

    #[test]
    fn a_gpgv_that_hangs_is_killed_with_what_it_started_and_leaves_nothing() {
        let dir = scratch("gpgv-hang");
        let out = dir.join("seen");
        let child_pid = dir.join("child.pid");
        let gpgv = fake_gpgv(
            &dir,
            &out,
            &format!(
                "/usr/bin/sleep 300 &\necho $! > '{}'\nwait",
                child_pid.display()
            ),
        );
        let (key, sig) = key_and_sig();
        let started = Instant::now();
        let got = verify_within(&gpgv, &sig, &key, "abc", Duration::from_millis(1500));
        assert_eq!(got, Signature::Unchecked);
        assert!(started.elapsed() < Duration::from_secs(10));
        let pid: i32 = std::fs::read_to_string(&child_pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z"))
            .unwrap_or(false);
        assert!(!alive, "what gpgv started outlived the timeout");
        let seen = std::fs::read_to_string(&out).unwrap();
        let home = seen.lines().find_map(|l| l.strip_prefix("home=")).unwrap();
        assert!(!std::path::Path::new(home).parent().unwrap().exists());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_gpgv_cannot_open_a_socket() {
        // The stand-in tries to connect to a listener of ours through bash's
        // /dev/tcp; with the filter on `socket` fails and nobody arrives.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = scratch("gpgv-socket");
        let out = dir.join("seen");
        let gpgv = fake_gpgv(
            &dir,
            &out,
            &format!(
                "if exec 3<>/dev/tcp/127.0.0.1/{port}; then echo connected > '{r}.net'; else echo blocked > '{r}.net'; fi 2>/dev/null",
                r = out.display()
            ),
        );
        let (key, sig) = key_and_sig();
        let _ = verify_within(&gpgv, &sig, &key, "abc", Duration::from_secs(20));
        let net = std::fs::read_to_string(format!("{}.net", out.display())).unwrap();
        assert_eq!(net.trim(), "blocked");
        assert!(
            matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "gpgv connected to the listener"
        );
    }

    #[test]
    fn a_gpgv_that_floods_its_output_does_not_stall_or_pass() {
        let dir = scratch("gpgv-flood");
        let out = dir.join("seen");
        // 8 MiB of status text, then exit 0: more than the pipe holds and
        // more than is read.
        let gpgv = fake_gpgv(&dir, &out, "head -c 8388608 /dev/zero | tr '\\0' 'x'");
        let (key, sig) = key_and_sig();
        let started = Instant::now();
        let got = verify_within(&gpgv, &sig, &key, "abc", Duration::from_secs(20));
        assert_eq!(got, Signature::Wrong);
        assert!(started.elapsed() < Duration::from_secs(10), "stalled");
    }

    #[test]
    fn an_inline_signature_is_not_a_signature_of_the_file() {
        if !has_gpgv() {
            eprintln!("skipped: no gpgv");
            return;
        }
        let Some(key) = signing::make_key("sig-inline") else {
            return;
        };
        // A genuine signature by the embedded key, but of some other text and
        // not detached: it must not read as "signed".
        let inline = signing::gpg(
            &key.home,
            &[
                "--armor",
                "--sign",
                "--local-user",
                &key.fingerprint,
                "--output",
                "-",
            ],
            Some(b"anything at all"),
        )
        .unwrap();
        let bytes = build::type2(
            &build::normal_squash().build(),
            &inline,
            &signing::public(&key),
        );
        let i = inspect_bytes("inline.AppImage", &bytes).unwrap();
        assert_eq!(i.signature, Signature::Wrong);
        let _ = sign::MAX_SIG;
    }
}
