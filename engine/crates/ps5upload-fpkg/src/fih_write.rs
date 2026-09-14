//! The finalized-image header block (`\x7FFIH`), 0x10000 bytes, little-endian.
//!
//! Field values are the ones measured on the samples; the digest-table slots are filled
//! with the game digest (the plaintext superblock's SHA3) at 0x30, 0x70 and 0xD0, and the
//! `0xB0` slot carries `SHA3-256(naps_pkg_layout.dat)`.

use crate::crypto::sha3;
use crate::BLOCK;

pub struct FihParams<'a> {
    /// The encrypted outer image's length in bytes (`0x18`).
    pub outer_size: u64,
    /// The outer superblock's block index, whose absolute offset goes to `0x20`.
    pub superblock_block: u64,
    /// `SHA3-256` of the superblock's plaintext block — the image digest.
    pub game_digest: [u8; 32],
    /// Absolute offset of the embedded container inside the finalized image.
    pub cnt_offset: u64,
    pub naps: &'a [u8],
    /// Block-aligned inner image size (the header's `0xA0`).
    pub inner_size: u64,
    /// The inner mount's metadata base block index (`0x50`).
    pub meta_base_block: u64,
    /// Directories below uroot plus every file (`0x94`/`0x98`).
    pub content_inodes: u32,
    /// The content version's 2-3-3 BCD word (`0x9C`).
    pub content_version: u32,
    /// App-payload file count (`0xF0`).
    pub app_file_count: u32,
    /// Non-empty flat-path-table count (`0xF8`).
    pub flt_count: u32,
}

pub fn write(p: &FihParams) -> Vec<u8> {
    let mut h = vec![0u8; BLOCK as usize];
    h[0..4].copy_from_slice(&[0x7F, b'F', b'I', b'H']);
    h[4] = 0x01;
    h[5] = 0x00; // debug
    h[6] = 0x03;
    h[0x08..0x0C].copy_from_slice(&1u32.to_le_bytes());
    h[0x10..0x18].copy_from_slice(&BLOCK.to_le_bytes());
    h[0x18..0x20].copy_from_slice(&p.outer_size.to_le_bytes());
    let sb_absolute = BLOCK + p.superblock_block * BLOCK;
    h[0x20..0x28].copy_from_slice(&sb_absolute.to_le_bytes());
    h[0x28..0x30].copy_from_slice(&BLOCK.to_le_bytes());
    let game_digest = p.game_digest;
    for at in [0x30usize, 0x70, 0xD0] {
        h[at..at + 32].copy_from_slice(&game_digest);
    }
    // The loader reads the inner superblock at this block index times the block size.
    h[0x50..0x54].copy_from_slice(&(p.meta_base_block as u32).to_le_bytes());
    h[0x58..0x60].copy_from_slice(&p.cnt_offset.to_le_bytes());
    h[0x60..0x68].copy_from_slice(&BLOCK.to_le_bytes());
    h[0x68..0x70].copy_from_slice(&0x0000_8000_0000_0000u64.to_le_bytes());
    let inner_blocks = (p.inner_size / BLOCK) as u32;
    h[0x90..0x94].copy_from_slice(&inner_blocks.to_le_bytes());
    h[0x94..0x98].copy_from_slice(&p.content_inodes.to_le_bytes());
    h[0x98..0x9C].copy_from_slice(&p.content_inodes.to_le_bytes());
    h[0x9C..0xA0].copy_from_slice(&p.content_version.to_le_bytes());
    h[0xA0..0xA8].copy_from_slice(&p.inner_size.to_le_bytes());
    h[0xA8..0xB0].copy_from_slice(&(p.naps.len() as u64).to_le_bytes());
    h[0xB0..0xD0].copy_from_slice(&sha3(p.naps));
    h[0xF0..0xF4].copy_from_slice(&p.app_file_count.to_le_bytes());
    h[0xF8..0xFC].copy_from_slice(&p.flt_count.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_carries_the_template_fields() {
        let naps = vec![7u8; 104];
        let fih = write(&FihParams {
            outer_size: 7 * BLOCK,
            superblock_block: 2,
            game_digest: [1u8; 32],
            cnt_offset: 7 * BLOCK,
            naps: &naps,
            inner_size: 4 * BLOCK,
            meta_base_block: 3,
            content_inodes: 3,
            content_version: 0x0100_1000,
            app_file_count: 1,
            flt_count: 1,
        });
        assert_eq!(fih.len(), BLOCK as usize);
        assert_eq!(&fih[0..5], &[0x7F, b'F', b'I', b'H', 0x01]);
        assert_eq!(fih[5], 0x00);
        assert_eq!(fih[6], 0x03);
        assert_eq!(fih[0x50], 3);
        assert_eq!(&fih[0x58..0x60], &(7 * BLOCK).to_le_bytes());
        assert_eq!(&fih[0x30..0x50], &[1u8; 32]);
        assert_eq!(&fih[0xB0..0xD0], &sha3(&naps));
        assert_eq!(&fih[0x20..0x28], &(3 * BLOCK).to_le_bytes());
    }
}
