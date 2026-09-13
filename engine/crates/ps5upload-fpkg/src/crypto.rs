//! Hashes, key derivation and CRC used by PS5 packages.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sha3::{Digest, Sha3_256};

/// The passcode debug FPKGs are built with when none is chosen.
pub const DEFAULT_PASSCODE: &str = "00000000000000000000000000000000";

pub fn sha3(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha3_256::digest(data));
    out
}

pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    for p in parts {
        mac.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// The PFS image key for a debug package, from its content id and passcode.
pub fn derive_ekpfs(content_id: &str, passcode: &str) -> [u8; 32] {
    let mut cid = [0u8; 48];
    let id = content_id.as_bytes();
    let n = id.len().min(48);
    cid[..n].copy_from_slice(&id[..n]);
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(&sha3(&1u32.to_be_bytes()));
    buf.extend_from_slice(&sha3(&cid));
    buf.extend_from_slice(passcode.as_bytes());
    sha3(&buf)
}

pub struct XtsKeys {
    pub tweak: [u8; 16],
    pub data: [u8; 16],
}

/// AES-XTS key pair for an image with this superblock seed.
pub fn derive_xts_keys(ekpfs: &[u8; 32], seed: &[u8; 16]) -> XtsKeys {
    let k = hmac_sha256(ekpfs, &[seed]);
    let enc = hmac_sha256(&k, &[&1u32.to_le_bytes(), seed]);
    let mut tweak = [0u8; 16];
    let mut data = [0u8; 16];
    tweak.copy_from_slice(&enc[..16]);
    data.copy_from_slice(&enc[16..]);
    XtsKeys { tweak, data }
}

const CRC32C_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0x82F6_3B78
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-32C (Castagnoli), as stored in `playgo-chunk.crc`.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC32C_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn crc32c_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    /// Values from Python's hashlib/hmac for the DLC sample's content id and
    /// its superblock seed; the same keys decrypt that real package.
    #[test]
    fn key_derivation_matches_real_package() {
        let ekpfs = derive_ekpfs("EP7579-PPSA17599_00-EXP33DLC10000PS5", DEFAULT_PASSCODE);
        assert_eq!(
            hex(&ekpfs),
            "6f2545336d611af09f15be54263809f1e2f0bc0062335e637463a9e7bee39b73"
        );
        let seed: [u8; 16] = [
            0x3a, 0xce, 0x52, 0x0f, 0x1d, 0xdb, 0xd3, 0xcb, 0x8a, 0xcb, 0xc4, 0x54, 0x3f, 0x95,
            0xfb, 0xa6,
        ];
        let keys = derive_xts_keys(&ekpfs, &seed);
        assert_eq!(hex(&keys.tweak), "04c9c2c17ea72b2be706be8ae56c9bff");
        assert_eq!(hex(&keys.data), "66cb5805c75c915a157f8ab9ccb80cab");
    }
}
