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
//! Every field of a record is written: the record packs all 72 bits, and a codec that leaves the
//! top nine at zero cannot reproduce a real descriptor's records — which is how the layout here
//! first went wrong, with the run-base window base truncated to 15 bits and the ninth byte, which
//! carries the high `kde` bits and the shuffle index, always zero.
//!
//! The header's `compType` is the codec for the whole image, and it carries the same algorithm ids
//! as the container header: 0 QuickZ, 1 Zlib, 2 Kraken. Each block record then says whether that
//! block carries a payload at all. A raw record carries one even chunk (`Even = 1`, with
//! `clen = min(len, 128 KiB) - 1`) and an odd chunk only when the unit is larger than the even
//! chunk, which is what the engine's raw-block path emits.
//!
//! A stored image still carries a run schedule: a run opens at every file's start — the compressed
//! cursor re-bases there — and `walk` opens one at every 16-record window, which the engine asserts
//! and without which a window has no anchor. The tail closes with a terminator, marked by bit 19 on
//! its record, without which the mount's own walk has no end.

use crate::{format_err, Result, BLOCK};

/// The `u2c` stride.
const U2C_LEN: usize = 10;
/// The `fidx` stride.
const FIDX_LEN: usize = 6;
/// One `cblockinfo` record's stride.
pub const CBLOCK_LEN: usize = 9;
/// One outer-block digest's stride (the sample's are all zero).
const OUTER_DIGEST_LEN: usize = 8;
/// A NAPS ublock: 256 KiB.
pub const UBLOCK: u64 = 0x40000;
/// The type byte the final `fidx` entry (the mount size) carries.
const FIDX_TYPE_MOUNT_END: u8 = 0x40;
/// The KDE predictor every raw record carries. The engine's verified profile uses zero for both
/// the full and the partial raw block, so one constant covers the whole stored image.
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
        /// Bit 19: the terminal/sentinel marker, set on the record that ends the stream.
        reserved19: bool,
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
        cblocks.push(decode_cblock(raw.try_into().unwrap()));
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

/// One `cblockinfo` record decoded from the 9 bytes the descriptor stores for it.
///
/// Public so a probe can read a descriptor whose other sections it cannot parse — the `fidx`
/// stride is not known for every real package, but this section always ends the blob.
pub fn decode_cblock(raw: &[u8; CBLOCK_LEN]) -> Cblock {
    let mut v = 0u128;
    for (i, b) in raw.iter().enumerate() {
        v |= u128::from(*b) << (8 * i);
    }
    let field = |lo: u32, hi: u32| ((v >> lo) & ((1u128 << (hi - lo + 1)) - 1)) as u32;
    let coffset = field(0, 17);
    if (v >> 18) & 1 != 0 {
        Cblock::RunBase {
            coffset_end_mod_256k: coffset,
            tweak: field(19, 46),
            key_slot: field(47, 49) as u8,
            coffset_start_256k: field(50, 71),
        }
    } else {
        Cblock::Block {
            coffset_start_mod_256k: coffset,
            reserved19: (v >> 19) & 1 != 0,
            uoffset_start: field(20, 37),
            clen_even_minus1: field(38, 54),
            even: field(55, 57) as u8,
            odd: field(58, 60) as u8,
            kde: field(61, 66) as u8,
            shuffle: field(68, 71) as u8,
        }
    }
}

fn encode_cblock(c: &Cblock) -> [u8; CBLOCK_LEN] {
    let mut v = 0u128;
    match c {
        Cblock::Block {
            coffset_start_mod_256k,
            reserved19,
            uoffset_start,
            clen_even_minus1,
            even,
            odd,
            kde,
            shuffle,
        } => {
            v |= u128::from(*coffset_start_mod_256k & 0x3_FFFF);
            v |= u128::from(*reserved19 as u8) << 19;
            v |= u128::from(*uoffset_start & 0x3_FFFF) << 20;
            v |= u128::from(*clen_even_minus1 & 0x1_FFFF) << 38;
            v |= u128::from(*even & 0x7) << 55;
            v |= u128::from(*odd & 0x7) << 58;
            v |= u128::from(*kde & 0x3F) << 61;
            v |= u128::from(*shuffle & 0xF) << 68;
        }
        Cblock::RunBase {
            coffset_end_mod_256k,
            tweak,
            key_slot,
            coffset_start_256k,
        } => {
            v |= u128::from(*coffset_end_mod_256k & 0x3_FFFF);
            v |= 1u128 << 18;
            v |= u128::from(*tweak & 0x0FFF_FFFF) << 19;
            v |= u128::from(*key_slot & 0x7) << 47;
            v |= u128::from(*coffset_start_256k & 0x3F_FFFF) << 50;
        }
    }
    let mut out = [0u8; CBLOCK_LEN];
    for (i, b) in out.iter_mut().enumerate() {
        *b = (v >> (8 * i)) as u8;
    }
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
    even_chunk_len: u64,
    stream_len: u64,
    even: u8,
    odd: u8,
    kde: u8,
    shuffle: u8,
    terminator: bool,
}

/// A stored block's plan: a raw 256 KiB unit carries one 128 KiB even chunk, and the cursor
/// advances by the unit's own length. A tail below 128 KiB has no odd chunk at all.
fn stored(start_run: bool, on_disk: u64, logical: u64, stream_len: u64) -> Plan {
    Plan {
        start_run,
        on_disk,
        logical,
        even_chunk_len: stream_len.min(0x2_0000),
        stream_len,
        even: 1,
        odd: u8::from(stream_len > 0x2_0000),
        kde: KDE_PAYLOAD,
        shuffle: 0,
        terminator: false,
    }
}

/// True when a run base has to be inserted ahead of `at`, because skipping it would leave a
/// 16-record window whose first record is not one. The scan stops as soon as the counter reaches
/// a window boundary, where the caller emits a run base anyway.
fn needs_cursor_preserving_run(plans: &[Plan], at: usize, counter: usize) -> bool {
    let mut counter = counter;
    let mut i = at;
    while i < plans.len() {
        if counter.is_multiple_of(16) {
            return false;
        }
        if plans[i].start_run && (counter + 1).is_multiple_of(16) {
            return true;
        }
        counter += 1;
        i += 1;
    }
    false
}

/// Walk a plan into cblockinfo records, and report `(record index, logical offset)` for every
/// per-block record.
///
/// A run base re-anchors the compressed-offset cursor: the record it writes names the window the
/// cursor sits in, and the cursor then restarts at that block's own on-disk offset. Between run
/// bases each block advances the cursor by its stream length. A run base opens every 16-record
/// window, so the window's first record always carries a fresh anchor.
fn walk(plans: &[Plan]) -> (Vec<Cblock>, Vec<(u32, u64)>) {
    let mut entries: Vec<Cblock> = Vec::with_capacity(plans.len() * 2);
    let mut by_std: Vec<(u32, u64)> = Vec::with_capacity(plans.len());
    let mut cursor: u64 = 0;

    for (i, p) in plans.iter().enumerate() {
        if !p.start_run && needs_cursor_preserving_run(plans, i, entries.len()) {
            entries.push(Cblock::RunBase {
                coffset_end_mod_256k: (cursor & 0x3_FFFF) as u32,
                tweak: (cursor >> 15) as u32,
                key_slot: 0,
                coffset_start_256k: (cursor / UBLOCK) as u32,
            });
        }
        // The first record needs no anchor of its own: the cursor already starts at the first
        // block's offset, which is why a real descriptor opens with a block, not a run base.
        if p.start_run || (!entries.is_empty() && entries.len().is_multiple_of(16)) {
            let end = (cursor & 0x3_FFFF) as u32;
            let offset = if p.start_run { p.on_disk } else { cursor };
            cursor = offset;
            entries.push(Cblock::RunBase {
                coffset_end_mod_256k: end,
                tweak: if p.terminator {
                    0
                } else {
                    (offset >> 15) as u32
                },
                key_slot: 0,
                coffset_start_256k: (offset / UBLOCK) as u32,
            });
        }

        let clen_even_minus1 = if p.terminator {
            0
        } else {
            p.even_chunk_len.saturating_sub(1).min(CLEN_MAX) as u32
        };
        by_std.push((entries.len() as u32, p.logical));
        entries.push(Cblock::Block {
            coffset_start_mod_256k: (cursor & 0x3_FFFF) as u32,
            reserved19: p.terminator,
            uoffset_start: (p.logical & 0x3_FFFF) as u32,
            clen_even_minus1,
            even: p.even,
            odd: p.odd,
            kde: p.kde,
            shuffle: p.shuffle,
        });
        cursor += p.stream_len;
    }
    (entries, by_std)
}

/// Build a layout for a stored inner image: every file is raw, so each splits into full 256 KiB
/// blocks and a tail, and a run opens wherever the physical offset stops being contiguous — the
/// first block of every file but the first — plus wherever a 16-record window needs one.
pub fn build(
    image_len: u64,
    ndblock: u64,
    files: &[(u64, u64, u64)],
    data_end: u64,
    meta_base: u64,
) -> Result<Vec<u8>> {
    build_with_meta(
        image_len,
        ndblock,
        files,
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
    files: &[(u64, u64, u64)],
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
    let num_files = files.len() as u32 + 3;
    if !meta_blocks.is_empty()
        && meta_blocks.len() as u64 != mount_size.saturating_sub(meta_base).div_ceil(UBLOCK)
    {
        return format_err(format!(
            "the metadata plan has {} blocks for a region of {}",
            meta_blocks.len(),
            mount_size.saturating_sub(meta_base).div_ceil(UBLOCK)
        ));
    }

    // The data region: one placement per file, each split the way the mount reads it back. A run
    // opens wherever the physical offset stops being the previous block's end — which is every
    // file's first block except the very first, since each file gets a boundary of its own.
    let mut plans: Vec<Plan> = Vec::new();
    let mut expected: u64 = 0;
    for &(logical, on_disk, size) in files {
        let full = size / UBLOCK;
        let tail = size - full * UBLOCK;
        for k in 0..full {
            let (d, l) = (on_disk + k * UBLOCK, logical + k * UBLOCK);
            plans.push(stored(d != expected, d, l, UBLOCK));
            expected = d + UBLOCK;
        }
        if tail > 0 || full == 0 {
            let (d, l) = (on_disk + full * UBLOCK, logical + full * UBLOCK);
            plans.push(stored(d != expected, d, l, tail));
            expected = d + tail;
        }
    }

    // The tail: padding over the gap between the data and the metadata, then the metadata's own
    // blocks — which open a run on the first one only — and a terminator marking the mount end.
    let padding = data_end & !(UBLOCK - 1);
    plans.push(stored(false, data_end, padding, 0x10));
    let meta_ublocks = mount_size.saturating_sub(meta_base).div_ceil(UBLOCK);
    if meta_blocks.is_empty() {
        // Nothing encoded: the region sits on disk at its own logical offsets.
        for i in 0..meta_ublocks {
            let logical = meta_base + i * UBLOCK;
            let len = UBLOCK.min(mount_size - logical);
            plans.push(stored(i == 0, meta_base + i * UBLOCK, logical, len));
        }
    } else {
        // Each block's payload sits where the container put it; the ones that did not compress are
        // stored blocks with nothing for the record to describe.
        for (i, b) in meta_blocks.iter().enumerate() {
            let logical = meta_base + i as u64 * UBLOCK;
            let mut p = stored(i == 0, meta_base + b.payload_at, logical, b.payload_len);
            if b.compressed {
                p.even_chunk_len = b.payload_len;
            }
            plans.push(p);
        }
    }
    let meta_end = meta_base + meta_ublocks * UBLOCK;
    plans.push(Plan {
        start_run: true,
        on_disk: meta_end,
        logical: mount_size,
        even_chunk_len: 0,
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
    let mut fidx: Vec<(u64, u8)> = files
        .iter()
        .map(|&(logical, _, _)| (logical, 0u8))
        .collect();
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
/// The files are laid out by the plan — `files` is `(logical, on_disk, size)` in afid order, the
/// same triple the descriptor is built from — because the image stores each file at its own
/// physical offset while the mount packs them contiguously. Everything after the data is a
/// container at `meta_base` that expands to the rest of the mount; one that does not expand to
/// exactly the region the layout describes is an error, which is what ties the image's tail to
/// the mount's.
///
/// The placements should come out of the descriptor itself: its records carry a block's logical
/// and physical offsets, which is the mapping a mount needs. [`crate::naps::Cblock`]'s field
/// boundaries are still wrong, so a record-driven reader would decode garbage; taking the plan's
/// triple is what keeps this reader honest until the codec is re-derived against a genuine
/// package.
pub fn reconstruct(image: &[u8], layout: &Layout, files: &[(u64, u64, u64)]) -> Result<Vec<u8>> {
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
    if data_end > meta_base {
        return format_err(format!(
            "the layout's data ends at {data_end:#x}, past its metadata base {meta_base:#x}"
        ));
    }

    let codec = crate::inner::MetaCodec::from_compression_type(layout.compression_type)?;
    let mount = crate::inner::logical_mount(image, meta_base, codec, files)?;
    if mount.len() as u64 != mount_size {
        return format_err(format!(
            "the metadata container expands the mount to {:#x} where the layout says {mount_size:#x}",
            mount.len()
        ));
    }
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

    /// The record codec has to reproduce a real descriptor's records byte for byte. The fields
    /// must account for all 72 bits, so a record whose top bits carry data cannot survive a codec
    /// that never writes them — which is exactly how the earlier layout went wrong unnoticed.
    #[test]
    fn the_record_codec_round_trips_the_samples_records() {
        let dir = std::env::var("PS5UPLOAD_SAMPLE_PKGS")
            .unwrap_or_else(|_| "/Volumes/Storage/PS5/pkgs".into());
        let path = std::path::Path::new(&dir).join("webbrowser.pkg");
        let Ok(pkg) = std::fs::read(&path) else {
            eprintln!("skip: {} not present", path.display());
            return;
        };
        let blob = outer_file(&pkg, "naps_pkg_layout.dat").unwrap();
        let layout = parse(&blob).unwrap();
        let at = blob.len() - layout.cblocks.len() * CBLOCK_LEN;
        for (i, c) in layout.cblocks.iter().enumerate() {
            let raw: &[u8; CBLOCK_LEN] = blob[at + i * CBLOCK_LEN..at + (i + 1) * CBLOCK_LEN]
                .try_into()
                .unwrap();
            assert_eq!(&encode_cblock(c), raw, "record {i} does not round-trip");
        }
    }

    /// Every 16-record window has to begin with a run base: the mount reads the table a window at
    /// a time and expects its first record to re-anchor the cursor.
    #[test]
    fn every_window_of_a_built_layout_opens_with_a_run() {
        let image_len = 40 * UBLOCK;
        let blob = build(
            image_len,
            image_len / 0x1000,
            &[(0, 0, UBLOCK * 4), (UBLOCK * 4, UBLOCK * 4, UBLOCK * 34)],
            UBLOCK * 38,
            UBLOCK * 38,
        )
        .unwrap();
        let layout = parse(&blob).unwrap();
        for (i, c) in layout.cblocks.iter().enumerate() {
            if i % 16 == 0 && i > 0 {
                assert!(
                    matches!(c, Cblock::RunBase { .. }),
                    "window {i} opens with {c:?}"
                );
            }
        }
    }

    /// The run schedule is the part of a stored image this writer used to omit entirely: without
    /// it the mount's walk has no anchors and no end.
    #[test]
    fn a_stored_image_carries_its_run_schedule() {
        let ndblock = 40u64;
        let mount_size = ndblock * BLOCK;
        // Each file opens on its own 64 KiB boundary, which is exactly what makes the physical
        // cursor non-contiguous and so what a run base has to re-anchor.
        let files = [
            (0, 0, 96),
            (96, crate::plan::FILE_ALIGN, 12752),
            (12848, 2 * crate::plan::FILE_ALIGN, 29670),
        ];
        let blob = build(
            mount_size,
            ndblock,
            &files,
            2 * crate::plan::FILE_ALIGN + 29670,
            0x80000,
        )
        .unwrap();
        let layout = parse(&blob).unwrap();

        // The first file sits where the cursor already is, so it needs no anchor of its own — a
        // real descriptor opens with a block record, not a run base.
        assert!(
            matches!(layout.cblocks.first(), Some(Cblock::Block { .. })),
            "the first file's block must not open a run: {:?}",
            layout.cblocks.first()
        );

        // Where each run base re-anchors: `cstart` carries the 256 KiB part of the offset and
        // `tweak` the 32 KiB one, which together rebuild the block's own position.
        let anchor = |c: &Cblock| match c {
            Cblock::RunBase {
                tweak,
                coffset_start_256k,
                ..
            } => ((*coffset_start_256k as u64) << 18) | ((u64::from(*tweak) & 7) << 15),
            other => panic!("expected a run base, got {other:?}"),
        };
        let anchors: Vec<u64> = layout
            .cblocks
            .iter()
            .filter(|c| matches!(c, Cblock::RunBase { .. }))
            .map(anchor)
            .collect();
        assert_eq!(
            anchors,
            vec![
                crate::plan::FILE_ALIGN,
                2 * crate::plan::FILE_ALIGN,
                0x80000,
                0xC0000,
                mount_size
            ],
            "each file start, the metadata region, the window's own anchor, the mount end"
        );

        // The terminator closes the layout: its per-block record carries the sentinel marker
        // and no payload length.
        match layout.cblocks.last() {
            Some(Cblock::Block {
                reserved19,
                clen_even_minus1,
                ..
            }) => {
                assert!(*reserved19, "the terminator carries the sentinel marker");
                assert_eq!(*clen_even_minus1, 0, "the terminator's clen");
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

        // Files the image already stores where the mount reads them: this fixture predates the
        // physical-offset model, so its logical and on-disk offsets coincide.
        let files = [(0u64, 0u64, 96u64), (96, 96, 12752), (12848, 12848, 29686)];
        let blob = build_with_meta(
            image.len() as u64,
            ndblock,
            &files,
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
                let even_chunk = if b.compressed {
                    b.payload_len
                } else {
                    b.payload_len.min(0x2_0000)
                };
                (even_chunk.saturating_sub(1), KDE_PAYLOAD)
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
                    reserved19,
                    ..
                } if *kde == KDE_PAYLOAD && !*reserved19 => {
                    assert_eq!(*even, 1, "every raw record carries an even chunk");
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

        let rebuilt = reconstruct(&image, &layout, &files).unwrap();
        assert_eq!(rebuilt.len(), mount_size as usize);
        // The data region lands where the plan places it; the gap between it and the metadata base
        // is padding the image never stores, and the metadata region expands out of its container.
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
            &[(0, 0, 96), (96, 96, 12752), (12848, 12848, 29670)],
            0xa626,
            meta_base,
            &written.blocks,
            COMP_KRAKEN,
        )
        .unwrap();
        let layout = parse(&blob).unwrap();
        assert_eq!(layout.compression_type, COMP_KRAKEN as u8);
        // Every non-terminator record is a raw block: it carries an even chunk, and the sentinel
        // marker belongs to the terminator alone.
        let blocks = &layout.cblocks[..layout.cblocks.len() - 1];
        for c in blocks {
            if let Cblock::Block {
                even, reserved19, ..
            } = c
            {
                assert_eq!(*even, 1, "every raw record carries an even chunk");
                assert!(!*reserved19, "only the terminator carries the marker");
            }
        }
    }
}
