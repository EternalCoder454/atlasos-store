#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::{checks, fields, utf8};

fuzz_target!(|data: &[u8]| {
    let f = fields(data, 2);
    if let (Some(a), Some(b)) = (utf8(f[0]), utf8(f[1])) {
        checks::https_url(a);
        checks::redirect(a, b);
        checks::redirect(&format!("https://{a}"), b);
        checks::app_id(a);
        checks::internal_path(a);
        checks::search_text(a);
        checks::text_validators(a);
        checks::version(a, b);
    }
});
