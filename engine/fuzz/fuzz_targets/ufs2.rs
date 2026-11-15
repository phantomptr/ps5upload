//! UFS2 reader (`ps5upload_fpkg::ufs2_source`, over `ps5upload_pkg::ufs2`): superblock,
//! cylinder groups, inodes, directory walk, direct and indirect block maps.
//! Input: a sparse image (`src/sparse.rs`). After ps5-dump-forge's `fuzz/fuzz_targets/ufs2.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::ufs2_source::Ufs2Source;
use ps5upload_fuzz::read_back;
use ps5upload_fuzz::sparse::Sparse;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut tree) = Ufs2Source::from_reader(Box::new(Sparse::decode(data)), "fuzz".into()) {
        read_back(&mut tree);
    }
});
