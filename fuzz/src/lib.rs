//! What the fuzz targets share: the parsers' invariants (the same file the
//! property tests in `crates/telamon-store-core/tests/prop_*.rs` use, so a
//! property found by one is checked by the other) and a few helpers to cut a
//! fuzzer's bytes into the fields a target needs.
//!
//! A target that takes several inputs reads them as fields separated by the
//! byte 0x1F (the "unit separator"); the seed corpus in `corpus/<target>/` is
//! written the same way, so a seed is a plain file.

#[path = "../../crates/telamon-store-core/tests/harness/checks.rs"]
pub mod checks;

use std::borrow::Cow;

/// The separator between the fields of a multi-input target.
pub const SEP: u8 = 0x1F;

/// `data` cut at every separator, at most `max` fields (the last one keeps
/// the rest); fewer fields come back empty.
pub fn fields(data: &[u8], max: usize) -> Vec<&[u8]> {
    let mut out: Vec<&[u8]> = data.splitn(max, |b| *b == SEP).collect();
    out.resize(max, &[]);
    out
}

/// A field as text, invalid UTF-8 replaced.
pub fn text(field: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(field)
}

/// A field as text, or `None` for invalid UTF-8 (the target skips the input:
/// the parser under test takes `&str`, and UTF-8 input is the point).
pub fn utf8(field: &[u8]) -> Option<&str> {
    std::str::from_utf8(field).ok()
}

/// A tar stream with every header's checksum made right, so that the
/// fuzzer's changes to names, types, sizes and link targets reach the
/// unpacker's own checks instead of failing the tar library's checksum.
/// Walks the headers by their size fields; stops at the first block it cannot
/// follow.
pub fn fix_tar_checksums(tar: &mut [u8]) {
    let mut at = 0usize;
    while at + 512 <= tar.len() {
        let block = &mut tar[at..at + 512];
        if block.iter().all(|b| *b == 0) {
            break;
        }
        block[148..156].fill(b' ');
        let sum: u32 = block.iter().map(|b| u32::from(*b)).sum();
        let octal = format!("{sum:06o}\0 ");
        block[148..156].copy_from_slice(octal.as_bytes());
        // The size field: octal digits (a base-256 size is not followed).
        let size_field = &block[124..136];
        if size_field[0] & 0x80 != 0 {
            break;
        }
        let digits: String = size_field
            .iter()
            .take_while(|b| (b'0'..=b'7').contains(b))
            .map(|b| *b as char)
            .collect();
        let size = u64::from_str_radix(&digits, 8).unwrap_or(0);
        let data = usize::try_from(size.div_ceil(512).saturating_mul(512)).unwrap_or(usize::MAX);
        at = match at.checked_add(512).and_then(|n| n.checked_add(data)) {
            Some(n) => n,
            None => break,
        };
    }
}
