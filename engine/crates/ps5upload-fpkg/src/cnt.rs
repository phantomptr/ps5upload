//! The `\x7FCNT` metadata container embedded in a finalized image.
//! Big-endian; every offset is relative to the start of the container.

use crate::crypto::sha3;
use crate::{be32, be64, format_err, PkgFile, Result};

const MAGIC: u32 = 0x7F43_4E54;
const ENTRY_LEN: usize = 0x20;
const HEADER_REGION: usize = 0x1000;

pub mod ids {
    pub const DIGESTS: u32 = 0x0001;
    pub const GENERAL_DIGESTS: u32 = 0x0080;
    pub const IMAGE_DIGESTS: u32 = 0x040A;
    pub const PARAM_JSON: u32 = 0x2000;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub id: u32,
    pub name_off: u32,
    pub flags1: u32,
    pub flags2: u32,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryDigest {
    /// The digest table's own slot, which real packages leave zero.
    SelfSlotZero,
    Match,
    Mismatch,
}

pub struct Cnt {
    pub bytes: Vec<u8>,
    pub content_id: String,
    pub body_offset: u64,
    pub body_size: u64,
    pub entries: Vec<Entry>,
}

/// Read the container at `cnt_offset` through the end of its last entry.
pub fn read(file: &mut PkgFile, cnt_offset: u64) -> Result<Cnt> {
    let head = file.read_at(cnt_offset, HEADER_REGION)?;
    if be32(&head, 0) != MAGIC {
        return format_err("embedded CNT magic mismatch");
    }
    let count = be32(&head, 0x10) as usize;
    let table = be32(&head, 0x18) as usize;
    let table_bytes = file.read_at(cnt_offset + table as u64, count * ENTRY_LEN)?;
    let mut end = table + count * ENTRY_LEN;
    for i in 0..count {
        let o = i * ENTRY_LEN;
        let off = be32(&table_bytes, o + 16) as usize;
        let size = be32(&table_bytes, o + 20) as usize;
        end = end.max(off + size);
    }
    Cnt::from_bytes(file.read_at(cnt_offset, end.max(HEADER_REGION))?)
}

impl Cnt {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Cnt> {
        if bytes.len() < HEADER_REGION || be32(&bytes, 0) != MAGIC {
            return format_err("not a CNT container");
        }
        let count = be32(&bytes, 0x10) as usize;
        let table = be32(&bytes, 0x18) as usize;
        if table + count * ENTRY_LEN > bytes.len() {
            return format_err("CNT entry table out of range");
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let o = table + i * ENTRY_LEN;
            let e = Entry {
                id: be32(&bytes, o),
                name_off: be32(&bytes, o + 4),
                flags1: be32(&bytes, o + 8),
                flags2: be32(&bytes, o + 12),
                offset: be32(&bytes, o + 16),
                size: be32(&bytes, o + 20),
            };
            if e.offset as usize + e.size as usize > bytes.len() {
                return format_err(format!("CNT entry {:#06x} out of range", e.id));
            }
            entries.push(e);
        }
        let content_id = String::from_utf8_lossy(&bytes[0x40..0x64])
            .trim_end_matches('\0')
            .to_string();
        Ok(Cnt {
            content_id,
            body_offset: be64(&bytes, 0x20),
            body_size: be64(&bytes, 0x28),
            entries,
            bytes,
        })
    }

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn payload(&self, e: &Entry) -> &[u8] {
        &self.bytes[e.offset as usize..e.offset as usize + e.size as usize]
    }

    /// `CNT+0xFE0 == SHA3(CNT[0..0xFE0])`.
    pub fn package_digest_ok(&self) -> bool {
        sha3(&self.bytes[..0xFE0]) == self.bytes[0xFE0..0x1000]
    }

    /// `CNT+0x100 == SHA3(entry 0x0001 payload)`.
    pub fn digest_table_digest_ok(&self) -> bool {
        match self.entry(ids::DIGESTS) {
            Some(e) => sha3(self.payload(e)) == self.bytes[0x100..0x120],
            None => false,
        }
    }

    /// Each entry's payload against its slot in the digest table.
    pub fn entry_digests(&self) -> Vec<(u32, EntryDigest)> {
        let Some(table) = self.entry(ids::DIGESTS).map(|e| self.payload(e)) else {
            return Vec::new();
        };
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let slot = table.get(i * 32..(i + 1) * 32).unwrap_or(&[]);
                let verdict = if e.id == ids::DIGESTS {
                    if slot.len() == 32 && slot.iter().all(|&b| b == 0) {
                        EntryDigest::SelfSlotZero
                    } else {
                        EntryDigest::Mismatch
                    }
                } else if slot == sha3(self.payload(e)) {
                    EntryDigest::Match
                } else {
                    EntryDigest::Mismatch
                };
                (e.id, verdict)
            })
            .collect()
    }

    /// `imagedigs.dat`: one digest per outer block, stored byte-reversed.
    /// Returned in natural order, i.e. directly comparable to `sha3(block)`.
    pub fn image_digests(&self) -> Option<Vec<[u8; 32]>> {
        let p = self.payload(self.entry(ids::IMAGE_DIGESTS)?);
        Some(
            p.as_chunks::<32>()
                .0
                .iter()
                .map(|c| {
                    let mut d = *c;
                    d.reverse();
                    d
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::sha3;

    /// A minimal CNT: two entries (the digest table and one payload) with
    /// every digest filled in the way real packages fill them.
    fn synthetic() -> Vec<u8> {
        let mut c = vec![0u8; 0x3000];
        c[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes());
        c[0x10..0x14].copy_from_slice(&2u32.to_be_bytes());
        c[0x18..0x1C].copy_from_slice(&0x2000u32.to_be_bytes());
        c[0x40..0x64].copy_from_slice(b"UP0000-PPSA01234_00-TESTGAME00000000");
        let table = 0x2000usize;
        let digests_off = 0x2100u32;
        let payload_off = 0x2200u32;
        let payload = b"{\"titleId\":\"PPSA01234\"}";
        let put = |c: &mut Vec<u8>, i: usize, id: u32, off: u32, size: u32| {
            let o = table + i * 0x20;
            c[o..o + 4].copy_from_slice(&id.to_be_bytes());
            c[o + 16..o + 20].copy_from_slice(&off.to_be_bytes());
            c[o + 20..o + 24].copy_from_slice(&size.to_be_bytes());
        };
        put(&mut c, 0, ids::DIGESTS, digests_off, 64);
        put(
            &mut c,
            1,
            ids::PARAM_JSON,
            payload_off,
            payload.len() as u32,
        );
        c[payload_off as usize..payload_off as usize + payload.len()].copy_from_slice(payload);
        let d = sha3(payload);
        c[digests_off as usize + 32..digests_off as usize + 64].copy_from_slice(&d);
        let table_digest = sha3(&c[digests_off as usize..digests_off as usize + 64]);
        c[0x100..0x120].copy_from_slice(&table_digest);
        let pkg = sha3(&c[..0xFE0]);
        c[0xFE0..0x1000].copy_from_slice(&pkg);
        c
    }

    #[test]
    fn synthetic_digests_verify() {
        let cnt = Cnt::from_bytes(synthetic()).unwrap();
        assert_eq!(cnt.content_id, "UP0000-PPSA01234_00-TESTGAME00000000");
        assert!(cnt.package_digest_ok());
        assert!(cnt.digest_table_digest_ok());
        assert_eq!(
            cnt.entry_digests(),
            vec![
                (ids::DIGESTS, EntryDigest::SelfSlotZero),
                (ids::PARAM_JSON, EntryDigest::Match)
            ]
        );
    }

    #[test]
    fn tampered_payload_is_caught() {
        let mut bytes = synthetic();
        bytes[0x2200] ^= 1;
        let cnt = Cnt::from_bytes(bytes).unwrap();
        assert_eq!(
            cnt.entry_digests()[1],
            (ids::PARAM_JSON, EntryDigest::Mismatch)
        );
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut bytes = synthetic();
        bytes[0] = 0;
        assert!(Cnt::from_bytes(bytes).is_err());
    }
}
