//! The packed output as a tree, without writing it: the source's loose files plus the
//! `ampr_zstd.index` and volumes `pack` would write, at their sizes from a
//! [`crate::writer::measure`] pass. A volume's bytes are made on demand: its header from the
//! index, each chunk re-read from its source file, compressed again by the one encoder `pack`
//! uses and checked against the index (codec and stored length; a zstd frame is decoded and
//! must give back the source bytes). So an image writer can take the packs straight from the
//! source, with no staging folder.
//!
//! Memory: the [`Index`] (16 B per chunk), one batch of stored chunks (one per compression
//! thread, at most [`crate::writer::MAX_THREADS`] x 256 KiB) and the next batch's source bytes,
//! read while this one compresses.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use ps5upload_fpkg::source::{SourceFile, SourceTree};
use ps5upload_fpkg::{Error, Result};
use rayon::prelude::*;

use crate::reader::{INDEX, decode_chunk};
use crate::writer::{encode_chunk, pool, read_chunk};
use crate::{
    CHUNK_SIZE, Codec, FileRecord, Index, VOLUME_HEADER_SIZE, VolumeHeader, encode_index,
    encode_volume_header, volume_name,
};

fn bad(message: impl Into<String>) -> Error {
    Error::Format(message.into())
}

/// See the module docs. `inner` is the tree the index was measured on.
pub struct PackedTree<'a> {
    inner: &'a mut dyn SourceTree,
    index: &'a Index,
    index_bytes: Vec<u8>,
    files: Vec<SourceFile>,
    empty_dirs: Vec<String>,
    volumes: HashMap<String, usize>,
    /// First chunk of each volume, then the chunk count.
    vol_first: Vec<usize>,
    pool: rayon::ThreadPool,
    cancel: &'a AtomicBool,
    /// The stored bytes of chunks `window.0..`, the batch made last.
    window: (usize, Vec<Vec<u8>>),
    /// Source bytes of the chunks from `.0` on, read while the last batch compressed.
    ahead: Option<(usize, Vec<Vec<u8>>)>,
}

/// The file chunk `c` belongs to and its offset there. The last record starting at or before
/// `c` holds it (an empty file starts where the next one does).
fn locate(index: &Index, c: usize) -> (&FileRecord, u64) {
    let i = index
        .files
        .partition_point(|file| file.first_chunk as usize <= c)
        - 1;
    let file = &index.files[i];
    (file, (c - file.first_chunk as usize) as u64 * CHUNK_SIZE)
}

/// The source bytes of up to `width` chunks from `from` (none past the last chunk).
fn read_raw(
    inner: &mut dyn SourceTree,
    index: &Index,
    from: usize,
    width: usize,
) -> Result<Vec<Vec<u8>>> {
    let to = from.saturating_add(width).min(index.chunks.len());
    (from..to)
        .map(|c| {
            let (file, offset) = locate(index, c);
            let size = (file.logical_size - offset).min(CHUNK_SIZE) as usize;
            read_chunk(inner, &file.path, offset, size)
        })
        .collect()
}

/// Chunk `c` made again from its source bytes and checked against the index: its stored bytes.
fn regenerate(index: &Index, c: usize, raw: Vec<u8>) -> Result<Vec<u8>> {
    let (file, offset) = locate(index, c);
    let want = index.chunks[c];
    let made = if file.store_only {
        Ok((Codec::Raw, raw))
    } else {
        encode_chunk(raw.clone(), index.zstd_level).and_then(|(codec, data)| {
            if codec == Codec::Zstd && decode_chunk(&data, codec, raw.len())? != raw {
                return Err(bad("its frame does not decode to the source bytes"));
            }
            Ok((codec, data))
        })
    };
    let why = match made {
        Ok((codec, data)) if codec == want.codec && data.len() == want.stored_size as usize => {
            return Ok(data);
        }
        Ok((codec, data)) => format!(
            "it came out {codec:?}, {} bytes; the measure pass recorded {:?}, {} bytes",
            data.len(),
            want.codec,
            want.stored_size
        ),
        Err(error) => error.to_string(),
    };
    Err(bad(format!(
        "{} at {offset}: the regenerated zstd chunk {c} differs from the measure pass: {why} \
         (the source changed between its two reads, or the compressor is not deterministic)",
        file.path
    )))
}

impl<'a> PackedTree<'a> {
    /// `index` must come from `measure` over `inner` (each packed file at its measured size);
    /// every other file of `inner` stays loose. `threads` compress (0: every core), at most
    /// [`crate::writer::MAX_THREADS`]; `cancel` stops a volume read between batches.
    pub fn new(
        inner: &'a mut dyn SourceTree,
        index: &'a Index,
        threads: usize,
        cancel: &'a AtomicBool,
    ) -> Result<Self> {
        let index_bytes = encode_index(index).map_err(|error| bad(error.to_string()))?;
        let sizes: HashMap<&str, u64> = inner
            .files()
            .iter()
            .map(|file| (file.path.as_str(), file.size))
            .collect();
        for file in &index.files {
            if sizes.get(file.path.as_str()) != Some(&file.logical_size) {
                return Err(bad(format!(
                    "{}: not in the source at its measured size",
                    file.path
                )));
            }
        }
        let packed: HashSet<&str> = index.files.iter().map(|file| file.path.as_str()).collect();
        let mut files: Vec<SourceFile> = inner
            .files()
            .iter()
            .filter(|file| !packed.contains(file.path.as_str()))
            .cloned()
            .collect();
        let names: Vec<String> = (0..index.volume_sizes.len())
            .map(|id| volume_name(id as u32))
            .collect();
        files.push(SourceFile {
            path: INDEX.into(),
            size: index_bytes.len() as u64,
        });
        files.extend(
            names
                .iter()
                .zip(&index.volume_sizes)
                .map(|(name, &size)| SourceFile {
                    path: name.clone(),
                    size,
                }),
        );
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(pair) = files.windows(2).find(|pair| pair[0].path == pair[1].path) {
            return Err(bad(format!(
                "{} is both a source file and a pack file",
                pair[0].path
            )));
        }
        let mut vol_first = vec![0; index.volume_sizes.len() + 1];
        for chunk in &index.chunks {
            vol_first[chunk.volume_id as usize + 1] += 1;
        }
        for id in 1..vol_first.len() {
            vol_first[id] += vol_first[id - 1];
        }
        Ok(Self {
            empty_dirs: inner.empty_dirs().to_vec(),
            inner,
            index,
            index_bytes,
            files,
            volumes: names
                .into_iter()
                .enumerate()
                .map(|(id, n)| (n, id))
                .collect(),
            vol_first,
            pool: pool(threads)?,
            cancel,
            window: (0, Vec::new()),
            ahead: None,
        })
    }

    /// Makes the batch from chunk `c` on (one chunk per thread, in parallel) while the next
    /// batch's source bytes are read.
    fn fill(&mut self, c: usize) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let width = self.pool.current_num_threads();
        let (index, inner) = (self.index, &mut *self.inner);
        let raw = match self.ahead.take() {
            Some((at, raw)) if at == c && !raw.is_empty() => raw,
            _ => read_raw(inner, index, c, width)?,
        };
        let next = c + raw.len();
        let mut made = None;
        let slot = &mut made;
        let ahead = self.pool.in_place_scope(|scope| {
            scope.spawn(move |_| {
                *slot = Some(
                    raw.into_par_iter()
                        .enumerate()
                        .map(|(i, raw)| regenerate(index, c + i, raw))
                        .collect::<Result<Vec<_>>>(),
                );
            });
            read_raw(inner, index, next, width)
        });
        self.window = (c, made.expect("the scope ran the task")?);
        self.ahead = Some((next, ahead?));
        Ok(())
    }

    /// The stored bytes of chunk `c`.
    fn chunk(&mut self, c: usize) -> Result<&[u8]> {
        let (at, made) = &self.window;
        if !(*at..at + made.len()).contains(&c) {
            self.fill(c)?;
        }
        Ok(&self.window.1[c - self.window.0])
    }

    fn read_volume(&mut self, id: usize, off: u64, len: usize) -> Result<Vec<u8>> {
        let size = self.index.volume_sizes[id];
        let end = off.saturating_add(len as u64).min(size);
        if off >= end {
            return Ok(Vec::new());
        }
        let mut out = vec![0u8; (end - off) as usize];
        let head = VOLUME_HEADER_SIZE as u64;
        if off < head {
            let header = encode_volume_header(VolumeHeader {
                id: id as u32,
                count: self.index.volume_sizes.len() as u32,
                archive_id: self.index.archive_id,
                payload_bytes: size - head,
            })
            .map_err(|error| bad(error.to_string()))?;
            let to = end.min(head);
            out[..(to - off) as usize].copy_from_slice(&header[off as usize..to as usize]);
        }
        // The first chunk that ends after `off`, then every chunk that starts before `end`.
        let (first, last) = (self.vol_first[id], self.vol_first[id + 1]);
        let chunks = &self.index.chunks[first..last];
        let from = first
            + chunks.partition_point(|chunk| {
                u64::from(chunk.offset) + u64::from(chunk.stored_size) <= off
            });
        for c in from..last {
            let at = u64::from(self.index.chunks[c].offset);
            if at >= end {
                break;
            }
            let data = self.chunk(c)?;
            let (s, e) = (at.max(off), (at + data.len() as u64).min(end));
            out[(s - off) as usize..(e - off) as usize]
                .copy_from_slice(&data[(s - at) as usize..(e - at) as usize]);
        }
        Ok(out)
    }
}

impl SourceTree for PackedTree<'_> {
    fn files(&self) -> &[SourceFile] {
        &self.files
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let size = self
            .files
            .binary_search_by(|file| file.path.as_str().cmp(path))
            .map(|i| self.files[i].size)
            .map_err(|_| bad(format!("{path} is not in the packed tree")))?;
        self.read_range(path, 0, usize::try_from(size).unwrap_or(usize::MAX))
    }

    fn read_range(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        if let Some(&id) = self.volumes.get(path) {
            return self.read_volume(id, offset, len);
        }
        if path == INDEX {
            let bytes = &self.index_bytes;
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(bytes.len());
            return Ok(bytes[start..start.saturating_add(len).min(bytes.len())].to_vec());
        }
        if self
            .files
            .binary_search_by(|file| file.path.as_str().cmp(path))
            .is_err()
        {
            return Err(bad(format!("{path} is not in the packed tree")));
        }
        self.inner.read_range(path, offset, len)
    }

    fn empty_dirs(&self) -> &[String] {
        &self.empty_dirs
    }

    fn describe(&self) -> String {
        format!(
            "{} as zstd packs ({} volumes, made on demand)",
            self.inner.describe(),
            self.index.volume_sizes.len()
        )
    }
}
