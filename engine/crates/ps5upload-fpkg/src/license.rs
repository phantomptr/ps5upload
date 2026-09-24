//! The debug license entries an application package carries: `license.dat` (a RIF) and
//! `license.info`. Without them the console refuses to start the game
//! (`sceSblACMgrGetFsSandboxType(.../eboot.bin) failed. 0x80020016`, measured on a FW 5.10 Phat,
//! where the same game in a package with these entries launched).
//!
//! Both are pure functions of the content id. `license.dat` encrypts a key block with
//! [`keys::RIF_DEBUG_KEY`] and signs its first 768 bytes with the debug RIF RSA key; the layout
//! was reproduced byte for byte against LibProsperoPkg's output for the same content id.

use num_bigint::BigUint;
use sha2::{Digest, Sha256};

use crate::keys;

/// `license.dat` and `license.info` are both of fixed size.
pub const LICENSE_DAT_LEN: usize = 1024;
pub const LICENSE_INFO_LEN: usize = 512;

/// The license content type of an application (game or app).
const APPLICATION: u16 = 32;

/// SHA-256 DigestInfo prefix for a PKCS#1 v1.5 signature.
const SHA256_DIGEST_INFO: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// A debug `license.dat` for an application with `content_id` (36 ASCII characters).
pub fn license_dat(content_id: &str) -> [u8; LICENSE_DAT_LEN] {
    let cid = content_id.as_bytes();
    let mut d = [0u8; LICENSE_DAT_LEN];
    d[0..4].copy_from_slice(b"RIF\0");
    d[4..8].copy_from_slice(&[0x00, 0x01, 0xff, 0xff]);
    d[20..32].copy_from_slice(&[
        0x51, 0x50, 0x61, 0x43, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    ]);
    d[32..32 + cid.len()].copy_from_slice(cid);
    d[80..82].copy_from_slice(&512u16.to_be_bytes());
    d[82..84].copy_from_slice(&16u16.to_be_bytes());
    d[84..86].copy_from_slice(&APPLICATION.to_be_bytes());
    d[86..88].copy_from_slice(&3u16.to_be_bytes());
    d[103] = 1;

    // The key block: the second half of SHA-256(content id padded to 48), encrypted under the
    // debug key with the first half as the IV, which is also stored in the clear before it.
    let mut padded = [0u8; 48];
    padded[..cid.len()].copy_from_slice(cid);
    let h = Sha256::digest(padded);
    let mut block = [0u8; 144];
    block[..16].copy_from_slice(&h[16..32]);
    crate::crypto::aes128_cbc_encrypt(
        &keys::RIF_DEBUG_KEY,
        h[..16].try_into().unwrap(),
        &mut block,
    );
    d[608..624].copy_from_slice(&h[..16]);
    d[624..768].copy_from_slice(&block);

    let signature = sign_sha256(&Sha256::digest(&d[..768]));
    d[768..].copy_from_slice(&signature);
    d
}

/// The matching `license.info`.
pub fn license_info(content_id: &str) -> [u8; LICENSE_INFO_LEN] {
    let cid = content_id.as_bytes();
    let mut d = [0u8; LICENSE_INFO_LEN];
    d[..cid.len()].copy_from_slice(cid);
    d[68..72].copy_from_slice(&u32::from(APPLICATION).to_be_bytes());
    d[76..80].copy_from_slice(&1u32.to_be_bytes());
    d
}

/// PKCS#1 v1.5 signature of a SHA-256 digest with the debug RIF key.
fn sign_sha256(digest: &[u8]) -> [u8; 256] {
    let mut em = [0xffu8; 256];
    em[0] = 0x00;
    em[1] = 0x01;
    let t = 256 - SHA256_DIGEST_INFO.len() - digest.len();
    em[t - 1] = 0x00;
    em[t..t + SHA256_DIGEST_INFO.len()].copy_from_slice(&SHA256_DIGEST_INFO);
    em[t + SHA256_DIGEST_INFO.len()..].copy_from_slice(digest);
    let n = BigUint::from_bytes_be(&keys::DEBUG_RIF_MODULUS);
    let d = BigUint::from_bytes_be(&keys::DEBUG_RIF_PRIVATE_EXPONENT);
    let s = BigUint::from_bytes_be(&em).modpow(&d, &n).to_bytes_be();
    let mut out = [0u8; 256];
    out[256 - s.len()..].copy_from_slice(&s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CID: &str = "UP4433-PPSA17221_00-MINECRAFTPS50000";

    /// The signature opens with the public exponent to the exact PKCS#1 block — which also
    /// catches a mistyped key.
    #[test]
    fn license_signature_verifies() {
        let lic = license_dat(CID);
        let n = BigUint::from_bytes_be(&keys::DEBUG_RIF_MODULUS);
        let m = BigUint::from_bytes_be(&lic[768..]).modpow(&BigUint::from(65537u32), &n);
        let digest = Sha256::digest(&lic[..768]);
        let em = m.to_bytes_be();
        assert_eq!(em[0], 0x01, "block type 1 after the leading zero");
        assert!(em.ends_with(&digest));
        assert!(em.windows(19).any(|w| w == SHA256_DIGEST_INFO));
    }

    /// The fields the console reads, as LibProsperoPkg's license for the same content id has
    /// them (its decrypted `license.dat` begins `RIF\0 0001ffff`, content id at 0x20,
    /// 0x0200 0x0010 0x0020 0x0003 at 0x50, a 1 at 0x67).
    #[test]
    fn license_layout() {
        let lic = license_dat(CID);
        assert_eq!(&lic[..8], b"RIF\0\x00\x01\xff\xff");
        assert_eq!(&lic[32..68], CID.as_bytes());
        assert_eq!(
            &lic[80..88],
            &[0x02, 0x00, 0x00, 0x10, 0x00, 0x20, 0x00, 0x03]
        );
        assert_eq!(lic[103], 1);
        let info = license_info(CID);
        assert_eq!(&info[..36], CID.as_bytes());
        assert_eq!(&info[68..72], &[0, 0, 0, 32]);
        assert_eq!(&info[76..80], &[0, 0, 0, 1]);
    }
}
