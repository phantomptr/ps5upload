//! Reading a console's files in place over AVA1 (the converter's `ps5://` source).
//!
//! Every call is a management call (`fs.stat`, `fs.list`, `fs.read`) on the pool's session,
//! the same path every other management call takes, so an unreachable or unpaired console fails
//! with the same `helper_not_ava1` / `not_paired` message an upload gives. Nothing here talks to
//! the console's FTP server.
//!
//! The reader keeps a window of the file in memory. A window is fetched as several `fs.read`
//! calls at once, and while the parser consumes one window the next is already being fetched,
//! as long as the reads stay sequential.

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ps5upload_core::fs_ops::{self, ListDirOptions};
use ps5upload_fpkg::remote_source::RemoteFiles;
use ps5upload_fpkg::ReadSeek;

/// One `fs.read` call asks for at most this much (the management read ceiling).
const CHUNK: u64 = 2 * 1024 * 1024;
/// Chunks fetched at once for one window.
const PARALLEL: u64 = 4;
/// What is held in memory and fetched ahead.
const WINDOW: u64 = CHUNK * PARALLEL;
/// Failed reads repeated before the error is the caller's.
const RETRIES: u32 = 2;
/// Directory listing page (the console's own ceiling).
const LIST_PAGE: u64 = 256;

/// Reads `(offset, len)` of one file; may return fewer bytes than asked (never more), and an
/// empty reply means the end of the file.
pub(crate) type Fetch = Arc<dyn Fn(u64, u64) -> io::Result<Vec<u8>> + Send + Sync>;

/// A failure that repeating the read cannot fix.
fn permanent(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::NotFound {
        return true;
    }
    let m = e.to_string();
    m.contains("helper_not_ava1") || m.contains("not_paired")
}

fn to_io(e: anyhow::Error) -> io::Error {
    let text = format!("{e:#}");
    let kind = if fs_ops::is_not_found(&text) {
        io::ErrorKind::NotFound
    } else {
        io::ErrorKind::Other
    };
    io::Error::new(kind, text)
}

/// `want` bytes at `offset`, looped until they arrived or the file ended, each call retried
/// on a transient failure. A short reply is looped, not taken as the end.
fn fetch_chunk(
    fetch: &Fetch,
    offset: u64,
    want: u64,
    retry_pause: Duration,
) -> io::Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::with_capacity(want as usize);
    let mut failures = 0;
    while (out.len() as u64) < want {
        let at = offset + out.len() as u64;
        match fetch(at, want - out.len() as u64) {
            Ok(b) if b.is_empty() => break,
            Ok(b) => {
                failures = 0;
                out.extend_from_slice(&b);
            }
            Err(e) if permanent(&e) || failures >= RETRIES => return Err(e),
            Err(_) => {
                failures += 1;
                std::thread::sleep(retry_pause * failures);
            }
        }
    }
    out.truncate(want as usize);
    Ok(out)
}

/// `want` bytes at `start`, fetched as up to [`PARALLEL`] chunks at once and joined in order.
/// If a chunk came back short, what follows it is dropped, so the result is always a prefix of
/// the range with no hole.
fn fetch_window(
    fetch: &Fetch,
    start: u64,
    want: u64,
    retry_pause: Duration,
) -> io::Result<Vec<u8>> {
    if want <= CHUNK {
        return fetch_chunk(fetch, start, want, retry_pause);
    }
    let spans: Vec<(u64, u64)> = (0..want.div_ceil(CHUNK))
        .map(|i| (start + i * CHUNK, CHUNK.min(want - i * CHUNK)))
        .collect();
    let parts: Vec<io::Result<Vec<u8>>> = std::thread::scope(|s| {
        let handles: Vec<_> = spans
            .iter()
            .map(|&(at, n)| s.spawn(move || fetch_chunk(fetch, at, n, retry_pause)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(io::Error::other("a console read thread panicked")))
            })
            .collect()
    });
    let mut out = Vec::with_capacity(want as usize);
    for (part, &(_, n)) in parts.into_iter().zip(&spans) {
        let b = part?;
        let short = (b.len() as u64) < n;
        out.extend_from_slice(&b);
        if short {
            break;
        }
    }
    Ok(out)
}

struct Ahead {
    start: u64,
    task: JoinHandle<io::Result<Vec<u8>>>,
}

/// A console file as a `ReadSeek`: `len` from `fs.stat`, bytes from `fs.read`.
pub(crate) struct ConsoleReader {
    len: u64,
    pos: u64,
    buf: Vec<u8>,
    buf_start: u64,
    /// Where the last fill ended; a fill that starts there is a sequential read.
    last_end: u64,
    fetch: Fetch,
    ahead: Option<Ahead>,
    retry_pause: Duration,
}

impl ConsoleReader {
    pub(crate) fn new(len: u64, fetch: Fetch) -> Self {
        Self {
            len,
            pos: 0,
            buf: Vec::new(),
            buf_start: 0,
            last_end: 0,
            fetch,
            ahead: None,
            retry_pause: Duration::from_millis(150),
        }
    }

    #[cfg(test)]
    fn with_retry_pause(mut self, d: Duration) -> Self {
        self.retry_pause = d;
        self
    }

    fn fill(&mut self) -> io::Result<()> {
        let want = WINDOW.min(self.len - self.pos);
        let sequential = self.pos == self.last_end;
        let prefetched = match self.ahead.take() {
            // A fetch that failed is repeated here, where the caller sees the final error.
            Some(a) if a.start == self.pos => a.task.join().ok().and_then(Result::ok),
            _ => None,
        };
        let data = match prefetched {
            Some(d) if !d.is_empty() => d,
            _ => fetch_window(&self.fetch, self.pos, want, self.retry_pause)?,
        };
        if data.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the console returned no data at {}", self.pos),
            ));
        }
        self.buf_start = self.pos;
        self.last_end = self.pos + data.len() as u64;
        self.buf = data;
        if sequential && self.last_end < self.len {
            let (fetch, start, pause) = (self.fetch.clone(), self.last_end, self.retry_pause);
            let n = WINDOW.min(self.len - start);
            self.ahead = Some(Ahead {
                start,
                task: std::thread::spawn(move || fetch_window(&fetch, start, n, pause)),
            });
        }
        Ok(())
    }
}

impl Read for ConsoleReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let buf_end = self.buf_start + self.buf.len() as u64;
        if self.pos < self.buf_start || self.pos >= buf_end {
            self.fill()?;
        }
        let at = (self.pos - self.buf_start) as usize;
        let n = out.len().min(self.buf.len() - at);
        out[..n].copy_from_slice(&self.buf[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ConsoleReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let next = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            ));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

/// The files of one console, over management calls. `addr` is the console's host.
pub(crate) struct ConsoleFiles {
    pub(crate) addr: String,
    pub(crate) label: String,
}

impl ConsoleFiles {
    fn stat_path(&self, path: &str) -> io::Result<fs_ops::PathStat> {
        fs_ops::fs_stat(&self.addr, path).map_err(to_io)
    }
}

impl RemoteFiles for ConsoleFiles {
    fn open(&self, path: &str) -> io::Result<Box<dyn ReadSeek>> {
        let st = self.stat_path(path)?;
        if st.kind == "dir" {
            return Err(io::Error::other(format!("{path} is a folder, not a file")));
        }
        let (addr, p) = (self.addr.clone(), path.to_string());
        let fetch: Fetch =
            Arc::new(move |off, len| fs_ops::fs_read(&addr, &p, off, len).map_err(to_io));
        Ok(Box::new(ConsoleReader::new(st.size, fetch)))
    }

    fn stat(&self, path: &str) -> io::Result<(u64, bool)> {
        let st = self.stat_path(path)?;
        Ok((st.size, st.kind == "dir"))
    }

    fn list(&self, dir: &str) -> io::Result<Vec<(String, bool, u64)>> {
        let mut out = Vec::new();
        let mut offset = 0u64;
        loop {
            let page = fs_ops::list_dir(
                &self.addr,
                dir,
                ListDirOptions {
                    offset,
                    limit: LIST_PAGE,
                },
            )
            .map_err(to_io)?;
            let n = page.entries.len() as u64;
            out.extend(
                page.entries
                    .into_iter()
                    .map(|e| (e.name, e.kind == "dir", e.size)),
            );
            offset += n;
            // A short page that was not cut off is the end; a full one means ask again.
            if n == 0 || (!page.truncated && n < LIST_PAGE) {
                return Ok(out);
            }
        }
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const NO_PAUSE: Duration = Duration::from_millis(1);

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn from_bytes(bytes: Vec<u8>) -> Fetch {
        Arc::new(move |off, len| {
            let a = (off as usize).min(bytes.len());
            let b = (a + len as usize).min(bytes.len());
            Ok(bytes[a..b].to_vec())
        })
    }

    fn reader(bytes: &[u8], fetch: Fetch) -> ConsoleReader {
        ConsoleReader::new(bytes.len() as u64, fetch).with_retry_pause(NO_PAUSE)
    }

    #[test]
    fn sequential_reads_return_the_whole_file_across_windows() {
        // Three windows and a ragged tail.
        let bytes = data((WINDOW * 2 + 12345) as usize);
        let mut r = reader(&bytes, from_bytes(bytes.clone()));
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn seek_then_read_matches_the_file_including_from_the_end() {
        let bytes = data((WINDOW + 8192) as usize);
        let mut r = reader(&bytes, from_bytes(bytes.clone()));
        assert_eq!(r.seek(SeekFrom::End(0)).unwrap(), bytes.len() as u64);
        let mut tail = [0u8; 100];
        r.seek(SeekFrom::End(-100)).unwrap();
        r.read_exact(&mut tail).unwrap();
        assert_eq!(&tail[..], &bytes[bytes.len() - 100..]);
        // Backwards into the first window, then a jump into the second.
        for at in [5u64, WINDOW + 3, 0, CHUNK + 1] {
            r.seek(SeekFrom::Start(at)).unwrap();
            let mut b = [0u8; 4096];
            r.read_exact(&mut b).unwrap();
            assert_eq!(&b[..], &bytes[at as usize..at as usize + 4096], "at {at}");
        }
        assert_eq!(r.stream_position().unwrap(), CHUNK + 1 + 4096);
        assert!(r.seek(SeekFrom::Current(-(1 << 40))).is_err());
        // Reading at the end is end-of-file, not an error.
        r.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(r.read(&mut [0u8; 8]).unwrap(), 0);
    }

    #[test]
    fn a_short_reply_is_looped_not_truncated() {
        let bytes = data((CHUNK * 3 + 99) as usize);
        let inner = from_bytes(bytes.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        // The console answers at most 100 KiB per call.
        let fetch: Fetch = Arc::new(move |off, len| {
            c.fetch_add(1, Ordering::SeqCst);
            inner(off, len.min(100 * 1024))
        });
        let mut r = reader(&bytes, fetch);
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, bytes);
        assert!(calls.load(Ordering::SeqCst) > 30);
    }

    #[test]
    fn a_dropped_read_is_retried_and_the_data_is_still_exact() {
        let bytes = data((CHUNK + 5) as usize);
        let inner = from_bytes(bytes.clone());
        let fail = Arc::new(AtomicUsize::new(2));
        let f = fail.clone();
        let fetch: Fetch = Arc::new(move |off, len| {
            if f.load(Ordering::SeqCst) > 0 {
                f.fetch_sub(1, Ordering::SeqCst);
                return Err(io::Error::other("connection reset"));
            }
            inner(off, len)
        });
        let mut r = reader(&bytes, fetch);
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn a_read_that_keeps_failing_is_the_callers_error() {
        let fetch: Fetch = Arc::new(|_, _| Err(io::Error::other("connection reset")));
        let mut r = ConsoleReader::new(10, fetch).with_retry_pause(NO_PAUSE);
        let e = r.read(&mut [0u8; 4]).unwrap_err();
        assert!(e.to_string().contains("connection reset"), "{e}");
    }

    #[test]
    fn a_pairing_or_helper_failure_is_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let fetch: Fetch = Arc::new(move |_, _| {
            c.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other(
                "payload rejected FS_READ(/x): not_paired: pair this app",
            ))
        });
        let mut r = ConsoleReader::new(10, fetch).with_retry_pause(NO_PAUSE);
        assert!(r.read(&mut [0u8; 4]).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_next_window_is_fetched_while_the_first_is_consumed() {
        let bytes = data((WINDOW * 3) as usize);
        let inner = from_bytes(bytes.clone());
        let starts = Arc::new(Mutex::new(Vec::new()));
        let s = starts.clone();
        let fetch: Fetch = Arc::new(move |off, len| {
            s.lock().unwrap().push(off);
            inner(off, len)
        });
        let mut r = reader(&bytes, fetch);
        let mut one = [0u8; 1];
        r.read_exact(&mut one).unwrap();
        // Only the first byte was consumed, yet the second window is already being asked for.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !starts.lock().unwrap().contains(&WINDOW) {
            assert!(std::time::Instant::now() < deadline, "no read-ahead");
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, bytes[1..]);
    }

    #[test]
    fn a_hole_in_a_parallel_window_is_cut_off_not_returned() {
        // The console's file ends 10 bytes into chunk 1, but a stale reply still answers for
        // chunk 3: nothing after the short chunk may be glued on.
        let bytes = data(WINDOW as usize);
        let inner = from_bytes(bytes.clone());
        let fetch: Fetch = Arc::new(move |off, len| {
            if off == CHUNK {
                return inner(off, 10);
            }
            if off > CHUNK && off < CHUNK * 3 {
                return Ok(Vec::new());
            }
            inner(off, len)
        });
        let got = fetch_window(&fetch, 0, WINDOW, NO_PAUSE).unwrap();
        assert_eq!(got, bytes[..(CHUNK + 10) as usize]);
    }

    #[test]
    fn the_file_ending_early_stops_a_window_at_the_real_end() {
        // stat said 100 bytes more than the console has.
        let bytes = data(1000);
        let mut r = ConsoleReader::new(1100, from_bytes(bytes.clone())).with_retry_pause(NO_PAUSE);
        let mut got = Vec::new();
        let e = r.read_to_end(&mut got).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(got, bytes);
    }
}
