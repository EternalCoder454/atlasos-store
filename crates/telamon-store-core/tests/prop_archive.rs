//! Property tests of the bundle unpacker (`native::archive::unpack`): an
//! archive of arbitrary bytes, of arbitrary tar headers, and of a real bundle
//! with hostile entries added, is refused or unpacked below the destination
//! and nowhere else. Every case runs in a sandbox folder with a canary beside
//! the destination. Bounded runs; `fuzz/` runs the same check guided by
//! coverage.

mod harness;

use harness::{checks, strat};
use proptest::prelude::*;
use proptest::sample::select;
use telamon_store_core::native::fake::{BundleBuilder, Raw};

fn entry_type() -> impl Strategy<Value = tar::EntryType> {
    prop_oneof![
        6 => select(vec![tar::EntryType::Regular, tar::EntryType::Directory, tar::EntryType::Symlink]),
        3 => select(vec![
            tar::EntryType::Link, tar::EntryType::Char, tar::EntryType::Block, tar::EntryType::Fifo,
            tar::EntryType::Continuous, tar::EntryType::GNUSparse, tar::EntryType::XGlobalHeader,
            tar::EntryType::XHeader, tar::EntryType::GNULongName, tar::EntryType::GNULongLink,
        ]),
        1 => any::<u8>().prop_map(tar::EntryType::new),
    ]
}

fn raw_entry() -> impl Strategy<Value = (String, tar::EntryType, Vec<u8>, Option<String>)> {
    (
        prop_oneof![
            3 => strat::rel_path(),
            2 => select(vec![
                "../evil", "bin/../../evil", "/etc/evil", "./x", "share//x", "a/b/../../../c", "..",
                ".", "./", "x/", "dir/", "bin", "share", "bin/telamon-gates", "telamon-bundle.json",
                "share/applications/net.eterneon.telamon.gates.desktop", "\u{202E}x", "a\0b",
                checks::ABSOLUTE_CANARY, "../../../telamon-prop-up-canary", "one/two/../../../x",
            ]).prop_map(str::to_string),
        ],
        entry_type(),
        strat::bytes(40),
        prop_oneof![
            2 => strat::link_target().prop_map(Some),
            1 => Just(None),
        ],
    )
        .prop_map(|(name, kind, data, link)| {
            let data = if kind == tar::EntryType::Regular || kind == tar::EntryType::Continuous {
                data
            } else {
                Vec::new()
            };
            (name, kind, data, link)
        })
}

/// A real bundle (a program, a desktop entry, an icon, ...) with hostile
/// entries added, extra files and links, and the inner manifest tampered with.
fn bundle() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::collection::vec(raw_entry(), 0..4),
        prop::collection::vec((strat::rel_path(), strat::bytes(32), any::<bool>()), 0..3),
        prop::collection::vec((strat::rel_path(), strat::link_target()), 0..3),
        0u8..6,
        any::<bool>(),
    )
        .prop_map(|(raws, files, links, tamper, skip)| {
            let mut b = BundleBuilder::new("net.eterneon.telamon.gates", "Telamon Gates", "1.0.0")
                .exe("telamon-gates");
            for (name, kind, data, link) in raws {
                b = b.raw(Raw {
                    name: name.into_bytes(),
                    kind,
                    data,
                    link: link.map(String::into_bytes),
                });
            }
            for (p, bytes, x) in files {
                b = b.file(&p, &bytes, x);
            }
            for (p, t) in links {
                b = b.link(&p, &t);
            }
            b = match tamper {
                0 => b.edit_inner(|m| m.files.truncate(1)),
                1 => b.edit_inner(|m| {
                    if let Some(f) = m.files.first_mut() {
                        f.sha256 = "0".repeat(64);
                    }
                }),
                2 => b.edit_inner(|m| {
                    if let Some(f) = m.files.first_mut() {
                        f.size += 1;
                    }
                }),
                3 => b.edit_inner(|m| m.links.clear()),
                _ => b,
            };
            if skip {
                b = b.no_manifest();
            }
            // The builder is test tooling and refuses some names itself.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| b.build().archive))
                .unwrap_or_default()
        })
}

/// A tar header written by hand, checksum fixed, fields mostly random: octal
/// and base-256 sizes, odd magic, long names, any type flag.
fn header() -> impl Strategy<Value = Vec<u8>> {
    (
        prop::collection::vec(any::<u8>(), 100),
        prop::collection::vec(any::<u8>(), 100),
        select(vec![
            b"0000644\0".to_vec(),
            b"7777777\0".to_vec(),
            b"4755\0\0\0\0".to_vec(),
            vec![0xff; 8],
            vec![0; 8],
        ]),
        prop_oneof![
            3 => (0u64..4096).prop_map(|n| format!("{n:011o}\0").into_bytes()),
            1 => Just(b"77777777777\0".to_vec()),
            1 => Just(vec![0x80, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
            1 => prop::collection::vec(any::<u8>(), 12),
        ],
        any::<u8>(),
        select(vec![
            b"ustar\x0000".to_vec(),
            b"ustar  \0".to_vec(),
            vec![0; 8],
            b"GNUtar\0\0".to_vec(),
        ]),
        strat::bytes(600),
        any::<bool>(),
    )
        .prop_map(|(name, link, mode, size, flag, magic, data, name_ascii)| {
            let mut h = [0u8; 512];
            let name: Vec<u8> = if name_ascii {
                name.iter().map(|b| b"abc/._-.."[*b as usize % 9]).collect()
            } else {
                name
            };
            h[..100].copy_from_slice(&name);
            h[100..108].copy_from_slice(&mode);
            h[124..136].copy_from_slice(&size);
            h[156] = flag;
            h[157..257].copy_from_slice(&link);
            h[257..265].copy_from_slice(&magic);
            h[148..156].copy_from_slice(b"        ");
            let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            let mut out = h.to_vec();
            out.extend_from_slice(&data);
            let pad = (512 - data.len() % 512) % 512;
            out.extend(std::iter::repeat_n(0u8, pad));
            out
        })
}

fn zstd(bytes: &[u8]) -> Vec<u8> {
    zstd::stream::encode_all(bytes, 1).unwrap()
}

proptest! {
    #![proptest_config(strat::config())]

    #[test]
    fn hostile_bundles(archive in bundle()) {
        prop_assume!(!archive.is_empty());
        checks::native_unpack(&archive);
    }

    #[test]
    fn damaged_bundles(archive in bundle().prop_flat_map(|a| strat::mutated(a, &[b"\x28\xb5\x2f\xfd", b"\0\0\0\0", b"\xff\xff\xff\xff"], 6))) {
        checks::native_unpack(&archive);
    }

    #[test]
    fn arbitrary_tar_headers(headers in prop::collection::vec(header(), 1..5), end in any::<bool>()) {
        let mut tar: Vec<u8> = headers.concat();
        if end {
            tar.extend_from_slice(&[0u8; 1024]);
        }
        checks::native_unpack(&zstd(&tar));
    }

    #[test]
    fn arbitrary_tar_streams(tar in strat::bytes(2000)) {
        checks::native_unpack(&zstd(&tar));
    }

    #[test]
    fn arbitrary_bytes(bytes in strat::bytes(400)) {
        checks::native_unpack(&bytes);
    }
}

#[test]
fn a_good_bundle_unpacks_and_nothing_else_happens() {
    let archive = BundleBuilder::new("net.eterneon.telamon.gates", "Telamon Gates", "1.0.0")
        .exe("telamon-gates")
        .build()
        .archive;
    checks::native_unpack(&archive);
}
