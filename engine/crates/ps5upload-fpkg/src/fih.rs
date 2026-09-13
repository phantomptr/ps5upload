//! The finalized-image (`\x7FFIH`) header. Little-endian.

use crate::{format_err, le16, le64, Result};

pub const HEADER_LEN: usize = 0x100;
const MAGIC: u32 = 0x7F46_4948;

pub struct Fih {
    pub signed_byte: u8,
    pub format_version: u16,
    pub pfs_offset: u64,
    pub pfs_size: u64,
    /// SHA3-256 of the plaintext outer superblock block.
    pub game_digest: [u8; 32],
    pub cnt_offset: u64,
}

impl Fih {
    /// `0x00` marks a debug image; retail images carry `0x80`.
    pub fn is_debug(&self) -> bool {
        self.signed_byte == 0
    }
}

pub fn parse(head: &[u8]) -> Result<Fih> {
    if head.len() < HEADER_LEN {
        return format_err("FIH header truncated");
    }
    if u32::from_be_bytes(head[0..4].try_into().unwrap()) != MAGIC {
        return format_err("not a finalized PS5 package (no \\x7FFIH magic)");
    }
    let mut game_digest = [0u8; 32];
    game_digest.copy_from_slice(&head[0x30..0x50]);
    Ok(Fih {
        signed_byte: head[5],
        format_version: le16(head, 6),
        pfs_offset: le64(head, 0x10),
        pfs_size: le64(head, 0x18),
        game_digest,
        cnt_offset: le64(head, 0x58),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_debug_header() {
        let mut h = vec![0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&0x7F46_4948u32.to_be_bytes());
        h[5] = 0x00;
        h[6..8].copy_from_slice(&3u16.to_le_bytes());
        h[0x10..0x18].copy_from_slice(&0x10000u64.to_le_bytes());
        h[0x18..0x20].copy_from_slice(&0x70000u64.to_le_bytes());
        h[0x30] = 0xAB;
        h[0x58..0x60].copy_from_slice(&0x80000u64.to_le_bytes());
        let f = parse(&h).unwrap();
        assert!(f.is_debug());
        assert_eq!(
            (f.pfs_offset, f.pfs_size, f.cnt_offset),
            (0x10000, 0x70000, 0x80000)
        );
        assert_eq!(f.game_digest[0], 0xAB);
    }

    #[test]
    fn rejects_cnt_magic() {
        let mut h = vec![0u8; HEADER_LEN];
        h[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes());
        assert!(parse(&h).is_err());
    }
}
