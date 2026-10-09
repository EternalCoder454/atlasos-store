#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::{checks, fields, text};

fuzz_target!(|data: &[u8]| {
    // desktop entry, prefix, then the programs the bundle holds in bin/.
    let f = fields(data, 5);
    let prefix = text(f[1]).replace('\0', "");
    let programs = f[2..]
        .iter()
        .map(|p| text(p).into_owned())
        .filter(|p| !p.is_empty())
        .collect();
    checks::native_desktop(f[0], std::path::Path::new(&prefix), &programs);
});
