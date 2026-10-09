//! Property tests of what the Store reads out of an AppImage without running
//! it: the ELF runtime's headers, the squashfs (superblock checks, links
//! resolved inside the image, caps), the desktop entry, metainfo and icon, and
//! the answer of the inspection helper. Bounded runs; `fuzz/` runs the same
//! checks guided by coverage.

mod harness;
#[path = "common/appimage.rs"]
mod image;

use std::collections::BTreeSet;

use backhand::compression::Compressor;
use harness::{checks, strat};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::json;
use telamon_store_core::appimage::squash::{self, Limits};

const SQUASH_DICT: &[&[u8]] = &[
    b"hsqs",
    b"\0\0\0\0",
    b"\xff\xff\xff\xff",
    b"\xff\xff\xff\xff\xff\xff\xff\xff",
    b"\x04\x00\x00\x00",
    b"\x01\x00",
    b"\x06\x00",
];
const ELF_DICT: &[&[u8]] = &[
    b"\x7fELF",
    b"AI",
    b"\x01",
    b"\x02",
    b"\xff\xff",
    b"\xff\xff\xff\xff\xff\xff\xff\xff",
    b".sha256_sig\0",
    b".sig_key\0",
    b"\0\0\0\0",
];
const ICON_DICT: &[&[u8]] = &[
    b"<script",
    b"<use ",
    b"href=",
    b"xlink:href=\"#a\"",
    b"href='http://x/y.svg'",
    b"<image",
    b"<!ENTITY",
    b"<!DOCTYPE",
    b"<style>",
    b"@import",
    b"foreignObject",
    b"<a ",
    b"javascript:",
    b"\xEF\xBB\xBF",
    b"\0",
    b"<?xml version=\"1.0\"?>",
    b"<svg",
    b"</svg>",
    b"\x89PNG\r\n\x1a\n",
    b"IHDR",
];

fn squash_path() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => select(vec![
            "/a.desktop", "/b.desktop", "/AppRun", "/.DirIcon", "/x.png", "/x.svg", "/usr/share/applications/y.desktop",
            "/usr/share/metainfo/org.x.Y.metainfo.xml", "/usr/share/icons/hicolor/48x48/apps/x.png",
            "/usr/share/icons/hicolor/scalable/apps/x.svg", "/usr/share/pixmaps/x.png", "/usr/lib/libx.so",
            "/usr/share/appdata/org.x.Y.appdata.xml",
        ]).prop_map(str::to_string),
        1 => "/usr/share/applications/[a-z]{1,6}\\.desktop",
        1 => "/[a-z]{1,6}",
    ]
}

fn link_target() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => select(vec![
            "a.desktop", "b.desktop", "x.png", "x.svg", "../a.desktop", "../../a.desktop", "/a.desktop",
            "/etc/passwd", "../../../../../../etc/passwd", "/", ".", "..", "", "l1", "l2", "l3", "usr/share",
            "/usr/share/applications/y.desktop", "../../../x.png", "./a.desktop", "a.desktop/", "//a.desktop",
        ]).prop_map(str::to_string),
        1 => "[a-z./]{1,12}",
    ]
}

fn desktop() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => Just(image::DESKTOP.as_bytes().to_vec()),
        2 => strat::mutated(image::DESKTOP.as_bytes().to_vec(), strat::DESKTOP_DICT, 6),
        2 => strat::desktop_text(),
    ]
}

fn xml() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => Just(image::APPSTREAM.as_bytes().to_vec()),
        3 => strat::mutated(image::APPSTREAM.as_bytes().to_vec(), &[b"<", b">", b"</", b"<![CDATA[", b"]]>", b"&amp;", b"&#0;", b"&#x202E;", b"&lt;b&gt;", b"<!ENTITY", b"<!DOCTYPE", b"<id>", b"</id>", b"<name>", b"xml:lang=", b"\xff"], 8),
        1 => strat::bytes(100),
    ]
}

fn icon_file() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        2 => (0u32..4000, 0u32..4000).prop_map(|(w, h)| image::fake_png(w, h)),
        2 => strat::mutated(b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 8 8\"><rect fill=\"url(#g)\" width=\"8\" height=\"8\"/></svg>".to_vec(), ICON_DICT, 6),
        1 => strat::bytes(80),
    ]
}

/// A squashfs of hostile files and links, in the compressors AppImages use.
/// The files, the links and the squashfs built from them.
type Image = (Vec<(String, Vec<u8>)>, Vec<(String, String)>, Vec<u8>);

fn squashfs() -> impl Strategy<Value = Image> {
    (
        prop::collection::vec(
            (squash_path(), prop_oneof![desktop(), xml(), icon_file()]),
            0..6,
        ),
        prop::collection::vec((squash_path(), link_target()), 0..6),
        select(vec![Compressor::Gzip, Compressor::Zstd, Compressor::Xz]),
    )
        .prop_map(|(mut files, mut links, comp)| {
            // One entry per path; a link may not share a name with a file.
            let mut seen = BTreeSet::new();
            files.retain(|(p, _)| seen.insert(p.clone()));
            links.retain(|(p, _)| seen.insert(p.clone()));
            let mut s = image::Squash {
                compressor: comp,
                ..image::Squash::default()
            };
            for (p, b) in &files {
                s = s.file(p, b.clone());
            }
            for (p, t) in &links {
                s = s.link(p, t);
            }
            let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.build()))
                .unwrap_or_default();
            (files, links, bytes)
        })
}

fn superblock_ops() -> impl Strategy<Value = Vec<strat::Op>> {
    prop::collection::vec(
        prop_oneof![
            (0usize..96, 0u8..4, 0u8..6).prop_map(|(p, w, v)| strat::Op::Extreme(p, w, v)),
            (0usize..96, any::<u8>()).prop_map(|(p, v)| strat::Op::Set(p, v)),
            (0usize..96, 0u8..8).prop_map(|(p, b)| strat::Op::Flip(p, b)),
            (0usize..1 << 20, 0u8..8).prop_map(|(p, b)| strat::Op::Flip(p, b)),
        ],
        1..5,
    )
}

proptest! {
    #![proptest_config(strat::config())]

    #[test]
    fn squashfs_with_hostile_links_reads_only_the_image((files, links, bytes) in squashfs()) {
        prop_assume!(!bytes.is_empty());
        checks::squashfs(&bytes);
        // Whatever a link yields is the content of a file of the image.
        let contents: BTreeSet<Vec<u8>> = files.iter().map(|(_, b)| b.clone()).collect();
        let file = checks::memfile(&bytes);
        let limits = Limits::default();
        let r = squash::with_tree(&file, 0, bytes.len() as u64, &limits, |tree| {
            for (p, _) in &links {
                let key = p.trim_start_matches('/');
                if let Ok(b) = tree.read(key, 1 << 20) {
                    prop_assert!(contents.contains(&b), "{key} read {} bytes that are no file of the image", b.len());
                }
            }
            Ok(())
        });
        if let Ok(inner) = r {
            inner?;
        }
    }

    #[test]
    fn damaged_squashfs_superblocks((_f, _l, bytes) in squashfs(), ops in superblock_ops()) {
        prop_assume!(!bytes.is_empty());
        checks::squashfs(&strat::apply(bytes, &ops, SQUASH_DICT));
    }

    #[test]
    fn mutated_squashfs(bytes in squashfs().prop_flat_map(|(_, _, b)| strat::mutated(b, SQUASH_DICT, 6))) {
        checks::squashfs(&bytes);
    }

    #[test]
    fn squashfs_from_noise(bytes in strat::bytes(300)) {
        checks::squashfs(&bytes);
        let mut with_magic = b"hsqs".to_vec();
        with_magic.extend_from_slice(&bytes);
        checks::squashfs(&with_magic);
    }

    #[test]
    fn damaged_appimages(ops in prop::collection::vec(prop_oneof![
        (0usize..64, 0u8..4, 0u8..6).prop_map(|(p, w, v)| strat::Op::Extreme(p, w, v)),
        (0usize..64, any::<u8>()).prop_map(|(p, v)| strat::Op::Set(p, v)),
        (0usize..1 << 14, any::<u8>()).prop_map(|(p, v)| strat::Op::Set(p, v)),
        (0usize..1 << 14, 0u8..8).prop_map(|(p, b)| strat::Op::Flip(p, b)),
        (0usize..1 << 14, 0usize..8).prop_map(|(p, d)| strat::Op::Splice(p, d)),
    ], 1..6)) {
        let base = image::type2(&image::normal_squash().build(), &[], &[]);
        checks::appimage_file(&strat::apply(base, &ops, ELF_DICT));
    }

    #[test]
    fn appimages_from_noise(bytes in strat::bytes(200)) {
        checks::appimage_file(&bytes);
        let mut elf = b"\x7fELF\x02\x01\x01\0AI\x02".to_vec();
        elf.extend_from_slice(&bytes);
        checks::appimage_file(&elf);
    }

    #[test]
    fn icons(bytes in icon_file()) {
        checks::icon(&bytes);
    }

    #[test]
    fn icons_from_noise(bytes in strat::bytes(120)) {
        checks::icon(&bytes);
    }

    #[test]
    fn helper_answers(
        base in Just(json!({
            "format": "type2", "size": 1234, "sha256": "ab".repeat(32), "file_name": "x.AppImage",
            "inspected": true, "note": "", "name": "X", "version": "1", "publisher": "P", "summary": "S",
            "app_id": "org.x.Y", "icon_kind": "png", "signature": {"state": "none"}, "origin": {"kind": "unknown"},
        })),
        edits in strat::jops(),
        text in strat::nasty_string(),
        tail in icon_file(),
        replace in 0usize..6,
    ) {
        let mut v = base;
        strat::jmutate(&mut v, &edits);
        if let Some(o) = v.as_object_mut() {
            let key = ["name", "version", "publisher", "summary", "note", "file_name"][replace];
            o.insert(key.into(), json!(text));
        }
        let mut bytes = serde_json::to_vec(&v).unwrap();
        bytes.push(b'\n');
        bytes.extend_from_slice(&tail);
        checks::inspection_decode(&bytes);
    }

    #[test]
    fn helper_answers_from_noise(bytes in strat::bytes(160)) {
        checks::inspection_decode(&bytes);
    }

    #[test]
    fn metainfo_xml(bytes in xml()) {
        checks::appstream_metainfo(&bytes);
    }
}

#[test]
fn a_normal_appimage_is_read() {
    let bytes = image::normal();
    checks::appimage_file(&bytes);
    checks::squashfs(&image::normal_squash().build());
}

/// FINDING (medium; found by the `appimage_file` fuzz target): backhand reads the
/// squashfs through a reader that adds the squashfs's start in the file (the
/// end of the ELF runtime) to every offset it seeks to, with an unchecked `+`.
/// The id and fragment tables are arrays of block offsets stored in the file,
/// and `check_superblock` walks only the inode and directory tables, so one
/// of those offsets near `u64::MAX` overflows that sum. It wrapped silently
/// before the release profile turned on `overflow-checks`; now it panics inside
/// backhand. The helper process and the workers contain the panic (the Store
/// says it could not look inside), but a hostile file should be refused, not
/// panic: expected, `Err(SquashError::Damaged(..))` from `squash::with_tree`
/// (check those offsets against `used`, or catch the unwind around backhand).
#[test]
fn a_table_block_offset_near_the_top_of_u64_is_refused_not_a_panic() {
    for field in [48usize, 80] {
        let squash = image::normal_squash().build();
        let mut bytes = image::type2(&squash, &[], &[]);
        let start = bytes.len() - squash.len();
        // The superblock's pointer to the table, and in the table, the offset of its first block.
        let table = u64::from_le_bytes(bytes[start + field..start + field + 8].try_into().unwrap())
            as usize;
        bytes[start + table..start + table + 8]
            .copy_from_slice(&0xFFFF_FFFF_FFFF_FF00u64.to_le_bytes());
        let file = checks::memfile(&bytes);
        let len = bytes.len() as u64;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            squash::with_tree(&file, start as u64, len, &Limits::default(), |_| ())
        }));
        assert!(
            r.is_ok(),
            "table at superblock field {field}: backhand panicked"
        );
    }
}

/// FINDING (low): `check_superblock` shifted by the superblock's 16-bit
/// `block_log` before checking it (`block_log > 20` is refused first now).
#[test]
fn a_block_log_over_63_is_refused_not_a_panic() {
    for log in [64u16, 100, 0x8000, u16::MAX] {
        let mut bytes = image::normal_squash().build();
        bytes[22..24].copy_from_slice(&log.to_le_bytes());
        let file = checks::memfile(&bytes);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            squash::with_tree(&file, 0, bytes.len() as u64, &Limits::default(), |_| ())
        }));
        assert!(matches!(r, Ok(Err(_))), "block_log {log}: {r:?}");
    }
}

/// FINDING (low): backhand reads directory blocks until it reaches the first
/// pointer of a lookup table (the fragment table's); one that is not on a block
/// boundary of the directory table made it read on past the image, a tiny block
/// at a time. The pointer is now checked against the chain in
/// `check_superblock`, before backhand reads anything.
#[test]
fn a_lookup_pointer_off_the_directory_chain_is_refused() {
    let mut bytes = image::normal_squash().build();
    let at = u64::from_le_bytes(bytes[80..88].try_into().unwrap()) as usize;
    let good = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    // One byte into a block, not at its start.
    bytes[at..at + 8].copy_from_slice(&(good + 1).to_le_bytes());
    let file = checks::memfile(&bytes);
    let r = squash::with_tree(&file, 0, bytes.len() as u64, &Limits::default(), |_| ());
    assert!(
        matches!(
            r,
            Err(squash::SquashError::Damaged(
                "directory table" | "metadata tables"
            ))
        ),
        "{r:?}"
    );
}
