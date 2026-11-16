//! A pack set's volumes made again from the source as they are read, for a pack set
//! [`super::pack::measure`]d instead of built: nothing is spooled on this computer.
//!
//! Why it is the same bytes. Everything in a volume follows from the measure: the 64-byte header
//! from the volume's record and the build id, every chunk's place (`offset`, `pack_id`), size and
//! codec from the manifest's chunk table, and zeros everywhere else (padding). What the table
//! does not hold is the chunk's bytes, and those are a function of the block's raw bytes and its
//! file's rule alone: `compress_block` is deterministic (lz4_flex's block encoder for `fast`, the
//! hash-chain encoder in `lz4.rs` for `hc`, neither with any state across blocks), and so is its
//! choice between LZ4 and raw. A chunk shared with an earlier one (identical blocks stored once)
//! has no bytes of its own. So each stored chunk is made again from its block, and checked
//! before a byte of it is handed out: the raw block's CRC against the CRC sidecar, the codec and
//! size against the table, and the stored bytes' CRC against the measure's. A chunk that differs
//! (the source changed after the measure) is an error, never a different chunk in a volume whose
//! manifest says otherwise.
//!
//! Memory: the parsed manifest (a 48-byte file record per file, a 16-byte chunk record per
//! chunk), the two CRC tables (4 bytes each per chunk), each volume's list of stored chunks
//! (4 bytes per chunk) and each packed file's path, next to the manifest and sidecar bytes the
//! packed game carries anyway (12 + 4 bytes per chunk); plus one window of made bytes and the
//! raw blocks behind it. For 300 000 files and 100 GB in 64 KiB chunks (1.6 M chunks): about
//! 70 MB of chunk tables, 40 MB of file records and paths, and at most 16 MiB of window and
//! 32 MiB (plus one block) of raw blocks at a time; at 16 KiB chunks the chunk tables are four
//! times that. The measure pass itself holds what a spooled build holds (the dedupe map of
//! every stored chunk's fingerprint, the chunk records and CRCs), and writes nothing.

use std::collections::HashMap;

use super::config::{Config, Rule};
use super::format::{crc32, data_header, parse_crc_sidecar, Manifest, CHUNK_SHARED};
use super::pack::{compress_block, Output};
use crate::source::SourceTree;
use crate::{format_err, Error, Result};

/// Volume bytes made at a time when a volume is read in order, and kept for the next read.
const WINDOW: u64 = 16 << 20;
/// Raw block bytes read from the source before they are compressed (on every worker at once).
const BATCH_RAW: u64 = 32 << 20;

/// The volumes of a measured pack set, made on demand.
pub struct Regen {
    config: Config,
    manifest: Manifest,
    raw_crcs: Vec<u32>,
    stored_crcs: Vec<u32>,
    /// Per volume: its stored chunks (indices into the chunk table), in offset order.
    by_volume: Vec<Vec<u32>>,
    /// Packed files with chunks, by first chunk: (first chunk, file record, path in the source).
    files: Vec<(u32, u32, String)>,
    names: HashMap<String, usize>,
    /// The bytes last made: (volume, first byte, bytes).
    window: Option<(usize, u64, Vec<u8>)>,
}

impl Regen {
    /// The volumes `output` (from [`super::pack::measure`] over `config`) describes.
    pub fn new(output: &Output, config: &Config) -> Result<Self> {
        let manifest = Manifest::parse(&output.manifest)?;
        let raw_crcs = parse_crc_sidecar(&output.crc, &manifest.build_id, manifest.chunks.len())?;
        if output.stored_crcs.len() != manifest.chunks.len() {
            return format_err("the measured pack set has no stored-chunk CRC for every chunk");
        }
        let mut by_volume: Vec<Vec<u32>> = vec![Vec::new(); manifest.packs.len()];
        for (i, c) in manifest.chunks.iter().enumerate() {
            if c.flags & CHUNK_SHARED != 0 {
                continue;
            }
            let list = by_volume
                .get_mut(usize::from(c.pack_id))
                .ok_or_else(|| Error::Format("a chunk names a volume that is not there".into()))?;
            if let Some(&prev) = list.last() {
                let p = &manifest.chunks[prev as usize];
                if p.offset + u64::from(p.stored_size) > c.offset {
                    return format_err("the measured chunks of a volume overlap");
                }
            }
            list.push(u32::try_from(i).map_err(|_| Error::Format("too many chunks".into()))?);
        }
        let mut packed = output.packed.iter();
        let mut files = Vec::new();
        for (i, f) in manifest.files.iter().enumerate() {
            if !f.packed() {
                continue;
            }
            let path = packed
                .next()
                .ok_or_else(|| Error::Format("the measure lists fewer packed files".into()))?;
            if f.chunk_count > 0 {
                files.push((f.first_chunk, i as u32, path.clone()));
            }
        }
        files.sort_by_key(|f| f.0);
        let mut names = HashMap::new();
        for i in 0..manifest.packs.len() {
            names.insert(manifest.pack_name(i)?.to_string(), i);
        }
        Ok(Self {
            config: config.clone(),
            manifest,
            raw_crcs,
            stored_crcs: output.stored_crcs.clone(),
            by_volume,
            files,
            names,
            window: None,
        })
    }

    /// Whether `name` (relative to `/app0`) is one of the volumes.
    pub fn has(&self, name: &str) -> bool {
        self.names.contains_key(name)
    }

    /// The size of the volume `name`.
    pub fn size(&self, name: &str) -> Option<u64> {
        self.names
            .get(name)
            .map(|&v| self.manifest.packs[v].file_size)
    }

    /// `len` bytes of the volume `name` from `offset` (fewer only past its end), made from
    /// `source`.
    pub fn read_range(
        &mut self,
        source: &mut dyn SourceTree,
        name: &str,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let vol = *self
            .names
            .get(name)
            .ok_or_else(|| Error::Format(format!("{name} is not a volume of the pack set")))?;
        let size = self.manifest.packs[vol].file_size;
        let end = offset.saturating_add(len as u64).min(size);
        if offset >= end {
            return Ok(Vec::new());
        }
        let held = |w: &(usize, u64, Vec<u8>)| {
            w.0 == vol && w.1 <= offset && end <= w.1 + w.2.len() as u64
        };
        if !self.window.as_ref().is_some_and(held) {
            let stop = end.max(offset.saturating_add(WINDOW)).min(size);
            let bytes = self.make(source, vol, offset, stop)?;
            self.window = Some((vol, offset, bytes));
        }
        let (_, at, bytes) = self.window.as_ref().expect("a window was just made");
        Ok(bytes[(offset - at) as usize..(end - at) as usize].to_vec())
    }

    /// The bytes `[start, stop)` of volume `vol`.
    fn make(
        &self,
        source: &mut dyn SourceTree,
        vol: usize,
        start: u64,
        stop: u64,
    ) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; (stop - start) as usize];
        let pack = &self.manifest.packs[vol];
        let header = data_header(
            vol as u32,
            &self.manifest.build_id,
            pack.file_size - pack.payload_bytes,
            pack.payload_bytes,
            pack.flags,
        );
        put(&mut buf, start, 0, &header);
        let chunks = &self.manifest.chunks;
        let list = &self.by_volume[vol];
        let first = list.partition_point(|&ci| {
            let c = &chunks[ci as usize];
            c.offset + u64::from(c.stored_size) <= start
        });
        let mut batch: Vec<(u32, Vec<u8>)> = Vec::new();
        let mut batch_raw = 0u64;
        let mut rules: HashMap<u32, (Rule, u64)> = HashMap::new();
        for &ci in &list[first..] {
            if chunks[ci as usize].offset >= stop {
                break;
            }
            let raw = self.raw_block(source, ci)?;
            batch_raw += raw.len() as u64;
            batch.push((ci, raw));
            if batch_raw >= BATCH_RAW {
                self.made(std::mem::take(&mut batch), &mut rules, &mut buf, start)?;
                batch_raw = 0;
            }
        }
        self.made(batch, &mut rules, &mut buf, start)?;
        Ok(buf)
    }

    /// The file a chunk belongs to: (file record, path, block within the file).
    fn owner(&self, ci: u32) -> Result<(u32, &str, u32)> {
        let at = self.files.partition_point(|f| f.0 <= ci);
        let (first, rec, path) = at
            .checked_sub(1)
            .and_then(|i| self.files.get(i))
            .ok_or_else(|| Error::Format("a chunk belongs to no packed file".into()))?;
        let local = ci - first;
        if local >= self.manifest.files[*rec as usize].chunk_count {
            return format_err("a stored chunk lies outside its file's chunks");
        }
        Ok((*rec, path, local))
    }

    /// Chunk `ci`'s block, read from the source.
    fn raw_block(&self, source: &mut dyn SourceTree, ci: u32) -> Result<Vec<u8>> {
        let (rec, path, local) = self.owner(ci)?;
        let f = &self.manifest.files[rec as usize];
        let len = Manifest::raw_size(f, local) as usize;
        let raw = source.read_range(path, u64::from(local) << f.block_shift, len)?;
        if raw.len() != len {
            return format_err(format!("{path} is shorter than when it was packed"));
        }
        Ok(raw)
    }

    /// Compresses `batch` on every worker, checks each chunk against the measure, and puts what
    /// falls inside `buf` (which starts at volume byte `start`) there.
    fn made(
        &self,
        batch: Vec<(u32, Vec<u8>)>,
        rules: &mut HashMap<u32, (Rule, u64)>,
        buf: &mut [u8],
        start: u64,
    ) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let mut jobs = Vec::with_capacity(batch.len());
        for (ci, raw) in batch {
            let (rec, path, _) = self.owner(ci)?;
            rules.entry(rec).or_insert_with(|| {
                let rule = self.config.select(path);
                let page = self.config.group_page(&rule.group);
                (rule, page)
            });
            jobs.push((ci, rec, raw));
        }
        let rules = &*rules;
        let workers = self.config.workers.max(1);
        let per = jobs.len().div_ceil(workers).max(1);
        let lane = crate::workshop::Lane::current();
        let results: Vec<Result<(u32, Vec<u8>)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .chunks(per)
                .map(|part| {
                    scope.spawn(move || {
                        lane.enter();
                        part.iter()
                            .map(|(ci, rec, raw)| {
                                let (rule, page) = &rules[rec];
                                self.check(*ci, raw, rule, *page).map(|b| (*ci, b))
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| match h.join() {
                    Ok(v) => v,
                    Err(_) => vec![format_err("a compression worker panicked")],
                })
                .collect()
        });
        for r in results {
            let (ci, stored) = r?;
            put(
                buf,
                start,
                self.manifest.chunks[ci as usize].offset,
                &stored,
            );
        }
        Ok(())
    }

    /// Chunk `ci` made again from `raw`, or why it is not the chunk the measure placed.
    fn check(&self, ci: u32, raw: &[u8], rule: &Rule, page: u64) -> Result<Vec<u8>> {
        let want = &self.manifest.chunks[ci as usize];
        let changed = || {
            let path = self.owner(ci).map_or("a packed file", |o| o.1);
            Error::Format(format!(
                "{path} {CHANGED}: chunk {ci} does not come out as it did, so nothing more is sent"
            ))
        };
        if crc32(raw) != self.raw_crcs[ci as usize] {
            return Err(changed());
        }
        let block = compress_block(raw, rule, page);
        let codec = block.codec;
        let stored = block.stored.unwrap_or_else(|| raw.to_vec());
        if codec != want.codec
            || stored.len() != want.stored_size as usize
            || crc32(&stored) != self.stored_crcs[ci as usize]
        {
            return Err(Error::Format(format!(
                "chunk {ci} of the LZ4 packs {NOT_REPEATED} ({} bytes, codec {codec}; measured \
                 {} bytes, codec {}): nothing more is sent",
                stored.len(),
                want.stored_size,
                want.codec
            )));
        }
        Ok(stored)
    }
}

/// Whether `message` (an error, wrapped in whatever context) says a volume could not be made
/// again as it was measured: the caller may pack into a spool instead.
pub fn not_made_again(message: &str) -> bool {
    message.contains(CHANGED) || message.contains(NOT_REPEATED)
}

const CHANGED: &str = "changed after its LZ4 packs were measured";
const NOT_REPEATED: &str = "did not compress the same way twice";

/// Copies what of `bytes` (at volume byte `at`) falls inside `buf` (at volume byte `start`).
fn put(buf: &mut [u8], start: u64, at: u64, bytes: &[u8]) {
    let end = start + buf.len() as u64;
    let (from, to) = (at.max(start), (at + bytes.len() as u64).min(end));
    if from < to {
        buf[(from - start) as usize..(to - start) as usize]
            .copy_from_slice(&bytes[(from - at) as usize..(to - at) as usize]);
    }
}
