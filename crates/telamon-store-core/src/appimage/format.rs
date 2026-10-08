//! What an AppImage looks like from the outside: the ELF runtime at its start,
//! the `AI` marker in the ELF header's padding, and the section headers that
//! hold the embedded signature. Nothing here runs the file.
//!
//! A type 2 AppImage is `[ELF runtime][squashfs]`; the squashfs starts where
//! the ELF ends (`e_shoff + e_shentsize * e_shnum`), which is how the runtime
//! itself finds it. A type 1 AppImage has an ISO 9660 image there instead.

use std::fs::File;
use std::os::unix::fs::FileExt;

/// The two kinds of AppImage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// ISO 9660 payload. Old; the Store does not look inside.
    Type1,
    /// Squashfs payload.
    Type2,
}

/// `AI` and the type byte, at offset 8 of the ELF header (`EI_ABIVERSION` and
/// the padding), which is where the AppImage spec puts them.
pub fn sniff(head: &[u8]) -> Option<Format> {
    if head.len() < 11 || head[..4] != *b"\x7fELF" || head[8..10] != *b"AI" {
        return None;
    }
    match head[10] {
        1 => Some(Format::Type1),
        2 => Some(Format::Type2),
        _ => None,
    }
}

/// Reads the first bytes of an open file and sniffs them.
pub fn sniff_file(file: &File) -> Option<Format> {
    let mut head = [0u8; 16];
    file.read_exact_at(&mut head, 0).ok()?;
    sniff(&head)
}

/// Most section headers read. A runtime has about 30; the cap only keeps a
/// hostile header from asking for a table the size of the file.
const MAX_SECTIONS: u64 = 1024;
/// Longest section name table read, in bytes.
const MAX_STRTAB: u64 = 64 * 1024;

/// One ELF section: its name and where its bytes are in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub offset: u64,
    pub size: u64,
}

/// The parts of the ELF header an AppImage reader needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elf {
    /// Where the ELF ends: the squashfs of a type 2 AppImage starts here.
    pub end: u64,
    pub sections: Vec<Section>,
}

/// Why the ELF part could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Not an ELF file, or one this reader does not follow.
    NotElf,
    /// The headers point outside the file or disagree with themselves.
    Damaged,
    Io,
}

struct Endian {
    big: bool,
}

impl Endian {
    fn u16(&self, b: &[u8]) -> u64 {
        let a = [b[0], b[1]];
        u64::from(if self.big {
            u16::from_be_bytes(a)
        } else {
            u16::from_le_bytes(a)
        })
    }
    fn u32(&self, b: &[u8]) -> u64 {
        let a = [b[0], b[1], b[2], b[3]];
        u64::from(if self.big {
            u32::from_be_bytes(a)
        } else {
            u32::from_le_bytes(a)
        })
    }
    fn u64(&self, b: &[u8]) -> u64 {
        let a = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        if self.big {
            u64::from_be_bytes(a)
        } else {
            u64::from_le_bytes(a)
        }
    }
}

/// Reads the ELF header and the section headers of `file`, `len` bytes long.
pub fn read_elf(file: &File, len: u64) -> Result<Elf, ElfError> {
    let mut h = [0u8; 64];
    let n = len.min(64) as usize;
    file.read_exact_at(&mut h[..n], 0)
        .map_err(|_| ElfError::Io)?;
    if n < 52 || h[..4] != *b"\x7fELF" {
        return Err(ElfError::NotElf);
    }
    let wide = match h[4] {
        1 => false,
        2 => true,
        _ => return Err(ElfError::NotElf),
    };
    let e = Endian {
        big: match h[5] {
            1 => false,
            2 => true,
            _ => return Err(ElfError::NotElf),
        },
    };
    if wide && n < 64 {
        return Err(ElfError::NotElf);
    }
    let (shoff, shentsize, shnum, shstrndx, min_ent) = if wide {
        (
            e.u64(&h[0x28..]),
            e.u16(&h[0x3A..]),
            e.u16(&h[0x3C..]),
            e.u16(&h[0x3E..]),
            64,
        )
    } else {
        (
            e.u32(&h[0x20..]),
            e.u16(&h[0x2E..]),
            e.u16(&h[0x30..]),
            e.u16(&h[0x32..]),
            40,
        )
    };
    if shnum == 0 || shnum > MAX_SECTIONS || shentsize < min_ent || shentsize > 256 {
        return Err(ElfError::Damaged);
    }
    let table = shentsize * shnum;
    let end = shoff.checked_add(table).ok_or(ElfError::Damaged)?;
    if shoff < 52 || end > len || shstrndx >= shnum {
        return Err(ElfError::Damaged);
    }
    let mut raw = vec![0u8; table as usize];
    file.read_exact_at(&mut raw, shoff)
        .map_err(|_| ElfError::Io)?;
    let header = |i: u64| -> (u64, u64, u64) {
        let s = &raw[(i * shentsize) as usize..((i + 1) * shentsize) as usize];
        if wide {
            (e.u32(s), e.u64(&s[0x18..]), e.u64(&s[0x20..]))
        } else {
            (e.u32(s), e.u32(&s[0x10..]), e.u32(&s[0x14..]))
        }
    };
    let (_, str_off, str_size) = header(shstrndx);
    if str_off.checked_add(str_size).is_none_or(|e| e > len) {
        return Err(ElfError::Damaged);
    }
    let mut names = vec![0u8; str_size.min(MAX_STRTAB) as usize];
    file.read_exact_at(&mut names, str_off)
        .map_err(|_| ElfError::Io)?;
    let mut sections = Vec::new();
    for i in 0..shnum {
        let (name, offset, size) = header(i);
        let Some(tail) = names.get(name as usize..) else {
            continue;
        };
        let raw_name = tail.split(|b| *b == 0).next().unwrap_or_default();
        let Ok(name) = std::str::from_utf8(raw_name) else {
            continue;
        };
        sections.push(Section {
            name: name.chars().take(64).collect(),
            offset,
            size,
        });
    }
    Ok(Elf { end, sections })
}

impl Elf {
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }

    /// The bytes of a section, at most `max`; `None` when it is missing, past
    /// the end of the file, or longer than `max`.
    pub fn read_section(&self, file: &File, len: u64, name: &str, max: u64) -> Option<Vec<u8>> {
        let s = self.section(name)?;
        if s.size > max || s.offset.checked_add(s.size)? > len {
            return None;
        }
        let mut buf = vec![0u8; s.size as usize];
        file.read_exact_at(&mut buf, s.offset).ok()?;
        Some(buf)
    }
}

/// Whether the file at `path` (a regular file; a link is followed) starts like
/// an AppImage. Reads 16 bytes and never blocks on a pipe.
pub fn sniff_path(path: &std::path::Path) -> Option<Format> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    sniff_file(&file)
}
