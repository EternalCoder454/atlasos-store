#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::checks;

fuzz_target!(|data: &[u8]| {
    checks::appstream_metainfo(data);
});
