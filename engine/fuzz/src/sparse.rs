//! A disk image as a fuzz input: a virtual length plus the few extents that are not zero.
//!
//! Real images are 100+ MiB of mostly holes (`.ffpkg` cylinder groups sit tens of MiB apart),
//! far too big to mutate as raw bytes. Encoded like this, a whole valid image is a seed of a
//! few hundred KiB and every mutation lands on metadata or data that exists.
//!
//! ```text
//! u32 LE   length in 4 KiB units (clamped to MAX_LEN)
//! repeat:  u32 LE offset in 4 KiB units, u16 LE n, n bytes (a short tail is taken as is)
//! ```
//!
//! Shared by the fuzz targets and `seedgen` (included by path, so seedgen stays free of
//! libfuzzer).

use std::io::{self, Read, Seek, SeekFrom};

pub const UNIT: u64 = 4096;
/// Large enough for the seeds (a small `.ffpkg` is ~131 MiB), small enough that a reader
/// sizing a buffer from the image length stays under libFuzzer's 2 GiB malloc limit.
pub const MAX_LEN: u64 = 512 << 20;

/// The decoded image: zeros except for `extents` (later ones win where they overlap).
pub struct Sparse {
    len: u64,
    extents: Vec<(u64, Vec<u8>)>,
    pos: u64,
}

impl Sparse {
    pub fn decode(data: &[u8]) -> Self {
        let le32 = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap());
        let mut len = 0;
        let mut extents = Vec::new();
        if data.len() >= 4 {
            len = (u64::from(le32(&data[..4])) * UNIT).min(MAX_LEN);
            let mut rest = &data[4..];
            while rest.len() >= 6 {
                let at = u64::from(le32(&rest[..4])) * UNIT;
                let n = usize::from(u16::from_le_bytes([rest[4], rest[5]]));
                let bytes = &rest[6..(6 + n).min(rest.len())];
                rest = &rest[6 + bytes.len()..];
                if at < len && !bytes.is_empty() {
                    let keep = bytes.len().min((len - at) as usize);
                    extents.push((at, bytes[..keep].to_vec()));
                }
            }
        }
        Self {
            len,
            extents,
            pos: 0,
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The whole image in memory, for APIs that take a slice or a path.
    pub fn to_vec(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.len as usize];
        for (at, bytes) in &self.extents {
            out[*at as usize..*at as usize + bytes.len()].copy_from_slice(bytes);
        }
        out
    }

    /// Write the image to `file` as a sparse file (holes stay holes).
    pub fn write_to(&self, file: &mut std::fs::File) -> io::Result<()> {
        use std::io::Write;
        file.set_len(0)?;
        file.set_len(self.len)?;
        for (at, bytes) in &self.extents {
            file.seek(SeekFrom::Start(*at))?;
            file.write_all(bytes)?;
        }
        Ok(())
    }
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let start = self.pos.min(self.len);
        let n = (buf.len() as u64).min(self.len - start) as usize;
        let buf = &mut buf[..n];
        buf.fill(0);
        let end = start + n as u64;
        for (at, bytes) in &self.extents {
            let (lo, hi) = (*at.max(&start), (at + bytes.len() as u64).min(end));
            if lo < hi {
                buf[(lo - start) as usize..(hi - start) as usize]
                    .copy_from_slice(&bytes[(lo - at) as usize..(hi - at) as usize]);
            }
        }
        self.pos = end;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek"))?;
        Ok(self.pos)
    }
}

/// Encode `image` (any length; it is padded to whole units): every run of non-zero 4 KiB
/// units becomes extents of at most 60 KiB.
pub fn encode(image: &[u8]) -> Vec<u8> {
    const MAX_RUN: usize = 15 * UNIT as usize;
    let unit = UNIT as usize;
    let units = image.len().div_ceil(unit);
    let mut out = (units as u32).to_le_bytes().to_vec();
    let nonzero = |u: usize| {
        image[u * unit..((u + 1) * unit).min(image.len())]
            .iter()
            .any(|&b| b != 0)
    };
    let mut u = 0;
    while u < units {
        if !nonzero(u) {
            u += 1;
            continue;
        }
        let start = u;
        while u < units && nonzero(u) && (u - start + 1) * unit <= MAX_RUN {
            u += 1;
        }
        let bytes = &image[start * unit..(u * unit).min(image.len())];
        out.extend_from_slice(&(start as u32).to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}
