//! `naps_pkg_layout.dat`: the map from the on-disk inner image to the mount.
//!
//! Layout (validated by parsing `webbrowser.pkg`'s 432-byte file exactly):
//!
//! * 16-byte header of two little-endian words —
//!   `{numFiles-1:24, compType:2, numKeys-1:2, numShuffle:4, numUBlocks:24}` and
//!   `{numOuterBlocks:24, numCblockInfo-2:24}`.
//! * Sections in fixed order with fixed strides: outer-block digests (8 B), shuffle
//!   patterns (8 B), `fidx` (6 B: 40-bit little-endian offset + a 1-byte type, the last
//!   entry the mount size with type `0x40`), `u2c` mapping (10 B: uint24 base plus seven
//!   per-ublock deltas), `cblockinfo` (9 B records, bit-packed).
//!
//! `cblockinfo` records are either run-base markers (`m_isRunBase` at bit 18, carrying the
//! AES-XTS tweak and key slot of an encrypted download run plus the base of its compressed
//! offset) or per-block records carrying the block's compressed offset, uncompressed offset,
//! first-sub-chunk compressed length, even/odd flags, the KDE predictor and a shuffle index.
//!
//! The header's `compType` is the codec for the whole image, and it carries the same algorithm ids
//! as the container header: 0 QuickZ, 1 Zlib, 2 Kraken. Each block record then says whether that
//! block carries a payload at all. The values below are the ones `webbrowser.pkg` uses, which is a
//! package that mounts: a block with a payload repeats the payload's length in `clen` with
//! `even`/`odd` clear, and a stored block leaves all three at zero — the console takes a stored
//! block's length from the ublock geometry and its bytes from the cursor.
//!
//! A stored image still carries a run schedule: a run opens at every file's start — the compressed
//! cursor re-bases there — and every eleventh 256 KiB block within a file, and the metadata region
//! opens one run on its first block. The tail closes with a terminator, without which the mount's
//! own walk has no end.

use crate::{format_err, Result, BLOCK};

/// The `u2c` stride.
const U2C_LEN: usize = 10;
/// The `fidx` stride.
const FIDX_LEN: usize = 6;
/// One `cblockinfo` record's stride.
const CBLOCK_LEN: usize = 9;
/// One outer-block digest's stride (the sample's are all zero).
const OUTER_DIGEST_LEN: usize = 8;
/// A NAPS ublock: 256 KiB.
pub const UBLOCK: u64 = 0x40000;
/// The type byte the final `fidx` entry (the mount size) carries.
const FIDX_TYPE_MOUNT_END: u8 = 0x40;
/// The KDE predictor a full stored block carries.
const KDE_STORED: u8 = 4;
/// The KDE predictor a partial stored block carries.
const KDE_STORED_PARTIAL: u8 = 0;
/// The KDE predictor a block that carries an encoded payload carries, measured on the sample's
/// records whose `clen` is non-zero. The reference calls this value "raw partial" and reserves
/// `2` for a compressed block; the sample disagrees, and the sample is the one that mounts.
const KDE_PAYLOAD: u8 = 0;
/// The `clen` field's width.
const CLEN_MAX: u64 = 0x1_FFFF;
/// The compression type the layout header records when a payload is a zlib stream. The field is
/// two bits and carries the same algorithm ids as the container header: 0 QuickZ, 1 Zlib,
/// 2 Kraken.
pub const COMP_ZLIB: u64 = 1;
/// The compression type a layout with no encoded payload carries.
pub const COMP_KRAKEN: u64 = 2;

/// One `cblockinfo` record, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cblock {
    /// A run base: re-anchors the compressed cursor and names the encryption slot.
    RunBase {
        coffset_end_mod_256k: u32,
        tweak: u32,
        key_slot: u8,
        coffset_start_256k: u32,
    },
    /// A per-block record.
    Block {
        coffset_start_mod_256k: u32,
        uoffset_start: u32,
        clen_even_minus1: u32,
        even: u8,
        odd: u8,
        kde: u8,
        shuffle: u8,
    },
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub num_files: u32,
    pub compression_type: u8,
    pub num_keys: u32,
    pub num_shuffle: u32,
    pub num_ublocks: u32,
    pub num_outer_blocks: u32,
    pub outer_digests: Vec<[u8; 8]>,
    pub shuffle_patterns: Vec<[u8; 8]>,
    /// `(offset, type)` per entry; the last entry is the mount size with type `0x40`.
    pub fidx: Vec<(u64, u8)>,
    /// `(base, deltas)` per entry, each covering eight ublocks.
    pub u2c: Vec<(u32, [u8; 7])>,
    pub cblocks: Vec<Cblock>,
}

impl Layout {
    /// The number of `u2c` entries the block count implies: one group per eight ublocks,
    /// plus a trailing group (measured on the sample: 19 ublocks carry four entries).
    pub fn u2c_count(num_ublocks: u32) -> usize {
        num_ublocks.div_ceil(8) as usize + 1
    }

    /// The mount size the layout's final `fidx` entry carries.
    pub fn mount_size(&self) -> u64 {
        self.fidx.last().map(|(o, _)| *o).unwrap_or(0)
    }
}

/// Parse a layout blob.
pub fn parse(blob: &[u8]) -> Result<Layout> {
    if blob.len() < 16 {
        return format_err("naps layout is shorter than its header");
    }
    let word0 = u64::from_le_bytes(blob[0..8].try_into().unwrap());
    let word1 = u64::from_le_bytes(blob[8..16].try_into().unwrap());
    let num_files = (word0 & 0xFF_FFFF) as u32 + 1;
    let compression_type = ((word0 >> 24) & 0x3) as u8;
    let num_keys = ((word0 >> 26) & 0x3) as u32 + 1;
    let num_shuffle = ((word0 >> 28) & 0xF) as u32;
    let num_ublocks = ((word0 >> 32) & 0xFF_FFFF) as u32;
    let num_outer_blocks = (word1 & 0xFF_FFFF) as u32;
    let num_cblock = ((word1 >> 24) & 0xFF_FFFF) as u32 + 2;

    let mut at = 16usize;
    let take = |at: &mut usize, len: usize, what: &str| -> Result<&[u8]> {
        let slice = blob
            .get(*at..*at + len)
            .ok_or_else(|| crate::Error::Format(format!("naps layout ends inside its {what}")))?;
        *at += len;
        Ok(slice)
    };

    let mut outer_digests = Vec::with_capacity(num_outer_blocks as usize);
    for _ in 0..num_outer_blocks {
        let raw = take(&mut at, OUTER_DIGEST_LEN, "outer digests")?;
        outer_digests.push(raw.try_into().unwrap());
    }
    let mut shuffle_patterns = Vec::with_capacity(num_shuffle as usize);
    for _ in 0..num_shuffle {
        let raw = take(&mut at, OUTER_DIGEST_LEN, "shuffle patterns")?;
        shuffle_patterns.push(raw.try_into().unwrap());
    }

    // The fidx count is package-dependent; the cblockinfo section is last, so what remains
    // before it (after the fixed strides) is the fidx section.
    let u2c_at =
        16 + num_outer_blocks as usize * OUTER_DIGEST_LEN + num_shuffle as usize * OUTER_DIGEST_LEN;
    let cblock_bytes = num_cblock as usize * CBLOCK_LEN;
    if blob.len() < u2c_at + cblock_bytes {
        return format_err("naps layout is shorter than its sections require");
    }
    let after_u2c = blob.len() - cblock_bytes;
    let fidx_bytes = after_u2c
        .checked_sub(u2c_at + Layout::u2c_count(num_ublocks) * U2C_LEN)
        .ok_or_else(|| {
            crate::Error::Format("naps layout has no room for its fidx section".into())
        })?;
    if fidx_bytes % FIDX_LEN != 0 {
        // The section widths this parser assumes are validated by `webbrowser.pkg` exactly, but
        // they do not all hold: PSVIETHOA's Minecraft (`naps_len` 769,240, 36,022 files, 6,348
        // ublocks, 46,584 cblocks) leaves 216,130 bytes here, which is not a whole number of
        // faces. Either the `u2c` count or its stride differs at that size, so treat this parser
        // as trustworthy for small descriptors and suspect for large ones.
        return format_err(format!(
            "naps fidx section is {fidx_bytes} bytes, not a multiple of 6 \
             (blob {} bytes, {num_files} files, {num_ublocks} ublocks, {num_cblock} cblocks, \
             {num_outer_blocks} outer blocks): the u2c count or stride differs from this parser's",
            blob.len()
        ));
    }

    let mut fidx = Vec::with_capacity(fidx_bytes / FIDX_LEN);
    for _ in 0..fidx_bytes / FIDX_LEN {
        let raw = take(&mut at, FIDX_LEN, "fidx")?;
        let offset = u64::from_le_bytes([raw[0], raw[1], raw[2], raw[3], raw[4], 0, 0, 0]);
        fidx.push((offset, raw[5]));
    }
    let mut u2c = Vec::with_capacity(Layout::u2c_count(num_ublocks));
    for _ in 0..Layout::u2c_count(num_ublocks) {
        let raw = take(&mut at, U2C_LEN, "u2c")?;
        let base = u32::from(raw[0]) | u32::from(raw[1]) << 8 | u32::from(raw[2]) << 16;
        let mut deltas = [0u8; 7];
        deltas.copy_from_slice(&raw[3..10]);
        u2c.push((base, deltas));
    }

    let mut cblocks = Vec::with_capacity(num_cblock as usize);
    for _ in 0..num_cblock {
        let raw = take(&mut at, CBLOCK_LEN, "cblockinfo")?;
        let mut lo = 0u64;
        for (i, b) in raw[..8].iter().enumerate() {
            lo |= u64::from(*b) << (8 * i);
        }
        let hi = u64::from(raw[8]);
        let coffset = (lo & 0x3_FFFF) as u32;
        if (lo >> 18) & 1 != 0 {
            cblocks.push(Cblock::RunBase {
                coffset_end_mod_256k: coffset,
                tweak: ((lo >> 19) & 0x0FFF_FFFF) as u32,
                key_slot: ((lo >> 47) & 0x3) as u8,
                coffset_start_256k: (((lo >> 49) & 0x7FFF) | ((hi & 0x1FF) << 15)) as u32,
            });
        } else {
            cblocks.push(Cblock::Block {
                coffset_start_mod_256k: coffset,
                uoffset_start: ((lo >> 19) & 0x3_FFFF) as u32,
                clen_even_minus1: ((lo >> 37) & 0x1_FFFF) as u32,
                even: ((lo >> 54) & 1) as u8,
                odd: ((lo >> 55) & 1) as u8,
                kde: ((lo >> 56) & 0x7) as u8,
                shuffle: ((lo >> 59) & 0xF) as u8,
            });
        }
    }

    Ok(Layout {
        num_files,
        compression_type,
        num_keys,
        num_shuffle,
        num_ublocks,
        num_outer_blocks,
        outer_digests,
        shuffle_patterns,
        fidx,
        u2c,
        cblocks,
    })
}

fn encode_cblock(c: &Cblock) -> [u8; CBLOCK_LEN] {
    let (lo, hi) = match c {
        Cblock::Block {
            coffset_start_mod_256k,
            uoffset_start,
            clen_even_minus1,
            even,
            odd,
            kde,
            shuffle,
        } => {
            let mut lo = u64::from(*coffset_start_mod_256k & 0x3_FFFF);
            lo |= u64::from(*uoffset_start & 0x3_FFFF) << 19;
            lo |= u64::from(*clen_even_minus1 & 0x1_FFFF) << 37;
            lo |= u64::from(*even & 1) << 54;
            lo |= u64::from(*odd & 1) << 55;
            lo |= u64::from(*kde & 0x7) << 56;
            lo |= u64::from(*shuffle & 0xF) << 59;
            (lo, 0u64)
        }
        Cblock::RunBase {
            coffset_end_mod_256k,
            tweak,
            key_slot,
            coffset_start_256k,
        } => {
            let mut lo = u64::from(*coffset_end_mod_256k & 0x3_FFFF);
            lo |= 1 << 18;
            lo |= u64::from(*tweak & 0x0FFF_FFFF) << 19;
            lo |= u64::from(*key_slot & 0x3) << 47;
            lo |= u64::from(*coffset_start_256k & 0x7FFF) << 49;
            let hi = u64::from((*coffset_start_256k >> 15) & 0x1FF);
            (lo, hi)
        }
    };
    let mut out = [0u8; CBLOCK_LEN];
    for (i, b) in out[..8].iter_mut().enumerate() {
        *b = (lo >> (8 * i)) as u8;
    }
    out[8] = hi as u8;
    out
}

/// One planned cblockinfo record, before the cursor walk serializes it: the per-block plan the
/// compressor would produce, plus whether the block opens a run.
///
/// `clen` is the record's own `ClenEvenMinus1` field — the payload's byte count for a block that
/// carries one, and zero for a stored block. A stored block's `clen`, `even` and `odd` are all
/// zero in the sample; the console takes a stored block's length from the ublock geometry and its
/// bytes from the cursor, so nothing else reads them.
struct Plan {
    start_run: bool,
    on_disk: u64,
    logical: u64,
    clen: u64,
    stream_len: u64,
    even: u8,
    odd: u8,
    kde: u8,
    shuffle: u8,
    terminator: bool,
}

/// A stored block's record: no payload, no sub-chunk flags, the stored predictor.
fn stored(start_run: bool, on_disk: u64, logical: u64, stream_len: u64, full: bool) -> Plan {
    Plan {
        start_run,
        on_disk,
        logical,
        clen: 0,
        stream_len,
        even: 0,
        odd: 0,
        kde: if full { KDE_STORED } else { KDE_STORED_PARTIAL },
        shuffle: 0,
        terminator: false,
    }
}

/// Walk a plan into cblockinfo records, and report `(record index, logical offset)` for every
/// per-block record. A run-base record re-anchors the compressed-offset cursor to the block's
/// position in the doubled offset space; each per-block record then advances it by its stream
/// length. This is what keeps a block's recorded offset meaningful once runs are in play.
fn walk(plans: &[Plan]) -> (Vec<Cblock>, Vec<(u32, u64)>) {
    let mut entries: Vec<Cblock> = Vec::with_capacity(plans.len() + plans.len() / 8 + 1);
    let mut by_std: Vec<(u32, u64)> = Vec::with_capacity(plans.len());
    let mut cursor: u64 = 0;
    for p in plans {
        if p.start_run {
            let coffset_end_mod_256k = (cursor & 0x3_FFFF) as u32;
            cursor = 2 * (p.on_disk / UBLOCK) * UBLOCK + p.on_disk % UBLOCK;
            entries.push(Cblock::RunBase {
                coffset_end_mod_256k,
                tweak: ((p.on_disk >> 15) as u32) & 0x0FFF_FFFF,
                key_slot: 0,
                coffset_start_256k: ((2 * (p.on_disk / UBLOCK)) as u32) & 0x7FFF,
            });
        }
        let coffset_start_mod_256k = (cursor & 0x3_FFFF) as u32;
        let (uoffset_start, clen_even_minus1, even, odd, kde, shuffle) = if p.terminator {
            (1, 1, 0, 0, 0, 0)
        } else {
            (
                (((p.logical & 0x3_FFFF) * 2) & 0x3_FFFF) as u32,
                p.clen.min(CLEN_MAX) as u32,
                p.even,
                p.odd,
                p.kde,
                p.shuffle,
            )
        };
        by_std.push((entries.len() as u32, p.logical));
        entries.push(Cblock::Block {
            coffset_start_mod_256k,
            uoffset_start,
            clen_even_minus1,
            even,
            odd,
            kde,
            shuffle,
        });
        cursor += p.stream_len;
    }
    (entries, by_std)
}

/// Build a layout for a stored inner image: every file is raw, so each splits into full 256 KiB
/// blocks and a tail, and a run opens at every file's start — the compressed cursor re-bases
/// there — plus every eleventh block within a file, the point the offset space re-bases at.
pub fn build(
    image_len: u64,
    ndblock: u64,
    afid_offsets: &[u64],
    data_end: u64,
    meta_base: u64,
) -> Result<Vec<u8>> {
    build_with_meta(
        image_len,
        ndblock,
        afid_offsets,
        data_end,
        meta_base,
        &[],
        COMP_KRAKEN,
    )
}

/// Build a layout for an image whose metadata region carries encoded payloads.
///
/// `meta_blocks` is the container's own per-block geometry, one entry per 256 KiB of the metadata
/// region, with `payload_at` relative to the container's start — which sits at `meta_base`. A
/// block that carries a payload records its length and the payload predictor; one that does not is
/// a stored block like any other. `compression_type` goes to the header, where the console reads
/// the codec every payload in the image uses.
pub fn build_with_meta(
    image_len: u64,
    ndblock: u64,
    afid_offsets: &[u64],
    data_end: u64,
    meta_base: u64,
    meta_blocks: &[crate::pfsc::BlockInfo],
    compression_type: u64,
) -> Result<Vec<u8>> {
    if image_len == 0 || ndblock == 0 || !meta_base.is_multiple_of(UBLOCK) {
        return format_err("naps needs a non-empty image with 256 KiB-aligned metadata");
    }
    let mount_size = ndblock * BLOCK;
    let num_ublocks = mount_size.div_ceil(UBLOCK) as u32;
    let num_outer_blocks = image_len.div_ceil(BLOCK) as u32;
    let num_files = afid_offsets.len() as u32 + 3;
    if !meta_blocks.is_empty()
        && meta_blocks.len() as u64 != mount_size.saturating_sub(meta_base).div_ceil(UBLOCK)
    {
        return format_err(format!(
            "the metadata plan has {} blocks for a region of {}",
            meta_blocks.len(),
            mount_size.saturating_sub(meta_base).div_ceil(UBLOCK)
        ));
    }

    // The data region: one placement per file, each split the way the mount reads it back.
    let mut plans: Vec<Plan> = Vec::new();
    for (i, &start) in afid_offsets.iter().enumerate() {
        let end = afid_offsets.get(i + 1).copied().unwrap_or(data_end);
        let size = end.saturating_sub(start);
        let full = size / UBLOCK;
        let tail = size - full * UBLOCK;
        let mut runs: Vec<u64> = vec![start];
        // A raw file's compressed cursor re-bases every eleventh 256 KiB block.
        let mut m = 11;
        while m < full {
            runs.push(start + m * UBLOCK);
            m += 11;
        }
        for k in 0..full {
            let on_disk = start + k * UBLOCK;
            plans.push(stored(
                runs.contains(&on_disk),
                on_disk,
                on_disk,
                0x8_0000,
                true,
            ));
        }
        if tail > 0 || full == 0 {
            let on_disk = start + full * UBLOCK;
            plans.push(stored(
                runs.contains(&on_disk),
                on_disk,
                on_disk,
                tail,
                false,
            ));
        }
    }

    // The tail: padding over the gap between the data and the metadata, then the metadata's own
    // blocks — which open a run on the first one only — and a terminator marking the mount end.
    let padding = data_end & !(UBLOCK - 1);
    plans.push(stored(false, data_end, padding, 0x10, true));
    let meta_ublocks = mount_size.saturating_sub(meta_base).div_ceil(UBLOCK);
    if meta_blocks.is_empty() {
        // Nothing encoded: the region sits on disk at its own logical offsets.
        for i in 0..meta_ublocks {
            let logical = meta_base + i * UBLOCK;
            let len = UBLOCK.min(mount_size - logical);
            plans.push(stored(i == 0, meta_base + i * UBLOCK, logical, len, true));
        }
    } else {
        // Each block's payload sits where the container put it; the ones that did not compress are
        // stored blocks with nothing for the record to describe.
        for (i, b) in meta_blocks.iter().enumerate() {
            let logical = meta_base + i as u64 * UBLOCK;
            let mut p = stored(
                i == 0,
                meta_base + b.payload_at,
                logical,
                b.payload_len,
                true,
            );
            if b.compressed {
                p.clen = b.payload_len;
                p.kde = KDE_PAYLOAD;
            }
            plans.push(p);
        }
    }
    let meta_end = meta_base + meta_ublocks * UBLOCK;
    plans.push(Plan {
        start_run: true,
        on_disk: meta_end,
        logical: mount_size,
        clen: 0,
        stream_len: 0,
        even: 0,
        odd: 0,
        kde: 0,
        shuffle: 0,
        terminator: true,
    });

    let (cblocks, by_std) = walk(&plans);
    let num_cblock = cblocks.len() as u32;

    // u2c: per ublock, the index of the first per-block record at or past it, as a base plus
    // seven deltas per group of eight.
    let mut sorted = by_std.clone();
    sorted.sort_by_key(|(_, logical)| *logical);
    let terminator = num_cblock - 1;
    let mut first: Vec<u32> = Vec::with_capacity(num_ublocks as usize);
    let mut p = 0usize;
    for u in 0..num_ublocks {
        let target = u64::from(u) * UBLOCK;
        while p < sorted.len() && sorted[p].1 < target {
            p += 1;
        }
        first.push(if p < sorted.len() {
            sorted[p].0
        } else {
            terminator
        });
    }
    let u2c: Vec<(u32, [u8; 7])> = (0..Layout::u2c_count(num_ublocks))
        .map(|g| {
            let base = *first.get(g * 8).unwrap_or(&terminator);
            let mut deltas = [0u8; 7];
            for (j, d) in deltas.iter_mut().enumerate() {
                let v = *first.get(g * 8 + 1 + j).unwrap_or(&terminator);
                *d = v.saturating_sub(base).min(u32::from(u8::MAX)) as u8;
            }
            (base, deltas)
        })
        .collect();

    // fidx: the afid offsets, then the data end, the metadata base and the mount size.
    let mut fidx: Vec<(u64, u8)> = afid_offsets.iter().map(|o| (*o, 0u8)).collect();
    fidx.push((data_end, 0));
    fidx.push((meta_base, 0));
    fidx.push((mount_size, FIDX_TYPE_MOUNT_END));

    let mut blob: Vec<u8> = Vec::with_capacity(
        16 + fidx.len() * FIDX_LEN + u2c.len() * U2C_LEN + cblocks.len() * CBLOCK_LEN,
    );
    // The codec every payload in the image was encoded with, one key, no shuffle patterns.
    let word0 = u64::from(num_files - 1) & 0xFF_FFFF
        | (compression_type & 0x3) << 24
        | (u64::from(num_ublocks) & 0xFF_FFFF) << 32;
    let word1 =
        u64::from(num_outer_blocks) & 0xFF_FFFF | (u64::from(num_cblock - 2) & 0xFF_FFFF) << 24;
    blob.extend_from_slice(&word0.to_le_bytes());
    blob.extend_from_slice(&word1.to_le_bytes());
    for _ in 0..num_outer_blocks {
        blob.extend_from_slice(&[0u8; OUTER_DIGEST_LEN]);
    }
    for (offset, kind) in &fidx {
        let bytes = offset.to_le_bytes();
        blob.extend_from_slice(&bytes[..5]);
        blob.push(*kind);
    }
    for (base, deltas) in &u2c {
        blob.extend_from_slice(&base.to_le_bytes()[..3]);
        blob.extend_from_slice(deltas);
    }
    for c in &cblocks {
        blob.extend_from_slice(&encode_cblock(c));
    }
    Ok(blob)
}

/// Rebuild the mount from an inner image and its layout.
///
/// The data region and the block-info table sit at their own logical offsets, so they copy
/// straight across. The metadata region does not: it is a container at `meta_base`, and it expands
/// to the rest of the mount. A container that does not expand to exactly the region the layout
/// describes is an error, which is what ties the image's tail to the mount's.
pub fn reconstruct(image: &[u8], layout: &Layout) -> Result<Vec<u8>> {
    let mount_size = layout.mount_size();
    if mount_size == 0 || mount_size > 1 << 40 {
        return format_err(format!("implausible mount size {mount_size:#x}"));
    }
    // The last three fidx faces are the data's end, the metadata base and the mount size.
    if layout.fidx.len() < 3 {
        return format_err("naps has no data-end / metadata-base fidx faces");
    }
    let base_at = layout.fidx.len() - 2;
    let data_end = layout.fidx[base_at - 1].0;
    let meta_base = layout.fidx[base_at].0;

    let mut mount = vec![0u8; mount_size as usize];
    let copied = data_end.min(image.len() as u64).min(meta_base) as usize;
    mount[..copied].copy_from_slice(&image[..copied]);

    let container = image
        .get(meta_base as usize..)
        .ok_or_else(|| crate::Error::Format("the image ends before its metadata base".into()))?;
    let plain = crate::pfsc::parse(container)?.decompress()?;
    let region = mount_size - meta_base;
    if plain.len() as u64 != region {
        return format_err(format!(
            "the metadata container expands to {} bytes where the layout says {region}",
            plain.len()
        ));
    }
    mount[meta_base as usize..].copy_from_slice(&plain);
    Ok(mount)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull one file out of a package's outer PFS, through the reader's own decryption. Each
    /// call gets its own temp file: the removal below would otherwise pull the file out from
    /// under a test running beside this one on the same sample.
    fn outer_file(pkg: &[u8], name: &str) -> Option<Vec<u8>> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "naps-sample-{}-{}-{}.pkg",
            std::process::id(),
            name.replace(['/', '.'], "_"),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, pkg).ok()?;
        let mut file = crate::PkgFile::open(&path).ok()?;
        let head = file.read_at(0, crate::fih::HEADER_LEN).ok()?;
        let fih = crate::fih::parse(&head).ok()?;
        let cnt = crate::cnt::read(&mut file, fih.cnt_offset).ok()?;
        let img =
            crate::outer::open(&mut file, &fih, &cnt, crate::crypto::DEFAULT_PASSCODE).ok()?;
        std::fs::remove_file(&path).ok();

        let nodes = img.dinodes();
        let uroot = nodes.get(2)?;
        let ino = img.dirents(uroot).into_iter().find(|d| d.name == name)?.ino as usize;
        let node = nodes.get(ino)?;
        let mut out = Vec::new();
        for d in node.direct.iter().take(node.blocks.min(12) as usize) {
            out.extend_from_slice(img.plaintext.get(d.block as usize)?);
        }
        out.truncate(node.size as usize);
        Some(out)
    }

    /// The real sample's layout: 432 bytes, five outer blocks, nineteen ublocks, eight fidx
    /// entries, four u2c entries and thirty-two cblockinfo records.
    #[test]
    fn parses_the_samples_layout() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("webbrowser.pkg");
        let Ok(pkg) = std::fs::read(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let blob = outer_file(&pkg, "naps_pkg_layout.dat").unwrap();
        assert_eq!(blob.len(), 432);
        let layout = parse(&blob).unwrap();
        assert_eq!(layout.num_files, 8);
        assert_eq!(layout.compression_type, 2);
        assert_eq!(layout.num_keys, 1);
        assert_eq!(layout.num_shuffle, 0);
        assert_eq!(layout.num_ublocks, 19);
        assert_eq!(layout.num_outer_blocks, 5);
        assert_eq!(layout.cblocks.len(), 32);
        assert_eq!(layout.u2c.len(), 4);
        let offsets: Vec<u64> = layout.fidx.iter().map(|(o, _)| *o).collect();
        assert_eq!(
            offsets,
            vec![0, 0x60, 0x3230, 0x323a, 0x6c98, 0xa626, 0x400000, 0x4a0000]
        );
        assert_eq!(layout.fidx.last().unwrap().1, 0x40);
        assert_eq!(layout.mount_size(), 0x4a0000);
    }

    /// The run schedule is the part of a stored image this writer used to omit entirely: without
    /// it the mount's walk has no anchors and no end.
    #[test]
    fn a_stored_image_carries_its_run_schedule() {
        let ndblock = 40u64;
        let mount_size = ndblock * BLOCK;
        let blob = build(mount_size, ndblock, &[0, 96, 12848], 0xa626, 0x80000).unwrap();
        let layout = parse(&blob).unwrap();

        // A file opens a run, so the first record is a run-base at the data's start.
        assert!(
            matches!(layout.cblocks.first(), Some(Cblock::RunBase { .. })),
            "the first file's block must open a run: {:?}",
            layout.cblocks.first()
        );

        // The metadata opens exactly one run, on its first block.
        let meta_run = layout
            .cblocks
            .iter()
            .filter(|c| matches!(c, Cblock::RunBase { .. }))
            .count();
        assert!(
            meta_run >= 2,
            "each file and the metadata open runs, saw {meta_run}"
        );

        // The terminator closes the layout: its per-block record carries the sentinel fields.
        match layout.cblocks.last() {
            Some(Cblock::Block {
                uoffset_start,
                clen_even_minus1,
                even,
                odd,
                ..
            }) => {
                assert_eq!(*uoffset_start, 1, "the terminator's uoffset");
                assert_eq!(*clen_even_minus1, 1, "the terminator's clen");
                assert_eq!((*even, *odd), (0, 0), "the terminator's flags");
            }
            other => panic!("the layout must end with the terminator record: {other:?}"),
        }
    }

    #[test]
    fn probe_sample_u2c() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("webbrowser.pkg");
        let Ok(pkg) = std::fs::read(&path) else {
            return;
        };
        let blob = outer_file(&pkg, "naps_pkg_layout.dat").unwrap();
        let layout = parse(&blob).unwrap();
        eprintln!("cblocks={} u2c={}", layout.cblocks.len(), layout.u2c.len());
        let mut starts = Vec::new();
        for (base, deltas) in &layout.u2c {
            starts.push(*base);
            for d in deltas {
                starts.push(*base + u32::from(*d));
            }
        }
        eprintln!("decoded table ({} entries): {:?}", starts.len(), starts);
        let mono = starts.windows(2).all(|w| w[0] <= w[1]);
        eprintln!("monotone: {mono}");
        let cb_start = blob.len() - 32 * 9;
        for i in 0..6 {
            let r = &blob[cb_start + i * 9..cb_start + i * 9 + 9];
            eprintln!(
                "  raw {i}: {}",
                r.iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
    }

    /// A metadata region that carries payloads: the descriptor's records describe the container's
    /// blocks, and the image reconstructs to the mount.
    #[test]
    fn builds_and_reconstructs_an_image_with_a_compressed_metadata_region() {
        let ndblock = 40u64;
        let meta_base = 0x80000u64;
        let mount_size = ndblock * BLOCK;
        let region_len = mount_size - meta_base;
        // A metadata region that deflates: a sparse structure with a repeating shape.
        let plain: Vec<u8> = (0..region_len)
            .map(|i| {
                if i % 0xA8 < 0x20 {
                    (i / 0xA8 % 7) as u8
                } else {
                    0
                }
            })
            .collect();
        // The block-info table's block, then the container at the metadata base.
        let mut image: Vec<u8> = (0..meta_base).map(|i| (i % 253) as u8).collect();
        let written = crate::pfsc::write_zlib(&plain).unwrap();
        image.extend_from_slice(&written.container);
        image.resize(image.len().div_ceil(BLOCK as usize) * BLOCK as usize, 0);

        let blob = build_with_meta(
            image.len() as u64,
            ndblock,
            &[0, 96, 12848],
            0xa626,
            meta_base,
            &written.blocks,
            COMP_ZLIB,
        )
        .unwrap();
        let layout = parse(&blob).unwrap();
        assert_eq!(layout.num_files, 6);
        assert_eq!(layout.mount_size(), mount_size);
        assert_eq!(layout.compression_type, COMP_ZLIB as u8);
        // The image is shorter than the mount, so the descriptor must say so.
        assert!(image.len() < mount_size as usize);

        // The metadata's records carry a payload length where the block deflated and nothing
        // where it did not, and no record claims a sub-chunk split.
        let meta_records: Vec<_> = written
            .blocks
            .iter()
            .map(|b| {
                if b.compressed {
                    (b.payload_len, KDE_PAYLOAD)
                } else {
                    (0, KDE_STORED)
                }
            })
            .collect();
        let got: Vec<(u64, u8)> = layout
            .cblocks
            .iter()
            .filter_map(|c| match c {
                Cblock::Block {
                    clen_even_minus1,
                    kde,
                    even,
                    odd,
                    ..
                } if *kde == KDE_PAYLOAD || *kde == KDE_STORED => {
                    assert_eq!(
                        (*even, *odd),
                        (0, 0),
                        "no record may claim a sub-chunk split"
                    );
                    Some((u64::from(*clen_even_minus1), *kde))
                }
                _ => None,
            })
            .collect();
        for want in &meta_records {
            assert!(
                got.contains(want),
                "the descriptor has no record for metadata block {want:?}"
            );
        }
        assert!(
            meta_records.iter().any(|(clen, _)| *clen > 0),
            "the fixture's metadata must actually deflate for this test to mean anything"
        );

        let rebuilt = reconstruct(&image, &layout).unwrap();
        assert_eq!(rebuilt.len(), mount_size as usize);
        // The data region copies straight across; the gap between it and the metadata base is
        // padding the image never stores, and the metadata region expands out of its container.
        assert_eq!(&rebuilt[..0xa626], &image[..0xa626]);
        assert!(rebuilt[0xa626..meta_base as usize].iter().all(|b| *b == 0));
        assert_eq!(&rebuilt[meta_base as usize..], &plain[..]);
    }

    /// A stored block's record describes nothing: the sample's records carry `clen = 0` and both
    /// sub-chunk flags clear, and the console takes a stored block's bytes from the cursor.
    #[test]
    fn a_stored_block_records_no_payload() {
        let ndblock = 40u64;
        let mount_size = ndblock * BLOCK;
        let meta_base = 0x80000u64;
        // A counter over a wide stride: deflate finds nothing to match in this, so every block
        // stays stored and the records have nothing to describe.
        let mut plain: Vec<u8> = Vec::new();
        let mut state = 0x243F_6A88_85A3_08D3u64;
        while (plain.len() as u64) < mount_size - meta_base {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            plain.extend_from_slice(&state.to_le_bytes());
        }
        let written = crate::pfsc::write_zlib(&plain).unwrap();
        assert!(
            written.blocks.iter().all(|b| !b.compressed),
            "all-zero blocks must stay stored"
        );
        let mut image: Vec<u8> = (0..meta_base).map(|i| (i % 253) as u8).collect();
        image.extend_from_slice(&written.container);
        image.resize(image.len().div_ceil(BLOCK as usize) * BLOCK as usize, 0);

        let blob = build_with_meta(
            image.len() as u64,
            ndblock,
            &[0, 96, 12848],
            0xa626,
            meta_base,
            &written.blocks,
            COMP_KRAKEN,
        )
        .unwrap();
        let layout = parse(&blob).unwrap();
        assert_eq!(layout.compression_type, COMP_KRAKEN as u8);
        // The terminator is a sentinel, not a stored block: its fields are deliberately non-zero.
        let blocks = &layout.cblocks[..layout.cblocks.len() - 1];
        for c in blocks {
            if let Cblock::Block {
                clen_even_minus1,
                even,
                odd,
                kde,
                ..
            } = c
            {
                if *kde == KDE_STORED || *kde == KDE_STORED_PARTIAL {
                    assert_eq!(
                        *clen_even_minus1, 0,
                        "a stored record must carry no payload"
                    );
                    assert_eq!((*even, *odd), (0, 0), "and no sub-chunk flags");
                }
            }
        }
    }
}
