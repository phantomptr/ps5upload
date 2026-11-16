//! Protected container entries (`flags1` bit 31): `nptitle.dat` and the two `npbind.dat` files a
//! package can keep only in its container, encrypted. [`crate::crypto::encrypt_entry`] is the
//! writer; this is its inverse.
//!
//! Ported from PS5 Dump Forge (`crates/ps5-dump-forge-fpkg/src/protected.rs`, its PR #47
//! "decrypt protected container entries instead of dropping them").
//!
//! key/iv = SHA3-256(row ‖ entry key): the first 16 bytes the IV, the last 16 the AES-128 key,
//! CBC. `row` is the entry's 32-byte table row as stored, and the entry key is the
//! passcode-derived key ([`derive_pfs_key`]) the row's `flags2 >> 12` names. Before a byte is
//! decrypted the key is tested against the container's entry-keys slot, so a wrong passcode
//! never passes garbage off as the file.

use crate::be32;
use crate::cnt::{ids, Cnt, Entry};
use crate::crypto::{aes128_cbc_decrypt, derive_pfs_key, sha3};

/// `flags1` bit 31: the entry is stored encrypted.
pub const FLAG: u32 = 0x8000_0000;

/// The most a protected entry may be and still be decrypted: they are a few KiB.
const MAX_SIZE: u32 = 1 << 20;

/// The entry-keys slot holds keys 0..=6.
const KEY_COUNT: u32 = 7;

const ROW_LEN: usize = 0x20;

/// `e` is stored encrypted.
pub fn is_protected(e: &Entry) -> bool {
    e.flags1 & FLAG != 0
}

/// Decrypt a protected entry in place: the inverse of [`crate::crypto::encrypt_entry`]. `data`
/// is whole 16-byte blocks.
pub fn decrypt_entry(row: &[u8; 32], entry_key: &[u8; 32], data: &mut [u8]) {
    let mut pre = Vec::with_capacity(64);
    pre.extend_from_slice(row);
    pre.extend_from_slice(entry_key);
    let iv_key = sha3(&pre);
    // The digest's first half is the IV, its second half the key.
    let key: [u8; 16] = std::array::from_fn(|i| iv_key[16 + i]);
    let iv: [u8; 16] = std::array::from_fn(|i| iv_key[i]);
    aes128_cbc_decrypt(&key, iv, data);
}

/// Whether `key`, the passcode-derived key `k`, passes the container's entry-keys slot (the
/// 32-byte seed digest, then `SHA3(k) ^ k` for keys 0..=6); `None` when the slot has no digest
/// for it.
pub fn key_check(cnt: &Cnt, key: &[u8; 32], k: u32) -> Option<bool> {
    let at = 32 + 32 * usize::try_from(k).ok()?;
    let keys = cnt.entry(ids::ENTRY_KEYS)?;
    let stored = cnt.payload(keys).get(at..at.checked_add(32)?)?;
    let mut want = sha3(key);
    for (w, b) in want.iter_mut().zip(key) {
        *w ^= b;
    }
    Some(stored == want)
}

/// The 32-byte table row of the container's entry `id`, as stored.
fn row_of(cnt: &Cnt, id: u32) -> Option<[u8; 32]> {
    let i = cnt.entries.iter().position(|e| e.id == id)?;
    let table = usize::try_from(be32(cnt.bytes.get(0x18..0x1C)?, 0)).ok()?;
    let at = table.checked_add(i.checked_mul(ROW_LEN)?)?;
    cnt.bytes.get(at..at.checked_add(ROW_LEN)?)?.try_into().ok()
}

/// The plaintext of the protected entry `e`, or why it can't be had (a reason in plain words,
/// for a note that says the file was left out).
pub fn plaintext(cnt: &Cnt, e: &Entry, passcode: &str) -> std::result::Result<Vec<u8>, String> {
    if e.size > MAX_SIZE {
        return Err(format!(
            "it is {} bytes, more than the {MAX_SIZE} this app decrypts",
            e.size
        ));
    }
    let k = (e.flags2 >> 12) & 0xF;
    if k >= KEY_COUNT {
        return Err(format!(
            "it names key {k}, which the package has no key for"
        ));
    }
    let key = derive_pfs_key(&cnt.content_id, passcode, k);
    match key_check(cnt, &key, k) {
        None => return Err("the package has no key check for its key".into()),
        Some(false) => {
            return Err("it is encrypted with a passcode other than the one used to open it".into())
        }
        Some(true) => {}
    }
    let row = row_of(cnt, e.id).ok_or("its row in the container's table is missing")?;
    // Stored padded to the cipher's block; `size` is the file's own length.
    let size = e.size as usize;
    let start = e.offset as usize;
    let mut data = start
        .checked_add(size.next_multiple_of(16))
        .and_then(|end| cnt.bytes.get(start..end))
        .ok_or("the package is cut short inside it")?
        .to_vec();
    decrypt_entry(&row, &key, &mut data);
    data.truncate(size);
    Ok(data)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::crypto::encrypt_entry;

    /// A container of two entries as a third-party builder writes it: the entry-keys slot (the
    /// seed digest and `SHA3(k) ^ k` for keys 0..=6, for `passcode`) and a protected
    /// `nptitle.dat` (0x0402) of `plain`, encrypted with `passcode`'s key 3. From Forge's
    /// `protected.rs` tests.
    pub(crate) fn container(content_id: &str, passcode: &str, plain: &[u8]) -> Vec<u8> {
        let (keys_at, title_at) = (0x1100usize, 0x1200usize);
        let padded = plain.len().next_multiple_of(16);
        let mut c = vec![0u8; title_at + padded];
        c[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes());
        c[0x10..0x14].copy_from_slice(&2u32.to_be_bytes());
        c[0x18..0x1C].copy_from_slice(&0x1000u32.to_be_bytes());
        c[0x40..0x40 + content_id.len()].copy_from_slice(content_id.as_bytes());
        let row =
            |c: &mut Vec<u8>, i: usize, id: u32, flags: (u32, u32), at: usize, size: usize| {
                let o = 0x1000 + i * 0x20;
                c[o..o + 4].copy_from_slice(&id.to_be_bytes());
                c[o + 8..o + 12].copy_from_slice(&flags.0.to_be_bytes());
                c[o + 12..o + 16].copy_from_slice(&flags.1.to_be_bytes());
                c[o + 16..o + 20].copy_from_slice(&(at as u32).to_be_bytes());
                c[o + 20..o + 24].copy_from_slice(&(size as u32).to_be_bytes());
            };
        row(&mut c, 0, ids::ENTRY_KEYS, (0, 0), keys_at, 32 + 7 * 32);
        row(&mut c, 1, 0x0402, (FLAG, 3 << 12), title_at, plain.len());
        for k in 0..KEY_COUNT {
            let key = derive_pfs_key(content_id, passcode, k);
            let mut digest = sha3(&key);
            for (d, b) in digest.iter_mut().zip(key) {
                *d ^= b;
            }
            let at = keys_at + 32 + 32 * k as usize;
            c[at..at + 32].copy_from_slice(&digest);
        }
        let row_bytes: [u8; 32] = c[0x1020..0x1040].try_into().unwrap();
        let mut data = plain.to_vec();
        data.resize(padded, 0);
        encrypt_entry(
            &row_bytes,
            &derive_pfs_key(content_id, passcode, 3),
            &mut data,
        );
        c[title_at..].copy_from_slice(&data);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::container;
    use super::*;
    use crate::crypto::DEFAULT_PASSCODE;

    const CONTENT_ID: &str = "UP0000-PPSA01234_00-TESTGAME00000000";

    fn plain() -> Vec<u8> {
        (0..50).map(|i| (i * 7 + 1) as u8).collect()
    }

    fn cnt(passcode: &str) -> Cnt {
        Cnt::from_bytes(container(CONTENT_ID, passcode, &plain())).unwrap()
    }

    /// A container-only `nptitle.dat` reads back as the file it was (padded to the cipher's
    /// block, truncated to its size).
    #[test]
    fn a_protected_entry_reads_back_as_its_file() {
        let c = cnt(DEFAULT_PASSCODE);
        let e = *c.entry(0x0402).unwrap();
        assert!(is_protected(&e));
        assert_eq!(plaintext(&c, &e, DEFAULT_PASSCODE).unwrap(), plain());
    }

    /// Another passcode fails the key check; it never yields garbage as if it were the file.
    #[test]
    fn a_wrong_passcode_yields_nothing() {
        let c = cnt(DEFAULT_PASSCODE);
        let e = *c.entry(0x0402).unwrap();
        let why = plaintext(&c, &e, "11111111111111111111111111111111").unwrap_err();
        assert!(why.contains("passcode"), "{why}");
    }

    /// No key check, a key index past the slot, an entry too large or cut short: refused in
    /// words, nothing decrypted.
    #[test]
    fn an_entry_that_cannot_be_checked_is_refused() {
        let mut c = cnt(DEFAULT_PASSCODE);
        let mut e = *c.entry(0x0402).unwrap();
        e.flags2 = 9 << 12;
        assert!(plaintext(&c, &e, DEFAULT_PASSCODE)
            .unwrap_err()
            .contains("key 9"));
        let mut e = *c.entry(0x0402).unwrap();
        e.size = MAX_SIZE + 1;
        assert!(plaintext(&c, &e, DEFAULT_PASSCODE)
            .unwrap_err()
            .contains("more than"));
        let mut e = *c.entry(0x0402).unwrap();
        e.size = 0x1000;
        assert!(plaintext(&c, &e, DEFAULT_PASSCODE)
            .unwrap_err()
            .contains("cut short"));
        // The entry-keys slot shrunk to the seed digest alone: no check for key 3.
        c.entries[0].size = 32;
        let e = *c.entry(0x0402).unwrap();
        assert!(plaintext(&c, &e, DEFAULT_PASSCODE)
            .unwrap_err()
            .contains("no key check"));
    }
}
