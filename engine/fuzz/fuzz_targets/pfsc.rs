//! `.ffpfsc` reader (`ps5upload_fpkg::pfsc_reader`): PFS layout, PFSC block table, zlib blocks.
//! Input: a sparse image (`src/sparse.rs`). After ps5-dump-forge's `fuzz/fuzz_targets/pfsc.rs`.
#![no_main]

use std::io::{Read, Seek, SeekFrom};

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::pfsc_reader::PfscReader;
use ps5upload_fuzz::sparse::Sparse;
use ps5upload_fuzz::READ_CAP;

fuzz_target!(|data: &[u8]| {
    let Ok(mut reader) = PfscReader::new(Sparse::decode(data)) else {
        return;
    };
    let _ = reader.inner_name();
    let len = reader.len();
    let mut buf = vec![0u8; READ_CAP];
    let _ = reader.read(&mut buf);
    // A block in the middle and the last one, through the cache.
    for at in [len / 2, len.saturating_sub(READ_CAP as u64)] {
        if reader.seek(SeekFrom::Start(at)).is_ok() {
            let _ = reader.read(&mut buf);
        }
    }
});
