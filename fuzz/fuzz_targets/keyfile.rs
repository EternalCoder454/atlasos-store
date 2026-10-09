#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::checks;

fuzz_target!(|data: &[u8]| {
    // The first byte picks the limits: the defaults or a small set.
    let Some((first, rest)) = data.split_first() else {
        return;
    };
    let limits = if first % 4 == 0 {
        telamon_store_core::keyfile::Limits::default()
    } else {
        telamon_store_core::keyfile::Limits {
            max_bytes: 64 << (first % 8),
            max_lines: 1 + usize::from(*first % 13),
            max_groups: 1 + usize::from(*first % 5),
            max_keys: 1 + usize::from(*first % 17),
            max_value: 1 + usize::from(*first % 40),
        }
    };
    checks::keyfile(rest, &limits);
});
