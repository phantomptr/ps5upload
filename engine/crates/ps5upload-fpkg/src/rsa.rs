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
    for i in 0..a.len() {
        let (x, b1) = a[i].overflowing_sub(b.get(i).copied().unwrap_or(0));
        let (x, b2) = x.overflowing_sub(borrow);
        a[i] = x;
        borrow = u64::from(b1 || b2);
    }
}

/// `a + b` mod `n`, both shorter than `n`.
fn add_mod(a: &Limbs, b: &Limbs, n: &Limbs) -> Limbs {
    let mut out = vec![0u64; a.len()];
    let mut carry = 0u64;
    for i in 0..a.len() {
        let sum = a[i] as u128 + b.get(i).copied().unwrap_or(0) as u128 + carry as u128;
        out[i] = sum as u64;
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
/// The block is `00 01 FF..FF 00 || message`. Deterministic on purpose: an identically
/// built package is byte-identical.
pub fn pkcs1_encrypt(modulus_be: &[u8], message: &[u8]) -> Vec<u8> {
    let k = modulus_be.len();
    assert!(k >= 11, "modulus too small");
    assert!(message.len() + 11 <= k, "message too long for the modulus");
    let mut block = vec![0u8; k];
    block[1] = 0x01;
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
            "6ecd4a56c4ae3589e4be5404536b72a1d68ee8b11ef3c4bfe9434bf9af6aa157f3e80835e0b5b111c0ea6ad888f4a83f29a83d32280b7b680a9fe286067bb140b358d4bbadf5510d0a6ae2411e00ce0d2c0392645c31e92ff7b23a35f9cc11b57707353c9ff6bfadbbb9880360e1cbe39b1854265f745bb2cbf12357bd21a27b19dc4dfcdf6c6cd4057d77917d26367509e2ff886a0e6b7f6226ab65e960bed88cca580720781b302548c8dc15b2566f4d64421e240aca96aa846e691f9bfb19f506f96d1629e3879d48e044b03bbf3b1e47cacac6800c89d35fc207a1153ff637bb3dfa12f448036e0365dfd9e5d5b3985623962b6d202d91123b09506862df2c712d7b3b475f23fe3c629fe131be5f7b68c59ba852f8237ebf240c39d386db30a746645dee76dbe137b0209166b27da3e9908f91f05777138b75be388d40eaa09e7e5017c6e47486e40333db385c95f82001e07bb6a9d549f8e52830815ba2c97f7e31e1479f11cef19e2521e7bc7f7756c58521ae7cbac2650d5ccb7e0e2d"
        );
        assert_eq!(
            hex(&pkcs1_encrypt(&keys::METADATA_MODULUS, b"0123456789abcdef")),
            "45dd5de26de3c68561c4c20261a68dd36e45929c444830ae30ab09c9f92a40960bd19f41e965285afc4176804397239d93c25ab6c29a1d3629efce8d4a1131566599bd30bc8125e5f1f25908ca53b7c8f7671e60535681fa15198f2fe7a4728fc4e77b65fd859bb8f0be3f26bb5ec0fcc006d829e82a36b79d288ee847395d42c1897d2d51a3c5216215c1a4f40bda6870ec8c8c6ab612a3dc5e0eb7225b7f565914594009ded42d736a8e37075671188abba87abc2c343d37836d8beee32016ba79321da2ccb44a33b196593392be919d2c12138083a43a4f81d4bbbc09eccf57235988951844a136f743d5e4909e4f50fefa4ae22f33c2eae10a6d8a496e5512fb37a85abf7087ebc4169a4d0a96aef8afbd0e94c48e86d9c5b231485b10b9835293252106c798f03522cebc77ee90c238f719de02f95e9622cf08a62926409d405df3d11354e5b6cea03ff5ceeae4339c585c6a65a8ee5a50e0707a0935e7bde53971f2a0317c270382933407ee1186c4a925daec6c004cf3821b92e281e2"
        );
    }

    #[test]
    fn block_has_the_pkcs1_shape() {
        let c = pkcs1_encrypt(&keys::MOUNT_IMAGE_MODULUS, &[0u8; 32]);
        assert_eq!(c.len(), 384);
        assert_ne!(c, vec![0u8; 384]);
    }
}
