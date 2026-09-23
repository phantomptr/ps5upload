//! A Kraken-compressed inner image, and the layout descriptor that lets the console read it.
//!
//! The image is data-first: every file's bytes, then the zeros between the data and the
//! metadata base, then the metadata region, each cut into blocks of up to 256 KiB that never
//! cross a file boundary, each block compressed ([`crate::kraken`]) and stored back to back.
//! The descriptor (`naps_pkg_layout.dat`) records every block: where its compressed bytes end,
//! how long its even half is, and each half's mode. Its format was decoded from Sony's own
//! packages and is written up in `docs/research/2026-09-23-ps5-kraken-package-blocks.md`.
//!
//! Compression runs across every core as a streaming pipeline, and every block is decoded
//! again and compared with its source before it is kept, so a package never carries a block
//! the console could not read back. The compressed image goes to a spool file first, because
//! the outer image's geometry depends on its final length.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::crypto::Hasher;
use crate::kraken::{self, Half};
use crate::plan::{self, Plan};
use crate::{format_err, Result};

const UBLOCK: u64 = 0x4_0000;
const WINDOW: u64 = 0x4_0000;

/// What a block holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// A content file, by afid.
    File(usize),
    /// Zeros between the data and the metadata base.
    Gap,
    /// The inner file system's metadata region.
    Meta,
}

/// One stored block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBlock {
    pub logical: u64,
    pub len: u32,
    /// Where its compressed bytes start in the image.
    pub stored_at: u64,
    /// `(stored length, is LZ)` per half, even then odd.
    pub halves: Vec<(u32, bool)>,
    pub owner: Owner,
}

impl StoredBlock {
    pub fn stored_len(&self) -> u64 {
        self.halves.iter().map(|h| u64::from(h.0)).sum()
    }
}

/// The compressed image, spooled to disk, and everything the package needs about it.
pub struct KrakenImage {
    pub blocks: Vec<StoredBlock>,
    /// The spooled image's length, padded to 64 KiB.
    pub image_len: u64,
    /// `SHA3-256` of each file's logical bytes, by afid.
    pub file_digests: Vec<[u8; 32]>,
    /// Where each file's first byte is stored, by afid.
    pub file_stored_at: Vec<u64>,
    pub mount_size: u64,
    /// The logical end of the data.
    pub data_end: u64,
    pub meta_base: u64,
}

/// A block to build, before compression.
struct Todo {
    logical: u64,
    len: usize,
    /// How much of `len` is the file's own; the rest is gap.
    real: usize,
    owner: Owner,
    /// For a file block: the path and the offset within the file.
    source: Option<(String, u64)>,
    generated_keystone: bool,
}

fn plan_blocks(plan: &Plan) -> (Vec<Todo>, u64) {
    let mut todo = Vec::new();
    let mut end = 0u64;
    for (afid, &fi) in plan.afid_order.iter().enumerate() {
        let f = &plan.files[fi];
        // Blocks tile a file up to the next file's start: the descriptor gives no other length.
        // That is the file itself, plus the zeros of a gap the planner spread it by.
        let span = match plan.afid_order.get(afid + 1) {
            Some(&next) => plan.files[next].logical_offset - f.logical_offset,
            None => f.size,
        };
        let mut off = 0u64;
        while off < span {
            let len = (span - off).min(UBLOCK);
            todo.push(Todo {
                logical: f.logical_offset + off,
                len: len as usize,
                real: f.size.saturating_sub(off).min(len) as usize,
                owner: Owner::File(afid),
                source: Some((f.path.clone(), off)),
                generated_keystone: f.generated && f.path == plan::KEYSTONE,
            });
            off += len;
        }
        end = end.max(f.logical_offset + f.size);
    }
    let mut at = end;
    while at < plan.meta_base {
        let len = (plan.meta_base - at).min(UBLOCK);
        todo.push(Todo {
            logical: at,
            len: len as usize,
            real: 0,
            owner: Owner::Gap,
            source: None,
            generated_keystone: false,
        });
        at += len;
    }
    let mount = plan.ndblock * crate::BLOCK;
    let mut at = plan.meta_base;
    while at < mount {
        let len = (mount - at).min(UBLOCK);
        todo.push(Todo {
            logical: at,
            len: len as usize,
            real: 0,
            owner: Owner::Meta,
            source: None,
            generated_keystone: false,
        });
        at += len;
    }
    (todo, end)
}

/// A source-file range reader.
pub type SourceRead<'r> = &'r mut dyn FnMut(&str, u64, usize) -> Result<Vec<u8>>;

/// Like [`crate::ffpfsc::pipeline`], but the calling thread both produces the work and consumes
/// the results, so neither needs to be `Send` — a source tree and a progress callback usually are
/// not. Only `work` runs on the pool. Results reach `sink` in production order.
fn pipeline_local<T: Send, U: Send>(
    threads: usize,
    mut produce: impl FnMut() -> Result<Option<T>>,
    work: impl Fn(T) -> Result<U> + Sync,
    mut sink: impl FnMut(U) -> Result<()>,
) -> Result<()> {
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex};
    let depth = threads * 8;
    std::thread::scope(|s| {
        let (tx_in, rx_in) = sync_channel::<(u64, T)>(depth);
        let (tx_out, rx_out) = sync_channel::<(u64, Result<U>)>(depth);
        let rx_in = Arc::new(Mutex::new(rx_in));
        let work = &work;
        for _ in 0..threads {
            let rx_in = Arc::clone(&rx_in);
            let tx_out = tx_out.clone();
            s.spawn(move || loop {
                let next = rx_in.lock().map(|rx| rx.recv());
                match next {
                    Ok(Ok((i, item))) => {
                        if tx_out.send((i, work(item))).is_err() {
                            return;
                        }
                    }
                    _ => return,
                }
            });
        }
        drop(rx_in);
        drop(tx_out);
        let (mut sent, mut next, mut finished) = (0u64, 0u64, false);
        let mut pending = std::collections::BTreeMap::new();
        let result = (|| -> Result<()> {
            loop {
                // Keep the pool fed without letting more than `depth` items be in flight.
                while !finished && sent - next < depth as u64 {
                    match produce()? {
                        Some(item) => {
                            tx_in
                                .send((sent, item))
                                .map_err(|_| crate::Error::Format("the pool stopped".into()))?;
                            sent += 1;
                        }
                        None => finished = true,
                    }
                }
                if next == sent {
                    return Ok(());
                }
                let (i, r) = rx_out
                    .recv()
                    .map_err(|_| crate::Error::Format("the pool stopped".into()))?;
                pending.insert(i, r);
                while let Some(r) = pending.remove(&next) {
                    sink(r?)?;
                    next += 1;
                }
            }
        })();
        drop(tx_in);
        result
    })
}

/// Compress the image described by `plan` into `spool`. `read` fetches a byte range of a source
/// file, `metadata` is the metadata region's logical bytes and `keystone` the generated one.
#[allow(clippy::too_many_arguments)]
pub fn compress(
    plan: &Plan,
    metadata: &[u8],
    keystone: &[u8],
    read: SourceRead<'_>,
    spool: &Path,
    threads: usize,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<KrakenImage> {
    let (todo, data_end) = plan_blocks(plan);
    let mount = plan.ndblock * crate::BLOCK;
    if metadata.len() as u64 != mount - plan.meta_base {
        return format_err("the metadata region does not fill the mount above its base");
    }
    let total: u64 = todo.iter().map(|t| t.len as u64).sum();
    let mut out = BufWriter::with_capacity(8 << 20, File::create(spool)?);
    let mut next = todo.into_iter();
    let mut blocks: Vec<StoredBlock> = Vec::new();
    let mut cursor = 0u64;
    let files = plan.afid_order.len();
    let mut file_digests = vec![[0u8; 32]; files];
    let mut file_stored_at = vec![0u64; files];
    let mut open: Option<(usize, Hasher)> = None;
    let mut done = 0u64;
    pipeline_local(
        threads.max(1),
        || {
            let Some(t) = next.next() else {
                return Ok(None);
            };
            if cancel.load(Ordering::Relaxed) {
                return format_err("the build was cancelled");
            }
            let bytes = match (&t.owner, &t.source) {
                (Owner::File(_), Some((path, off))) => {
                    let mut b = if t.real == 0 {
                        Vec::new()
                    } else if t.generated_keystone {
                        keystone
                            .get(*off as usize..*off as usize + t.real)
                            .ok_or_else(|| crate::Error::Format("keystone is short".into()))?
                            .to_vec()
                    } else {
                        let b = read(path, *off, t.real)?;
                        if b.len() != t.real {
                            return format_err(format!(
                                "{path} gave {} bytes at {off} where the plan fixed {}",
                                b.len(),
                                t.real
                            ));
                        }
                        b
                    };
                    b.resize(t.len, 0);
                    b
                }
                (Owner::Meta, _) => {
                    let at = (t.logical - plan.meta_base) as usize;
                    metadata[at..at + t.len].to_vec()
                }
                _ => vec![0u8; t.len],
            };
            Ok(Some((t, bytes)))
        },
        |(t, bytes)| {
            let halves = kraken::encode_block(&bytes);
            // Proof before it is kept: the block decodes back to exactly its source.
            if kraken::decode_block(&halves, bytes.len())? != bytes {
                return format_err(format!("block at {:#x} does not round-trip", t.logical));
            }
            Ok((t, bytes, halves))
        },
        |(t, bytes, halves)| {
            // Everything is stored back to back, so the stored position never jumps and the only
            // anchors the descriptor needs are the ones that open each 16-record window.
            if let Owner::File(afid) = t.owner {
                if open.as_ref().is_none_or(|(a, _)| *a != afid) {
                    if let Some((a, h)) = open.take() {
                        file_digests[a] = h.finish();
                    }
                    open = Some((afid, Hasher::new()));
                    file_stored_at[afid] = cursor;
                }
                if let Some((_, h)) = open.as_mut() {
                    h.update(&bytes[..t.real]);
                }
            }
            let stored_at = cursor;
            let mut parts = Vec::with_capacity(2);
            for h in &halves {
                let b = h.bytes();
                out.write_all(b)?;
                cursor += b.len() as u64;
                parts.push((b.len() as u32, matches!(h, Half::Lz(_))));
            }
            blocks.push(StoredBlock {
                logical: t.logical,
                len: t.len as u32,
                stored_at,
                halves: parts,
                owner: t.owner,
            });
            done += t.len as u64;
            progress(done, total);
            Ok(())
        },
    )?;
    if let Some((a, h)) = open.take() {
        file_digests[a] = h.finish();
    }
    // Files with no bytes still have a digest: that of nothing.
    for (afid, &fi) in plan.afid_order.iter().enumerate() {
        if plan.files[fi].size == 0 {
            file_digests[afid] = Hasher::new().finish();
            file_stored_at[afid] = blocks
                .iter()
                .find(|b| b.logical >= plan.files[fi].logical_offset)
                .map_or(cursor, |b| b.stored_at);
        }
    }
    let image_len = cursor.next_multiple_of(crate::BLOCK).max(crate::BLOCK);
    out.write_all(&vec![0u8; (image_len - cursor) as usize])?;
    out.flush()?;
    Ok(KrakenImage {
        blocks,
        image_len,
        file_digests,
        file_stored_at,
        mount_size: mount,
        data_end,
        meta_base: plan.meta_base,
    })
}

/// Read the spooled image back one 64 KiB block at a time.
pub fn spool_block(spool: &mut File, index: u64, buf: &mut [u8]) -> Result<()> {
    spool.seek(SeekFrom::Start(index * crate::BLOCK))?;
    spool.read_exact(buf)?;
    Ok(())
}

// ─────────────────────────────── the layout descriptor ───────────────────────────────

fn block_record(b: &StoredBlock) -> u128 {
    let mut v = u128::from(((b.logical % WINDOW) >> 4) as u32 & 0x3FFF);
    v |= u128::from(b.halves[0].0 - 1) << 14;
    v |= 1u128 << 31;
    let mut flags = 0u32;
    if b.halves[0].1 {
        flags |= 1 << 1; // even half is LZ, literal mode 1
    }
    if b.halves.get(1).is_some_and(|h| h.1) {
        flags |= 1 << 4; // odd half is LZ, literal mode 1
    }
    v |= u128::from(flags) << 32;
    v |= u128::from(((b.stored_at + b.stored_len()) % WINDOW) as u32) << 48;
    v
}

/// An anchor carries its stored position three ways: in 1 MiB units (bits 0–25), in 256 KiB
/// windows (bits 26–47) and within the window (bits 48–65) — measured on Sony's packages, whose
/// anchors past 8 MiB have exactly these values.
fn anchor_record(position: u64) -> u128 {
    u128::from(position >> 20) & ((1 << 26) - 1)
        | (u128::from(position / WINDOW) << 26)
        | (u128::from((position % WINDOW) as u32) << 48)
}

/// The descriptor for `image`, with `fidx` its boundary table: each file's logical start in
/// afid order, then the data end, the metadata base and the mount size.
pub fn layout(image: &KrakenImage, file_starts: &[u64]) -> Result<Vec<u8>> {
    // Bit 66 of a record says the next record is an anchor. It is how a reader tells the two
    // apart — nothing in an anchor itself does — so it is set before every anchor, and two
    // anchors never follow each other.
    const RUN_END: u128 = 1 << 66;
    const TERMINATOR: u128 = 2 << 66;
    let mut recs: Vec<u128> = vec![0]; // index 0: the start anchor, at stored position 0
    let mut block_index: Vec<(u64, usize)> = Vec::with_capacity(image.blocks.len());
    let mut cursor = 0u64;
    let mut last_block: Option<usize> = None;
    let push_anchor = |recs: &mut Vec<u128>, last: Option<usize>, at: u64| {
        if let Some(i) = last {
            recs[i] |= RUN_END;
        }
        recs.push(anchor_record(at));
    };
    for b in &image.blocks {
        // An anchor where the stored position jumps (Sony's layouts do, at file boundaries; ours
        // are contiguous), and one opening every 16-record window. A block always separates two
        // anchors, which is what lets bit 66 announce them.
        let mut anchored = false;
        if b.stored_at != cursor {
            push_anchor(&mut recs, last_block, b.stored_at);
            anchored = true;
        }
        if recs.len().is_multiple_of(16) && !anchored {
            push_anchor(&mut recs, last_block, b.stored_at);
        }
        block_index.push((b.logical, recs.len()));
        last_block = Some(recs.len());
        recs.push(block_record(b));
        cursor = b.stored_at + b.stored_len();
    }
    if let Some(i) = last_block {
        recs[i] |= RUN_END;
    }
    recs.push(anchor_record(cursor) | TERMINATOR);
    let sentinel = recs.len();
    recs.push(u128::from(
        ((image.mount_size % WINDOW) >> 4) as u32 & 0x3FFF,
    ));

    let ublocks = image.mount_size.div_ceil(UBLOCK);
    let files = file_starts.len() + 3;
    let outer_blocks = image.image_len.div_ceil(crate::BLOCK);
    let records = recs.len();
    if records < 2 || records - 2 > 0xFF_FFFF || ublocks > 0xFF_FFFF {
        return format_err("the image is past what the layout descriptor can describe");
    }

    // u2c: per ublock, the first block record starting at or after it.
    let first: Vec<usize> = (0..=ublocks)
        .map(|n| {
            let target = n * UBLOCK;
            let p = block_index.partition_point(|(logical, _)| *logical < target);
            block_index.get(p).map_or(sentinel, |(_, idx)| *idx)
        })
        .collect();
    let groups = (ublocks / 8 + 1) as usize;

    let mut blob =
        Vec::with_capacity(64 + outer_blocks as usize * 8 + files * 6 + groups * 10 + records * 9);
    let word0 = (files as u64 - 1) & 0xFF_FFFF | 2 << 24 | (ublocks & 0xFF_FFFF) << 32;
    let word1 = outer_blocks & 0xFF_FFFF | ((records as u64 - 2) & 0xFF_FFFF) << 24;
    blob.extend_from_slice(&word0.to_le_bytes());
    blob.extend_from_slice(&word1.to_le_bytes());
    blob.resize(blob.len() + outer_blocks as usize * 8, 0);
    let mut fidx: Vec<(u64, u8)> = file_starts.iter().map(|&s| (s, 0)).collect();
    fidx.push((image.data_end, 0));
    fidx.push((image.meta_base, 0));
    fidx.push((image.mount_size, 0x40));
    for (offset, kind) in fidx {
        blob.extend_from_slice(&offset.to_le_bytes()[..5]);
        blob.push(kind);
    }
    for g in 0..groups {
        let at = |n: usize| *first.get(n).unwrap_or(&sentinel);
        let base = at(g * 8);
        blob.extend_from_slice(&(base as u32).to_le_bytes()[..3]);
        for j in 1..8 {
            // A clamped delta would send the console to the wrong record, so a group this dense
            // is refused; `plan::build_with` spreads small files to keep them under a byte.
            let Ok(delta) = u8::try_from(at(g * 8 + j) - base) else {
                return format_err(format!(
                    "ublocks {}..{} hold too many blocks for the layout descriptor",
                    g * 8,
                    g * 8 + 8
                ));
            };
            blob.push(delta);
        }
    }
    blob.resize(blob.len().next_multiple_of(8), 0);
    blob.extend_from_slice(&[0x00, 0x00, 0x04]); // 0x040000, the ublock size
    for r in &recs {
        blob.extend_from_slice(&r.to_le_bytes()[..9]);
    }
    blob.resize(blob.len().next_multiple_of(8), 0);

    // Read it back the way the console will, and require exactly the blocks we stored.
    let walked = describe(&blob)?;
    if walked.len() != image.blocks.len() {
        return format_err("the descriptor does not walk back to its blocks");
    }
    for (w, b) in walked.iter().zip(&image.blocks) {
        if w.logical != b.logical
            || w.len != u64::from(b.len)
            || w.stored_at != b.stored_at
            || w.stored_len != b.stored_len()
            || w.even_len != u64::from(b.halves[0].0)
        {
            return format_err(format!(
                "the descriptor misdescribes the block at {:#x}",
                b.logical
            ));
        }
    }
    Ok(blob)
}

/// A block as a descriptor records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescribedBlock {
    pub logical: u64,
    pub len: u64,
    pub stored_at: u64,
    pub even_len: u64,
    pub stored_len: u64,
    pub even_lz: bool,
    pub odd_lz: bool,
    /// The record's mode bits 32..=36, as stored: 32 and 35 are NOT the even and odd halves'
    /// literal modes, 33 and 36 mark LZ halves, 34 Sony's bare entropy-array forms. Lets a
    /// reader with a full Kraken decoder read Sony's own blocks, which ours does not.
    pub mode_bits: u8,
}

/// Walk a descriptor the way the console does: records in order, anchors moving the stored
/// cursor, blocks tiling the mount from each file boundary. Used to check what [`layout`]
/// writes, and able to read Sony's own packages.
pub fn describe(blob: &[u8]) -> Result<Vec<DescribedBlock>> {
    let word = |at: usize| u64::from_le_bytes(blob[at..at + 8].try_into().unwrap());
    if blob.len() < 16 {
        return format_err("descriptor too short");
    }
    let (w0, w1) = (word(0), word(8));
    let files = (w0 & 0xFF_FFFF) as usize + 1;
    let ublocks = (w0 >> 32) & 0xFF_FFFF;
    let outer = (w1 & 0xFF_FFFF) as usize;
    let records = ((w1 >> 24) & 0xFF_FFFF) as usize + 2;
    let fidx_at = 16 + outer * 8;
    let mut bounds: Vec<u64> = (0..files)
        .map(|i| {
            let r = &blob[fidx_at + i * 6..fidx_at + i * 6 + 6];
            u64::from_le_bytes([r[0], r[1], r[2], r[3], r[4], 0, 0, 0])
        })
        .collect();
    let mount = *bounds.last().unwrap_or(&0);
    bounds.sort_unstable();
    bounds.dedup();
    let u2c_end = fidx_at + files * 6 + (ublocks / 8 + 1) as usize * 10;
    let rec_at = u2c_end.next_multiple_of(8) + 3;
    if blob.len() < rec_at + records * 9 {
        return format_err("descriptor shorter than its records");
    }
    let mut out = Vec::new();
    let (mut cursor, mut logical) = (0u64, 0u64);
    let mut next_is_anchor = true; // index 0 is the start anchor
    for i in 0..records.saturating_sub(1) {
        let r = &blob[rec_at + i * 9..rec_at + i * 9 + 9];
        let mut x = [0u8; 16];
        x[..9].copy_from_slice(r);
        let v = u128::from_le_bytes(x);
        let f = |lo: u32, w: u32| ((v >> lo) & ((1u128 << w) - 1)) as u64;
        let anchor = next_is_anchor;
        next_is_anchor = f(66, 1) == 1 && !anchor;
        if anchor {
            cursor = (f(26, 22) << 18) | f(48, 18);
            continue;
        }
        let next_bound = *bounds.iter().find(|&&b| b > logical).unwrap_or(&mount);
        let len = (next_bound - logical).min(UBLOCK);
        if (logical % WINDOW) >> 4 != f(0, 14) {
            return format_err(format!("record {i}: logical offset disagrees"));
        }
        // A block ends where its record says, within the window. The one case that cannot say
        // so is a block stored whole and raw — exactly 256 KiB, ending where it began modulo the
        // window — so a raw even half with nothing else to mark it takes the full window. (Sony's
        // zero-byte blocks, which also end where they begin, carry bit 34.)
        let mut end = (cursor & !(WINDOW - 1)) | f(48, 18);
        if end < cursor {
            end += WINDOW;
        }
        let even_raw = f(33, 1) == 0 && f(14, 17) + 1 == len.min(kraken::HALF as u64);
        if end == cursor && even_raw && f(34, 1) == 0 && len > kraken::HALF as u64 {
            end += WINDOW;
        }
        out.push(DescribedBlock {
            logical,
            len,
            stored_at: cursor,
            even_len: f(14, 17) + 1,
            stored_len: end - cursor,
            even_lz: f(33, 1) == 1,
            odd_lz: f(36, 1) == 1,
            mode_bits: f(32, 5) as u8,
        });
        cursor = end;
        logical += len;
    }
    if logical != mount {
        return format_err(format!(
            "blocks tile to {logical:#x}, the mount is {mount:#x}"
        ));
    }
    Ok(out)
}

/// Decode block `b` of a stored image.
pub fn decode_described(image: &[u8], b: &DescribedBlock) -> Result<Vec<u8>> {
    let src = image
        .get(b.stored_at as usize..(b.stored_at + b.stored_len) as usize)
        .ok_or_else(|| crate::Error::Format("a block runs past the image".into()))?;
    let even_logical = b.len.min(kraken::HALF as u64);
    let mut halves = Vec::new();
    let (e, o) = src.split_at((b.even_len as usize).min(src.len()));
    halves.push(if b.even_lz || (e.len() as u64) < even_logical {
        Half::Lz(e.to_vec())
    } else {
        Half::Raw(e.to_vec())
    });
    if b.len > kraken::HALF as u64 {
        let odd_logical = b.len - kraken::HALF as u64;
        halves.push(if b.odd_lz || (o.len() as u64) < odd_logical {
            Half::Lz(o.to_vec())
        } else {
            Half::Raw(o.to_vec())
        });
    }
    kraken::decode_block(&halves, b.len as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_of(
        blocks: Vec<StoredBlock>,
        mount: u64,
        data_end: u64,
        meta_base: u64,
    ) -> KrakenImage {
        let end = blocks.last().map_or(0, |b| b.stored_at + b.stored_len());
        KrakenImage {
            blocks,
            image_len: end.next_multiple_of(crate::BLOCK).max(crate::BLOCK),
            file_digests: Vec::new(),
            file_stored_at: Vec::new(),
            mount_size: mount,
            data_end,
            meta_base,
        }
    }

    fn b(logical: u64, len: u32, at: u64, halves: &[(u32, bool)], owner: Owner) -> StoredBlock {
        StoredBlock {
            logical,
            len,
            stored_at: at,
            halves: halves.to_vec(),
            owner,
        }
    }

    /// The Web Browser's own descriptor bytes, regenerated from its block list. Sony's hint bits
    /// (67–71) are not modelled, so they are masked out of the comparison.
    #[test]
    fn records_and_tables_follow_the_decoded_format() {
        let blocks = vec![
            b(0, 0x60, 0, &[(0x60, false)], Owner::File(0)),
            b(0x60, 0x31d0, 0x10000, &[(0x31d0, false)], Owner::File(1)),
            b(0x3230, 0xa, 0x20000, &[(0xa, false)], Owner::File(2)),
            b(0x323a, 0x3a5e, 0x2000a, &[(0x6cc, true)], Owner::File(3)),
        ];
        // The Web Browser's first four files, as a mount of their own.
        let img = image_of(blocks, 0x6c98, 0x6c98, 0x6c98);
        let blob = layout(&img, &[0, 0x60, 0x3230, 0x323a]).unwrap();
        assert_eq!(describe(&blob).unwrap().len(), 4);
        // Records: start anchor, keystone, run end → anchor 0x10000, ...
        let files = 7;
        let rec_at = (16usize + 3 * 8 + files * 6 + 10).next_multiple_of(8) + 3;
        let rec = |i: usize| {
            let mut x = [0u8; 16];
            x[..9].copy_from_slice(&blob[rec_at + i * 9..rec_at + i * 9 + 9]);
            u128::from_le_bytes(x) & !(0x1Fu128 << 67)
        };
        assert_eq!(rec(0), 0);
        let hex = |v: u128| v.to_le_bytes()[..9].to_vec();
        assert_eq!(
            hex(rec(1)),
            [0x00, 0xc0, 0x17, 0x80, 0x00, 0x00, 0x60, 0x00, 0x04]
        );
        assert_eq!(hex(rec(2)), [0, 0, 0, 0, 0, 0, 0, 0, 0x01]);
        assert_eq!(
            hex(rec(3)),
            [0x06, 0xc0, 0x73, 0x8c, 0x00, 0x00, 0xd0, 0x31, 0x05]
        );
        assert_eq!(hex(rec(4)), [0, 0, 0, 0, 0, 0, 0, 0, 0x02]);
        assert_eq!(
            hex(rec(5)),
            [0x23, 0x43, 0x02, 0x80, 0x00, 0x00, 0x0a, 0x00, 0x02]
        );
        // The LZ block: flags 0x02 (even half LZ, literal mode 1), end 0x206d6.
        assert_eq!(
            hex(rec(6))[..8],
            [0x23, 0xc3, 0xb2, 0x81, 0x02, 0x00, 0xd6, 0x06]
        );
    }

    #[test]
    fn a_compressed_image_walks_back_to_its_blocks() {
        // Two files and a metadata region, laid out and walked back.
        let blocks = vec![
            b(
                0,
                0x40000,
                0,
                &[(0x100, true), (0x80, true)],
                Owner::File(0),
            ),
            b(0x40000, 0x1234, 0x180, &[(0x1234, false)], Owner::File(0)),
            b(0x41234, 0x1000, 0x13b4, &[(0x40, true)], Owner::File(1)),
            b(
                0x42234,
                0x3ddcc,
                0x13f4,
                &[(0x30, true), (0x30, true)],
                Owner::Gap,
            ),
            b(
                0x80000,
                0x40000,
                0x10000,
                &[(0x200, true), (0x200, true)],
                Owner::Meta,
            ),
        ];
        let img = image_of(blocks.clone(), 0xc0000, 0x42234, 0x80000);
        let blob = layout(&img, &[0, 0x41234]).unwrap();
        let d = describe(&blob).unwrap();
        assert_eq!(d.len(), blocks.len());
        for (want, got) in blocks.iter().zip(&d) {
            assert_eq!(got.logical, want.logical);
            assert_eq!(got.len, u64::from(want.len));
            assert_eq!(got.stored_at, want.stored_at);
            assert_eq!(got.stored_len, want.stored_len());
            assert_eq!(got.even_len, u64::from(want.halves[0].0));
        }
    }

    #[test]
    fn a_window_anchor_opens_every_sixteenth_record() {
        let mut blocks = Vec::new();
        let mut at = 0u64;
        for i in 0..40u64 {
            blocks.push(b(
                i * 0x40000,
                0x40000,
                at,
                &[(0x100, true), (0x100, true)],
                Owner::File(0),
            ));
            at += 0x200;
        }
        let img = image_of(blocks.clone(), 40 * 0x40000, 40 * 0x40000, 40 * 0x40000);
        let blob = layout(&img, &[0]).unwrap();
        assert_eq!(describe(&blob).unwrap().len(), 40);
    }
}
