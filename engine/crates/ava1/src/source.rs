//! Where a sender reads from (SPEC.md §11). Paths are relative to the source's root and
//! '/'-separated; "" is the root itself.
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

pub trait ReadAt: Send {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize>;
}

/// Reads until `buf` is full or the file ends; returns the bytes read.
pub fn read_full_at(r: &mut dyn ReadAt, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read_at(off + n as u64, &mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceMeta {
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub mtime: u64,
    pub mode: u32,
    pub is_dir: bool,
}

pub trait Source: Send + Sync {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>>;
    /// Direct children of `rel`: (name, metadata).
    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>>;
    fn stat(&self, rel: &str) -> io::Result<SourceMeta>;
    /// Called once when the upload that reads this source is ending, before its readers
    /// are joined. A source whose reads can park on something other than a disk (a
    /// relay waiting for another connection's bytes) uses it to wake them, so an ended
    /// job does not wait for a stall bound. Reads after `close` may fail. Must not block.
    fn close(&self) {}
}

/// Any `Read + Seek` as a `ReadAt` (seeks only when the position differs).
pub struct SeekReader<T> {
    inner: T,
    pos: Option<u64>,
}

impl<T> SeekReader<T> {
    pub fn new(inner: T) -> Self {
        Self { inner, pos: None }
    }
}

impl<T: Read + Seek + Send> ReadAt for SeekReader<T> {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos != Some(off) {
            self.inner.seek(SeekFrom::Start(off))?;
        }
        let n = self.inner.read(buf)?;
        self.pos = Some(off + n as u64);
        Ok(n)
    }
}

/// This computer's disk under `root`.
pub struct LocalSource {
    root: PathBuf,
}

impl LocalSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }
}

pub(crate) fn meta_of(m: &std::fs::Metadata) -> SourceMeta {
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    #[cfg(unix)]
    let mode = std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o7777;
    #[cfg(not(unix))]
    let mode = if m.is_dir() { 0o755 } else { 0o644 };
    SourceMeta {
        size: if m.is_dir() { 0 } else { m.len() },
        mtime,
        mode,
        is_dir: m.is_dir(),
    }
}

impl Source for LocalSource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        Ok(Box::new(SeekReader::new(std::fs::File::open(
            self.path(rel),
        )?)))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(self.path(rel))? {
            let e = e?;
            let Ok(name) = e.file_name().into_string() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "a file name is not UTF-8",
                ));
            };
            // Symlinks are followed (metadata, not symlink_metadata), as the older uploader did: a link
            // to a file elsewhere is read under its in-tree name, a dangling one fails the
            // walk, and a cycle fails at the OS's symlink limit (MAX_PATH at the latest).
            out.push((name, meta_of(&std::fs::metadata(e.path())?)));
        }
        Ok(out)
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        Ok(meta_of(&std::fs::metadata(self.path(rel))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A `ReadAt` over a byte string that caps each read at `chunk` bytes and
    /// returns `interrupts` Interrupted errors before any data.
    struct ChopReader {
        data: Vec<u8>,
        chunk: usize,
        interrupts: usize,
    }

    impl ReadAt for ChopReader {
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupts > 0 {
                self.interrupts -= 1;
                return Err(io::Error::new(io::ErrorKind::Interrupted, "injected"));
            }
            let off = off as usize;
            if off >= self.data.len() {
                return Ok(0);
            }
            let n = self.chunk.min(buf.len()).min(self.data.len() - off);
            buf[..n].copy_from_slice(&self.data[off..off + n]);
            Ok(n)
        }
    }

    #[test]
    fn read_full_at_accumulates_short_reads_and_retries_interrupts() {
        let data: Vec<u8> = (0..100u8).collect();
        let mut r = ChopReader {
            data: data.clone(),
            chunk: 3,
            interrupts: 2,
        };
        let mut buf = [0u8; 80];
        assert_eq!(read_full_at(&mut r, 7, &mut buf).unwrap(), 80);
        assert_eq!(&buf[..], &data[7..87]);
    }

    #[test]
    fn read_full_at_stops_at_end_of_file() {
        let data: Vec<u8> = (0..100u8).collect();
        let mut r = ChopReader {
            data: data.clone(),
            chunk: 10,
            interrupts: 0,
        };
        let mut buf = [0u8; 200];
        assert_eq!(read_full_at(&mut r, 50, &mut buf).unwrap(), 50);
        assert_eq!(&buf[..50], &data[50..]);
        assert_eq!(read_full_at(&mut r, 150, &mut buf).unwrap(), 0);
    }

    #[test]
    fn seek_reader_reads_at_offsets_and_reuses_the_position() {
        let data: Vec<u8> = (0..16u8).collect();
        let mut r = SeekReader::new(Cursor::new(data.clone()));
        let mut buf = [0u8; 4];
        r.read_at(5, &mut buf).unwrap();
        assert_eq!(&buf, &data[5..9]);
        // Same offset: no seek, but the read still starts there.
        r.read_at(5, &mut buf).unwrap();
        assert_eq!(&buf, &data[5..9]);
        // Jump back, then continue from the new position without a further seek.
        r.read_at(0, &mut buf).unwrap();
        assert_eq!(&buf, &data[0..4]);
        r.read_at(4, &mut buf).unwrap();
        assert_eq!(&buf, &data[4..8]);
    }
}
