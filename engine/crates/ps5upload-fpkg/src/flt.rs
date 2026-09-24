//! The PS5 `\x7fFLT` flat-path table: the path hash and the table writer.
//!
//! Both image layers index their files with a 64-bit hash of the ASCII-uppercased path
//! (leading `/` stripped). The hash is a three-lane Keccak-like sponge with fixed seeds.
//! Both sample packages' outer tables reproduce exactly from this implementation.

const SEED0: u64 = 0x92ca_8aab_26a2_4f51;
const SEED1: u64 = 0x09bb_b761_a41b_c44d;
const ROUND_CONST: u64 = 0x8000_0000_8000_8081;

/// Header size; entries follow.
pub const HEADER_LEN: usize = 0x40;
const ENTRY_LEN: usize = 16;

/// The header's constant seed (the two global hash seeds, little-endian).
pub const HEADER_SEED: [u8; 16] = [
    0x51, 0x4f, 0xa2, 0x26, 0xab, 0x8a, 0xca, 0x92, 0x4d, 0xc4, 0x1b, 0xa4, 0x61, 0xb7, 0xbb, 0x09,
];

/// `inode_flat_path_table` bit for a zero-length file.
pub const FLAG_EMPTY: u64 = 0x2000_0000;

/// Packs an `inode_flat_path_table` payload: inode in the low 24 bits, bit 30 for a
/// directory, bit 31 for a node outside the apr (top-level file) set, afid from bit 40.
pub fn pack_inode_entry(inode: u32, is_dir: bool, is_subtree: bool, afid: u32) -> u64 {
    let mut packed = (inode & 0xFF_FFFF) as u64;
    if is_dir {
        packed |= 0x4000_0000;
    }
    if is_subtree {
        packed |= 0x8000_0000;
    }
    packed |= ((if is_dir { 0xFF_FFFF } else { afid }) as u64) << 40;
    packed
}

/// Packs an `apr_flat_path_table` payload: uncompressed size in the low 40 bits, afid above.
pub fn pack_apr_entry(uncompressed_size: u64, afid: u32) -> u64 {
    (uncompressed_size & 0xFF_FFFF_FFFF) | ((afid as u64) << 40)
}

/// Serializes a table: the 0x40-byte header plus 16-byte `{hash, packed}` entries sorted
/// ascending by hash.
pub fn write(entries: &[(u64, u64)]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(hash, _)| *hash);
    let mut out = vec![0u8; HEADER_LEN + sorted.len() * ENTRY_LEN];
    out[0..4].copy_from_slice(&1u32.to_le_bytes());
    out[4] = ENTRY_LEN as u8;
    out[8..12].copy_from_slice(&(HEADER_LEN as u32).to_le_bytes());
    out[0x20..0x24].copy_from_slice(&[0x7F, b'F', b'L', b'T']);
    out[0x2C..0x30].copy_from_slice(&(sorted.len() as u32).to_le_bytes());
    out[0x30..0x40].copy_from_slice(&HEADER_SEED);
    for (i, (hash, packed)) in sorted.iter().enumerate() {
        let at = HEADER_LEN + i * ENTRY_LEN;
        out[at..at + 8].copy_from_slice(&hash.to_le_bytes());
        out[at + 8..at + 16].copy_from_slice(&packed.to_le_bytes());
    }
    out
}

/// The flat-path-table hash of a path: ASCII-uppercased with the leading `/` stripped.
pub fn hash_path(path: &str) -> u64 {
    let path = path.strip_prefix('/').unwrap_or(path);
    hash_bytes(path.to_ascii_uppercase().as_bytes())
}

/// The raw three-lane sponge over exact bytes.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut s0 = SEED0;
    let mut s1 = SEED0.rotate_left(11);
    let mut s2 = SEED0.rotate_left(23);
    let mut tail = 0u64;
    if !bytes.is_empty() {
        let words = (bytes.len() - 1) >> 3;
        let (mut a0, mut a1, mut a2) = (s0, s1, s2);
        let mut off = 0;
        for _ in 0..words {
            a0 ^= u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
            off += 8;
            let t18 = ((a2 ^ a1).rotate_left(5) ^ a0).rotate_right(11);
            let t12 = ((a2 ^ a0).rotate_left(17) ^ a1).rotate_left(11);
            a2 = ((a1 ^ a0).rotate_left(1) ^ a2).rotate_right(5);
            a0 = (!t12 & a2) ^ t18 ^ ROUND_CONST;
            a1 = (!a2 & t18) ^ t12;
            a2 ^= !t18 & t12;
        }
        s0 = a0;
        s1 = a1;
        s2 = a2;
        for (j, b) in bytes[off..].iter().enumerate() {
            tail |= (*b as u64) << (8 * j);
        }
    }
    let u16 = s1;
    let u11 = s2;
    let u6 = tail ^ s0 ^ SEED1;
    let u17 = (u11 ^ u16).rotate_left(5) ^ u6;
    let u18 = (u11 ^ u6).rotate_left(17) ^ u16;
    let u11 = (u16 ^ u6).rotate_left(1) ^ u11;
    (!u18.rotate_left(11) & u11.rotate_right(5)) ^ u17.rotate_right(11) ^ ROUND_CONST
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two entries of the real samples' outer `\x7fFLT` block.
    #[test]
    fn sample_path_hashes() {
        assert_eq!(hash_path("pfs_image.dat"), 0xa656_27bd_d815_4701);
        assert_eq!(hash_path("naps_pkg_layout.dat"), 0xc683_f67a_1dec_ecaf);
    }

    #[test]
    fn path_is_uppercased_and_slash_stripped() {
        assert_eq!(hash_path("/PFS_IMAGE.DAT"), hash_path("pfs_image.dat"));
    }

    #[test]
    fn empty_and_short_inputs_terminate() {
        assert_ne!(hash_bytes(&[]), hash_bytes(b"a"));
        assert_ne!(hash_bytes(b"a"), hash_bytes(b"ab"));
    }

    /// Byte-for-byte the outer `\x7fFLT` block both samples carry (inodes 3 and 4 of the
    /// fixed outer template).
    #[test]
    fn writes_the_samples_outer_table() {
        let table = write(&[
            (
                hash_path("pfs_image.dat"),
                pack_inode_entry(3, false, false, 0),
            ),
            (
                hash_path("naps_pkg_layout.dat"),
                pack_inode_entry(4, false, false, 1),
            ),
        ]);
        let expected = concat!(
            "0100000010000000400000000000000000000000000000000000000000000000",
            "7f464c54000000000000000002000000514fa226ab8aca924dc41ba461b7bb09",
            "014715d8bd2756a60300000000000000afecec1d7af683c60400000000010000",
        );
        let hex = table.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex, expected);
    }
}
