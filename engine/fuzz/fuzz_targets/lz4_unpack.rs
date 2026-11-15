//! A game read back out of its LZ4 packs (`ampr_pack::image::Lz4Unpacked::open`) over an
//! in-memory tree of manifest + `.crc` + volume 0, then every file read back.
//! Input: `[len u32 LE][manifest][len u32 LE][crc][volume 0]` (`src/mem_tree.rs`). After
//! ps5-dump-forge's `fuzz/fuzz_targets/lz4_unpack.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::ampr_pack::image::{Lz4Unpacked, Unpacking};
use ps5upload_fuzz::mem_tree::{pack_tree, LZ4_NAMES};
use ps5upload_fuzz::read_back;

fuzz_target!(|data: &[u8]| {
    match Lz4Unpacked::open(Box::new(pack_tree(data, LZ4_NAMES))) {
        Ok(Unpacking::Decoded(mut tree)) => read_back(tree.as_mut()),
        Ok(Unpacking::NotPacked(mut tree)) => read_back(tree.as_mut()),
        Err(_) => {}
    }
});
