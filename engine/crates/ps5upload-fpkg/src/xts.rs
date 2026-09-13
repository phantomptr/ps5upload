//! AES-128-XTS for one data unit (IEEE 1619), as PS5 PFS images use it.
//!
//! One 64 KiB block is one data unit; its sector number is the tweak.
//! Blocks are whole multiples of 16 bytes, so ciphertext stealing is never
//! needed.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use aes::{Aes128, Block};

use crate::crypto::XtsKeys;

/// Set on the sector number of signed (metadata) blocks.
pub const SIGNED_SECTOR_FLAG: u64 = 1 << 47;

pub struct Xts {
    data: Aes128,
    tweak: Aes128,
}

/// Multiply the tweak by x in GF(2^128), little-endian byte order.
fn double(t: &mut [u8; 16]) {
    let carry = t[15] >> 7;
    for i in (1..16).rev() {
        t[i] = (t[i] << 1) | (t[i - 1] >> 7);
    }
    t[0] <<= 1;
    if carry != 0 {
        t[0] ^= 0x87;
    }
}

impl Xts {
    pub fn new(keys: &XtsKeys) -> Self {
        Self {
            data: Aes128::new(&keys.data.into()),
            tweak: Aes128::new(&keys.tweak.into()),
        }
    }

    fn start_tweak(&self, sector: u64) -> [u8; 16] {
        let mut t = [0u8; 16];
        t[..8].copy_from_slice(&sector.to_le_bytes());
        let mut blk = Block::from(t);
        self.tweak.encrypt_block(&mut blk);
        t.copy_from_slice(&blk);
        t
    }

    pub fn encrypt(&self, sector: u64, buf: &mut [u8]) {
        self.apply(sector, buf, true);
    }

    pub fn decrypt(&self, sector: u64, buf: &mut [u8]) {
        self.apply(sector, buf, false);
    }

    fn apply(&self, sector: u64, buf: &mut [u8], encrypt: bool) {
        assert!(
            !buf.is_empty() && buf.len().is_multiple_of(16),
            "XTS data unit must be a non-zero multiple of 16 bytes"
        );
        let mut t = self.start_tweak(sector);
        for chunk in buf.as_chunks_mut::<16>().0 {
            let mut blk = Block::from(std::array::from_fn::<u8, 16, _>(|i| chunk[i] ^ t[i]));
            if encrypt {
                self.data.encrypt_block(&mut blk);
            } else {
                self.data.decrypt_block(&mut blk);
            }
            *chunk = std::array::from_fn(|i| blk[i] ^ t[i]);
            double(&mut t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{sha3, XtsKeys};

    fn keys() -> XtsKeys {
        let mut data = [0u8; 16];
        let mut tweak = [0u8; 16];
        for i in 0..16 {
            data[i] = i as u8;
            tweak[i] = 16 + i as u8;
        }
        XtsKeys { tweak, data }
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn two_blocks_sector_zero() {
        let mut buf = [0u8; 32];
        Xts::new(&keys()).encrypt(0, &mut buf);
        assert_eq!(
            hex(&buf),
            "f071a2b402c105ea37024133e24d6ef6212e8cc0175e1b6b32657d54f159daf6"
        );
    }

    #[test]
    fn full_block_data_and_signed_sectors() {
        let plain: Vec<u8> = (0..0x10000u32).map(|i| (i * 7) as u8).collect();
        let x = Xts::new(&keys());

        let mut data = plain.clone();
        x.encrypt(5, &mut data);
        assert_eq!(hex(&data[..16]), "7cd15324d71af9c318a1a7cd1cdf33e4");
        assert_eq!(
            hex(&sha3(&data)),
            "4b08520fde76f492d305b9bf740a294a0ccd64cbbd3e2783aebdd06c9353c483"
        );

        let mut signed = plain.clone();
        x.encrypt(SIGNED_SECTOR_FLAG | 5, &mut signed);
        assert_eq!(hex(&signed[..16]), "20e0ed830c5b7fa7536743e0f93b91fb");
        assert_eq!(
            hex(&sha3(&signed)),
            "e4b131dcb9d6fadb69de4ff7b0737f3f519ec6e57b5119c2a3148b53f2e54d42"
        );

        x.decrypt(5, &mut data);
        assert_eq!(data, plain);
    }
}
