//! The PS5 PFS compression container (`PFSC`), stored-profile.
//!
//! An inner-image file whose logical bytes are not stored verbatim lives on disk inside one of
//! these: a 0x48-byte header, a seven-entry section directory, then the section data — the
//! block-boundary table, a SHA3-256 digest per logical block, a 16-byte signature per block and,
//! last, the block payloads.
//!
//! This module writes the *stored* profile: every block's payload is its own logical bytes, so no
//! entropy coder is needed. That is what the encoder itself emits for incompressible data, so the
//! result is a container a reader cannot tell from a compressed one except by the block flags.
//!
//! Layout and field formulas are the ones the reference implementation documents and its reader
//! round-trips; the constants were re-derived here and are covered by the tests below.

use crate::crypto::sha3;
use crate::Error;

/// `'PFSC'`.
const MAGIC: u32 = 0x4353_4650;
/// Container format version this writer emits.
const VERSION: u16 = 3;
/// Directory entries, in the fixed order below.
const SECTION_COUNT: u16 = 7;
/// The encode-parameter word at `0x0C`: v3 / Kraken / 256 KiB blocks / window 18.
const ENCODE_PARAM_0C: u32 = 0x0802;
/// Section directory entries are 16 bytes.
const DIR_ENTRY: usize = 16;
/// Where the sections start: past the header and the directory.
const OFF1: usize = 0x48 + SECTION_COUNT as usize * DIR_ENTRY;
/// Section data is 8-byte aligned; the block payloads are 0x400-aligned.
const SECTION_ALIGN: usize = 8;
const DATA_ALIGN: usize = 0x400;
/// The logical block size of the container (the format's default for v3).
pub const BLOCK: usize = 0x40000;
/// A block larger than this is carried as two sub-chunks.
const SUB_CHUNK: usize = 0x20000;
/// Boundary-table flag: sub-chunk 0 is verbatim; and the same for sub-chunk 1.
const FLAG_RAW_COPY: u64 = 0x0C;
const FLAG_RAW_COPY_HIGH: u64 = 0xC0;
/// Bits of the boundary entry's second word that carry the first sub-chunk's length minus one.
const SIZE_HINT_SHIFT: u64 = 44;
const SIZE_HINT_MASK: u64 = 0x1_FFFF;
/// Bits of the boundary entry's first word that carry the flags.
const FLAG_SHIFT: u64 = 48;
/// The Kraken level recorded in the header. Stored blocks do not use it, but the file digest does.
const LEVEL: u64 = 7;
/// Sliding-window bits recorded in the header (the only valid value).
const WINDOW_BITS: u64 = 18;

/// id=1: a 20-byte constant (the encoder's source revision hash).
const GIT_HASH: [u8; 20] = [
    0x23, 0x98, 0x7d, 0x16, 0xc9, 0x20, 0x9a, 0xc7, 0x28, 0x37, 0x19, 0x32, 0x7e, 0x0f, 0x50, 0x6b,
    0xbc, 0xf4, 0x59, 0xf4,
];

/// id=2: the shuffle-pattern field-width table (constant; the default profile shuffles nothing).
const SHUFFLE_TABLE: [u8; 64] = [
    0x04, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x01, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
    0x08, 0x02, 0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x06, 0x02, 0x02, 0x04, 0x00, 0x00,
    0x01, 0x01, 0x06, 0x01, 0x01, 0x06, 0x00, 0x00, 0x04, 0x04, 0x04, 0x04, 0x00, 0x00, 0x00, 0x00,
];

/// The bytes a block contributes to its signature: two running-sum pairs over the block's
/// logical bytes, the first covering a 0x10000-byte window zero-extended to its full length.
fn block_signature(block: &[u8]) -> [u8; 16] {
    const WINDOW: usize = 0x10000;
    let whole = block.len().min(0x100_0000);
    let mut whole_sum: u64 = 0;
    let mut whole_weighted: u64 = whole as u64;
    for &b in &block[..whole] {
        whole_sum = whole_sum.wrapping_add(b as u64);
        whole_weighted = whole_weighted.wrapping_add(whole_sum);
    }
    let window = block.len().min(WINDOW);
    let mut window_sum: u64 = 0;
    let mut window_weighted: u64 = 0;
    for &b in &block[..window] {
        window_sum = window_sum.wrapping_add(b as u64);
        window_weighted = window_weighted.wrapping_add(window_sum);
    }
    if block.len() < WINDOW {
        window_weighted = window_weighted.wrapping_add(window_sum * (WINDOW - block.len()) as u64);
    }
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&((window_weighted << 25) ^ window_sum).to_le_bytes());
    out[8..].copy_from_slice(
        &(((whole_weighted & 0xFFFF_FFFF) << 32) | (whole_sum & 0xFFFF_FFFF)).to_le_bytes(),
    );
    out
}

fn align(value: usize, to: usize) -> usize {
    value.div_ceil(to) * to
}

fn put_dir_entry(out: &mut [u8], index: usize, id: u16, offset: usize, size: usize) {
    let p = 0x48 + index * DIR_ENTRY;
    out[p..p + 2].copy_from_slice(&id.to_le_bytes());
    out[p + 2..p + 6].copy_from_slice(&(offset as u32).to_le_bytes());
    out[p + 6..p + 8].copy_from_slice(&((offset >> 32) as u16).to_le_bytes());
    out[p + 10..p + 14].copy_from_slice(&(size as u32).to_le_bytes());
    out[p + 14..p + 16].copy_from_slice(&((size >> 32) as u16).to_le_bytes());
}

/// The file-level digest over the header parameters and three of the sections.
fn file_digest(
    block_size: u32,
    param0c: u32,
    param10: u64,
    logical_size: u64,
    shuffle: &[u8],
    boundaries: &[u8],
    digests: &[u8],
) -> [u8; 32] {
    let mut header = [0u8; 32];
    header[0..4].copy_from_slice(&1u32.to_le_bytes());
    header[4..8].copy_from_slice(&block_size.to_le_bytes());
    header[8..12].copy_from_slice(&param0c.to_le_bytes());
    header[16..24].copy_from_slice(&param10.to_le_bytes());
    header[24..32].copy_from_slice(&logical_size.to_le_bytes());
    let mut pre = Vec::with_capacity(32 + shuffle.len() + boundaries.len() + digests.len());
    pre.extend_from_slice(&header);
    pre.extend_from_slice(shuffle);
    pre.extend_from_slice(boundaries);
    pre.extend_from_slice(digests);
    sha3(&pre)
}

/// How many bytes [`write_stored`] produces for a payload of this length.
///
/// The container's framing is a pure function of the length, so a caller can plan the on-disk
/// size of a stream it has not written yet.
pub fn container_len(payload_len: usize) -> usize {
    let block_count = payload_len.div_ceil(BLOCK).max(1);
    let sec3 = (block_count + 1) * DIR_ENTRY;
    let sec4 = block_count * 32;
    let sec5 = block_count * DIR_ENTRY;
    let off2 = align(OFF1 + GIT_HASH.len(), SECTION_ALIGN);
    let off3 = align(off2 + SHUFFLE_TABLE.len(), SECTION_ALIGN);
    let off4 = align(off3 + sec3, SECTION_ALIGN);
    let off5 = align(off4 + sec4, SECTION_ALIGN);
    let off6 = align(off5 + sec5, SECTION_ALIGN);
    align(off6, DATA_ALIGN) + payload_len
}

/// Wrap `payload` in a stored-profile `PFSC` container.
///
/// The result is larger than the payload by the container's own metadata; a caller that has a
/// choice should only wrap what it cannot store verbatim, exactly as an encoder's
/// "keep compressed only when it wins" rule does.
pub fn write_stored(payload: &[u8]) -> Vec<u8> {
    let block_size = BLOCK as u32;
    let block_count = payload.len().div_ceil(BLOCK).max(1);
    let param10 = 2u64 | (LEVEL << 8) | (WINDOW_BITS << 16);

    let sec1 = GIT_HASH.len();
    let sec2 = SHUFFLE_TABLE.len();
    let sec3 = (block_count + 1) * DIR_ENTRY;
    let sec4 = block_count * 32;
    let sec5 = block_count * DIR_ENTRY;

    let off1 = OFF1;
    let off2 = align(off1 + sec1, SECTION_ALIGN);
    let off3 = align(off2 + sec2, SECTION_ALIGN);
    let off4 = align(off3 + sec3, SECTION_ALIGN);
    let off5 = align(off4 + sec4, SECTION_ALIGN);
    let off6 = align(off5 + sec5, SECTION_ALIGN);
    let off7 = align(off6, DATA_ALIGN);

    let mut boundaries = vec![0u8; sec3];
    let mut digests = vec![0u8; sec4];
    let mut signatures = vec![0u8; sec5];

    let mut cumulative_comp = 0u64;
    let mut cumulative_uncomp = 0u64;
    for i in 0..block_count {
        let at = i * BLOCK;
        let block = &payload[at.min(payload.len())..(at + BLOCK).min(payload.len())];
        let e = i * DIR_ENTRY;
        let flags = FLAG_RAW_COPY
            | if block.len() > SUB_CHUNK {
                FLAG_RAW_COPY_HIGH
            } else {
                0
            };
        let hint = (block.len().min(SUB_CHUNK).saturating_sub(1) as u64) & SIZE_HINT_MASK;
        boundaries[e..e + 8]
            .copy_from_slice(&(cumulative_comp | (flags << FLAG_SHIFT)).to_le_bytes());
        boundaries[e + 8..e + 16]
            .copy_from_slice(&(cumulative_uncomp | (hint << SIZE_HINT_SHIFT)).to_le_bytes());
        digests[i * 32..(i + 1) * 32].copy_from_slice(&sha3(block));
        signatures[i * DIR_ENTRY..(i + 1) * DIR_ENTRY].copy_from_slice(&block_signature(block));
        cumulative_comp += block.len() as u64;
        cumulative_uncomp += block.len() as u64;
    }
    // Sentinel: the totals, no flags and no hint.
    let s = block_count * DIR_ENTRY;
    boundaries[s..s + 8].copy_from_slice(&cumulative_comp.to_le_bytes());
    boundaries[s + 8..s + 16].copy_from_slice(&payload.len().to_le_bytes());

    let total = off7 + payload.len();
    let mut out = vec![0u8; total];
    out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    out[4..6].copy_from_slice(&VERSION.to_le_bytes());
    out[6..8].copy_from_slice(&SECTION_COUNT.to_le_bytes());
    out[8..12].copy_from_slice(&block_size.to_le_bytes());
    out[12..16].copy_from_slice(&ENCODE_PARAM_0C.to_le_bytes());
    out[16..24].copy_from_slice(&param10.to_le_bytes());
    out[24..32].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    out[32..40].copy_from_slice(&(total as u64).to_le_bytes());
    put_dir_entry(&mut out, 0, 1, off1, sec1);
    put_dir_entry(&mut out, 1, 2, off2, sec2);
    put_dir_entry(&mut out, 2, 3, off3, sec3);
    put_dir_entry(&mut out, 3, 4, off4, sec4);
    put_dir_entry(&mut out, 4, 5, off5, sec5);
    put_dir_entry(&mut out, 5, 6, off6, 0);
    put_dir_entry(&mut out, 6, 7, off7, payload.len());
    out[off1..off1 + sec1].copy_from_slice(&GIT_HASH);
    out[off2..off2 + sec2].copy_from_slice(&SHUFFLE_TABLE);
    out[off3..off3 + sec3].copy_from_slice(&boundaries);
    out[off4..off4 + sec4].copy_from_slice(&digests);
    out[off5..off5 + sec5].copy_from_slice(&signatures);
    out[off7..].copy_from_slice(payload);

    let digest = file_digest(
        block_size,
        ENCODE_PARAM_0C,
        param10,
        payload.len() as u64,
        &SHUFFLE_TABLE,
        &boundaries,
        &digests,
    );
    out[0x28..0x48].copy_from_slice(&digest);
    out
}

/// What a parsed container says about itself.
pub struct Container {
    pub version: u16,
    pub block_size: u32,
    pub logical_size: u64,
    /// Per block: `(flags, first_sub_chunk_len, payload_len, logical_len)`.
    pub blocks: Vec<(u64, u64, u64, u64)>,
    /// The block payloads as they sit on disk, in order.
    pub payloads: Vec<Vec<u8>>,
    /// Whether every block is flagged verbatim.
    pub all_stored: bool,
    /// Whether the header's file digest reproduces.
    pub digest_ok: bool,
}

/// Parse a container. Blocks are returned as they sit on disk; only the stored profile can be
/// turned back into logical bytes here.
pub fn parse(container: &[u8]) -> crate::Result<Container> {
    if container.len() < OFF1 || u32::from_le_bytes(container[0..4].try_into().unwrap()) != MAGIC {
        return crate::format_err("not a PFSC container");
    }
    let le16 = |at: usize| u16::from_le_bytes(container[at..at + 2].try_into().unwrap());
    let le32 = |at: usize| u32::from_le_bytes(container[at..at + 4].try_into().unwrap());
    let le64 = |at: usize| u64::from_le_bytes(container[at..at + 8].try_into().unwrap());
    let version = le16(4);
    let sections = le16(6);
    let block_size = le32(8);
    let param0c = le32(0x0C);
    let param10 = le64(0x10);
    let logical_size = le64(0x18);

    let mut dir = Vec::new();
    for i in 0..sections as usize {
        let p = 0x48 + i * DIR_ENTRY;
        let id = le16(p);
        let off = le32(p + 2) as u64 | ((le16(p + 6) as u64) << 32);
        let size = le32(p + 10) as u64 | ((le16(p + 14) as u64) << 32);
        dir.push((id, off as usize, size as usize));
    }
    let section = |id: u16| {
        dir.iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, o, s)| (*o, *s))
    };
    let (off3, sec3) = section(3).ok_or_else(|| Error::Format("no boundary table".into()))?;
    let (off4, sec4) = section(4).ok_or_else(|| Error::Format("no block digests".into()))?;
    let (off7, _) = section(7).ok_or_else(|| Error::Format("no block data".into()))?;
    let (off2, sec2) = section(2).unwrap_or((0, 0));

    // The boundary table has one entry per block plus a sentinel holding the totals.
    let entry = |i: usize| {
        let e = off3 + i * DIR_ENTRY;
        (le64(e), le64(e + 8))
    };
    let count = sec3 / DIR_ENTRY - 1;
    let mut blocks = Vec::with_capacity(count);
    let mut payloads = Vec::with_capacity(count);
    for i in 0..count {
        let (first, second) = entry(i);
        let (next_first, next_second) = entry(i + 1);
        let payload_len = (next_first & 0xFFF_FFFF_FFFF) - (first & 0xFFF_FFFF_FFFF);
        let logical_len = (next_second & 0xFFF_FFFF_FFFF) - (second & 0xFFF_FFFF_FFFF);
        let hint = ((second >> SIZE_HINT_SHIFT) & SIZE_HINT_MASK) + 1;
        let at = off7 + (first & 0xFFF_FFFF_FFFF) as usize;
        payloads.push(
            container[at.min(container.len())..(at + payload_len as usize).min(container.len())]
                .to_vec(),
        );
        blocks.push((first >> FLAG_SHIFT, hint, payload_len, logical_len));
    }

    let digest_ok = sec2 > 0
        && container[0x28..0x48]
            == file_digest(
                block_size,
                param0c,
                param10,
                logical_size,
                &container[off2..off2 + sec2],
                &container[off3..off3 + sec3],
                &container[off4..off4 + sec4],
            );

    Ok(Container {
        version,
        block_size,
        logical_size,
        all_stored: blocks.iter().all(|(f, _, _, _)| f & FLAG_RAW_COPY != 0),
        blocks,
        payloads,
        digest_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_container_round_trips_through_its_own_tables() {
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let c = write_stored(&payload);
        let parsed = parse(&c).unwrap();
        assert_eq!(parsed.version, 3);
        assert_eq!(parsed.block_size, BLOCK as u32);
        assert_eq!(parsed.logical_size, payload.len() as u64);
        assert!(parsed.all_stored);
        assert_eq!(parsed.blocks.len(), 1);
        assert!(parsed.digest_ok, "the header's file digest must reproduce");
        // One stored block is the payload itself.
        assert_eq!(parsed.payloads.concat(), payload);
    }

    #[test]
    fn a_multi_block_container_numbers_its_blocks() {
        let payload = vec![0xABu8; BLOCK * 2 + 17];
        let c = write_stored(&payload);
        let parsed = parse(&c).unwrap();
        assert_eq!(parsed.blocks.len(), 3);
        assert!(parsed.digest_ok);
        // Every block is the logical block over a 256 KiB stride; only the last is short.
        assert_eq!(parsed.blocks[0].2, BLOCK as u64);
        assert_eq!(parsed.blocks[1].2, BLOCK as u64);
        assert_eq!(parsed.blocks[2].2, 17);
        assert_eq!(parsed.blocks[0].3, BLOCK as u64);
        assert_eq!(parsed.blocks[2].3, 17);
        // A 256 KiB block carries two sub-chunks, so its hint is the 128 KiB first one.
        assert_eq!(parsed.blocks[0].1, SUB_CHUNK as u64);
        assert_eq!(parsed.blocks[2].1, 17);
        assert_eq!(parsed.payloads.concat(), payload);
    }

    #[test]
    fn a_small_payload_still_gets_one_block() {
        let c = write_stored(b"hello");
        let parsed = parse(&c).unwrap();
        assert_eq!(parsed.blocks.len(), 1);
        assert_eq!(parsed.logical_size, 5);
        assert!(parsed.digest_ok);
        assert_eq!(parsed.payloads.concat(), b"hello");
    }

    #[test]
    fn the_block_signature_is_stable_and_position_independent() {
        let a = block_signature(&vec![7u8; 1000]);
        let b = block_signature(&vec![7u8; 1000]);
        assert_eq!(a, b);
        let mut c = vec![7u8; 1000];
        c[999] = 8;
        assert_ne!(block_signature(&c), a);
    }
}
