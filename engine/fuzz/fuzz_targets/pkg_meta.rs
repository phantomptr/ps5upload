//! The package inspector (`ps5upload_pkg`): header and metadata (`parse_pkg_from`), the entry
//! table and its entries (`pkg_entries_from`, `read_pkg_entry_from`), and a PS3 package's
//! header (`parse_ps3`). This is what the Library reads from every `.pkg` it lists.
//! Input: a sparse image (`src/sparse.rs`).
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;
use ps5upload_fuzz::sparse::Sparse;
use ps5upload_fuzz::MAX_FILES;
use ps5upload_pkg::{parse_pkg_from, pkg_entries_from, ps3, read_pkg_entry_from};

fuzz_target!(|data: &[u8]| {
    let mut image = Sparse::decode(data);
    let len = image.len();
    let _ = parse_pkg_from(&mut image, len, Path::new("fuzz.pkg"));
    if let Ok(entries) = pkg_entries_from(&mut image, len) {
        for e in entries.iter().take(MAX_FILES) {
            let _ = read_pkg_entry_from(&mut image, e, 1 << 20);
        }
    }
    let _ = ps3::parse_ps3(&mut image, len);
});
