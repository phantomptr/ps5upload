//! The image inside a `.ffpfsc`, readable at any offset.
//!
//! A `.ffpfsc` holds one file (normally an `.exfat` or `.ffpkg` game image) either stored as
//! it is or as a zlib `PFSC` container of 64 KiB blocks (see [`crate::ffpfsc`]). This reader
//! is `Read + Seek` over that file's own bytes: a stored file is a window into the image; a
//! compressed one decodes the block a read lands in, keeping the last few decoded, so the
//! converter's mostly-sequential reads decode each block once.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use flate2::read::ZlibDecoder;

use crate::ffpfsc::{layout, BLOCK, PFSC_HEADER_LEN, PFSC_MAGIC};
use crate::{format_err, Result};

/// Decoded blocks kept: a directory walk revisits a handful of blocks; a sequential read needs
/// one.
const CACHE_BLOCKS: usize = 8;

pub struct PfscReader<R: Read + Seek = File> {
    inner: R,
    name: String,
    /// Where the file (or its container) starts in the image.
    base: u64,
    len: u64,
    /// Absolute offsets (from `base`) of each stored block, plus the end; `None` when the file
    /// is stored as it is.
    offsets: Option<Vec<u64>>,
    pos: u64,
    /// Most recently used last.
    cache: Vec<(u64, Vec<u8>)>,
}

impl PfscReader<File> {
    pub fn open(path: &Path) -> Result<Self> {
        Self::new(File::open(path)?)
    }
}

impl<R: Read + Seek> PfscReader<R> {
    pub fn new(mut inner: R) -> Result<Self> {
        let image_len = inner.seek(SeekFrom::End(0))?;
        let l = layout(&mut inner)?;
        let fits = |end: Option<u64>| end.is_some_and(|e| e <= image_len);
        if !fits(l.base.checked_add(l.stored)) {
            return format_err("the image is shorter than the file it holds");
        }
        let offsets = if l.compressed {
            inner.seek(SeekFrom::Start(l.base))?;
            let mut ph = [0u8; PFSC_HEADER_LEN];
            inner.read_exact(&mut ph)?;
            let le32 = |at: usize| u32::from_le_bytes(ph[at..at + 4].try_into().unwrap());
            let le64 = |at: usize| u64::from_le_bytes(ph[at..at + 8].try_into().unwrap());
            if le32(0) != PFSC_MAGIC || le64(0x10) != BLOCK {
                return format_err("the file is flagged compressed but holds no PFSC container");
            }
            let blocks = le64(0x28) / BLOCK;
            if blocks != l.raw_size.div_ceil(BLOCK) {
                return format_err("the container's length disagrees with the file size");
            }
            // The table must lie inside the container before it is allocated.
            let table_len = blocks.checked_add(1).and_then(|n| n.checked_mul(8));
            let table_at = l.base.checked_add(le64(0x18));
            let (Some(table_len), Some(table_at)) = (table_len, table_at) else {
                return format_err("the container's block table is out of range");
            };
            if !fits(table_at.checked_add(table_len)) {
                return format_err("the container's block table runs past the image");
            }
            let mut table = vec![0u8; table_len as usize];
            inner.seek(SeekFrom::Start(table_at))?;
            inner.read_exact(&mut table)?;
            let offsets: Vec<u64> = table
                .chunks(8)
                .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let ordered = offsets
                .windows(2)
                .all(|w| w[1] >= w[0] && w[1] - w[0] <= BLOCK);
            if !ordered || offsets.last() != Some(&l.stored) {
                return format_err("the container's block offsets are damaged");
            }
            Some(offsets)
        } else {
            None
        };
        Ok(Self {
            inner,
            name: l.name,
            base: l.base,
            len: l.raw_size,
            offsets,
            pos: 0,
            cache: Vec::new(),
        })
    }

    /// The size of the file inside.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The name the file carries inside the image (`PPSA01234.exfat`).
    pub fn inner_name(&self) -> &str {
        &self.name
    }

    /// Decoded block `n`, from the cache or the image.
    fn block(&mut self, n: u64) -> io::Result<&[u8]> {
        if let Some(i) = self.cache.iter().position(|(b, _)| *b == n) {
            let hit = self.cache.remove(i);
            self.cache.push(hit);
        } else {
            let offsets = self
                .offsets
                .as_ref()
                .expect("only compressed files have blocks");
            let (start, end) = (offsets[n as usize], offsets[n as usize + 1]);
            let mut stored = vec![0u8; (end - start) as usize];
            self.inner.seek(SeekFrom::Start(self.base + start))?;
            self.inner.read_exact(&mut stored)?;
            let plain = if stored.len() == BLOCK as usize {
                stored
            } else {
                // Bounded: a hostile stream cannot inflate past one block.
                let mut d = Vec::with_capacity(BLOCK as usize);
                ZlibDecoder::new(&stored[..])
                    .take(BLOCK + 1)
                    .read_to_end(&mut d)?;
                if d.len() != BLOCK as usize {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{}: block {n} does not decode to 64 KiB", self.name),
                    ));
                }
                d
            };
            if self.cache.len() >= CACHE_BLOCKS {
                self.cache.remove(0);
            }
            self.cache.push((n, plain));
        }
        Ok(&self.cache.last().expect("just pushed").1)
    }
}

impl<R: Read + Seek> Read for PfscReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let left = self.len - self.pos;
        if self.offsets.is_none() {
            let n = (buf.len() as u64).min(left) as usize;
            self.inner.seek(SeekFrom::Start(self.base + self.pos))?;
            let got = self.inner.read(&mut buf[..n])?;
            self.pos += got as u64;
            return Ok(got);
        }
        let (n, off) = (self.pos / BLOCK, (self.pos % BLOCK) as usize);
        let want = (buf.len() as u64).min(left).min(BLOCK - off as u64) as usize;
        let block = self.block(n)?;
        buf[..want].copy_from_slice(&block[off..off + want]);
        self.pos += want as u64;
        Ok(want)
    }
}

impl<R: Read + Seek> Seek for PfscReader<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match target {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            )),
        }
    }
}
