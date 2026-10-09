#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::{SEP, checks, fields, text, utf8};

fuzz_target!(|data: &[u8]| {
    // prefix, then entries `kind path 0x1F content`: file, executable file,
    // or link (the content is the target). Entries are separated by 0x1E.
    let mut parts = data.splitn(2, |b| *b == SEP);
    let prefix = text(parts.next().unwrap_or_default()).replace('\0', "");
    let rest = parts.next().unwrap_or_default();
    let mut files = Vec::new();
    let mut links = Vec::new();
    for entry in rest.split(|b| *b == 0x1E).take(16) {
        let f = fields(entry, 3);
        let Some(path) = utf8(f[1]) else { continue };
        match f[0] {
            b"l" => {
                if let Some(t) = utf8(f[2]) {
                    links.push((path.to_string(), t.to_string()));
                }
            }
            kind => files.push((path.to_string(), f[2].to_vec(), kind == b"x")),
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    files.retain(|(p, _, _)| seen.insert(p.clone()));
    links.retain(|(p, _)| seen.insert(p.clone()));
    checks::native_plan(&files, &links, std::path::Path::new(&prefix));
});
