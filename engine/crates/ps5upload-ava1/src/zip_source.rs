//! A zip archive as an AVA1 source. Entry reads inflate on demand.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ava1::gen;
use ava1::manifest::{self, Entry, Manifest};
use ava1::source::{ReadAt, Source, SourceMeta};

/// Where one entry's bytes live in the archive file, found once at open.
#[derive(Clone, Copy)]
struct Located {
    stored: bool,
    data_start: u64,
    compressed: u64,
    /// The central directory's CRC-32 of the decompressed bytes.
    crc: u32,
}

/// An entry's decompressed bytes do not match its stored CRC-32 (or the stream is
/// damaged or short). Carried inside an `InvalidData` `io::Error`; the upload turns
/// it into the terminal reason `ava1_zip_corrupt`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ZipCorrupt(pub String);

fn corrupt(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, ZipCorrupt(msg))
}

/// Whether `e` (or anything it wraps) is a [`ZipCorrupt`].
pub fn is_zip_corrupt(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if c.is::<ZipCorrupt>() {
            return true;
        }
        if let Some(io) = c.downcast_ref::<io::Error>() {
            if io.get_ref().is_some_and(|i| i.is::<ZipCorrupt>()) {
                return true;
            }
        }
        cur = c.source();
    }
    false
}

pub struct ZipSource {
    path: PathBuf,
    files: BTreeMap<String, (Located, SourceMeta)>,
    dirs: BTreeSet<String>,
}

impl ZipSource {
    pub fn open(path: &Path, excludes: &[String]) -> io::Result<(Manifest, Self)> {
        let f = std::fs::File::open(path)?;
        let mut zip = zip::ZipArchive::new(f).map_err(invalid_zip)?;
        let mut files = BTreeMap::new();
        let mut dirs = BTreeSet::new();
        for i in 0..zip.len() {
            let entry = zip.by_index_raw(i).map_err(invalid_zip)?;
            let name = entry.name().trim_end_matches('/');
            if name.is_empty() {
                continue;
            }
            manifest::check_path(name)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{name}: {e}")))?;
            if ps5upload_core::excludes::is_excluded_strings(Path::new(name), excludes) {
                continue;
            }
            let parts: Vec<_> = name.split('/').collect();
            let mut parent = String::new();
            for component in &parts[..parts.len() - 1] {
                if !parent.is_empty() {
                    parent.push('/');
                }
                parent.push_str(component);
                dirs.insert(parent.clone());
            }
            if entry.is_dir() {
                dirs.insert(name.to_owned());
            } else {
                let stored = match entry.compression() {
                    zip::CompressionMethod::Stored => true,
                    zip::CompressionMethod::Deflated => false,
                    other => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("{name}: unsupported zip compression {other:?}"),
                        ))
                    }
                };
                if entry.encrypted() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{name}: encrypted zip entries are not supported"),
                    ));
                }
                let data_start = entry.data_start().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{name}: no data offset"),
                    )
                })?;
                files.insert(
                    name.to_owned(),
                    (
                        Located {
                            stored,
                            data_start,
                            compressed: entry.compressed_size(),
                            crc: entry.crc32(),
                        },
                        SourceMeta {
                            size: entry.size(),
                            mtime: entry
                                .last_modified()
                                .map(|t| {
                                    crate::archive_time::civil_to_unix(
                                        t.year().into(),
                                        t.month().into(),
                                        t.day().into(),
                                        t.hour().into(),
                                        t.minute().into(),
                                        t.second().into(),
                                    )
                                })
                                .unwrap_or(0),
                            mode: entry.unix_mode().unwrap_or(0o644) & 0o7777,
                            is_dir: false,
                        },
                    ),
                );
            }
        }
        if files.keys().any(|p| dirs.contains(p)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "zip path is both file and directory",
            ));
        }
        let mut entries = Vec::with_capacity(files.len() + dirs.len());
        for d in &dirs {
            entries.push(Entry {
                kind: gen::ENTRY_DIR,
                mode: 0o755,
                size: 0,
                mtime: 0,
                path: d.clone(),
                root: None,
            });
        }
        for (p, (_, m)) in &files {
            entries.push(Entry {
                kind: gen::ENTRY_FILE,
                mode: m.mode,
                size: m.size,
                mtime: m.mtime,
                path: p.clone(),
                root: None,
            });
        }
        // Match manifest::walk's depth-first component ordering.
        entries.sort_by(|a, b| a.path.split('/').cmp(b.path.split('/')));
        Ok((
            Manifest { entries },
            Self {
                path: path.to_owned(),
                files,
                dirs,
            },
        ))
    }
}

/// A damaged deflate stream is corruption; any other read failure is an ordinary I/O error.
fn inflate_error(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::InvalidData {
        corrupt(format!("zip entry is damaged: {e}"))
    } else {
        e
    }
}

/// A zip library error as an `io::Error`: its own I/O errors keep their kind (so a read that
/// failed is not mistaken for a bad archive); everything else is a format problem.
fn invalid_zip(e: zip::result::ZipError) -> io::Error {
    match e {
        zip::result::ZipError::Io(io) => io,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

/// One open entry. Reads are sequential in practice (`run_upload` walks a file front
/// to back), so the inflater is kept between calls and only restarted when a read
/// goes backwards: total work is O(entry), not O(entry x reads).
pub struct ZipEntryReader {
    file: BufReader<std::fs::File>,
    at: Located,
    size: u64,
    inflater: Option<flate2::read::DeflateDecoder<io::Take<BufReader<std::fs::File>>>>,
    /// Uncompressed offset the inflater is at.
    pos: u64,
    restarts: u32,
    /// CRC-32 of the decompressed stream from offset 0 up to `pos`; meaningful only
    /// while `tracking` (the reader has seen every byte since the last restart).
    crc: flate2::Crc,
    tracking: bool,
    /// A stored entry's CRC-32 has been checked over all of it.
    verified: bool,
}

impl ZipEntryReader {
    /// How many times the inflater was started (1 for a front-to-back read).
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    fn start(&mut self) -> io::Result<()> {
        let mut f = BufReader::with_capacity(256 << 10, self.file.get_ref().try_clone()?);
        f.seek(SeekFrom::Start(self.at.data_start))?;
        self.inflater = Some(flate2::read::DeflateDecoder::new(
            f.take(self.at.compressed),
        ));
        self.pos = 0;
        self.crc = flate2::Crc::new();
        self.tracking = true;
        self.restarts += 1;
        Ok(())
    }
}

impl ReadAt for ZipEntryReader {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let r = self.read_at_inner(off, buf);
        if r.is_err() {
            // A failed read leaves the decoder mid-stream at an unknown spot: a retry
            // must start over, never continue from there.
            self.inflater = None;
        }
        r
    }
}

impl ZipEntryReader {
    fn read_at_inner(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || off >= self.size {
            return Ok(0);
        }
        if self.at.stored {
            // A stored entry's bytes are exactly its size, and no read passes them.
            let want = (buf.len() as u64).min(self.size - off) as usize;
            if !self.verified {
                if off == 0 {
                    self.crc = flate2::Crc::new();
                    self.pos = 0;
                    self.tracking = true;
                }
                if !(self.tracking && off == self.pos) {
                    // A read that is not the next piece of a front-to-back pass (a resume
                    // sends only the groups the console lacks): the running CRC cannot vouch
                    // for this entry, so check all of it once before any byte of it is sent.
                    self.verify_whole_stored()?;
                }
            }
            self.file.seek(SeekFrom::Start(self.at.data_start + off))?;
            self.file
                .read_exact(&mut buf[..want])
                .map_err(|e| match e.kind() {
                    io::ErrorKind::UnexpectedEof => {
                        corrupt("zip entry ends before its declared size".into())
                    }
                    _ => e,
                })?;
            if !self.verified {
                self.crc.update(&buf[..want]);
                self.pos += want as u64;
                self.verify_at_end()?;
            }
            return Ok(want);
        }
        if self.inflater.is_none() || off < self.pos {
            self.start()?;
        }
        let dec = self.inflater.as_mut().expect("started above");
        let mut scratch = [0u8; 16 << 10];
        while self.pos < off {
            let n = ((off - self.pos) as usize).min(scratch.len());
            let got = dec.read(&mut scratch[..n]).map_err(inflate_error)?;
            if got == 0 {
                return Err(corrupt("zip entry ends before its declared size".into()));
            }
            self.crc.update(&scratch[..got]);
            self.pos += got as u64;
        }
        // Fill the buffer: a deflate stream yields short reads, and callers treat a
        // short read as the end of the file.
        let want = (buf.len() as u64).min(self.size - off) as usize;
        let mut filled = 0;
        while filled < want {
            let got = dec.read(&mut buf[filled..want]).map_err(inflate_error)?;
            if got == 0 {
                return Err(corrupt("zip entry ends before its declared size".into()));
            }
            self.crc.update(&buf[filled..filled + got]);
            filled += got;
        }
        self.pos += filled as u64;
        self.verify_at_end()?;
        Ok(filled)
    }

    /// Once a full pass has been read, its CRC must equal the directory's. A pass is
    /// verified exactly once: a restart (backwards read) begins a new one.
    fn verify_at_end(&mut self) -> io::Result<()> {
        if self.tracking && self.pos == self.size {
            let got = self.crc.sum();
            if got != self.at.crc {
                return Err(corrupt(format!(
                    "zip entry CRC-32 mismatch (stored {:08x}, computed {got:08x})",
                    self.at.crc
                )));
            }
            self.verified = true;
        }
        Ok(())
    }

    /// Reads a stored entry once, front to back, and checks its CRC-32 against the directory's.
    /// Leaves the entry marked verified, so it happens once however the reads then come.
    fn verify_whole_stored(&mut self) -> io::Result<()> {
        let mut f = self.file.get_ref().try_clone()?;
        f.seek(SeekFrom::Start(self.at.data_start))?;
        let mut f = f.take(self.size);
        let mut crc = flate2::Crc::new();
        let mut chunk = vec![0u8; 256 << 10];
        let mut left = self.size;
        while left > 0 {
            let n = f.read(&mut chunk)?;
            if n == 0 {
                return Err(corrupt("zip entry ends before its declared size".into()));
            }
            crc.update(&chunk[..n]);
            left -= n as u64;
        }
        if crc.sum() != self.at.crc {
            return Err(corrupt(format!(
                "zip entry CRC-32 mismatch (stored {:08x}, computed {:08x})",
                self.at.crc,
                crc.sum()
            )));
        }
        self.verified = true;
        self.tracking = false;
        Ok(())
    }
}

impl ZipSource {
    /// `Source::open` with the concrete reader (its `restarts` is a test seam).
    pub fn open_entry(&self, rel: &str) -> io::Result<ZipEntryReader> {
        let (at, meta) = self
            .files
            .get(rel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))?;
        if at.stored && at.compressed != meta.size {
            return Err(corrupt(format!(
                "{rel}: a stored entry of {} bytes is {} bytes in the archive",
                meta.size, at.compressed
            )));
        }
        Ok(ZipEntryReader {
            file: BufReader::with_capacity(256 << 10, std::fs::File::open(&self.path)?),
            at: *at,
            size: meta.size,
            inflater: None,
            pos: 0,
            restarts: 0,
            crc: flate2::Crc::new(),
            tracking: true,
            verified: false,
        })
    }
}

impl Source for ZipSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        Ok(Box::new(self.open_entry(rel)?))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        if !rel.is_empty() && !self.dirs.contains(rel) {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_owned()));
        }
        let prefix = if rel.is_empty() {
            String::new()
        } else {
            format!("{rel}/")
        };
        let mut out = BTreeMap::new();
        for d in &self.dirs {
            if let Some(rest) = d.strip_prefix(&prefix) {
                if !rest.contains('/') {
                    out.insert(
                        rest.to_owned(),
                        SourceMeta {
                            is_dir: true,
                            mode: 0o755,
                            ..SourceMeta::default()
                        },
                    );
                }
            }
        }
        for (p, (_, meta)) in &self.files {
            if let Some(rest) = p.strip_prefix(&prefix) {
                if !rest.contains('/') {
                    out.insert(rest.to_owned(), *meta);
                }
            }
        }
        Ok(out.into_iter().collect())
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        if let Some((_, meta)) = self.files.get(rel) {
            return Ok(*meta);
        }
        if rel.is_empty() || self.dirs.contains(rel) {
            return Ok(SourceMeta {
                is_dir: true,
                mode: 0o755,
                ..SourceMeta::default()
            });
        }
        Err(io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn body(n: usize, k: u8) -> Vec<u8> {
        (0..n)
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(k))
            .collect()
    }

    /// A stored archive of `a` then `b`; returns its path and where `a`'s data starts.
    fn stored_zip(tag: &str, a: &[u8], b: &[u8]) -> (PathBuf, u64) {
        let p = std::env::temp_dir().join(format!("p5a-zs-{tag}-{}.zip", std::process::id()));
        let mut z = zip::ZipWriter::new(std::fs::File::create(&p).unwrap());
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("a.bin", o).unwrap();
        z.write_all(a).unwrap();
        z.start_file("b.bin", o).unwrap();
        z.write_all(b).unwrap();
        z.finish().unwrap();
        let (_, s) = ZipSource::open(&p, &[]).unwrap();
        let start = s.files["a.bin"].0.data_start;
        (p, start)
    }

    #[test]
    fn a_corrupt_stored_entry_is_caught_by_a_resume_that_reads_only_its_tail() {
        let (a, b) = (body(300_000, 1), body(1000, 2));
        let (p, start) = stored_zip("tail", &a, &b);
        let mut bytes = std::fs::read(&p).unwrap();
        bytes[(start + 5) as usize] ^= 0xff; // bit rot far from the part that gets read
        std::fs::write(&p, &bytes).unwrap();
        let (_, s) = ZipSource::open(&p, &[]).unwrap();
        let mut r = s.open_entry("a.bin").unwrap();
        let mut buf = vec![0u8; 100_000];
        // The resume sends only the last group: nothing before it is ever read.
        let e = r.read_at(200_000, &mut buf).unwrap_err();
        assert!(is_zip_corrupt(&e), "{e}");
        // A clean entry passes the same resume.
        let (p2, _) = stored_zip("tail-ok", &a, &b);
        let (_, s2) = ZipSource::open(&p2, &[]).unwrap();
        let mut r2 = s2.open_entry("a.bin").unwrap();
        assert_eq!(r2.read_at(200_000, &mut buf).unwrap(), 100_000);
        assert_eq!(&buf[..], &a[200_000..]);
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(p2);
    }

    #[test]
    fn a_stored_read_never_crosses_into_the_next_entry_and_sizes_must_agree() {
        let (a, b) = (body(5000, 1), body(5000, 2));
        let (p, _) = stored_zip("bound", &a, &b);
        let (_, s) = ZipSource::open(&p, &[]).unwrap();
        let mut r = s.open_entry("a.bin").unwrap();
        let mut buf = vec![0u8; 8000];
        assert_eq!(r.read_at(4000, &mut buf).unwrap(), 1000);
        assert_eq!(&buf[..1000], &a[4000..]);
        // An entry whose uncompressed size disagrees with its stored length is corrupt.
        let mut s2 = ZipSource::open(&p, &[]).unwrap().1;
        s2.files.get_mut("a.bin").unwrap().1.size = 6000;
        let e = s2.open_entry("a.bin").err().expect("must refuse");
        assert!(is_zip_corrupt(&e), "{e}");
        let _ = std::fs::remove_file(p);
    }
}
