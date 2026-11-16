//! The package readers behind `pkg_source::open`: a PS5 package (`PkgSource::from_reader`: FIH,
//! container, outer PFS, inner image, Kraken blocks) and a PS4 one (`Ps4Package::open`), each
//! over the input's bytes, no file on disk.
//! Input: a sparse image (`src/sparse.rs`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::crypto::DEFAULT_PASSCODE;
use ps5upload_fpkg::pkg_source::PkgSource;
use ps5upload_fpkg::ps4_pkg::{self, Ps4Package};
use ps5upload_fuzz::read_back;
use ps5upload_fuzz::sparse::Sparse;

fuzz_target!(|data: &[u8]| {
    let len = Sparse::decode(data).len();
    let image = || Box::new(Sparse::decode(data));
    if let Ok(mut tree) = PkgSource::from_reader(image(), len, DEFAULT_PASSCODE, "fuzz".into()) {
        let _ = tree.content_id().len();
        read_back(&mut tree);
    }
    let _ = ps4_pkg::peek(image(), len);
    if let Ok(mut tree) = Ps4Package::open(image(), len, DEFAULT_PASSCODE, "fuzz".into()) {
        let _ = tree.info().title_id();
        read_back(&mut tree);
    }
});
