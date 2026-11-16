//! PFS readers. `.ffpfs` through both of them (`ps5upload_fpkg::pfs_source::open`, which also
//! follows a nested game image, and `ps5_dump_forge_pfs::PfsSource`: header, inode table,
//! directory walk, compressed files), and `.ffpfsc` through `open_ffpfsc` (the outer PFS, the
//! PFSC window over its one file, and the nested image's reader).
//! Input: a sparse image (`src/sparse.rs`). After ps5-dump-forge's `fuzz/fuzz_targets/pfs.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5_dump_forge_pfs::{open_ffpfsc, PfsSource};
use ps5upload_fuzz::read_back;
use ps5upload_fuzz::sparse::Sparse;

fuzz_target!(|data: &[u8]| {
    if let Ok(mut tree) =
        ps5upload_fpkg::pfs_source::open(Box::new(Sparse::decode(data)), "fuzz".into())
    {
        read_back(tree.as_mut());
    }
    if let Ok(mut tree) = PfsSource::from_reader(Box::new(Sparse::decode(data)), "fuzz".into()) {
        let _ = tree.header();
        read_back(&mut tree);
    }
    if let Ok((mut tree, _)) = open_ffpfsc(Box::new(Sparse::decode(data)), "fuzz") {
        read_back(tree.as_mut());
    }
});
