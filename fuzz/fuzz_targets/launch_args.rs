#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::{SEP, checks, text};

fuzz_target!(|data: &[u8]| {
    let args: Vec<String> = data
        .split(|b| *b == SEP)
        .take(80)
        .map(|a| text(a).into_owned())
        .collect();
    checks::launch_args(&args);
});
