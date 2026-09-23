//! The `\x7FCNT` metadata container embedded in a finalized image.
//! Big-endian; every offset is relative to the start of the container.

use crate::crypto::sha3;
use crate::{be32, be64, format_err, PkgFile, Result};

const MAGIC: u32 = 0x7F43_4E54;
const ENTRY_LEN: usize = 0x20;
const HEADER_REGION: usize = 0x1000;

pub mod ids {
    pub const DIGESTS: u32 = 0x0001;
    pub const ENTRY_KEYS: u32 = 0x0010;
    pub const IMAGE_KEY: u32 = 0x0020;
    pub const GENERAL_DIGESTS: u32 = 0x0080;
    pub const METAS: u32 = 0x0100;
    pub const ENTRY_NAMES: u32 = 0x0200;
    pub const IMAGE_DIGESTS: u32 = 0x040A;
    pub const PLAYGO_CHUNK: u32 = 0x1001;
    pub const ICON0_PNG: u32 = 0x1200;
    pub const ICON0_DDS: u32 = 0x1280;
    pub const PARAM_JSON: u32 = 0x2000;
    pub const PLAYGO_HASH_TABLE: u32 = 0x2010;
    pub const PLAYGO_FICM: u32 = 0x2011;
    pub const SAVE_DATA_PNG: u32 = 0x100D;
    pub const PIC0_PNG: u32 = 0x1220;
    pub const SND0_AT9: u32 = 0x1240;
    pub const PIC0_DDS: u32 = 0x12A0;
    pub const PIC1_DDS: u32 = 0x12C0;
    pub const TROPHY: u32 = 0x1480;
    pub const UDS: u32 = 0x14A0;
    pub const PIC2_DDS: u32 = 0x2060;

    /// The presentation entries the "system" general digest covers, in the id order it
    /// hashes them. Derived from a Publishing Tools package with all eight: the slot is the
    /// `SHA3` of their digests concatenated. Trophy and UDS data are not in it, and a
    /// package with only the two icons reduces to the icon-only formula the samples show.
    pub const SYSTEM_DIGEST_IDS: [u32; 8] = [
        SAVE_DATA_PNG,
        ICON0_PNG,
        PIC0_PNG,
        SND0_AT9,
        ICON0_DDS,
        PIC0_DDS,
        PIC1_DDS,
        PIC2_DDS,
    ];
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

/// Read the container at `cnt_offset` through the end of its body region.
pub fn read(file: &mut PkgFile, cnt_offset: u64) -> Result<Cnt> {
    let head = file.read_at(cnt_offset, HEADER_REGION)?;
    if be32(&head, 0) != MAGIC {
        return format_err("embedded CNT magic mismatch");
    }
    let count = be32(&head, 0x10) as usize;
    let table = be32(&head, 0x18) as usize;
    let table_bytes = file.read_at(cnt_offset + table as u64, count * ENTRY_LEN)?;
    // The body region can reach past the last entry (the body digest covers it), so
    // size the read by the header's body offset/size as well.
    let mut end = (table + count * ENTRY_LEN).max((be64(&head, 0x20) + be64(&head, 0x28)) as usize);
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

    /// `CNT+0x100 == SHA3(CNT[off .. off+size])` with `off`/`size` from the header fields.
    ///
    /// Measured on the three real samples.
    pub fn header_rollup_ok(&self) -> bool {
        let off = be64(&self.bytes, 0x20) as usize;
        let size = be32(&self.bytes, 0x1C) as usize;
        let pre = self.bytes.get(off..off + size);
        pre.is_some_and(|p| sha3(p) == self.bytes[0x100..0x120])
    }

    /// `CNT+0x160 == SHA3(body region)`, the region the header's body offset/size locate.
    pub fn body_digest_ok(&self) -> bool {
        let off = self.body_offset as usize;
        let size = self.body_size as usize;
        let pre = self.bytes.get(off..off + size);
        pre.is_some_and(|p| sha3(p) == self.bytes[0x160..0x180])
    }

    /// `CNT+0x460 == SHA3(finalized-image header block)`.
    pub fn fih_digest_ok(&self, fih_block: &[u8]) -> bool {
        sha3(fih_block) == self.bytes[0x460..0x480]
    }

    /// The `0x510` descriptor pairs hold the image-key and imagedigs entries' (offset, size).
    pub fn descriptor_ok(&self) -> bool {
        let pair = |at: usize| (be32(&self.bytes, at), be32(&self.bytes, at + 4));
        let key = self.entry(ids::IMAGE_KEY).map(|e| (e.offset, e.size));
        let digests = self.entry(ids::IMAGE_DIGESTS).map(|e| (e.offset, e.size));
        key == Some(pair(0x510)) && digests == Some(pair(0x518))
    }

    /// Recomputes the GeneralDigests slots (entry `0x0080`) that the package carries the
    /// inputs for. Each formula was verified on `webbrowser.pkg` on 2026-09-13.
    ///
    /// Slots, in table order: Content, Game, Header, System, MajorParam, Param, Playgo,
    /// Trophy, Manual, Keymap, Origin, Target, OriginGame, TargetGame.
    pub fn general_digests(&self, game_digest: &[u8; 32]) -> Vec<(&'static str, bool)> {
        let Some(gd) = self.entry(ids::GENERAL_DIGESTS).map(|e| self.payload(e)) else {
            return Vec::new();
        };
        let slot = |i: usize| gd.get(0x20 + i * 32..0x20 + (i + 1) * 32);
        let matches = |i: usize, digest: &[u8; 32]| slot(i).is_some_and(|s| s == digest);
        let concat = |digests: &[[u8; 32]]| {
            let mut pre = Vec::with_capacity(digests.len() * 32);
            for d in digests {
                pre.extend_from_slice(d);
            }
            sha3(&pre)
        };
        let entry_digest = |id: u32| self.entry(id).map(|e| sha3(self.payload(e)));

        let mut out = Vec::new();
        {
            let mut pre = Vec::with_capacity(0x38 + 64);
            pre.extend_from_slice(&self.bytes[0x40..0x78]);
            pre.extend_from_slice(game_digest);
            pre.extend_from_slice(&[0u8; 32]);
            let expected = sha3(&pre);
            out.push(("cnt general digest content", matches(0, &expected)));
        }
        out.push(("cnt general digest game", matches(1, game_digest)));
        {
            let mut pre = Vec::with_capacity(0xC0);
            pre.extend_from_slice(&self.bytes[0..0x40]);
            pre.extend_from_slice(&self.bytes[0x400..0x480]);
            let expected = sha3(&pre);
            out.push(("cnt general digest header", matches(2, &expected)));
        }
        let system: Vec<[u8; 32]> = ids::SYSTEM_DIGEST_IDS
            .iter()
            .filter_map(|id| entry_digest(*id))
            .collect();
        if !system.is_empty() {
            out.push(("cnt general digest system", matches(3, &concat(&system))));
        }
        if let (Some(chunk), Some(hash), Some(ficm)) = (
            entry_digest(ids::PLAYGO_CHUNK),
            entry_digest(ids::PLAYGO_HASH_TABLE),
            entry_digest(ids::PLAYGO_FICM),
        ) {
            let expected = concat(&[chunk, hash, ficm]);
            out.push(("cnt general digest playgo", matches(6, &expected)));
        }
        if let Some(param) = entry_digest(ids::PARAM_JSON) {
            out.push(("cnt general digest param", matches(5, &param)));
        }
        out.push(("cnt general digest target", matches(11, game_digest)));
        out
    }

    /// Whether `CNT[at..at+32]` holds `SHA3(payload of entry id)`.
    ///
    /// Measured on both debug samples: the digest table's digest sits at
    /// `0x140`, the image key's (`0x0020`) at `0x520` and imagedigs' at
    /// `0x540`.
    pub fn entry_digest_at(&self, id: u32, at: usize) -> bool {
        match self.entry(id) {
            Some(e) => sha3(self.payload(e)) == self.bytes[at..at + 32],
            None => false,
        }
    }

    /// `CNT+0x140 == SHA3(entry 0x0001 payload)`.
    pub fn digest_table_digest_ok(&self) -> bool {
        self.entry_digest_at(ids::DIGESTS, 0x140)
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
        c[0x140..0x160].copy_from_slice(&table_digest);
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

/// Helpers for the writer tests: containers that carry only what they need.
#[cfg(test)]
pub(crate) mod test_support {
    /// A minimal CNT whose single entry is the imagedigs table (id `0x040A`), stored the
    /// way a real package stores it: each digest byte-reversed.
    pub(crate) fn minimal_cnt(content_id: &str, digests: &[[u8; 32]]) -> Vec<u8> {
        let mut c = vec![0u8; 0x3000];
        c[0..4].copy_from_slice(&0x7F43_4E54u32.to_be_bytes());
        c[0x10..0x14].copy_from_slice(&1u32.to_be_bytes());
        c[0x18..0x1C].copy_from_slice(&0x2000u32.to_be_bytes());
        let id = content_id.as_bytes();
        let n = id.len().min(0x24);
        c[0x40..0x40 + n].copy_from_slice(&id[..n]);
        let payload = 0x2100u32;
        c[0x2000..0x2004].copy_from_slice(&0x040Au32.to_be_bytes());
        c[0x2010..0x2014].copy_from_slice(&payload.to_be_bytes());
        c[0x2014..0x2018].copy_from_slice(&((digests.len() * 32) as u32).to_be_bytes());
        let mut bytes = Vec::with_capacity(digests.len() * 32);
        for d in digests {
            let mut reversed = *d;
            reversed.reverse();
            bytes.extend_from_slice(&reversed);
        }
        c[payload as usize..payload as usize + bytes.len()].copy_from_slice(&bytes);
        c
    }
}
