//! Kraken image descriptors (`kraken_image::describe`, and `naps::parse` on the same bytes) and
//! the blocks they describe (`decode_described`).
//! Input: u32 LE descriptor length, the descriptor, then the stored image. After
//! ps5-dump-forge's `fuzz/fuzz_targets/kraken_descriptor.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::kraken_image::{decode_described, describe};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let n = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    let rest = &data[4..];
    let (blob, image) = rest.split_at(n.min(rest.len()));
    let _ = ps5upload_fpkg::naps::parse(blob);
    let Ok(blocks) = describe(blob) else {
        return;
    };
    for b in blocks.iter().take(16) {
        let _ = decode_described(image, b);
    }
});
