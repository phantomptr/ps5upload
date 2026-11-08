//! Convert straight to the console: an image build whose bytes go to the PS5 over AVA1 as they
//! are written, so the image never exists on this computer.
//!
//! Why it works. Every plain image writer here goes front to back: the exFAT writer only
//! writes, the UFS2 writer skips forward over holes, and the PFS writer is sequential. Each one
//! knows its size once planned (`Planned::image_size`), which is what AVA1 needs for its
//! manifest. So the writer runs on the job's thread into [`PipeWriter`] (forward seeks become
//! zeros, as `ps5_dump_forge_pfs::Stream` does), which hands 1 MiB pieces down a bounded pipe
//! to [`ImageSeq`], a forward-only AVA1 source (`ava1::seq`, the same kind of source an `.lz4`
//! link is, see `link_lz4`). AVA1 does the rest: the console writes `<name>.ava-part`, checks
//! the BLAKE3 of the whole file at commit and only then renames it into place, so ShadowMount+
//! never sees half an image.
//!
//! Resume. A dropped connection ends the AVA1 pass; the next session asks for another pass,
//! the writer runs again from the start (same files, same pinned time: same bytes), and AVA1
//! sends only what the console does not hold yet. The job id is derived from the plan, the
//! console and the path, so starting the same conversion again after the engine restarted finds
//! the console's parked job too.
//!
//! Read-back. After the commit the image is opened on the console through its own reader over
//! the helper's `fs.read` (`console_read::ConsoleFiles`, any [`RemoteFiles`] in tests), and the
//! same check a local build makes runs against it (sampled, or every byte on request). An image
//! that fails it is deleted from the console.
//!
//! Threads: the writer runs on the caller's thread; the AVA1 upload runs on a scoped thread
//! that is always joined before this returns, and AVA1 joins its own decode thread, so a cancel
//! or a failure leaves nothing running.

use std::collections::HashMap;
use std::io::{self, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ava1::manifest::Manifest;
use ava1::seq::{EntrySink, Keep, Restart, SeqSource};
use ava1::source::{ReadAt, Source, SourceMeta};
use ps5upload_core::transfer::TransferResult;
use ps5upload_fpkg::ffpfsc_block::Level;
use ps5upload_fpkg::remote_source::RemoteFiles;
use ps5upload_fpkg::sample::VerifyMode;
use ps5upload_fpkg::source::SourceTree;

use super::{resume_key, verify, HashingTree, ImageBuilt, ImageFormat, Planned};

/// What goes down the pipe at a time; also how often a stop is noticed.
const PIECE: usize = 1 << 20;
/// Pieces in flight between the writer and AVA1: bounded memory.
const PIPE: usize = 8;
/// How often a waiting side looks at the cancel flag.
const POLL: Duration = Duration::from_millis(100);

/// One piece of the image, or how the run ended.
enum Piece {
    Data(Vec<u8>),
    End(Result<(), String>),
}

/// One run of the writer from the first byte, asked for by an AVA1 pass.
struct Run {
    tx: SyncSender<Piece>,
    /// Set when the pass that asked is over: the run stops at its next write.
    abort: Arc<AtomicBool>,
}

/// Sets a run's abort flag when the pass ends, however it ends.
struct StopRun(Arc<AtomicBool>);

impl Drop for StopRun {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn stopped() -> io::Error {
    io::Error::other("the image stream was stopped")
}

/// The image as a writer sees it: `Write + Seek`, forward only. A forward seek writes zeros,
/// `End(0)` is the current position, and going back is an error. Bytes go down the pipe in
/// [`PIECE`]s; a send waits while the pipe is full, and fails once the pass is gone.
pub(crate) struct PipeWriter<'a> {
    tx: &'a SyncSender<Piece>,
    abort: &'a AtomicBool,
    buf: Vec<u8>,
    pos: u64,
    size: u64,
}

impl<'a> PipeWriter<'a> {
    fn new(tx: &'a SyncSender<Piece>, abort: &'a AtomicBool, size: u64) -> Self {
        Self {
            tx,
            abort,
            buf: Vec::with_capacity(PIECE),
            pos: 0,
            size,
        }
    }

    fn push(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        if self.abort.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        let full = std::mem::replace(&mut self.buf, Vec::with_capacity(PIECE));
        // The receiver is dropped when its pass ends, which is what ends a send that waits.
        self.tx.send(Piece::Data(full)).map_err(|_| stopped())
    }

    fn take(&mut self, mut data: &[u8]) -> io::Result<()> {
        if self
            .pos
            .checked_add(data.len() as u64)
            .is_none_or(|e| e > self.size)
        {
            return Err(io::Error::other(format!(
                "the image runs past its planned {} bytes",
                self.size
            )));
        }
        while !data.is_empty() {
            let n = (PIECE - self.buf.len()).min(data.len());
            self.buf.extend_from_slice(&data[..n]);
            self.pos += n as u64;
            data = &data[n..];
            if self.buf.len() == PIECE {
                self.push()?;
            }
        }
        Ok(())
    }

    fn zeros(&mut self, mut n: u64) -> io::Result<()> {
        static ZEROS: [u8; 64 * 1024] = [0; 64 * 1024];
        while n > 0 {
            let k = n.min(ZEROS.len() as u64) as usize;
            self.take(&ZEROS[..k])?;
            n -= k as u64;
        }
        Ok(())
    }

    /// The last piece, once the writer is done: the image must be exactly its planned size.
    fn finish(&mut self) -> io::Result<()> {
        if self.pos != self.size {
            return Err(io::Error::other(format!(
                "the image is {} bytes, not the planned {}",
                self.pos, self.size
            )));
        }
        self.push()
    }
}

impl Write for PipeWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.take(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for PipeWriter<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::End(0) => Some(self.pos),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(_) => None,
        };
        match target {
            Some(t) if t >= self.pos => {
                self.zeros(t - self.pos)?;
                Ok(t)
            }
            _ => Err(io::Error::other(
                "an image streamed to the console is written forward only",
            )),
        }
    }
}

/// The image being written, as a forward-only AVA1 source (and, for the manifest, a one-file
/// [`Source`] that only answers `stat`). Each pass asks the writer for a new run from the
/// start and delivers what comes down the pipe.
pub(crate) struct ImageSeq {
    name: String,
    size: u64,
    /// To the writer's thread; `None` once the upload is over.
    runs: Mutex<Option<Sender<Run>>>,
    passes: AtomicU32,
}

impl ImageSeq {
    fn new(name: String, size: u64, runs: Sender<Run>) -> Self {
        Self {
            name,
            size,
            runs: Mutex::new(Some(runs)),
            passes: AtomicU32::new(0),
        }
    }

    /// No more runs: the writer's thread stops waiting for them.
    fn close_runs(&self) {
        self.runs.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

impl Source for ImageSeq {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{rel}: an image streamed to the console is read sequentially"),
        ))
    }

    fn list(&self, rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        if !rel.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_string()));
        }
        Ok(vec![(self.name.clone(), self.stat(&self.name)?)])
    }

    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        if rel.is_empty() {
            return Ok(SourceMeta {
                size: 0,
                mtime: 0,
                mode: 0o755,
                is_dir: true,
            });
        }
        if rel != self.name {
            return Err(io::Error::new(io::ErrorKind::NotFound, rel.to_string()));
        }
        Ok(SourceMeta {
            size: self.size,
            mtime: 0,
            mode: 0o644,
            is_dir: false,
        })
    }
}

impl SeqSource for ImageSeq {
    fn pass(
        &self,
        _restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if want(&self.name, self.size) == Keep::Skip {
            return Ok(());
        }
        self.passes.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::sync_channel(PIPE);
        let abort = Arc::new(AtomicBool::new(false));
        let _stop = StopRun(abort.clone());
        {
            let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            let sent = runs.as_ref().is_some_and(|r| {
                r.send(Run {
                    tx,
                    abort: abort.clone(),
                })
                .is_ok()
            });
            if !sent {
                return Err(io::Error::other("the image build is over"));
            }
        }
        sink.begin(&self.name)?;
        let mut got = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::Error::other("transfer_cancelled"));
            }
            match rx.recv_timeout(POLL) {
                Ok(Piece::Data(bytes)) => {
                    got += bytes.len() as u64;
                    if got > self.size {
                        return Err(io::Error::other(format!(
                            "the image runs past its planned {} bytes",
                            self.size
                        )));
                    }
                    sink.data(&bytes)?;
                }
                Ok(Piece::End(Ok(()))) => break,
                Ok(Piece::End(Err(e))) => return Err(io::Error::other(e)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("the image writer stopped"))
                }
            }
        }
        if got != self.size {
            return Err(io::Error::other(format!(
                "the image came to {got} bytes, not the planned {}",
                self.size
            )));
        }
        sink.end()
    }

    fn restart_for(&self, _file_id: u32) -> Restart {
        Restart::START
    }

    // No `close`: AVA1 calls it at the end of every session, and the next session after a
    // dropped connection needs another run. The upload's thread closes the runs once it is over.
}

/// Sends one single-file AVA1 job to the console: the manifest, the source for it, the
/// forward-only source that is read, and the job id. The engine's is `upload_with_seq_in` on
/// its pool; a test's points at a loopback receiver.
pub(crate) type SendImage<'a> = dyn Fn(Manifest, Arc<dyn Source>, Arc<dyn SeqSource>, [u8; 16]) -> anyhow::Result<TransferResult>
    + Sync
    + 'a;

/// Where the image goes and how the console is reached.
pub(crate) struct ConsoleTarget<'a> {
    /// The console, as the job names it (part of the job id).
    pub console: String,
    /// The image's full path on the console, its name included.
    pub path: String,
    pub send: &'a SendImage<'a>,
    /// The console's files, for the read-back and the free-space credit.
    pub files: Arc<dyn RemoteFiles>,
    /// Deletes a file on the console (an image that failed its read-back).
    pub remove: &'a (dyn Fn(&str) -> anyhow::Result<()> + Sync),
    /// The room on the destination's drive, as the console reports it; `None` when it could not
    /// be asked (then AVA1's own check at the job's open still applies: nothing is guessed).
    pub room: Option<ps5upload_ava1::space::Room>,
}

/// What a conversion to the console did.
pub(crate) struct Streamed {
    pub built: ImageBuilt,
    pub transfer: TransferResult,
    /// AVA1 passes the writer served (more than one after a dropped connection).
    pub passes: u32,
}

/// The AVA1 job id: the same plan to the same place on the same console is the same job, so a
/// second try finds the console's parked one.
fn job_id(console: &str, path: &str, key: &str) -> [u8; 16] {
    let mut h = blake3::Hasher::new();
    h.update(b"ps5upload image to console\0");
    h.update(ps5upload_core::events::console_addr(console).as_bytes());
    h.update(b"\0");
    h.update(path.as_bytes());
    h.update(b"\0");
    h.update(key.as_bytes());
    let mut id = [0u8; 16];
    id.copy_from_slice(&h.finalize().as_bytes()[..16]);
    id
}

/// Where the time pinned into an image streamed to `path` on `console` is kept between tries.
fn stamp_file(data: &std::path::Path, console: &str, path: &str) -> std::path::PathBuf {
    let mut h = blake3::Hasher::new();
    h.update(ps5upload_core::events::console_addr(console).as_bytes());
    h.update(b"\0");
    h.update(path.as_bytes());
    data.join("convert-to-console")
        .join(format!("{}.stamp", &h.finalize().to_hex()[..32]))
}

/// The time an image streamed to `path` on `console` carries (a PFS image's inodes, a generated
/// index): the one an unfinished earlier try used, kept as long as a crashed local build's
/// checkpoint, so starting again gives the same bytes and goes on from what the console holds;
/// else now, kept for the next try. `data`: the engine's data folder (none: nothing is kept).
pub(crate) fn console_stamp(data: Option<&std::path::Path>, console: &str, path: &str) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let Some(data) = data else {
        return now;
    };
    let file = stamp_file(data, console, path);
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < super::RESUME_KEPT);
    if fresh {
        if let Some(saved) = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            return saved;
        }
    }
    let _ = std::fs::create_dir_all(file.parent().unwrap_or(data));
    let _ = std::fs::write(&file, now.to_string());
    now
}

/// The image is on the console and checked: its time is not needed again.
pub(crate) fn forget_console_stamp(data: Option<&std::path::Path>, console: &str, path: &str) {
    if let Some(data) = data {
        let _ = std::fs::remove_file(stamp_file(data, console, path));
    }
}

fn hex(id: &[u8; 16]) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// The up-front space check: the image, less what the console's part file already holds, must
/// fit what the drive can take. The figures are the console's own.
fn room_check(
    size: u64,
    held: u64,
    room: &ps5upload_ava1::space::Room,
) -> Result<(), ps5upload_ava1::space::SpaceRefusal> {
    let need = size.saturating_sub(held);
    if need <= room.allocatable_bytes {
        return Ok(());
    }
    Err(ps5upload_ava1::space::SpaceRefusal {
        volume: room.volume.clone(),
        need_bytes: need,
        avail_bytes: room.allocatable_bytes,
        free_bytes: room.free_bytes,
        reserve_bytes: room.reserve_bytes,
        promised_bytes: 0,
        reused_bytes: held,
        image: room.image,
        late: false,
    })
}

fn no_room(refusal: ps5upload_ava1::space::SpaceRefusal) -> anyhow::Error {
    let failure = ps5upload_ava1::upload::UploadFailure {
        reason: refusal.reason().into(),
        detail: refusal.to_string(),
    };
    anyhow::Error::from(failure).context(refusal)
}

/// Plans `source` as a `format` image and streams it to `target.path` on the console, then reads
/// it back there and compares it with the source (`mode`, `verify_chunk` as for a local build).
/// `time` pins a PFS image's inode time (and must be the time any generated index in `source`
/// carries), so every run gives the same bytes; `extra` adds what else the bytes depend on to the
/// job id (a zstd build's level and emulator). `stage` reports `plan`, `write` and `verify`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_to_console(
    format: ImageFormat,
    time: Option<i64>,
    extra: &str,
    source: &mut dyn SourceTree,
    target: &ConsoleTarget,
    mode: VerifyMode,
    verify_chunk: u64,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> anyhow::Result<Streamed> {
    if source.files().is_empty() {
        anyhow::bail!("{} has no files in it", source.describe());
    }
    let started = std::time::Instant::now();
    stage("plan", 0, 0);
    let planned = Planned::plan(format, &*source, time, cancel).map_err(anyhow::Error::msg)?;
    let size = planned.image_size();
    let want_sizes: HashMap<String, u64> = source
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    let want_empty: Vec<String> = source.empty_dirs().to_vec();
    let data: u64 = want_sizes.values().sum();
    if let Some(room) = &target.room {
        // A part file from an earlier try is credited: AVA1 goes on from it.
        let held = target
            .files
            .stat(&format!("{}.ava-part", target.path))
            .map_or(0, |(n, _)| n.min(size));
        room_check(size, held, room).map_err(no_room)?;
    }
    let key = format!(
        "{}\0{extra}",
        resume_key(format, None, Level::default(), time, &want_sizes, size)
    );
    let job = job_id(&target.console, &target.path, &key);
    let name = target
        .path
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{} names no file", target.path))?
        .to_string();
    let (runs_tx, runs_rx) = mpsc::channel::<Run>();
    let seq = Arc::new(ImageSeq::new(name.clone(), size, runs_tx));
    let manifest = ava1::manifest::single(seq.as_ref(), &name)?;
    crate::log_at!(
        Info,
        "convert",
        "to console: {} as {} ({size} bytes, {} files) -> {}:{} job={}",
        source.describe(),
        format.extension(),
        want_sizes.len(),
        target.console,
        target.path,
        hex(&job)
    );

    let mut hashing = HashingTree::new(source);
    let sent = std::thread::scope(|s| {
        let up = {
            let seq = seq.clone();
            let send = target.send;
            std::thread::Builder::new()
                .name("image-to-console".into())
                .spawn_scoped(s, move || {
                    let r = send(manifest, seq.clone(), seq.clone(), job);
                    // Whatever happened, the writer stops waiting for another run.
                    seq.close_runs();
                    r
                })
        };
        let up = match up {
            Ok(h) => h,
            Err(e) => {
                seq.close_runs();
                return Err(anyhow::anyhow!("could not start the upload: {e}"));
            }
        };
        serve_runs(&runs_rx, &up, size, &mut |w| {
            planned
                .write(&mut hashing, w, 0, cancel, &mut |d, t| stage("write", d, t))
                .map_err(|e| e.to_string())
        });
        match up.join() {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!("the upload to the console panicked")),
        }
    });
    let transfer = match sent {
        Ok(t) => t,
        Err(e) if cancel.load(Ordering::Relaxed) => {
            crate::log_at!(Info, "convert", "to console: cancelled ({e:#})");
            anyhow::bail!("cancelled");
        }
        Err(e) => return Err(e),
    };
    let passes = seq.passes.load(Ordering::Relaxed);
    let sent_at = started.elapsed();
    let hashes = hashing.hashes(cancel).map_err(anyhow::Error::msg)?;

    // The read-back, on the console. A failure here removes the image: it is not what was built.
    let checked = (|| -> Result<ps5upload_fpkg::sample::VerifySummary, String> {
        let mut image =
            ps5upload_fpkg::remote_source::open_remote(target.files.clone(), &target.path)
                .map_err(|e| format!("the image could not be read back on the PS5: {e}"))?;
        verify(
            &mut *image,
            &hashes,
            &want_sizes,
            &want_empty,
            mode,
            verify_chunk,
            cancel,
            &mut |d, t| stage("verify", d, t),
        )
    })();
    let summary = match checked {
        Ok(s) => s,
        Err(e) => {
            if cancel.load(Ordering::Relaxed) {
                // Committed and whole, only not read back: it stays, and the job says so.
                anyhow::bail!(
                    "cancelled while reading {} back on the PS5; the image is there but was not checked",
                    target.path
                );
            }
            let removed = match (target.remove)(&target.path) {
                Ok(()) => "it was removed from the PS5".to_string(),
                Err(r) => format!("removing it from the PS5 failed too ({r:#}): delete it by hand"),
            };
            anyhow::bail!("{e}; {removed}");
        }
    };
    crate::engine_log::record(
        "info",
        format!(
            "image to console: format={} files={} data_bytes={data} image_bytes={size} passes={passes} send_ms={} verify_ms={} verify={} job={}",
            format.extension(),
            want_sizes.len(),
            sent_at.as_millis(),
            (started.elapsed() - sent_at).as_millis(),
            super::log_line(&summary),
            hex(&job),
        ),
    );
    let mut detail = format!(
        "{}, {} files, {data} data bytes, streamed to the PS5, {}",
        format.extension(),
        want_sizes.len(),
        summary.describe()
    );
    if let Some(free) = planned.free_bytes() {
        detail.push_str(&format!(", {free} bytes free"));
    }
    if passes > 1 {
        detail.push_str(&format!(
            ", written {passes} times after the connection dropped"
        ));
    }
    Ok(Streamed {
        built: ImageBuilt {
            image_bytes: size,
            files: want_sizes.len() as u64,
            detail,
        },
        transfer,
        passes,
    })
}

/// Serves the runs AVA1 asks for, one after another, until the upload is over.
fn serve_runs<T>(
    runs: &Receiver<Run>,
    upload: &std::thread::ScopedJoinHandle<'_, T>,
    size: u64,
    write: &mut dyn FnMut(&mut PipeWriter) -> Result<u64, String>,
) {
    loop {
        match runs.recv_timeout(POLL) {
            Ok(run) => {
                let mut w = PipeWriter::new(&run.tx, &run.abort, size);
                let ended = write(&mut w)
                    .and_then(|n| {
                        if n == size {
                            Ok(())
                        } else {
                            Err(format!("the writer made {n} bytes of the planned {size}"))
                        }
                    })
                    .and_then(|()| w.finish().map_err(|e| e.to_string()));
                // A pass that is gone does not read this; that is fine.
                let _ = run.tx.send(Piece::End(ended));
            }
            Err(RecvTimeoutError::Timeout) if upload.is_finished() => return,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
