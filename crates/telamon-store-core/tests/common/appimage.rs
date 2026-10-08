//! Builds AppImages for the tests, in memory: an ELF64 runtime with the
//! `.sha256_sig` and `.sig_key` sections, followed by a squashfs written with
//! `backhand`'s writer. Nothing here runs anything.
#![allow(dead_code)]

use std::io::Cursor;

use backhand::compression::Compressor;
use backhand::{FilesystemCompressor, FilesystemWriter, NodeHeader};

pub const SIG_SIZE: usize = 1024;
pub const KEY_SIZE: usize = 8192;

/// A squashfs under construction.
pub struct Squash {
    pub files: Vec<(String, Vec<u8>)>,
    pub symlinks: Vec<(String, String)>,
    pub compressor: Compressor,
}

impl Default for Squash {
    fn default() -> Squash {
        Squash {
            files: Vec::new(),
            symlinks: Vec::new(),
            compressor: Compressor::Gzip,
        }
    }
}

impl Squash {
    pub fn file(mut self, path: &str, data: impl Into<Vec<u8>>) -> Squash {
        self.files.push((path.to_string(), data.into()));
        self
    }

    pub fn link(mut self, path: &str, target: &str) -> Squash {
        self.symlinks.push((path.to_string(), target.to_string()));
        self
    }

    pub fn zstd(mut self) -> Squash {
        self.compressor = Compressor::Zstd;
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut w = FilesystemWriter::default();
        w.set_compressor(FilesystemCompressor::new(self.compressor, None).unwrap());
        let header = NodeHeader::new(0o755, 0, 0, 0);
        for path in self
            .files
            .iter()
            .map(|f| &f.0)
            .chain(self.symlinks.iter().map(|l| &l.0))
        {
            if let Some(parent) = std::path::Path::new(path).parent()
                && parent != std::path::Path::new("/")
            {
                let _ = w.push_dir_all(parent, header);
            }
        }
        for (path, data) in &self.files {
            w.push_file(Cursor::new(data.clone()), path, header)
                .unwrap();
        }
        for (path, target) in &self.symlinks {
            w.push_symlink(target, path, header).unwrap();
        }
        let mut out = Cursor::new(Vec::new());
        w.write(&mut out).unwrap();
        out.into_inner()
    }
}

const NAMES: &[u8] = b"\0.shstrtab\0.sha256_sig\0.sig_key\0";

/// The ELF runtime: header, signature and key sections (padded with zeros to
/// their real sizes), section names and the section header table. Its length
/// is where the squashfs starts.
pub fn runtime(sig: &[u8], key: &[u8], marker: u8) -> Vec<u8> {
    assert!(sig.len() <= SIG_SIZE && key.len() <= KEY_SIZE);
    let sig_off = 64u64;
    let key_off = sig_off + SIG_SIZE as u64;
    let names_off = key_off + KEY_SIZE as u64;
    let shoff = (names_off + NAMES.len() as u64).div_ceil(8) * 8;
    let mut b = vec![0u8; 64];
    b[..4].copy_from_slice(b"\x7fELF");
    b[4] = 2; // 64-bit
    b[5] = 1; // little endian
    b[6] = 1;
    b[8..10].copy_from_slice(b"AI");
    b[10] = marker;
    b[16..18].copy_from_slice(&2u16.to_le_bytes()); // executable
    b[18..20].copy_from_slice(&62u16.to_le_bytes()); // x86-64
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
    b[0x34..0x36].copy_from_slice(&64u16.to_le_bytes());
    b[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes());
    b[0x3C..0x3E].copy_from_slice(&4u16.to_le_bytes());
    b[0x3E..0x40].copy_from_slice(&1u16.to_le_bytes());
    let mut s = sig.to_vec();
    s.resize(SIG_SIZE, 0);
    b.extend_from_slice(&s);
    let mut k = key.to_vec();
    k.resize(KEY_SIZE, 0);
    b.extend_from_slice(&k);
    b.extend_from_slice(NAMES);
    b.resize(shoff as usize, 0);
    let header = |name: u32, ty: u32, off: u64, size: u64| {
        let mut h = vec![0u8; 64];
        h[0..4].copy_from_slice(&name.to_le_bytes());
        h[4..8].copy_from_slice(&ty.to_le_bytes());
        h[24..32].copy_from_slice(&off.to_le_bytes());
        h[32..40].copy_from_slice(&size.to_le_bytes());
        h
    };
    b.extend_from_slice(&[0u8; 64]);
    b.extend(header(1, 3, names_off, NAMES.len() as u64));
    b.extend(header(11, 1, sig_off, SIG_SIZE as u64));
    b.extend(header(23, 1, key_off, KEY_SIZE as u64));
    b
}

/// A type 2 AppImage: runtime then squashfs.
pub fn type2(squash: &[u8], sig: &[u8], key: &[u8]) -> Vec<u8> {
    let mut b = runtime(sig, key, 2);
    b.extend_from_slice(squash);
    b
}

/// A type 1 AppImage (an ISO 9660 payload, here only the marker and filler).
pub fn type1() -> Vec<u8> {
    let mut b = runtime(&[], &[], 1);
    b.resize(40_000, 0);
    b[32769..32774].copy_from_slice(b"CD001");
    b
}

/// A PNG header of the given size with a body that is not a real image:
/// enough for the checks that read the header.
pub fn fake_png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
    b.extend_from_slice(&13u32.to_be_bytes());
    b.extend_from_slice(b"IHDR");
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0]);
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(b"\0\0\0\0IEND\xaeB`\x82");
    b
}

pub const APPSTREAM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>org.example.Sample</id>
  <name>Sample Draw</name>
  <summary>Draw things</summary>
  <developer><name>Example Studio</name></developer>
  <project_license>MIT</project_license>
  <releases>
    <release version="2.1.0" date="2026-09-01"/>
    <release version="2.0.0" date="2026-01-01"/>
  </releases>
</component>
"#;

pub const DESKTOP: &str = "[Desktop Entry]\nType=Application\nName=Sample Draw Desktop\nExec=sample-draw %U\nIcon=sample-draw\nCategories=Graphics;\n";

/// A normal squashfs: desktop entry, icon, metainfo.
pub fn normal_squash() -> Squash {
    Squash::default()
        .file("/AppRun", b"#!/bin/sh\n".to_vec())
        .file("/sample-draw.desktop", DESKTOP)
        .file("/sample-draw.png", fake_png(256, 256))
        .link("/.DirIcon", "sample-draw.png")
        .file(
            "/usr/share/metainfo/org.example.Sample.appdata.xml",
            APPSTREAM,
        )
}

pub fn normal() -> Vec<u8> {
    type2(&normal_squash().build(), &[], &[])
}

/// A real file of the given bytes in `dir`.
pub fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/// A fresh scratch folder under the target directory.
pub fn scratch(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::path::PathBuf::from(option_env!("CARGO_TARGET_TMPDIR").unwrap_or("/tmp")).join(
        format!(
            "appimage-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ),
    );
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
