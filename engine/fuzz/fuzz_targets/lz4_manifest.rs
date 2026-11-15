//! LZ4 pack small files on arbitrary bytes: the AMPRPAK4 manifest (`Manifest::parse`), then
//! with its build id the `.crc` sidecar, the `.runtime` settings and a volume header.
//! Input: `[len u32 LE][manifest]` then the bytes the other three parsers each get.
#![no_main]

use libfuzzer_sys::fuzz_target;
use ps5upload_fpkg::ampr_pack::format::{
    parse_crc_sidecar, parse_data_header, Manifest, RuntimeSettings,
};
use ps5upload_fuzz::mem_tree::split;

fuzz_target!(|data: &[u8]| {
    let parts = split(data, 2);
    let (manifest, other) = (parts[0], parts[1]);
    let _ = parse_data_header(other);
    let (id, chunks) = match Manifest::parse(manifest) {
        Ok(m) => {
            for i in 0..m.files.len().min(64) {
                let _ = m.path(i);
            }
            for i in 0..m.packs.len().min(64) {
                let _ = m.pack_name(i);
            }
            (m.build_id, m.chunks.len())
        }
        Err(_) => ([0; 16], 4),
    };
    let _ = parse_crc_sidecar(other, &id, chunks);
    let _ = RuntimeSettings::parse(other, &id);
});
