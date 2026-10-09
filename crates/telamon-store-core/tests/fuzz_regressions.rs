//! Replays the inputs in `fuzz/regressions/` (each crashed a fuzz target once)
//! through the same checks, so `cargo test` keeps a fixed bug fixed without
//! the fuzzer. Only the targets that take the input as it is; the others have
//! their own unit tests.

mod harness;

use std::path::PathBuf;

fn replay(dir: &str, check: fn(&[u8])) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions");
    let folder = root.join(dir);
    let mut n = 0;
    for entry in std::fs::read_dir(&folder).unwrap_or_else(|e| panic!("{}: {e}", folder.display()))
    {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        check(&bytes);
        n += 1;
    }
    assert!(n > 0, "no regression inputs in {}", folder.display());
}

#[test]
fn squashfs_inputs_that_crashed_the_reader() {
    replay("squashfs", harness::checks::squashfs);
}

#[test]
fn appimage_inputs_that_crashed_the_reader() {
    replay("appimage_file", harness::checks::appimage_file);
}

#[test]
fn icons_that_tripped_the_screening() {
    replay("icon", harness::checks::icon);
}
