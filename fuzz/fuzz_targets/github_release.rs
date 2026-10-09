#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::{checks, fields, text};

fuzz_target!(|data: &[u8]| {
    // A repository in the catalog is `Owner/name`; the release is the rest.
    let f = fields(data, 2);
    let repo = text(f[0]);
    checks::github_release(f[1], if repo.is_empty() { checks::REPO } else { &repo });
});
