//! zstd pack small files on arbitrary bytes: the AMPRZPK1 index (`decode_index`, which must
//! encode again when it parsed) and a volume header (`decode_volume_header`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5_dump_forge_zstd::{decode_index, decode_volume_header, encode_index};

fuzz_target!(|data: &[u8]| {
    if let Ok(index) = decode_index(data) {
        let _ = encode_index(&index);
    }
    let _ = decode_volume_header(data);
});
