//! Package headers: the finalized-image header (`fih::parse`) and the container it points to
//! (`cnt::read`), with every digest check the verifier runs on them; and the container at
//! offset 0, where a PS4 package keeps it.
//! Input: a sparse image (`src/sparse.rs`). After ps5-dump-forge's `fuzz/fuzz_targets/fih_cnt.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::{cnt, fih, PkgFile};
use ps5upload_fuzz::sparse::Sparse;

fn checks(container: &cnt::Cnt, head: &[u8], game_digest: &[u8; 32]) {
    let _ = container.package_digest_ok();
    let _ = container.header_rollup_ok();
    let _ = container.sc_entries2_ok();
    let _ = container.body_digest_ok();
    let _ = container.fih_digest_ok(head);
    let _ = container.descriptor_ok();
    let _ = container.digest_table_digest_ok();
    let _ = container.general_digests(game_digest);
    let _ = container.image_digests();
    for (id, _) in container.entry_digests() {
        if let Some(entry) = container.entry(id) {
            let _ = container.payload(entry).len();
            let _ = container.padded_payload(entry).len();
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let image = Sparse::decode(data);
    let len = image.len();
    let mut file = PkgFile::from_reader(Box::new(image), len);
    let Ok(head) = file.read_at(0, fih::HEADER_LEN) else {
        return;
    };
    if let Ok(container) = cnt::read(&mut file, 0) {
        checks(&container, &head, &[0; 32]);
    }
    let Ok(header) = fih::parse(&head) else {
        return;
    };
    let _ = header.is_debug();
    if let Ok(container) = cnt::read(&mut file, header.cnt_offset) {
        checks(&container, &head, &header.game_digest);
    }
});
