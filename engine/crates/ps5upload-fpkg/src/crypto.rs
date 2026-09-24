//! Hashes, key derivation and CRC used by PS5 packages.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use sha3::{Digest, Sha3_256};

/// The passcode debug FPKGs are built with when none is chosen.
pub const DEFAULT_PASSCODE: &str = "00000000000000000000000000000000";

/// An incremental SHA3-256, for hashing bytes that are never held all at once.
pub struct Hasher(Sha3_256);

impl Hasher {
    pub fn new() -> Self {
        Self(Sha3_256::new())
    }

    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }

    pub fn finish(self) -> [u8; 32] {
        let mut out = [0u8; 32];
        out.copy_from_slice(&self.0.finalize());
        out
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

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

/// A passcode-derived package key for one key index:
/// `SHA3(SHA3(index as BE32) ‖ SHA3(content id padded to 48) ‖ passcode)`.
///
/// Index 1 is the PFS image key; the entry-keys slot carries indices 0..=6.
pub fn derive_pfs_key(content_id: &str, passcode: &str, index: u32) -> [u8; 32] {
    let mut cid = [0u8; 48];
    let id = content_id.as_bytes();
    let n = id.len().min(48);
    cid[..n].copy_from_slice(&id[..n]);
    let mut buf = Vec::with_capacity(96);
    buf.extend_from_slice(&sha3(&index.to_be_bytes()));
    buf.extend_from_slice(&sha3(&cid));
    buf.extend_from_slice(passcode.as_bytes());
    sha3(&buf)
}

/// Encrypt a protected container entry in place (flags1 bit 31; `nptitle.dat`, the npbind
/// files). `data` is the plaintext padded to 16 bytes; `row` is the entry's final 32-byte
/// table row, which the key depends on, so it is encrypted last.
///
/// key/iv = SHA3-256(row || derive_pfs_key(content id, passcode, key index)): iv the first
/// 16 bytes, AES-128 key the last 16, CBC. Verified by decrypting every protected entry of a
/// package that launches on a console (LibProsperoPkg's Minecraft): license, nptitle, both
/// npbind files. Only the content id and the passcode go in — no fixed key.
pub fn encrypt_entry(row: &[u8; 32], entry_key: &[u8; 32], data: &mut [u8]) {
    let mut pre = Vec::with_capacity(64);
    pre.extend_from_slice(row);
    pre.extend_from_slice(entry_key);
    let iv_key = sha3(&pre);
    aes128_cbc_encrypt(
        &iv_key[16..].try_into().unwrap(),
        iv_key[..16].try_into().unwrap(),
        data,
    );
}

/// AES-128-CBC encryption in place; `data` must be whole 16-byte blocks.
pub fn aes128_cbc_encrypt(key: &[u8; 16], iv: [u8; 16], data: &mut [u8]) {
    use aes::cipher::{BlockCipherEncrypt, KeyInit as _};
    debug_assert!(data.len().is_multiple_of(16));
    let cipher = aes::Aes128::new(&(*key).into());
    let mut prev = iv;
    for chunk in data.as_chunks_mut::<16>().0 {
        for (b, p) in chunk.iter_mut().zip(prev) {
            *b ^= p;
        }
        let mut blk = aes::Block::from(*chunk);
        cipher.encrypt_block(&mut blk);
        chunk.copy_from_slice(&blk);
        prev = *chunk;
    }
}

/// The PFS image key for a debug package, from its content id and passcode.
pub fn derive_ekpfs(content_id: &str, passcode: &str) -> [u8; 32] {
    derive_pfs_key(content_id, passcode, 1)
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

const CRC32_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
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

/// The ZIP member CRC — the classic CRC-32 (poly `0xEDB88320`) that the ZIP format
/// specifies. Not the Castagnoli one the PlayGo CRC table uses: an install segment written
/// with `crc32c` here fails every standard unzip with "Bad CRC-32".
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC32_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
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

    /// The ZIP member checksum, against the universal check value for the classic CRC-32.
    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"hello"), 0x3610_A686);
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
