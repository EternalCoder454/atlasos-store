#![no_main]

use libfuzzer_sys::fuzz_target;
use telamon_store_fuzz::checks;

fuzz_target!(|data: &[u8]| {
    // The input is a tar stream; its checksums are made right and it is
    // compressed, so the mutations land on names, types, sizes and links.
    let mut tar = data.to_vec();
    telamon_store_fuzz::fix_tar_checksums(&mut tar);
    let packed = zstd::stream::encode_all(&tar[..], 1).expect("compresses");
    checks::native_unpack(&packed);
});
