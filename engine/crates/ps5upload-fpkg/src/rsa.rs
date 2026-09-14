//! RSA public-key operations for the metadata signature and the CNT key wraps.
//!
//! Off-console only one operation is needed: PKCS#1 v1.5 *encryption* under a big-endian
//! modulus with the universal exponent 65537. The padding is deterministic, so a given
//! input always produces the same block.
//!
//! Arithmetic is schoolbook over 64-bit limbs with modular double-and-add, which is more
//! than fast enough for a handful of 3072-bit operations per package.

use std::cmp::Ordering;

/// The public exponent every PKG key uses.
pub const E: u64 = 65537;

type Limbs = Vec<u64>;

fn from_be(bytes: &[u8]) -> Limbs {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(8));
    for chunk in bytes.rchunks(8) {
        let mut limb = [0u8; 8];
        limb[8 - chunk.len()..].copy_from_slice(chunk);
        out.push(u64::from_be_bytes(limb));
    }
    out
}

fn to_be(limbs: &[u64], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for (i, limb) in limbs.iter().enumerate().take(len.div_ceil(8)) {
        let start = len - (i + 1) * 8;
        out[start..start + 8].copy_from_slice(&limb.to_be_bytes());
    }
    out
}

/// `a` versus `b`, both padded to the same length.
fn cmp(a: &[u64], b: &[u64]) -> Ordering {
    for i in (0..a.len().max(b.len())).rev() {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

/// `a - b` in place; `a` must be at least `b`.
fn sub_assign(a: &mut [u64], b: &[u64]) {
    let mut borrow = 0u64;
    for (x, y) in a.iter_mut().zip(b) {
        let (diff, b1) = x.overflowing_sub(*y);
        let (diff, b2) = diff.overflowing_sub(borrow);
        *x = diff;
        borrow = u64::from(b1 || b2);
    }
}

/// `a + b` mod `n`; `a` and `b` hold `n.len()` limbs.
fn add_mod(a: &Limbs, b: &Limbs, n: &Limbs) -> Limbs {
    let mut out = vec![0u64; n.len()];
    let mut carry = 0u64;
    for ((slot, x), y) in out.iter_mut().zip(a).zip(b) {
        let sum = *x as u128 + *y as u128 + carry as u128;
        *slot = sum as u64;
        carry = (sum >> 64) as u64;
    }
    if carry != 0 || cmp(&out, n) != Ordering::Less {
        sub_assign(&mut out, n);
    }
    out
}

/// `2a` mod `n`.
fn double_mod(a: &Limbs, n: &Limbs) -> Limbs {
    add_mod(a, a, n)
}

/// `a * b` mod `n` by double-and-add over the bits of `b`.
fn mul_mod(a: &Limbs, b: &Limbs, n: &Limbs) -> Limbs {
    let mut acc = vec![0u64; n.len()];
    for limb in b.iter().rev() {
        for bit in (0..64).rev() {
            acc = double_mod(&acc, n);
            if (limb >> bit) & 1 == 1 {
                acc = add_mod(&acc, a, n);
            }
        }
    }
    acc
}

/// `base^exp` mod `n`.
fn pow_mod(base: &Limbs, exp: u64, n: &Limbs) -> Limbs {
    let mut acc = vec![0u64; n.len()];
    acc[0] = 1;
    for bit in (0..64 - exp.leading_zeros()).rev() {
        acc = mul_mod(&acc, &acc, n);
        if (exp >> bit) & 1 == 1 {
            acc = mul_mod(&acc, base, n);
        }
    }
    acc
}

/// PKCS#1 v1.5 encryption of `message` under a big-endian modulus, exponent 65537.
///
/// The block is `00 02 <nonzero padding> 00 || message` — the *encryption* padding (EME-PKCS1-v1_5),
/// which is what these fields are: a console decrypts them with the private half. The signature
/// padding (`00 01 FF..FF 00`) makes a strict decryptor reject the block outright, which is how
/// the console answered our earlier packages. The padding bytes are fixed rather than random so
/// that an identically built package is byte-identical.
pub fn pkcs1_encrypt(modulus_be: &[u8], message: &[u8]) -> Vec<u8> {
    let k = modulus_be.len();
    assert!(k >= 11, "modulus too small");
    assert!(message.len() + 11 <= k, "message too long for the modulus");
    let mut block = vec![0u8; k];
    block[1] = 0x02;
    for b in block.iter_mut().take(k - message.len() - 1).skip(2) {
        *b = 0xFF;
    }
    block[k - message.len()..].copy_from_slice(message);
    let n = from_be(modulus_be);
    let c = pow_mod(&from_be(&block), E, &n);
    to_be(&c, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Vectors computed with Python's `pow(m, 65537, n)` over the same modulus, so the
    /// limb arithmetic is checked against an independent implementation.
    #[test]
    fn pkcs1_encrypt_matches_reference_vectors() {
        let digest: Vec<u8> = (0..32).collect();
        assert_eq!(
            hex(&pkcs1_encrypt(&keys::METADATA_MODULUS, &digest)),
            "43c644ee5d4d2989aa602457e2a368486f1f90c659d7505b8d14dd9a81fdd4c2ff6a84691562e067960a1f00ec05d985b4e786cf6b8045021ce2624daba3cbb51f0d16279a6a7f942c2b4a75cfbf1975c44f1c615b3f32f643f2c38ba96ce5eaeba8aa4bd383bb0f5828e930c96ea0a7117d52df69256157b393f84f48a51c6b4613d603be6faa6da93abb17776d0df8cc845db02bf0c1343c6a29b292ed086b7bc10076c1055b52df14adc52c647184db41e0117a52a330f56db832a7073a0089d64016a0d436bae711b4ef7ded932b1237238ce5fc742bf7cacccdcecc6472b6b2bd005ef20622db9b68ad56676c559afd3975f32272c24684f5b08774dcaef7a371033495f864755af0b6320d23c56fbc239c5198687beee22602b2a68c7e8ad25bced42eeeb486936dc7a8521f9e45284d96db7f04c51c498ba01095900bdbee9bb07bf5b56e7e07cae48feb38cc9c2d5cd9319d5d38e0ce4837bb6ef781857a9e152ef32b4f1cc2e2654bb26ac14731fbbcfc8a1cd2696491e3100179a2"
        );
        assert_eq!(
            hex(&pkcs1_encrypt(&keys::METADATA_MODULUS, b"0123456789abcdef")),
            "2b32ea134a812ae8c5e0445dff3c594c900b755c73263032df80443aee91dee4178176a69c08eece658a4c2e61bbd4f95ff58268b4b4618cff7cb5c78d2545cdfbddea66687a137e54f5fd466d89fbb91dea1aaa7683540bb7f4e7a55454ea2746dfbbcf055c07897f01838951baa432f36bb2ebb68120e6c93df86090dc74eb0597acfd4c6a45ca71f828ef4b4fd346de0c8c093350f3891cb893938733f083ad6f23e29d5761f02f9afca4d5a59a4bc202b23a64d197ad6397105bd48fb260d34f69e15e5a8c1633dd8198e5571f78d2f57918484f3370e2fd12d0cb0509f9385fe22427e11c87e5333d9c7935445e486a21aa5ce9583aa307dc37c9a34fccaa6dbf9c95da0fe62b268c4e4667c2befb090c18ef205bc4e78a074faa3ad1bce281ffe1ef21b653f73947994a9bc16658a2c12917ec74029c8b58770be29973eaf81e04a936fd7cf7a382e2ff31d28f931dfda6ed45a0cf703e22f10dedd951cbb343eee7bef70492ea5a1439236081ff33a0666fd1f09578417de86b33d9c9"
        );
    }

    #[test]
    fn block_has_the_pkcs1_shape() {
        let c = pkcs1_encrypt(&keys::MOUNT_IMAGE_MODULUS, &[0u8; 32]);
        assert_eq!(c.len(), 384);
        assert_ne!(c, vec![0u8; 384]);
    }
}
