//! Console → computer downloads over AVA1: a folder or file landing on disk
//! (`to_local`) and a folder or file streamed straight into a `.zip` (`to_zip`).
//! Blocking, like the upload adapters: call from `spawn_blocking` or a plain thread.
//!
//! The console check (`console::require_ava1`), the terminal-versus-retryable split, the
//! `error_reason` words and the retry/backoff loop are the upload adapters' own
//! (`upload.rs`): a download differs only in which side holds the sink.

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ava1::gen;
use ava1::journal::{self, State};
use ava1::manifest::{self, Manifest};
use ava1::packlog::LoggedGroup;
use ava1::recv::{download_job, is_fd_exhausted, LocalSink, RecvOptions, Sink};
use ava1::send::{Progress, SendError};
use ps5upload_core::download::DownloadKind;
use zip::write::SimpleFileOptions;

use crate::pool::{pool, Pool};
use crate::upload::{refusal, wait, SessionGate, UploadFailure, STALL_LIMIT};
use crate::zip_stored::StoredZipSink;

/// The grant a download extends to the console (SPEC.md §12.4).
const CREDIT: u64 = 64 << 20;

/// The counters the engine's 200 ms ticker reads. All are stored absolutely (never
/// `fetch_add`), and only ever raised: a counter that steps backwards reads as a bug
/// to the person watching it. `total` is the ticker's dynamic total
/// (`TickerContext::dynamic_total_bytes`): the AVA1 path only learns the size when the
/// console's manifest arrives, so it is filled from the manifest, not from a Status.
#[derive(Clone, Default)]
pub struct Counters {
    pub bytes: Arc<AtomicU64>,
    pub files: Arc<AtomicU64>,
    pub files_finalized: Arc<AtomicU64>,
    pub bytes_finalized: Arc<AtomicU64>,
    pub total: Option<Arc<AtomicU64>>,
}

/// Copies AVA1's progress into the counters every 200 ms until dropped, and once more on
/// drop. Mapping (the upload bridge's rule): AVA1 only knows a file is done when it is
/// durable, so `bytes` and `bytes_finalized` ← `bytes_durable`, `files` and
/// `files_finalized` ← `files_durable`. `base_*` carry the work of earlier attempts
/// (zip only): the counter is monotonic "work done" and, by design, may end above the
/// archive's final byte count after a restart.
struct Ticker {
    handle: tokio::task::AbortHandle,
    state: Arc<TickState>,
}

struct TickState {
    p: Arc<Progress>,
    c: Counters,
    base_bytes: u64,
    base_files: u64,
}

impl TickState {
    fn store(&self) {
        let bytes = self.base_bytes + self.p.bytes_durable.load(Ordering::Relaxed);
        let files = self.base_files + self.p.files_durable.load(Ordering::Relaxed);
        self.c.bytes.fetch_max(bytes, Ordering::Relaxed);
        self.c.bytes_finalized.fetch_max(bytes, Ordering::Relaxed);
        self.c.files.fetch_max(files, Ordering::Relaxed);
        self.c.files_finalized.fetch_max(files, Ordering::Relaxed);
        if let Some(t) = &self.c.total {
            let total = self.p.bytes_total.load(Ordering::Relaxed);
            if total > 0 {
                t.store(total, Ordering::Release);
            }
        }
    }
}

impl Ticker {
    fn start(p: Arc<Progress>, c: &Counters, base_bytes: u64, base_files: u64) -> Ticker {
        let state = Arc::new(TickState {
            p,
            c: c.clone(),
            base_bytes,
            base_files,
        });
        let s = state.clone();
        let handle = tokio::spawn(async move {
            loop {
                s.store();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .abort_handle();
        Ticker { handle, state }
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        self.handle.abort();
        self.state.store();
    }
}

fn basename(src: &str) -> Result<&str> {
    let name = src.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if name.is_empty() || name == "." || name == ".." || name.contains('\\') {
        return Err(anyhow!("{src:?} has no usable file name"));
    }
    Ok(name)
}

const WINDOWS_RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

/// Why a path component cannot be written safely on every host, if it cannot.
fn host_unsafe_component(comp: &str) -> Option<&'static str> {
    // CONIN$ / CONOUT$ are device names too; a '$' alone is fine ("price$").
    let device_stem = comp.split('.').next().unwrap_or(comp).trim_end_matches(' ');
    if WINDOWS_RESERVED.contains(&device_stem.to_ascii_uppercase().as_str()) {
        return Some("a reserved device name");
    }
    if comp.contains(':') {
        return Some("a name with ':' (a drive or stream on Windows)");
    }
    if comp.ends_with('.') || comp.ends_with(' ') {
        return Some("a name ending in '.' or a space");
    }
    // The device name is the part before the first dot: `CON.txt` is still CON.
    let stem = comp.split('.').next().unwrap_or(comp).trim_end_matches(' ');
    let up = stem.to_ascii_uppercase();
    let numbered = |p: &str| {
        up.strip_prefix(p).is_some_and(|n| {
            let mut c = n.chars();
            matches!(
                (c.next(), c.next()),
                (Some('1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'), None)
            )
        })
    };
    if WINDOWS_RESERVED.contains(&up.as_str()) || numbered("COM") || numbered("LPT") {
        return Some("a reserved device name");
    }
    None
}

/// What the manifest of a download may look like (peer-supplied data: `Manifest::
/// from_pages` already ran `check_path` on every path; this adds the shape the request
/// promised and the one path rule `check_path` leaves to the host OS).
pub(crate) fn check_shape(m: &Manifest, single: bool) -> io::Result<()> {
    let bad = |why: String| io::Error::new(io::ErrorKind::InvalidData, why);
    for e in &m.entries {
        manifest::check_path(&e.path).map_err(|e| bad(e.to_string()))?;
        // A backslash is a separator on Windows: `a\..\b` would climb out of the root.
        if e.path.contains('\\') {
            return Err(bad(format!("{:?} contains a backslash", e.path)));
        }
        // Peer-supplied names are written on THIS computer, whatever it is. A ':' makes
        // `C:/x` a drive path and `C:evil` a drive-relative one on Windows (and an
        // alternate data stream on NTFS); the reserved device names and a trailing dot
        // or space name something other than the file asked for. Refused everywhere so
        // the console's data lands the same way on every host.
        for comp in e.path.split('/') {
            if let Some(why) = host_unsafe_component(comp) {
                return Err(bad(format!("{:?}: {why}", e.path)));
            }
        }
    }
    if single {
        let files = m
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .count();
        if m.entries.len() != 1 || files != 1 {
            return Err(bad(format!(
                "a single-file download received a manifest of {} entries",
                m.entries.len()
            )));
        }
    }
    Ok(())
}

/// `LocalSink` plus the checks above. The landing root is `dest_dir/<basename>` for both
/// kinds (the manifest's paths are root-relative), and every byte goes through a
/// `.ava-part` sibling in the destination's own directory, renamed once at the end.
struct CheckedSink {
    inner: LocalSink,
    root: PathBuf,
    single: bool,
}

impl Sink for CheckedSink {
    fn prepare(&self, m: &Manifest) -> io::Result<()> {
        check_shape(m, self.single)?;
        if self.single && self.root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} is a folder; a file cannot replace it",
                    self.root.display()
                ),
            ));
        }
        self.inner.prepare(m)
    }
    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        self.inner.write_at(id, off, data)
    }
    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        self.inner.write_whole(id, data)
    }
    fn write_whole_root(&self, id: u32, root: &[u8; 32], data: &[u8]) -> io::Result<()> {
        self.inner.write_whole_root(id, root, data)
    }
    fn sync(&self, ids: &[u32]) -> io::Result<()> {
        self.inner.sync(ids)
    }
    // The durable-by-log hooks go to the `LocalSink` too (the log is its decision: on by
    // default off macOS, `PS5UPLOAD_AVA1_LOG_SMALL` to override), or a download would never
    // take that path while the receiver believed it could.
    fn enable_log(&self, dir: &Path) {
        self.inner.enable_log(dir)
    }
    fn sync_batch(&self, small: &[u32], large: &[u32]) -> io::Result<Vec<LoggedGroup>> {
        self.inner.sync_batch(small, large)
    }
    fn batch_journaled(&self, groups: &[LoggedGroup]) {
        self.inner.batch_journaled(groups)
    }
    fn unswept(&self) -> usize {
        self.inner.unswept()
    }
    fn log_pressure(&self) -> bool {
        self.inner.log_pressure()
    }
    fn sweep(&self, force: bool) -> io::Result<Vec<u32>> {
        self.inner.sweep(force)
    }
    fn sweep_journaled(&self, ids: &[u32]) {
        self.inner.sweep_journaled(ids)
    }
    fn recover_log(&self, st: &State) -> io::Result<Vec<u32>> {
        self.inner.recover_log(st)
    }
    fn log_cleanup(&self) {
        self.inner.log_cleanup()
    }
    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read_at(id, off, buf)
    }
    fn commit(&self, id: u32) -> io::Result<()> {
        self.inner.commit(id)
    }
    fn finish(&self) -> io::Result<()> {
        self.inner.finish()
    }
    fn resume_key(&self) -> Option<(String, bool)> {
        self.inner.resume_key()
    }
}

/// The zip entry's name. A folder's entries sit under `<basename>/`; a single file's
/// entry is exactly `<basename>` (FTX2's `enumerate_download_set` puts the basename in
/// `rel_path` for a file and `download_to_zip_ex` writes it verbatim, so prefixing here
/// would produce `foo.pkg/foo.pkg`).
pub fn zip_entry_name(single: bool, basename: &str, rel: &str) -> String {
    if single {
        basename.to_owned()
    } else {
        format!("{basename}/{rel}")
    }
}

struct ZipState {
    m: Option<Arc<Manifest>>,
    zip: Option<zip::ZipWriter<BufWriter<std::fs::File>>>,
    /// The entry being written, and how much of it is in the archive.
    current: Option<u32>,
    written: u64,
    /// Highest id started: ordered delivery never goes back.
    last: Option<u32>,
    started: usize,
    finished: bool,
    /// Bytes of single-group files between their write and their commit. The receiver
    /// verifies such a file by reading it back (`read_at`) before it commits; a deflate
    /// stream cannot be read back, so the sink keeps the (at most one group) bytes
    /// itself. Larger files are verified from their outboards and need nothing.
    kept: std::collections::HashMap<u32, Vec<u8>>,
}

/// An ordered download appended into a `.zip`. Deflate entries cannot be seeked into, so
/// the receiver must deliver each file's bytes contiguously and in file order (the
/// ordered flag); anything else is a protocol violation, never garbage in the archive.
/// Writes `<dest>.ava-part` and renames over `dest` in `finish`, so the final path never
/// holds a half-written archive; an abandoned sink removes its part file.
/// Empty directories are not entries (FTX2's zip manifest holds files only). Empty
/// files are appended at `finish`: the receiver creates them up front, out of order,
/// and a zip can only have one entry open at a time.
pub struct ZipSink {
    dest: PathBuf,
    part: PathBuf,
    single: bool,
    base: String,
    st: Mutex<ZipState>,
}

fn zip_err(e: zip::result::ZipError) -> io::Error {
    io::Error::other(e)
}

/// A file arrived a second time (the receiver re-requested it after a verify mismatch).
/// A deflate stream cannot rewrite what it already holds, so the archive attempt is
/// abandoned and restarted — like a dropped connection — instead of being reported as
/// a bad manifest.
#[derive(Debug)]
struct ZipRestart(String);

impl std::fmt::Display for ZipRestart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ZipRestart {}

pub(crate) fn zip_restart(why: impl Into<String>) -> io::Error {
    io::Error::other(ZipRestart(why.into()))
}

/// True for the error a sink returns when a file it already wrote is sent again.
#[doc(hidden)]
pub fn is_zip_restart(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|inner| inner.is::<ZipRestart>())
}

/// A file that keeps failing verification must end the job, not restart forever.
const MAX_RETRY_RESTARTS: u32 = 3;
/// Waits for descriptors to free up before a download gives up on them.
const MAX_FD_WAITS: u32 = 12;

pub(crate) fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

impl ZipSink {
    /// A folder download: entries are `<prefix>/<root-relative path>`.
    pub fn new(path: PathBuf, prefix: &str) -> Self {
        Self::build(path, prefix, false)
    }

    /// A single-file download: the one entry is exactly `name`.
    pub fn single(path: PathBuf, name: &str) -> Self {
        Self::build(path, name, true)
    }

    fn build(dest: PathBuf, base: &str, single: bool) -> Self {
        let mut part = dest.clone().into_os_string();
        part.push(".ava-part");
        Self {
            dest,
            part: PathBuf::from(part),
            single,
            base: base.to_owned(),
            st: Mutex::new(ZipState {
                m: None,
                zip: None,
                current: None,
                written: 0,
                last: None,
                started: 0,
                finished: false,
                kept: Default::default(),
            }),
        }
    }

    fn opts() -> SimpleFileOptions {
        // zip64 so a single >4 GiB game file is encoded correctly (the FTX2 zip's rule).
        SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true)
    }

    fn append(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let m =
            st.m.clone()
                .ok_or_else(|| invalid("data before the manifest"))?;
        let e = m
            .entry(id)
            .filter(|e| e.kind == gen::ENTRY_FILE)
            .ok_or_else(|| invalid(format!("data for {id}, which is not a file")))?;
        if e.size == 0 {
            // Written at `finish`; the receiver's up-front empty write lands here.
            return if data.is_empty() {
                Ok(())
            } else {
                Err(invalid(format!("data for empty file {id}")))
            };
        }
        if st.current != Some(id) {
            if st.last.is_some_and(|l| id <= l) {
                // From the start of a file already written: a retry, not a violation.
                return Err(if off == 0 {
                    zip_restart(format!("file {id} was sent again"))
                } else {
                    invalid(format!("file {id} arrived out of order"))
                });
            }
            if let Some(cur) = st.current {
                let want = m.entry(cur).map(|c| c.size).unwrap_or(0);
                if st.written != want {
                    return Err(invalid(format!(
                        "file {cur} ended at {} of {want} bytes",
                        st.written
                    )));
                }
            }
            if off != 0 {
                return Err(invalid(format!(
                    "file {id} starts at offset {off}: a gap in the archive stream"
                )));
            }
            let name = zip_entry_name(self.single, &self.base, &e.path);
            let zip = st.zip.as_mut().ok_or_else(|| invalid("archive not open"))?;
            zip.start_file(name, Self::opts()).map_err(zip_err)?;
            st.current = Some(id);
            st.last = Some(id);
            st.written = 0;
            st.started += 1;
        } else if off == 0 && st.written > 0 {
            // The current file again from its first byte (a retry after a mismatch).
            return Err(zip_restart(format!("file {id} was sent again")));
        } else if off != st.written {
            // A gap or an overlap (a duplicate range): never append it.
            return Err(invalid(format!(
                "file {id} wrote at offset {off} but the archive is at {}",
                st.written
            )));
        }
        if st.written + data.len() as u64 > e.size {
            return Err(invalid(format!("file {id} wrote past its size")));
        }
        let zip = st.zip.as_mut().ok_or_else(|| invalid("archive not open"))?;
        zip.write_all(data)?;
        st.written += data.len() as u64;
        if ava1::verify::groups(e.size) < 2 {
            st.kept.entry(id).or_default().extend_from_slice(data);
        }
        Ok(())
    }
}

impl Drop for ZipSink {
    fn drop(&mut self) {
        let finished = self.st.lock().map(|s| s.finished).unwrap_or(false);
        if !finished {
            let _ = std::fs::remove_file(&self.part);
        }
    }
}

impl Sink for ZipSink {
    /// Truncates: `File::create` discards whatever an earlier attempt left in the part
    /// file, so two archives' bytes are never mixed.
    fn prepare(&self, m: &Manifest) -> io::Result<()> {
        check_shape(m, self.single)?;
        let f = std::fs::File::create(&self.part)?;
        let mut st = self.st.lock().unwrap();
        st.m = Some(Arc::new(m.clone()));
        st.zip = Some(zip::ZipWriter::new(BufWriter::new(f)));
        st.current = None;
        st.written = 0;
        st.last = None;
        st.started = 0;
        st.kept.clear();
        Ok(())
    }
    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        self.append(id, off, data)
    }
    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        self.append(id, 0, data)
    }
    fn sync(&self, _ids: &[u32]) -> io::Result<()> {
        Ok(())
    }
    fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let st = self.st.lock().unwrap();
        let kept = st
            .kept
            .get(&id)
            .ok_or_else(|| io::Error::from(io::ErrorKind::Unsupported))?;
        let end = off as usize + buf.len();
        if end > kept.len() {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        buf.copy_from_slice(&kept[off as usize..end]);
        Ok(buf.len())
    }
    fn commit(&self, id: u32) -> io::Result<()> {
        self.st.lock().unwrap().kept.remove(&id);
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        let mut st = self.st.lock().unwrap();
        let m =
            st.m.clone()
                .ok_or_else(|| invalid("finished before the manifest"))?;
        let nonempty = m
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE && e.size > 0)
            .count();
        if let Some(cur) = st.current {
            let want = m.entry(cur).map(|c| c.size).unwrap_or(0);
            if st.written != want {
                return Err(invalid(format!(
                    "file {cur} ended at {} of {want}",
                    st.written
                )));
            }
        }
        if st.started != nonempty {
            return Err(invalid(format!(
                "the archive holds {} of {nonempty} files",
                st.started
            )));
        }
        let mut zip = st.zip.take().ok_or_else(|| invalid("archive not open"))?;
        for e in m
            .entries
            .iter()
            .filter(|e| e.kind == gen::ENTRY_FILE && e.size == 0)
        {
            zip.start_file(
                zip_entry_name(self.single, &self.base, &e.path),
                Self::opts(),
            )
            .map_err(zip_err)?;
        }
        let mut out = zip.finish().map_err(zip_err)?;
        out.flush()?;
        let f = out.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&self.part, &self.dest)?; // same directory by construction
        st.finished = true;
        Ok(())
    }
}

/// Maps the receiver's terminal errors to what the engine's `job_failed_from_err` already
/// understands (`UploadFailure` with a stable `error_reason`). Nothing here is retried:
/// by the time a local write or the final rename fails, the bytes are already down, and
/// asking again would pull gigabytes for the same failure.
fn terminal(e: SendError) -> anyhow::Error {
    match e {
        SendError::Cancelled => anyhow!("transfer_cancelled"),
        SendError::Refused { status, message } => refusal(status, message).into(),
        SendError::Source(e) if e.kind() == io::ErrorKind::InvalidData => UploadFailure {
            reason: "ava1_bad_manifest".into(),
            detail: format!("the console sent a download this computer will not write: {e}"),
        }
        .into(),
        SendError::Source(e) => UploadFailure {
            reason: "ava1_local_io".into(),
            detail: format!("writing the download on this computer failed: {e}"),
        }
        .into(),
        other => anyhow!(other),
    }
}

/// Job id of attempt `n`: attempt 0 is the job's own id (so the journal directory, the
/// `JobOpen` and the job record agree); a later attempt exists only because the archive
/// was started over, and hashes the id with the attempt so it gets a journal of its own.
/// A resumed connection is the SAME attempt and keeps the id.
fn attempt_id(job_id: [u8; 16], attempt: u32) -> [u8; 16] {
    if attempt == 0 {
        return job_id;
    }
    let mut h = blake3::Hasher::new();
    h.update(&job_id);
    h.update(&attempt.to_le_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.finalize().as_bytes()[..16]);
    id
}

/// One download, retried across connection loss. `make_sink` builds the sink for an
/// attempt.
///
/// `fresh_per_attempt` is the Deflate zip: a deflate stream cannot be seeked into, and the
/// journal's ranges cannot reconstruct compressed bytes, so a dropped connection restarts
/// the archive from byte zero as a NEW job with a new journal (a 40 GiB zip that drops at
/// 90% pays for it again, which is why Stored is the default). The previous attempt's
/// archive is discarded (`ZipSink::prepare` truncates, and an abandoned sink deletes its
/// part file) so two archives are never mixed, and the restart is logged with the attempt
/// number and the bytes spent.
///
/// With `fresh_per_attempt == false` a dropped connection resumes the same job id: the
/// sink re-derives its position from the journal (`Sink::position`; the Stored zip resumes
/// at the durable byte). A file the receiver asks to have re-sent (`ZipRestart`) starts
/// the archive over under either setting, since a zip cannot rewrite what it holds.
#[allow(clippy::too_many_arguments)]
fn run(
    pool: &Pool,
    console: &str,
    src: &str,
    flags: u32,
    job_id: [u8; 16],
    fresh_per_attempt: bool,
    make_sink: &dyn Fn() -> Arc<dyn Sink>,
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    let cancel = cancel.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
    let jobs_dir = pool.ava_dir().join("jobs");
    let _live = pool.live_job(&job_id); // the journal sweep leaves a running job alone
    crate::block_on(async {
        SessionGate::identity(pool)?;
        let mut sink = make_sink();
        let mut backoff = Duration::from_millis(250);
        let mut gate = SessionGate::default();
        let (mut base_bytes, mut base_files) = (0u64, 0u64);
        let mut attempt = 0u32;
        let mut retry_restarts = 0u32;
        let mut busy = 0u32;
        let mut fd_waits = 0u32;
        let (mut last_at, mut last_work) = (Instant::now(), 0u64);
        let mut progress = Arc::new(Progress::default());
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("transfer_cancelled"));
            }
            let work = base_bytes + progress.bytes_durable.load(Ordering::Relaxed);
            if work > last_work {
                (last_at, last_work) = (Instant::now(), work);
            } else if last_at.elapsed() > STALL_LIMIT {
                return Err(anyhow!(
                    "no durable progress for {STALL_LIMIT:?}; giving up"
                ));
            }
            let session = match pool.session(console).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(failure) = gate.failed(&e) {
                        return Err(failure.into());
                    }
                    wait(&mut backoff, &e.to_string()).await;
                    continue;
                }
            };
            gate.connected();
            let id = attempt_id(job_id, attempt);
            let _live_attempt = pool.live_job(&id);
            let mut link = session.job(id);
            let _ticker = Ticker::start(progress.clone(), counters, base_bytes, base_files);
            let o = RecvOptions {
                credit: CREDIT,
                flags,
                jobs_dir: jobs_dir.clone(),
                // `ordered` must agree with the flags (download_job rewrites `flags`).
                ordered: flags & gen::JF_ORDERED != 0,
                progress: progress.clone(),
                cancel: cancel.clone(),
                progress_deadline: None,
            };
            let (why, dropped) = match download_job(&mut link, src, flags, sink.clone(), o).await {
                Ok(r) => {
                    let _ = std::fs::remove_dir_all(journal::job_dir(&jobs_dir, &id));
                    return Ok(r.bytes);
                }
                Err(SendError::Disconnected(why)) => (why, true),
                // The console answered BUSY to the JobOpen: not now. Same bounded backoff, same attempt.
                Err(SendError::Refused { status, message }) if status == gen::ERR_BUSY => {
                    busy += 1;
                    if busy > pool.busy_tries() {
                        return Err(crate::upload::busy_failure(pool.busy_tries(), &message));
                    }
                    wait(&mut backoff, &format!("the console is busy: {message}")).await;
                    continue;
                }
                // The computer is out of file descriptors (other programs hold them): wait and
                // resume the same attempt; what is durable stays durable. Bounded, and the stall
                // limit ends it too.
                Err(SendError::Source(e)) if is_fd_exhausted(&e) && fd_waits < MAX_FD_WAITS => {
                    fd_waits += 1;
                    (
                        format!("this computer is out of file descriptors: {e}"),
                        true,
                    )
                }
                Err(SendError::Source(e))
                    if is_zip_restart(&e) && retry_restarts < MAX_RETRY_RESTARTS =>
                {
                    retry_restarts += 1;
                    (e.to_string(), false)
                }
                Err(e) => return Err(terminal(e)),
            };
            let durable = progress.bytes_durable.load(Ordering::Relaxed);
            if dropped {
                pool.forget(console).await;
            }
            if fresh_per_attempt || !dropped {
                let _ = std::fs::remove_dir_all(journal::job_dir(&jobs_dir, &id));
                base_bytes += durable;
                base_files += progress.files_durable.load(Ordering::Relaxed);
                attempt += 1;
                progress = Arc::new(Progress::default());
                drop(std::mem::replace(&mut sink, make_sink()));
                let _ = writeln!(
                    std::io::stderr(),
                    "ava1: zip download restarts from the beginning (attempt {attempt}, \
                     {base_bytes} bytes already spent): {why}"
                );
                wait(&mut backoff, "restarting the archive").await;
            } else {
                wait(&mut backoff, &format!("{why} ({durable} bytes durable)")).await;
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub fn to_local_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_dir: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    // The manifest's paths are root-relative, so the landing root is
    // `dest_dir/<basename>` for both kinds; for a file that root is the file's own path.
    // Not `dest_dir` (loses the basename) and not a stripped component (double-nests).
    let target = dest_dir.join(basename(src)?);
    // The request kind decides single-file, never the manifest's shape: a folder holding
    // exactly one file must stay a folder.
    let single = kind == DownloadKind::File;
    let mut flags = if single { gen::JF_SINGLE_FILE } else { 0 };
    if unsafe_read {
        flags |= gen::JF_UNSAFE_READ;
    }
    // The staging path every kind of landing uses (`<root>.ava-part`: the staging
    // folder of a new folder, the part file of a single file).
    let part = {
        let mut p = target.clone().into_os_string();
        p.push(".ava-part");
        PathBuf::from(p)
    };
    let job_dir = journal::job_dir(&pool.ava_dir().join("jobs"), &job_id);
    // A fresh job (no journal records) cannot own what is already in the staging path:
    // it is what an earlier, failed download left behind, and renaming it into place
    // would put that manifest's files into this one's folder. A job with a journal is
    // a resume: the staging path is its work.
    if journal_is_empty(&job_dir) {
        remove_path(&part);
    }
    let target_dir = target.clone();
    let sink: Arc<dyn Sink> = Arc::new(CheckedSink {
        inner: LocalSink::new(target.clone(), single),
        root: target,
        single,
    });
    let r = run(
        pool,
        console,
        src,
        flags,
        job_id,
        false,
        &move || sink.clone(),
        counters,
        cancel,
    );
    if let Err(e) = &r {
        cleanup_after_failure(e, &job_dir, &part, &target_dir, single);
    }
    r
}

/// What a failed download leaves behind. A cancel, a refusal or a bad manifest is final:
/// nothing will resume the job, so neither the journal nor the staged bytes may outlive it.
/// A local I/O failure (a full disk, a descriptor shortage that outlasted the waits) is
/// recoverable once the cause is gone, and the next run of the same job resumes from the
/// journal and the staged tree, so both stay.
///
/// A folder downloaded into a folder that already exists is not staged: each large file is written
/// as `<name>.ava-part` beside its final place, inside that folder. Those part files belong to the
/// job too; the journal's saved manifest names them (and only them: a `.ava-part` the job does not
/// own is left alone), so they go with the journal (review 009 #5).
fn cleanup_after_failure(
    e: &anyhow::Error,
    job_dir: &Path,
    part: &Path,
    target: &Path,
    single: bool,
) {
    let recoverable = e
        .downcast_ref::<UploadFailure>()
        .is_some_and(|f| f.reason == "ava1_local_io");
    if !recoverable {
        if !single {
            remove_part_files(job_dir, target);
        }
        let _ = std::fs::remove_dir_all(job_dir);
        remove_path(part);
    }
}

/// Removes the in-place `<file>.ava-part` of every file the job's saved manifest lists, under
/// `target`. Nothing when the manifest is missing or unreadable (a job that never got that far
/// wrote no part files).
fn remove_part_files(job_dir: &Path, target: &Path) {
    let Ok(m) = journal::read_manifest(job_dir) else {
        return;
    };
    for e in m.entries.iter().filter(|e| e.kind == gen::ENTRY_FILE) {
        let mut p = target.join(&e.path).into_os_string();
        p.push(".ava-part");
        // `remove_file` takes a symlink itself, never what it points at.
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

fn journal_is_empty(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut it) => it.next().is_none(),
        Err(_) => true,
    }
}

fn remove_path(p: &Path) {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => {
            let _ = std::fs::remove_dir_all(p);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(p);
        }
        Err(_) => {}
    }
}

#[allow(clippy::too_many_arguments)]
pub fn to_local(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_dir: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_local_in(
        pool(),
        console,
        src,
        kind,
        dest_dir,
        unsafe_read,
        job_id,
        counters,
        cancel,
    )
}

/// How a zip download stores its entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZipCompression {
    /// Uncompressed entries (the default): game data is already compressed, and the archive
    /// resumes at the byte a dropped connection reached.
    #[default]
    Stored,
    /// Deflated entries, for text-heavy trees. A deflate stream cannot be continued, so a
    /// dropped connection restarts the whole archive from zero: it cannot resume.
    Deflate,
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_zip_with_in(
        pool,
        console,
        src,
        kind,
        dest_zip,
        unsafe_read,
        ZipCompression::Stored,
        job_id,
        counters,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip_with_in(
    pool: &Pool,
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    compression: ZipCompression,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    let name = basename(src)?.to_owned();
    let single = kind == DownloadKind::File;
    // A zip needs in-order bytes.
    let mut flags = gen::JF_ORDERED;
    if single {
        flags |= gen::JF_SINGLE_FILE;
    }
    if unsafe_read {
        flags |= gen::JF_UNSAFE_READ;
    }
    let dest = dest_zip.to_path_buf();
    let deflate = compression == ZipCompression::Deflate;
    let r = run(
        pool,
        console,
        src,
        flags,
        job_id,
        deflate,
        &move || -> Arc<dyn Sink> {
            match (deflate, single) {
                (true, true) => Arc::new(ZipSink::single(dest.clone(), &name)),
                (true, false) => Arc::new(ZipSink::new(dest.clone(), &name)),
                (false, true) => Arc::new(StoredZipSink::single(dest.clone(), &name)),
                (false, false) => Arc::new(StoredZipSink::new(dest.clone(), &name)),
            }
        },
        counters,
        cancel,
    );
    if r.is_err() {
        // Terminal (a cancel, a refusal, a local write failure, the stall limit): nothing
        // resumes this job, so its journal must not outlive it (the sink removes its part).
        let _ = std::fs::remove_dir_all(journal::job_dir(&pool.ava_dir().join("jobs"), &job_id));
    }
    r
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_zip_with(
        console,
        src,
        kind,
        dest_zip,
        unsafe_read,
        ZipCompression::Stored,
        job_id,
        counters,
        cancel,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn to_zip_with(
    console: &str,
    src: &str,
    kind: DownloadKind,
    dest_zip: &Path,
    unsafe_read: bool,
    compression: ZipCompression,
    job_id: [u8; 16],
    counters: &Counters,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<u64> {
    to_zip_with_in(
        pool(),
        console,
        src,
        kind,
        dest_zip,
        unsafe_read,
        compression,
        job_id,
        counters,
        cancel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_entry_names_follow_one_rule() {
        assert_eq!(zip_entry_name(false, "Game", "a/b.bin"), "Game/a/b.bin");
        assert_eq!(zip_entry_name(true, "foo.pkg", "ignored"), "foo.pkg");
    }

    #[test]
    fn attempt_ids_differ_only_after_a_restart() {
        let id = [7u8; 16];
        assert_eq!(attempt_id(id, 0), id);
        assert_ne!(attempt_id(id, 1), id);
        assert_ne!(attempt_id(id, 1), attempt_id(id, 2));
    }

    #[test]
    fn basename_refuses_names_that_climb() {
        assert_eq!(basename("/data/foo/").unwrap(), "foo");
        assert!(basename("/").is_err());
        assert!(basename("/a/..").is_err());
    }

    fn entry(kind: u8, path: &str) -> manifest::Entry {
        manifest::Entry {
            kind,
            mode: 0o644,
            size: 0,
            mtime: 0,
            path: path.into(),
            root: None,
        }
    }

    fn files(paths: &[&str]) -> Manifest {
        Manifest {
            entries: paths.iter().map(|p| entry(gen::ENTRY_FILE, p)).collect(),
        }
    }

    #[test]
    fn check_shape_refuses_paths_a_host_would_misplace() {
        for bad in [
            "C:/x",
            "C:evil",
            "a/C:/x",
            "a\\b",
            "CON",
            "con",
            "a/NUL.txt",
            "Aux.tar.gz",
            "COM1",
            "lpt9.log",
            "CONIN$",
            "conout$",
            "CONIN$.txt",
            "COM\u{b9}",
            "com\u{b2}.txt",
            "LPT\u{b3}",
            "x.",
            "a/x ",
            "dir./f",
            "s:stream",
        ] {
            let e = check_shape(&files(&[bad]), false);
            assert!(e.is_err(), "{bad:?} must be refused");
            assert_eq!(e.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn check_shape_accepts_ordinary_and_lookalike_names() {
        for good in [
            "a/b.txt",
            "COM",
            "COM0",
            "COM10",
            "CONSOLE",
            "console.txt",
            "NULL",
            "LPT",
            ".hidden",
            "a b/c d",
            "x.y",
            "price$",
            "CONIN",
            "CON$X",
        ] {
            check_shape(&files(&[good]), false)
                .unwrap_or_else(|e| panic!("{good:?} must be accepted: {e}"));
        }
    }

    #[test]
    fn a_single_file_manifest_must_be_exactly_one_file() {
        check_shape(&files(&["one"]), true).unwrap();
        assert!(check_shape(&files(&["one", "two"]), true).is_err());
        let dir = Manifest {
            entries: vec![entry(gen::ENTRY_DIR, "d")],
        };
        assert!(
            check_shape(&dir, true).is_err(),
            "a directory is not a file"
        );
        let dir_and_file = Manifest {
            entries: vec![entry(gen::ENTRY_DIR, "d"), entry(gen::ENTRY_FILE, "d/f")],
        };
        assert!(check_shape(&dir_and_file, true).is_err());
        // A folder download may hold one file.
        check_shape(&dir_and_file, false).unwrap();
    }

    /// A sink that wraps `ZipSink` and, on its first write, reports that a file was
    /// sent again (what the receiver's FileRetry causes), once per test.
    struct RetrySink {
        inner: ZipSink,
        fail_first_write: bool,
        failed: AtomicBool,
    }

    impl Sink for RetrySink {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            if self.fail_first_write && !self.failed.swap(true, Ordering::Relaxed) {
                return Err(zip_restart("injected: file sent again"));
            }
            self.inner.write_at(id, off, data)
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            if self.fail_first_write && !self.failed.swap(true, Ordering::Relaxed) {
                return Err(zip_restart("injected: file sent again"));
            }
            self.inner.write_whole(id, data)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
    }

    /// `LocalSink` whose first write fails as the OS does when the process is out of
    /// descriptors.
    struct EmfileOnce {
        inner: LocalSink,
        failed: AtomicBool,
    }

    impl EmfileOnce {
        fn fail(&self) -> io::Result<()> {
            if !self.failed.swap(true, Ordering::Relaxed) {
                return Err(io::Error::from_raw_os_error(24));
            }
            Ok(())
        }
    }

    impl Sink for EmfileOnce {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            self.fail()?;
            self.inner.write_at(id, off, data)
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            self.fail()?;
            self.inner.write_whole(id, data)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn resume_key(&self) -> Option<(String, bool)> {
            self.inner.resume_key()
        }
    }

    #[test]
    fn a_descriptor_shortage_is_waited_out_not_terminal() {
        let d = std::env::temp_dir().join(format!("p5a-emfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        for i in 0..3u8 {
            std::fs::write(d.join(format!("share/G/f{i}")), vec![i + 1; 5000]).unwrap();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let addr = rt.block_on(folder_host(&d, key));
        let pool = Pool::new(ava).with_addr(addr);
        let out = d.join("out/G");
        let out2 = out.clone();
        let make = move || -> Arc<dyn Sink> {
            Arc::new(EmfileOnce {
                inner: LocalSink::new(out2.clone(), false),
                failed: AtomicBool::new(false),
            })
        };
        let bytes = run(
            &pool,
            "c",
            "G",
            0,
            [8; 16],
            false,
            &make,
            &Counters::default(),
            None,
        )
        .expect("EMFILE is retried, not terminal");
        assert_eq!(bytes, 15000);
        for i in 0..3u8 {
            assert_eq!(
                std::fs::read(out.join(format!("f{i}"))).unwrap(),
                vec![i + 1; 5000]
            );
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    async fn folder_host(dir: &Path, engine_key: [u8; 32]) -> String {
        use ava1::host::FolderHost;
        use ava1::peers::PeerStore;
        use ava1::server::{self, ServerCtx};
        let mut peers = PeerStore::in_memory();
        peers.add(engine_key, "engine").unwrap();
        let rpc: server::RpcHandler = Box::new(|_, _| ava1::session::RpcReply {
            status: gen::ERR_UNKNOWN_METHOD,
            body: Vec::new(),
        });
        let ctx = ServerCtx::new(
            ava1::keys::Identity::generate().unwrap(),
            "host",
            peers,
            rpc,
        )
        .with_jobs(Arc::new(FolderHost {
            root: dir.join("share"),
            jobs_dir: dir.join("hjobs"),
        }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        addr
    }

    // A re-sent file restarts the archive attempt (like a drop) instead of failing the
    // download as a bad manifest.
    #[test]
    fn a_zip_download_restarts_when_a_file_is_sent_again() {
        let d = std::env::temp_dir().join(format!("p5a-zip-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        for i in 0..3u8 {
            std::fs::write(d.join(format!("share/G/f{i}")), vec![i + 1; 5000]).unwrap();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let addr = rt.block_on(folder_host(&d, key));
        let pool = Pool::new(ava).with_addr(addr);
        let dest = d.join("g.zip");
        let made = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (made2, dest2) = (made.clone(), dest.clone());
        let make = move || -> Arc<dyn Sink> {
            let n = made2.fetch_add(1, Ordering::Relaxed);
            Arc::new(RetrySink {
                inner: ZipSink::new(dest2.clone(), "G"),
                fail_first_write: n == 0,
                failed: AtomicBool::new(false),
            })
        };
        let flags = gen::JF_ORDERED;
        // The test thread owns no runtime context: `run` blocks on its own.
        let bytes = run(
            &pool,
            "c",
            "G",
            flags,
            [9; 16],
            true,
            &make,
            &Counters::default(),
            None,
        )
        .expect("a re-sent file must restart the archive, not fail it");
        assert_eq!(bytes, 15000);
        // Two sinks were built: the original and the restart.
        assert_eq!(made.load(Ordering::Relaxed), 2);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        assert_eq!(z.len(), 3);
        for i in 0..3u8 {
            let mut e = z.by_name(&format!("G/f{i}")).unwrap();
            let mut got = Vec::new();
            io::Read::read_to_end(&mut e, &mut got).unwrap();
            assert_eq!(got, vec![i + 1; 5000]);
        }
        assert!(!d.join("g.zip.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A sink that counts every byte handed to it and, once, drops the connection when the
    /// count reaches `kill_at`: what a flaky link does mid-download.
    struct DropSink {
        inner: Arc<dyn Sink>,
        written: Arc<AtomicU64>,
        kill_at: u64,
        killed: AtomicBool,
        proxy: Arc<ava1_chaos::ChaosProxy>,
        /// Damages the part file between the drop and the resume (the 2nd `prepare`).
        damage: Option<fn(&Path)>,
        part: PathBuf,
        prepares: std::sync::atomic::AtomicU32,
        /// What the sink had been handed when the last sync that began before the drop started:
        /// the bytes a journal commit could have covered. Everything written after it, up to the
        /// resume, is what a drop may lose (and a resume then rewrites).
        synced_floor: Arc<AtomicU64>,
        /// What the sink had been handed when the resume began (its second `prepare`).
        at_resume: Arc<AtomicU64>,
    }

    impl DropSink {
        fn count(&self, n: usize) {
            let w = self.written.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
            if w >= self.kill_at && !self.killed.swap(true, Ordering::Relaxed) {
                self.proxy.kill_all();
            }
        }
    }

    impl Sink for DropSink {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            if self.prepares.fetch_add(1, Ordering::Relaxed) == 1 {
                self.at_resume
                    .store(self.written.load(Ordering::Relaxed), Ordering::Relaxed);
                if let Some(f) = self.damage {
                    f(&self.part);
                }
            }
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            self.inner.write_at(id, off, data)?;
            self.count(data.len());
            Ok(())
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            self.inner.write_whole(id, data)?;
            self.count(data.len());
            Ok(())
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            let handed = self.written.load(Ordering::Relaxed);
            let before_drop = !self.killed.load(Ordering::Relaxed);
            let r = self.inner.sync(ids);
            if r.is_ok() && before_drop {
                self.synced_floor.store(handed, Ordering::Relaxed);
            }
            r
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn position(
            &self,
            done: &std::collections::BTreeSet<u32>,
            partial: &std::collections::BTreeMap<u32, ava1::ranges::RangeSet>,
        ) -> io::Result<()> {
            self.inner.position(done, partial)
        }
    }

    /// Downloads `files` x `size` bytes into a Stored zip, dropping the connection at
    /// ~40% of the bytes. Returns (bytes the sink was handed in all, total bytes, run
    /// the bytes a drop could have lost); the archive is checked against the source byte for byte.
    fn drop_at_forty_percent(
        tag: &str,
        files: usize,
        size: usize,
        bps: u64,
        damage: Option<fn(&Path)>,
    ) -> (u64, u64, u64) {
        let d = std::env::temp_dir().join(format!("p5a-zip-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        let mut want = Vec::new();
        for i in 0..files {
            let b: Vec<u8> = (0..size).map(|k| (k * 7 + i * 13) as u8).collect();
            std::fs::write(d.join(format!("share/G/f{i:02}")), &b).unwrap();
            want.push(b);
        }
        let total = (files * size) as u64;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let host = rt.block_on(folder_host(&d, key));
        let proxy = Arc::new(
            rt.block_on(ava1_chaos::ChaosProxy::start(
                host.parse().unwrap(),
                ava1_chaos::ChaosConfig {
                    bytes_per_sec: Some(bps),
                    ..Default::default()
                },
            ))
            .unwrap(),
        );
        let pool = Pool::new(ava).with_addr(proxy.addr.to_string());
        let dest = d.join("g.zip");
        let written = Arc::new(AtomicU64::new(0));
        let synced_floor = Arc::new(AtomicU64::new(0));
        let at_resume = Arc::new(AtomicU64::new(0));
        let sink: Arc<dyn Sink> = Arc::new(DropSink {
            inner: Arc::new(StoredZipSink::new(dest.clone(), "G")),
            written: written.clone(),
            kill_at: total * 4 / 10,
            killed: AtomicBool::new(false),
            proxy: proxy.clone(),
            damage,
            part: d.join("g.zip.ava-part"),
            prepares: Default::default(),
            synced_floor: synced_floor.clone(),
            at_resume: at_resume.clone(),
        });
        let c = Counters::default();
        let bytes = run(
            &pool,
            "c",
            "G",
            gen::JF_ORDERED,
            [5; 16],
            false,
            &move || sink.clone(),
            &c,
            None,
        )
        .expect("a dropped Stored zip download must resume");
        assert_eq!(bytes, total);
        assert!(pool.attempts() >= 2, "the connection never dropped");
        assert_eq!(
            c.bytes.load(Ordering::Relaxed),
            total,
            "resumed, not restarted: the work counter never needed more than the archive"
        );
        let mut z = zip::ZipArchive::new(std::fs::File::open(&dest).unwrap()).unwrap();
        assert_eq!(z.len(), files);
        for (i, b) in want.iter().enumerate() {
            let mut e = z.by_name(&format!("G/f{i:02}")).unwrap();
            assert_eq!(e.compression(), zip::CompressionMethod::Stored);
            let mut got = Vec::new();
            io::Read::read_to_end(&mut e, &mut got).unwrap();
            assert!(&got == b, "G/f{i:02} differs");
        }
        assert!(!d.join("g.zip.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
        (
            written.load(Ordering::Relaxed),
            total,
            // The most a drop could have lost: handed before the resume, not yet synced.
            at_resume
                .load(Ordering::Relaxed)
                .saturating_sub(synced_floor.load(Ordering::Relaxed)),
        )
    }

    // Step 2: whole entries are kept. 1 MiB entries are one group each, so only the entry
    // in flight can be lost; at 4 MiB/s a sync batch holds well under an entry.
    #[test]
    fn a_stored_zip_resumes_with_at_most_one_entry_resent() {
        let (handed, total, could_lose) =
            drop_at_forty_percent("entry", 16, 1 << 20, 4 << 20, None);
        let resent = handed - total;
        // What a drop loses is what was handed to the sink since the last sync that began before
        // it, plus a commit's worth of slack (the journal record follows the sync). That is the
        // property: nothing synced is sent twice. A fixed byte bound only held on an idle host,
        // where the sync period is short next to the link's rate.
        assert!(
            resent <= could_lose + (1 << 20),
            "{resent} bytes were sent twice but only {could_lose} were unsynced at the drop"
        );
    }

    // Step 3: inside an entry. 12 MiB entries are 12 groups; the cut lands inside one, and
    // what is resent is the un-journaled tail (a sync batch), never the entry.
    #[test]
    fn a_stored_zip_resumes_mid_entry_with_at_most_a_group_or_two_resent() {
        let (handed, total, could_lose) = drop_at_forty_percent("mid", 4, 12 << 20, 8 << 20, None);
        let resent = handed - total;
        assert!(
            resent <= could_lose + (1 << 20),
            "{resent} bytes were sent twice but only {could_lose} were unsynced at the drop"
        );
        assert!(resent < 12 << 20, "the entry was sent again whole");
    }

    // The sink cannot honour the journal (the archive is shorter than the journal's cut):
    // the receiver resets the journal, the archive restarts from 0, the download completes.
    #[test]
    fn a_resume_the_sink_cannot_honour_starts_over_and_still_verifies() {
        let (handed, total, _) = drop_at_forty_percent(
            "fallback",
            4,
            12 << 20,
            8 << 20,
            Some(|p| {
                let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
                f.set_len(1000).unwrap();
            }),
        );
        // Everything sent before the drop was thrown away and sent again.
        assert!(
            handed - total >= total * 3 / 10,
            "only {} bytes were resent",
            handed - total
        );
    }

    // Bit rot inside the in-flight entry's durable bytes is caught by the receiver's
    // re-check against the group CVs (a multi-group file's outboard): the file restarts,
    // and the finished archive still matches the source.
    #[test]
    fn rot_in_an_in_flight_entrys_durable_bytes_is_caught() {
        let (handed, total, _) = drop_at_forty_percent(
            "rot",
            4,
            12 << 20,
            8 << 20,
            Some(|p| {
                // 1 MiB into the second entry: inside its durable prefix at the 40% drop.
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(p)
                    .unwrap();
                let at = (13 << 20) as u64;
                let mut b = [0u8; 1];
                std::os::unix::fs::FileExt::read_exact_at(&f, &mut b, at).unwrap();
                b[0] ^= 0xff;
                std::os::unix::fs::FileExt::write_all_at(&f, &b, at).unwrap();
            }),
        );
        assert!(handed > total, "the rotted file was not sent again");
    }

    /// Records every write so a test can check the order bytes were delivered in.
    struct OrderSink {
        inner: StoredZipSink,
        seen: Mutex<Vec<(u32, u64, usize)>>,
    }

    impl Sink for OrderSink {
        fn prepare(&self, m: &Manifest) -> io::Result<()> {
            self.inner.prepare(m)
        }
        fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
            self.seen.lock().unwrap().push((id, off, data.len()));
            self.inner.write_at(id, off, data)
        }
        fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
            self.seen.lock().unwrap().push((id, 0, data.len()));
            self.inner.write_whole(id, data)
        }
        fn sync(&self, ids: &[u32]) -> io::Result<()> {
            self.inner.sync(ids)
        }
        fn read_at(&self, id: u32, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.inner.read_at(id, off, buf)
        }
        fn commit(&self, id: u32) -> io::Result<()> {
            self.inner.commit(id)
        }
        fn finish(&self) -> io::Result<()> {
            self.inner.finish()
        }
        fn position(
            &self,
            done: &std::collections::BTreeSet<u32>,
            partial: &std::collections::BTreeMap<u32, ava1::ranges::RangeSet>,
        ) -> io::Result<()> {
            self.inner.position(done, partial)
        }
    }

    // An ordered job that is not resuming has no skip list: every byte of every file is
    // delivered, in (file, offset) order, with no gaps.
    #[test]
    fn a_fresh_ordered_job_delivers_every_byte_in_order() {
        let d = std::env::temp_dir().join(format!("p5a-ord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("share/G")).unwrap();
        let sizes = [3 << 20, 100, (2 << 20) + 17, 1 << 20, 5];
        for (i, n) in sizes.iter().enumerate() {
            std::fs::write(d.join(format!("share/G/f{i}")), vec![i as u8 + 1; *n]).unwrap();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let ava = d.join("ava");
        let key = ava1::keys::Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let addr = rt.block_on(folder_host(&d, key));
        let pool = Pool::new(ava).with_addr(addr);
        let sink = Arc::new(OrderSink {
            inner: StoredZipSink::new(d.join("g.zip"), "G"),
            seen: Mutex::new(Vec::new()),
        });
        let s2: Arc<dyn Sink> = sink.clone();
        run(
            &pool,
            "c",
            "G",
            gen::JF_ORDERED,
            [6; 16],
            false,
            &move || s2.clone(),
            &Counters::default(),
            None,
        )
        .unwrap();
        let seen = sink.seen.lock().unwrap().clone();
        let mut next = (0u32, 0u64);
        let sz: Vec<u64> = (0..5)
            .map(|i| {
                std::fs::metadata(d.join(format!("share/G/f{i}")))
                    .unwrap()
                    .len()
            })
            .collect();
        for (id, off, n) in seen {
            assert_eq!((id, off), next, "delivery out of order or with a gap");
            next = if off + n as u64 == sz[id as usize] {
                (id + 1, 0)
            } else {
                (id, off + n as u64)
            };
        }
        assert_eq!(next, (5, 0), "not every byte was delivered");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_rewrite_from_offset_zero_is_a_restart_and_a_gap_is_not() {
        let d = std::env::temp_dir().join(format!("p5a-zip-sink-rw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut m = files(&["a", "b"]);
        m.entries[0].size = 100;
        m.entries[1].size = 50;
        let s = ZipSink::new(d.join("o.zip"), "P");
        s.prepare(&m).unwrap();
        s.write_at(0, 0, &[1; 100]).unwrap();
        s.write_at(1, 0, &[2; 10]).unwrap();
        // An earlier, finished file sent again, and the current one again.
        assert!(is_zip_restart(&s.write_at(0, 0, &[1; 100]).unwrap_err()));
        assert!(is_zip_restart(&s.write_at(1, 0, &[2; 10]).unwrap_err()));
        // A gap is a protocol violation, not a retry.
        let gap = s.write_at(1, 30, &[2; 10]).unwrap_err();
        assert!(!is_zip_restart(&gap));
        assert_eq!(gap.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_recoverable_local_failure_keeps_its_journal_and_staging() {
        let d = std::env::temp_dir().join(format!("p5a-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (job, part) = (d.join("job"), d.join("Game.ava-part"));
        let fill = || {
            std::fs::create_dir_all(&job).unwrap();
            std::fs::write(job.join("journal"), b"x").unwrap();
            std::fs::create_dir_all(&part).unwrap();
        };
        let local_io = terminal(SendError::Source(io::Error::other("disk full")));
        fill();
        cleanup_after_failure(&local_io, &job, &part, &d, false);
        assert!(job.exists() && part.exists(), "resume lost its work");
        for e in [
            anyhow!("transfer_cancelled"),
            terminal(SendError::Source(invalid("bad name"))),
            terminal(SendError::Refused {
                status: gen::ERR_PATH,
                message: "no".into(),
            }),
        ] {
            fill();
            cleanup_after_failure(&e, &job, &part, &d, false);
            assert!(!job.exists() && !part.exists(), "a final failure left work");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn terminal_errors_keep_their_reasons() {
        let reason = |e: SendError| {
            let e = terminal(e);
            match e.downcast_ref::<UploadFailure>() {
                Some(f) => f.reason.clone(),
                None => e.to_string(),
            }
        };
        assert_eq!(reason(SendError::Cancelled), "transfer_cancelled");
        assert_eq!(
            reason(SendError::Source(io::Error::from(
                io::ErrorKind::PermissionDenied
            ))),
            "ava1_local_io"
        );
        assert_eq!(
            reason(SendError::Source(io::Error::other("disk full"))),
            "ava1_local_io"
        );
        assert_eq!(
            reason(SendError::Source(invalid("bad name"))),
            "ava1_bad_manifest"
        );
        assert_eq!(
            reason(SendError::Refused {
                status: gen::ERR_PATH,
                message: "no".into()
            }),
            "ava1_not_allowed"
        );
    }

    #[test]
    fn a_stale_staging_path_is_cleared_only_for_a_fresh_journal() {
        let d = std::env::temp_dir().join(format!("p5a-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("j")).unwrap();
        assert!(journal_is_empty(&d.join("missing")));
        assert!(journal_is_empty(&d.join("j")));
        std::fs::write(d.join("j/rec"), b"x").unwrap();
        assert!(!journal_is_empty(&d.join("j")));
        std::fs::create_dir_all(d.join("p.ava-part/sub")).unwrap();
        remove_path(&d.join("p.ava-part"));
        assert!(!d.join("p.ava-part").exists());
        std::fs::write(d.join("f.ava-part"), b"x").unwrap();
        remove_path(&d.join("f.ava-part"));
        assert!(!d.join("f.ava-part").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_final_failure_into_an_existing_folder_removes_its_in_place_part_files_only() {
        let d = std::env::temp_dir().join(format!("p5a-inplace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (job, target) = (d.join("job"), d.join("Game"));
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(target.join("sub")).unwrap();
        let file = |path: &str| manifest::Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size: 5 << 20,
            mtime: 0,
            path: path.into(),
            root: None,
        };
        let m = Manifest {
            entries: vec![file("big.bin"), file("sub/big2.bin")],
        };
        journal::write_manifest(&job, &m).unwrap();
        std::fs::write(job.join("journal"), b"x").unwrap();
        // what a failed run left: the job's part files, the final file it would replace, and a
        // part file that is not the job's
        std::fs::write(target.join("big.bin.ava-part"), b"p").unwrap();
        std::fs::write(target.join("sub/big2.bin.ava-part"), b"p").unwrap();
        std::fs::write(target.join("big.bin"), b"old").unwrap();
        std::fs::write(target.join("other.ava-part"), b"mine").unwrap();
        let part = d.join("Game.ava-part"); // staging: absent, the folder existed
        let local_io = terminal(SendError::Source(io::Error::other("disk full")));
        cleanup_after_failure(&local_io, &job, &part, &target, false);
        assert!(
            target.join("big.bin.ava-part").exists(),
            "a recoverable failure keeps its work"
        );
        cleanup_after_failure(&anyhow!("transfer_cancelled"), &job, &part, &target, false);
        assert!(!target.join("big.bin.ava-part").exists());
        assert!(!target.join("sub/big2.bin.ava-part").exists());
        assert!(!job.exists());
        assert_eq!(std::fs::read(target.join("big.bin")).unwrap(), b"old");
        assert!(target.join("other.ava-part").exists(), "not the job's");
        let _ = std::fs::remove_dir_all(&d);
    }
}
