//! The PS5 `\x7fFLT` flat-path-table path hash.
//!
//! Both image layers index their files with a 64-bit hash of the ASCII-uppercased path
//! (leading `/` stripped). The hash is a three-lane Keccak-like sponge with fixed seeds.
//! Both sample packages' outer tables reproduce exactly from this implementation.

const SEED0: u64 = 0x92ca_8aab_26a2_4f51;
const SEED1: u64 = 0x09bb_b761_a41b_c44d;
const ROUND_CONST: u64 = 0x8000_0000_8000_8081;

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
}
