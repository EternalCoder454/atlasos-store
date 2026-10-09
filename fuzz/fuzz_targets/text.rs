#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::{checks, utf8};

fuzz_target!(|data: &[u8]| {
    // The first byte is the cap; the rest is the text.
    let Some((max, rest)) = data.split_first() else {
        return;
    };
    if let Some(s) = utf8(rest) {
        checks::text_clean(s, usize::from(*max));
    }
});
