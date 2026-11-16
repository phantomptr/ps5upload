//! The checks a build runs on a game's own files before it writes anything
//! (`source::readiness`, `packed_kind`, the LZ4 pack checks): module headers, `param.json`,
//! pack indexes, each read from an in-memory tree.
//! Input: `[len u32 LE][eboot.bin][len u32 LE][sce_sys/param.json][sce_module/libSceAmpr.sprx]`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::{ampr_pack, source};
use ps5upload_fuzz::mem_tree::{split, Mem};

fuzz_target!(|data: &[u8]| {
    let names = [
        "eboot.bin",
        "sce_sys/param.json",
        "sce_module/libSceAmpr.sprx",
    ];
    let parts = split(data, 3);
    let _ = ampr_pack::runtime_support(parts[2]);
    let mut tree = Mem::new(
        names
            .iter()
            .zip(parts)
            .map(|(n, p)| (n.to_string(), p.to_vec())),
    );
    let _ = source::readiness(&mut tree);
    let _ = source::packed_kind(&mut tree);
    let _ = source::imports_ampr(&mut tree, "eboot.bin");
    let _ = ampr_pack::image::readiness(&mut tree);
    let _ = ampr_pack::image::refusal(&mut tree);
    let _ = ampr_pack::image::emulator_needed_at(&mut tree);
});
