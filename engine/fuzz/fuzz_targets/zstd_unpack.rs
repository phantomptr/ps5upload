//! A game read back out of its zstd packs (`ps5_dump_forge_zstd::reader::unpack`) over an
//! in-memory tree of index + volume 0, then every file read back.
//! Input: `[len u32 LE][index][len u32 LE][unused][volume 0]` (`src/mem_tree.rs`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5_dump_forge_zstd::reader::unpack;
use ps5upload_fuzz::mem_tree::{pack_tree, ZSTD_NAMES};
use ps5upload_fuzz::read_back;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut tree) = unpack(Box::new(pack_tree(data, ZSTD_NAMES))) {
        let _ = tree.identical_paths().len();
        let _ = tree.differing_paths().len();
        read_back(&mut tree);
    }
});
