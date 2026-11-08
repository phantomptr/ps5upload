//! `.ffpfsc`: an inner image (`.exfat`, `.ffpkg` or `.ffpfs`) as a zlib PFSC container, the one
//! file of a minimal PFS, built as the inner writer writes it, with no staged image.
//!
//! Three stages run at once. The inner writer writes into a [`Stream`] on the calling thread,
//! which cuts 64 KiB blocks straight into buffers from a fixed pool (no copy per block) and
//! hands each to the workers. The workers encode them ([`ffpfsc_block`]: zeros and blocks that
//! cannot shrink skip the deflate, every compressed block is inflated back and compared before
//! it is kept). One writer thread puts the results back in order, writes them, and returns the
//! buffers to the pool. So the calling thread only reads and cuts, memory is bounded by the
//! pool, nothing reorders, and an error anywhere (a worker's, the output's, the inner writer's)
//! comes back to the calling thread, which joins every thread before it returns.
//!
//! In the output, by byte: blocks 0..6 are the PFS (header, inodes, super-root, path table,
//! reserved, `uroot`), written last; at block 6 the PFSC header and its table of `n + 1`
//! block offsets at `0x400`, written once the stream is done; the stored blocks from `data_at`
//! (64 KiB, grown by whole blocks when the table outgrows `0x400..0x10000`). A block is kept
//! compressed when that saves [`WrapOptions::min_block_gain`] percent and is shorter than 64 KiB
//! (a reader tells the two apart by length alone), else stored raw. The container is kept even
//! if it does not shrink: a stream cannot be re-read to store the image raw instead.
//!
//! [`ffpfsc_block`]: ps5upload_fpkg::ffpfsc_block

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ps5upload_fpkg::ffpfsc_block::{BlockEncoder, BlockOptions, Level, Stored};
use ps5upload_fpkg::{Error, Result};

use crate::image::{self, Sink};
use crate::{BLOCK, check_cancel, err, now};

/// Where the inner image's container starts: after header, inodes, super-root, path table,
/// the reserved block and `uroot`.
const FILE_BLOCK: u64 = 6;
pub(crate) const PFSC_MAGIC: u32 = 0x4353_4650;
const PFSC_VERSION_WORD: u32 = 6;
pub(crate) const PFSC_HEADER_LEN: usize = 0x30;
pub(crate) const PFSC_TABLE_AT: u64 = 0x400;
/// Blocks in flight per worker: the pool the stream cuts into.
const DEPTH_PER_WORKER: usize = 8;

#[derive(Debug, Clone)]
pub struct WrapOptions {
    /// Deflate level, 1–12.
    pub level: u32,
    /// Percent a block must shrink by to be stored compressed (at most 90).
    pub min_block_gain: u8,
    /// Also try level 12 on each block `level` shrank, keeping the shorter.
    pub strong: bool,
    /// Worker threads; 0 uses every core.
    pub threads: usize,
    /// The PFS timestamp (seconds since 1970); `None` is now.
    pub time: Option<i64>,
}

impl WrapOptions {
    /// The options of a compression level, every core, the time now.
    pub fn at(level: Level) -> Self {
        let b = level.options();
        Self {
            level: b.level,
            min_block_gain: b.min_gain,
            strong: b.strong,
            threads: 0,
            time: None,
        }
    }

    fn block(&self) -> BlockOptions {
        BlockOptions {
            level: self.level,
            min_gain: self.min_block_gain,
            strong: self.strong,
        }
    }

    /// What encodes the blocks, for a log line.
    pub fn encoder(&self) -> String {
        self.block().encoder()
    }

    /// The worker count these options mean on this machine.
    pub fn worker_threads(&self) -> usize {
        match self.threads {
            0 => ps5upload_fpkg::workshop::workshop_threads(),
            n => n,
        }
    }
}

/// [`Level::Balanced`].
impl Default for WrapOptions {
    fn default() -> Self {
        Self::at(Level::Balanced)
    }
}

/// What was written.
#[derive(Debug, Clone, Default)]
pub struct WrapReport {
    pub image_size: u64,
    /// The inner image's length.
    pub raw_size: u64,
    /// The container's length (header, table, stored blocks).
    pub stored_size: u64,
    /// 64 KiB blocks of the inner image.
    pub blocks: u64,
    /// Of those, the ones stored compressed (zero blocks included).
    pub compressed_blocks: u64,
    /// Blocks of zeros (stored compressed, never deflated).
    pub zero_blocks: u64,
    /// Blocks stored raw because their bytes ruled out a gain, without a deflate.
    pub prechecked_blocks: u64,
    /// Compressed blocks the stronger encoder made shorter.
    pub strong_blocks: u64,
    /// Blocks for which a libdeflate stream was turned down as one the console misreads.
    pub refused_blocks: u64,
    /// Of those, the blocks stored as zlib-rs's stream instead.
    pub fallback_blocks: u64,
    pub threads: usize,
    /// The calling thread's time in the inner writer (reading the source and cutting blocks),
    /// less the time it waited for a free buffer.
    pub feed: Duration,
    /// The calling thread's wait for a free buffer: the workers or the writer were behind.
    pub feed_wait: Duration,
    /// Encoding time summed over the workers.
    pub encode: Duration,
    /// The writer's time in writes to the output.
    pub write: Duration,
    /// The whole stream, first byte to last block written.
    pub wall: Duration,
}

/// Bytes between the container's start and its first block: 64 KiB, grown by whole blocks
/// when the offset table outgrows `0x400..0x10000`.
fn data_at(raw_size: u64) -> Result<u64> {
    let table = raw_size
        .div_ceil(BLOCK)
        .checked_add(1)
        .and_then(|n| n.checked_mul(8))
        .ok_or_else(too_big)?;
    let room = BLOCK - PFSC_TABLE_AT;
    let extra = table.saturating_sub(room).div_ceil(BLOCK) * BLOCK;
    BLOCK.checked_add(extra).ok_or_else(too_big)
}

/// The largest `.ffpfsc` an inner image of `raw_size` bytes can make (every block stored
/// raw), for preflight.
pub fn container_size_max(raw_size: u64) -> Result<u64> {
    let blocks = raw_size.div_ceil(BLOCK);
    (FILE_BLOCK * BLOCK)
        .checked_add(data_at(raw_size)?)
        .and_then(|n| n.checked_add(blocks.checked_mul(BLOCK)?))
        .ok_or_else(too_big)
}

/// Build a `.ffpfsc` holding `inner_name` (whose name tells SMP the filesystem) into `out`, an
/// empty file the caller owns, fsyncs and renames. `fill` writes the inner image into the
/// [`Stream`]: exactly `raw_size` bytes, forward only. Returns what `fill` returned.
pub fn wrap<T>(
    inner_name: &str,
    raw_size: u64,
    out: &mut File,
    opts: &WrapOptions,
    cancel: &AtomicBool,
    fill: impl FnOnce(&mut Stream<'_>) -> Result<T>,
) -> Result<(T, WrapReport)> {
    let encode = |e: &mut BlockEncoder, raw: &[u8]| e.encode(raw);
    wrap_with(inner_name, raw_size, out, opts, cancel, &encode, fill)
}

/// Where a `.ffpfsc` build goes on from, and how it says where the next one can.
///
/// What makes going on possible: a block's stored form is a function of its 64 KiB and the
/// options alone (each worker's encoder is reset per block, and the writer puts the results back
/// in order), so the container a crashed build left is, block for block, what a new one would
/// write, as long as the inner image is the same bytes. The offset table is the one thing kept
/// only in memory until the end; at each checkpoint the entries so far go into its place in the
/// file, so a new build reads them back from there.
pub struct Resume<'a> {
    /// Inner-image blocks already stored in the output (0: from the start, an empty output).
    pub blocks: u64,
    /// Container bytes between checkpoints.
    pub every: u64,
    /// Told `(blocks, bytes)` once the first `blocks` blocks are durable in the output, which
    /// holds `bytes` for certain. An error stops the build.
    pub saved: &'a (dyn Fn(u64, u64) -> io::Result<()> + Sync),
}

/// [`wrap`] going on from `resume.blocks` blocks a crashed build stored in `out`, and saving
/// checkpoints as it writes. `fill` writes the whole inner image as ever: the blocks already
/// stored are cut and dropped, so it may write zeros in place of their bytes.
pub fn wrap_resumable<T>(
    inner_name: &str,
    raw_size: u64,
    out: &mut File,
    opts: &WrapOptions,
    cancel: &AtomicBool,
    resume: &Resume<'_>,
    fill: impl FnOnce(&mut Stream<'_>) -> Result<T>,
) -> Result<(T, WrapReport)> {
    let encode = |e: &mut BlockEncoder, raw: &[u8]| e.encode(raw);
    wrap_inner(
        inner_name,
        raw_size,
        out,
        opts,
        cancel,
        &encode,
        Some(resume),
        fill,
    )
}

/// The table entries a crashed build saved for its first `n` blocks, checked for sense: they
/// start at `data_at`, and every block takes between 1 byte and a whole block.
fn saved_offsets(out: &mut File, base: u64, data_at: u64, n: u64, blocks: u64) -> Result<Vec<u64>> {
    if n > blocks {
        return Err(err(format!(
            "the checkpoint says {n} blocks of an image of {blocks}"
        )));
    }
    let mut raw = vec![0u8; ((n + 1) * 8) as usize];
    out.seek(SeekFrom::Start(base + PFSC_TABLE_AT))?;
    out.read_exact(&mut raw)?;
    let offsets: Vec<u64> = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .collect();
    let sound = offsets.first() == Some(&data_at)
        && offsets
            .windows(2)
            .all(|w| w[1] > w[0] && w[1] - w[0] <= BLOCK);
    if !sound {
        return Err(err("the block table saved for going on is damaged"));
    }
    Ok(offsets)
}

/// How a worker encodes one block with its own encoder (a seam for the tests).
type Encode = dyn Fn(&mut BlockEncoder, &[u8]) -> io::Result<Stored> + Sync;

/// Why a block has no stored form.
enum Failure {
    Io(io::Error),
    Panic(String),
}

/// A block back from a worker: its place, its buffer (for the pool, and the bytes when raw),
/// and what it became.
type Done = (u64, Vec<u8>, std::result::Result<Stored, Failure>);

/// What the writer hands back: every block's start in the container then the end, and the
/// counts and time for the report.
struct Written {
    offsets: Vec<u64>,
    report: WrapReport,
}

fn wrap_with<T>(
    inner_name: &str,
    raw_size: u64,
    out: &mut File,
    opts: &WrapOptions,
    cancel: &AtomicBool,
    encode: &Encode,
    fill: impl FnOnce(&mut Stream<'_>) -> Result<T>,
) -> Result<(T, WrapReport)> {
    wrap_inner(inner_name, raw_size, out, opts, cancel, encode, None, fill)
}

#[allow(clippy::too_many_arguments)]
fn wrap_inner<T>(
    inner_name: &str,
    raw_size: u64,
    out: &mut File,
    opts: &WrapOptions,
    cancel: &AtomicBool,
    encode: &Encode,
    resume: Option<&Resume<'_>>,
    fill: impl FnOnce(&mut Stream<'_>) -> Result<T>,
) -> Result<(T, WrapReport)> {
    let block_opts = opts.block();
    block_opts.check().map_err(err)?;
    if raw_size == 0 {
        return Err(err("the image inside a .ffpfsc cannot be empty"));
    }
    let time = opts.time.unwrap_or_else(now);
    // The worst case must fit the PFS's 32-bit pointers before anything is written.
    let data_at = data_at(raw_size)?;
    let max = container_size_max(raw_size)? - FILE_BLOCK * BLOCK;
    image::single(inner_name, raw_size, max, time)?;
    let base = FILE_BLOCK * BLOCK;
    let skip = resume.map_or(0, |r| r.blocks);
    let offsets = if skip == 0 {
        if out.seek(SeekFrom::End(0))? != 0 {
            return Err(err("the .ffpfsc output is not empty"));
        }
        vec![data_at]
    } else {
        saved_offsets(out, base, data_at, skip, raw_size.div_ceil(BLOCK))?
    };
    check_cancel(cancel)?;
    // From the start: the first block's place. Going on: past the last block stored, anything
    // a crash left after it cut off.
    let end = base + offsets.last().copied().unwrap_or(data_at);
    if skip > 0 {
        out.set_len(end)?;
    }
    out.seek(SeekFrom::Start(end))?;
    let threads = opts.worker_threads();
    let depth = threads * DEPTH_PER_WORKER;
    // Every zero block is stored as this.
    let zero = BlockEncoder::new(block_opts).zero_block()?;
    let failed = AtomicBool::new(false);
    let started = Instant::now();

    let (filled, encode_time, panicked, written) = std::thread::scope(|s| {
        let (work, queue) = mpsc::sync_channel::<(u64, Vec<u8>)>(depth);
        let (done, results) = mpsc::channel::<Done>();
        let (free, pool) = mpsc::channel::<Vec<u8>>();
        for _ in 0..depth {
            let _ = free.send(vec![0u8; BLOCK as usize]);
        }
        // Only the workers hold the queue: once they are all gone, a submit fails at once.
        let queue = Arc::new(Mutex::new(queue));
        let lane = ps5upload_fpkg::workshop::Lane::current();
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let queue = Arc::clone(&queue);
                let done = done.clone();
                s.spawn(move || {
                    lane.enter();
                    worker(&queue, &done, encode, block_opts)
                })
            })
            .collect();
        drop(queue);
        drop(done);
        let failed = &failed;
        let zero = &zero[..];
        let sink = BufWriter::with_capacity(8 << 20, &mut *out);
        let writer = s.spawn(move || {
            lane.enter();
            let r = write_blocks(results, free, sink, offsets, base, resume, zero, cancel);
            if r.is_err() {
                failed.store(true, Ordering::Relaxed);
            }
            r
        });
        let mut stream = Stream {
            work: Some(work),
            pool,
            block: None,
            fill: 0,
            seq: 0,
            skip,
            raw_size,
            pos: 0,
            cancel,
            failed,
            waited: Duration::ZERO,
        };
        let fill_started = Instant::now();
        let filled = fill(&mut stream).and_then(|t| {
            stream.finish()?;
            Ok(t)
        });
        let feed = fill_started.elapsed();
        let waited = stream.waited;
        // Closes the queue: idle workers see it and exit, then the writer.
        drop(stream);
        let mut encode_time = Duration::ZERO;
        let mut panicked = None;
        for w in workers {
            match w.join() {
                Ok(t) => encode_time += t,
                Err(payload) => {
                    panicked.get_or_insert(panic_text(&*payload));
                }
            }
        }
        let written = match writer.join() {
            Ok(r) => r,
            Err(payload) => Err(Failure::Panic(panic_text(&*payload))),
        };
        (
            filled.map(|t| (t, feed.saturating_sub(waited), waited)),
            encode_time,
            panicked,
            written,
        )
    });
    // The root error wins: a worker's panic or failure over what it made the inner writer
    // return, a cancel over the error it caused.
    if let Some(why) = panicked {
        return Err(err(format!("a compression worker panicked: {why}")));
    }
    let Written {
        offsets,
        report: counts,
    } = match written {
        Ok(w) => w,
        Err(_) if cancel.load(Ordering::Relaxed) => return Err(Error::Cancelled),
        Err(Failure::Panic(why)) => {
            return Err(err(format!("a compression worker panicked: {why}")));
        }
        Err(Failure::Io(e)) => return Err(Error::Io(e)),
    };
    let (value, feed, feed_wait) = match filled {
        Err(_) if cancel.load(Ordering::Relaxed) => return Err(Error::Cancelled),
        r => r?,
    };
    check_cancel(cancel)?;
    let wall = started.elapsed();

    // The container's header and table, then the PFS around it.
    let blocks = raw_size.div_ceil(BLOCK);
    if offsets.len() as u64 != blocks + 1 {
        return Err(err(format!(
            "internal error: {} of {blocks} blocks written",
            offsets.len().saturating_sub(1)
        )));
    }
    let stored = offsets.last().copied().unwrap_or(data_at);
    out.seek(SeekFrom::Start(base))?;
    let mut sink = Sink::new(&mut *out, cancel);
    sink.write(&pfsc_header(blocks, data_at))?;
    sink.zeros_to(PFSC_TABLE_AT)?;
    for o in &offsets {
        sink.write(&o.to_le_bytes())?;
    }
    sink.zeros_to(data_at)?;
    sink.w.flush()?;
    drop(sink);

    let img = image::single(inner_name, raw_size, stored, time)?;
    out.seek(SeekFrom::Start(0))?;
    let mut sink = Sink::new(&mut *out, cancel);
    image::write_meta(&mut sink, &img)?;
    sink.expect(base)?;
    sink.w.flush()?;
    drop(sink);
    let image_size = img.ndblock * BLOCK;
    // The zero tail of the container's last block.
    out.set_len(image_size)?;
    Ok((
        value,
        WrapReport {
            image_size,
            raw_size,
            stored_size: stored,
            blocks,
            threads,
            feed,
            feed_wait,
            encode: encode_time,
            wall,
            ..counts
        },
    ))
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

/// Puts the workers' results back in order and writes them, returning each buffer to the pool
/// once its block is out, and with `resume` makes a checkpoint every so often. `offsets`: the
/// blocks before the first to come, and where it goes. Stops at the first failure.
#[allow(clippy::too_many_arguments)]
fn write_blocks(
    results: Receiver<Done>,
    free: Sender<Vec<u8>>,
    mut out: BufWriter<&mut File>,
    mut offsets: Vec<u64>,
    base: u64,
    resume: Option<&Resume<'_>>,
    zero: &[u8],
    cancel: &AtomicBool,
) -> std::result::Result<Written, Failure> {
    let mut report = WrapReport::default();
    let mut waiting: BTreeMap<u64, (Vec<u8>, Stored)> = BTreeMap::new();
    let mut next = offsets.len() as u64 - 1;
    // Table entries already in the file (a crashed build's), and bytes since the last checkpoint.
    let mut in_table = if next > 0 { offsets.len() } else { 0 };
    let mut since = 0u64;
    for (seq, buf, result) in results {
        waiting.insert(seq, (buf, result?));
        while let Some((buf, stored)) = waiting.remove(&next) {
            if ps5upload_fpkg::workshop::checkpoint(cancel) {
                return Err(Failure::Io(io::Error::other("cancelled")));
            }
            let bytes: &[u8] = match &stored {
                Stored::Zero => {
                    report.zero_blocks += 1;
                    report.compressed_blocks += 1;
                    zero
                }
                Stored::Raw { prechecked } => {
                    report.prechecked_blocks += u64::from(*prechecked);
                    &buf
                }
                Stored::Zlib {
                    bytes,
                    strong,
                    refused,
                    fallback,
                } => {
                    report.compressed_blocks += 1;
                    report.strong_blocks += u64::from(*strong);
                    report.refused_blocks += u64::from(*refused);
                    report.fallback_blocks += u64::from(*fallback);
                    bytes
                }
            };
            let t = Instant::now();
            out.write_all(bytes).map_err(Failure::Io)?;
            report.write += t.elapsed();
            let end = offsets.last().copied().unwrap_or(0) + bytes.len() as u64;
            offsets.push(end);
            since += bytes.len() as u64;
            // The stream may be gone already (an error elsewhere): nothing to hand it then.
            let _ = free.send(buf);
            next += 1;
            if let Some(r) = resume.filter(|r| since >= r.every) {
                since = 0;
                checkpoint(&mut out, &offsets, &mut in_table, base, r, next)
                    .map_err(Failure::Io)?;
            }
        }
    }
    if !waiting.is_empty() {
        return Err(Failure::Io(io::Error::other(
            "internal error: a compressed block never came back",
        )));
    }
    let t = Instant::now();
    out.flush().map_err(Failure::Io)?;
    report.write += t.elapsed();
    Ok(Written { offsets, report })
}
/// Makes the blocks so far durable, puts their table entries in place in the file, and says so.
fn checkpoint(
    out: &mut BufWriter<&mut File>,
    offsets: &[u64],
    in_table: &mut usize,
    base: u64,
    resume: &Resume<'_>,
    blocks: u64,
) -> io::Result<()> {
    out.flush()?;
    let file: &mut File = out.get_mut();
    let end = file.stream_position()?;
    let entries: Vec<u8> = offsets[*in_table..]
        .iter()
        .flat_map(|o| o.to_le_bytes())
        .collect();
    file.seek(SeekFrom::Start(base + PFSC_TABLE_AT + 8 * *in_table as u64))?;
    file.write_all(&entries)?;
    file.seek(SeekFrom::Start(end))?;
    file.sync_data()?;
    *in_table = offsets.len();
    (resume.saved)(blocks, end)
}

fn pfsc_header(blocks: u64, data_at: u64) -> [u8; PFSC_HEADER_LEN] {
    let mut h = [0u8; PFSC_HEADER_LEN];
    h[0x00..0x04].copy_from_slice(&PFSC_MAGIC.to_le_bytes());
    h[0x08..0x0C].copy_from_slice(&PFSC_VERSION_WORD.to_le_bytes());
    h[0x0C..0x10].copy_from_slice(&(BLOCK as u32).to_le_bytes());
    h[0x10..0x18].copy_from_slice(&BLOCK.to_le_bytes());
    h[0x18..0x20].copy_from_slice(&PFSC_TABLE_AT.to_le_bytes());
    h[0x20..0x28].copy_from_slice(&data_at.to_le_bytes());
    h[0x28..0x30].copy_from_slice(&(blocks * BLOCK).to_le_bytes());
    h
}

/// Takes blocks until the queue closes, and returns the time spent encoding. A panic in the
/// encoder is caught and sent as the block's failure, so the writer never waits for a block
/// that will not come; the queue lock is never held across the encode.
fn worker(
    queue: &Mutex<Receiver<(u64, Vec<u8>)>>,
    done: &Sender<Done>,
    encode: &Encode,
    opts: BlockOptions,
) -> Duration {
    let mut encoder = BlockEncoder::new(opts);
    let mut busy = Duration::ZERO;
    loop {
        let next = match queue.lock() {
            Ok(q) => q.recv(),
            Err(_) => return busy,
        };
        let Ok((seq, raw)) = next else {
            return busy;
        };
        ps5upload_fpkg::workshop::sync_thread_priority();
        let t = Instant::now();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| encode(&mut encoder, &raw)));
        busy += t.elapsed();
        let (result, fatal) = match result {
            Ok(r) => (r.map_err(Failure::Io), false),
            Err(payload) => (Err(Failure::Panic(panic_text(&*payload))), true),
        };
        // The writer may be gone already (an error elsewhere); nothing to tell it then.
        if done.send((seq, raw, result)).is_err() || fatal {
            return busy;
        }
    }
}

/// The inner image as it is written: forward-only `Write + Seek` on the calling thread.
/// Seeking forward writes zeros (cancellable per block); `End(0)` is the current position;
/// going back is an error.
pub struct Stream<'a> {
    /// To the workers; `None` once the last block is in.
    work: Option<SyncSender<(u64, Vec<u8>)>>,
    /// Free 64 KiB buffers, back from the writer.
    pool: Receiver<Vec<u8>>,
    /// The block being cut, `fill` bytes of it so far.
    block: Option<Vec<u8>>,
    fill: usize,
    /// The next block's place.
    seq: u64,
    /// Blocks before this one are stored already (going on from a crash): cut, then dropped.
    skip: u64,
    raw_size: u64,
    /// Inner-image bytes taken.
    pos: u64,
    cancel: &'a AtomicBool,
    /// Set by the writer when it stops on an error.
    failed: &'a AtomicBool,
    /// Time spent waiting for a free buffer.
    waited: Duration,
}

fn worker_failed() -> io::Error {
    io::Error::other("compression worker failed")
}

impl Stream<'_> {
    fn take(&mut self, mut buf: &[u8]) -> io::Result<()> {
        if self
            .pos
            .checked_add(buf.len() as u64)
            .is_none_or(|e| e > self.raw_size)
        {
            return Err(io::Error::other(format!(
                "the inner image runs past its planned {} bytes",
                self.raw_size
            )));
        }
        while !buf.is_empty() {
            let fill = self.fill;
            let block = match &mut self.block {
                Some(b) => b,
                None => {
                    let t = Instant::now();
                    let b = self.pool.recv().map_err(|_| worker_failed())?;
                    self.waited += t.elapsed();
                    self.block.insert(b)
                }
            };
            let n = (BLOCK as usize - fill).min(buf.len());
            block[fill..fill + n].copy_from_slice(&buf[..n]);
            self.fill += n;
            buf = &buf[n..];
            self.pos += n as u64;
            if self.fill == BLOCK as usize {
                self.submit()?;
            }
        }
        Ok(())
    }

    fn zeros(&mut self, mut n: u64) -> io::Result<()> {
        static ZEROS: [u8; BLOCK as usize] = [0; BLOCK as usize];
        while n > 0 {
            let k = (BLOCK - self.fill as u64).min(n);
            self.take(&ZEROS[..k as usize])?;
            n -= k;
        }
        Ok(())
    }

    /// Hands the cut block to the workers. The queue holds as many as the pool has buffers, so
    /// the send never waits.
    fn submit(&mut self) -> io::Result<()> {
        if ps5upload_fpkg::workshop::checkpoint(self.cancel) {
            return Err(io::Error::other("the .ffpfsc build was cancelled"));
        }
        if self.failed.load(Ordering::Relaxed) {
            return Err(worker_failed());
        }
        if self.seq < self.skip {
            // Stored already: the buffer is kept for the next block.
            self.fill = 0;
            self.seq += 1;
            return Ok(());
        }
        let Some(mut block) = self.block.take() else {
            return Ok(());
        };
        // A pooled buffer holds an earlier block: the short last one is padded with zeros.
        block[self.fill..].fill(0);
        self.fill = 0;
        let work = self.work.as_ref().ok_or_else(worker_failed)?;
        work.send((self.seq, block)).map_err(|_| worker_failed())?;
        self.seq += 1;
        Ok(())
    }

    /// The last (padded) block, then closes the queue.
    fn finish(&mut self) -> Result<()> {
        if self.pos != self.raw_size {
            return Err(err(format!(
                "the inner image is {} bytes, not the planned {}",
                self.pos, self.raw_size
            )));
        }
        // A kept buffer with nothing cut into it is no block.
        if self.block.is_some() && self.fill > 0 {
            self.submit()?;
        }
        self.work = None;
        Ok(())
    }
}

impl Write for Stream<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.take(buf)?;
        Ok(buf.len())
    }

    /// Blocks are written by the writer thread as they come; `wrap` waits for the rest.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Stream<'_> {
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
            _ => Err(io::Error::other("the .ffpfsc stream only moves forward")),
        }
    }
}

fn too_big() -> Error {
    err("the image is too large for a .ffpfsc")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pfs-wrap-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Runs `f` on its own thread and fails the test if it does not return in time: every
    /// failure path must join its workers and come back, never hang.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(30))
            .expect("wrap did not return: a thread is stuck")
    }

    fn opts(threads: usize) -> WrapOptions {
        WrapOptions {
            threads,
            time: Some(1_700_000_000),
            ..WrapOptions::default()
        }
    }

    /// Writes `n` bytes of compressible data in odd-sized pieces.
    fn feed(s: &mut Stream<'_>, n: u64) -> Result<()> {
        let piece: Vec<u8> = (0..10_007u32).map(|i| (i % 7) as u8).collect();
        let mut left = n;
        while left > 0 {
            let k = left.min(piece.len() as u64) as usize;
            s.write_all(&piece[..k])?;
            left -= k as u64;
        }
        Ok(())
    }

    fn plain(e: &mut BlockEncoder, raw: &[u8]) -> io::Result<Stored> {
        e.encode(raw)
    }

    #[test]
    fn data_at_grows_with_the_table() {
        let room_blocks = (BLOCK - PFSC_TABLE_AT) / 8 - 1;
        assert_eq!(data_at(1).unwrap(), BLOCK);
        assert_eq!(data_at(room_blocks * BLOCK).unwrap(), BLOCK);
        assert_eq!(data_at(room_blocks * BLOCK + 1).unwrap(), 2 * BLOCK);
        assert_eq!(
            container_size_max(room_blocks * BLOCK + 1).unwrap(),
            6 * BLOCK + 2 * BLOCK + (room_blocks + 1) * BLOCK
        );
        assert!(container_size_max(u64::MAX).is_err());
    }

    #[test]
    fn fill_of_the_wrong_length_fails() {
        let d = temp("len");
        for (wrote, planned) in [(100_000u64, 200_000u64), (300_000, 200_000)] {
            let path = d.join(format!("{wrote}.ffpfsc"));
            let mut out = File::create(&path).unwrap();
            let cancel = AtomicBool::new(false);
            let e = wrap("x.exfat", planned, &mut out, &opts(2), &cancel, |s| {
                feed(s, wrote)
            })
            .unwrap_err();
            let e = e.to_string();
            assert!(e.contains("planned"), "{e}");
        }
    }

    #[test]
    fn a_worker_error_is_the_error() {
        let d = temp("werr");
        let r = bounded(move || {
            let mut out = File::create(d.join("e.ffpfsc")).unwrap();
            let calls = AtomicUsize::new(0);
            let encode = move |e: &mut BlockEncoder, raw: &[u8]| {
                if calls.fetch_add(1, Ordering::Relaxed) == 5 {
                    return Err(io::Error::other("zlib broke"));
                }
                plain(e, raw)
            };
            let cancel = AtomicBool::new(false);
            wrap_with(
                "x.exfat",
                64 * BLOCK,
                &mut out,
                &opts(2),
                &cancel,
                &encode,
                |s| feed(s, 64 * BLOCK),
            )
            .map(|_| ())
        });
        let e = r.unwrap_err().to_string();
        assert!(e.contains("zlib broke"), "{e}");
    }

    #[test]
    fn a_worker_panic_is_the_error() {
        let d = temp("panic");
        let r = bounded(move || {
            let mut out = File::create(d.join("p.ffpfsc")).unwrap();
            let calls = AtomicUsize::new(0);
            let encode = move |e: &mut BlockEncoder, raw: &[u8]| {
                if calls.fetch_add(1, Ordering::Relaxed) == 3 {
                    panic!("encoder exploded");
                }
                plain(e, raw)
            };
            let cancel = AtomicBool::new(false);
            wrap_with(
                "x.exfat",
                64 * BLOCK,
                &mut out,
                &opts(3),
                &cancel,
                &encode,
                |s| feed(s, 64 * BLOCK),
            )
            .map(|_| ())
        });
        let e = r.unwrap_err().to_string();
        assert!(e.contains("panicked: encoder exploded"), "{e}");
    }

    /// Every worker gone while the queue is at its depth: the next submit or the oldest
    /// reply fails at once instead of blocking.
    #[test]
    fn all_workers_gone_fails_fast() {
        let d = temp("gone");
        let r = bounded(move || {
            let mut out = File::create(d.join("g.ffpfsc")).unwrap();
            let encode = |_: &mut BlockEncoder, _: &[u8]| -> io::Result<Stored> {
                panic!("every worker dies")
            };
            let cancel = AtomicBool::new(false);
            let mut fill_err = None;
            let r = wrap_with(
                "x.exfat",
                256 * BLOCK,
                &mut out,
                &opts(2),
                &cancel,
                &encode,
                |s| {
                    let r = feed(s, 256 * BLOCK);
                    fill_err = r.as_ref().err().map(|e| e.to_string());
                    r
                },
            );
            (r.map(|_| ()), fill_err)
        });
        let (r, fill_err) = r;
        let fill_err = fill_err.expect("the stream itself reported the failure");
        assert!(fill_err.contains("compression worker failed"), "{fill_err}");
        let e = r.unwrap_err().to_string();
        assert!(e.contains("panicked: every worker dies"), "{e}");
    }

    /// A write error on the output comes straight back from the stream: here a read-only
    /// handle, full deque and full buffer.
    #[test]
    fn an_output_error_is_the_error() {
        let d = temp("out");
        let path = d.join("ro.ffpfsc");
        File::create(&path).unwrap();
        let r = bounded(move || {
            let mut out = File::open(&path).unwrap();
            let cancel = AtomicBool::new(false);
            // Incompressible-sized writes fill the 8 MiB buffer quickly.
            let raw = 512 * BLOCK;
            wrap("x.exfat", raw, &mut out, &opts(1), &cancel, |s| {
                let mut x = 0x2545_F491u32;
                let block: Vec<u8> = (0..BLOCK)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 17;
                        x ^= x << 5;
                        x as u8
                    })
                    .collect();
                for _ in 0..512 {
                    s.write_all(&block)?;
                }
                Ok(())
            })
            .map(|_| ())
        });
        let e = r.unwrap_err();
        assert!(matches!(e, Error::Io(_)), "{e}");
    }

    #[test]
    fn cancel_mid_stream_is_cancelled() {
        let d = temp("cancel");
        let r = bounded(move || {
            let mut out = File::create(d.join("c.ffpfsc")).unwrap();
            let cancel = AtomicBool::new(false);
            wrap("x.exfat", 64 * BLOCK, &mut out, &opts(2), &cancel, |s| {
                feed(s, 10 * BLOCK)?;
                cancel.store(true, Ordering::Relaxed);
                // A forward seek is zero fill, and cancellable like any write.
                s.seek(SeekFrom::Start(64 * BLOCK - 1))?;
                s.write_all(&[0])?;
                Ok(())
            })
            .map(|_| ())
        });
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }

    #[test]
    fn the_stream_only_moves_forward() {
        let d = temp("seek");
        let mut out = File::create(d.join("s.ffpfsc")).unwrap();
        let cancel = AtomicBool::new(false);
        let ((), report) = wrap("x.exfat", 3 * BLOCK, &mut out, &opts(2), &cancel, |s| {
            assert_eq!(s.seek(SeekFrom::End(0))?, 0);
            s.write_all(b"head")?;
            assert_eq!(s.stream_position()?, 4);
            assert!(s.seek(SeekFrom::Start(1)).is_err());
            assert!(s.seek(SeekFrom::End(1)).is_err());
            s.seek(SeekFrom::Start(3 * BLOCK - 1))?;
            s.write_all(&[1])?;
            Ok(())
        })
        .unwrap();
        assert_eq!(report.blocks, 3);
        assert_eq!(report.compressed_blocks, 3);
        assert_eq!(out.metadata().unwrap().len(), report.image_size);
    }

    /// A block's stored form follows from its bytes alone: any number of workers, and a build
    /// that goes on from a crash's checkpoint, make the same file.
    #[test]
    fn the_container_is_the_same_bytes_however_it_is_made() {
        let d = temp("same");
        let n = 40 * BLOCK + 123;
        // Compressible, random and zero stretches, so every stored form turns up.
        let mut x = 0x9E37_79B9u32;
        let raw: Vec<u8> = (0..n)
            .map(|i| match (i / BLOCK) % 3 {
                0 => (i % 13) as u8,
                1 => {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                }
                _ => 0,
            })
            .collect();
        let make = |threads: usize, path: &std::path::Path, resume: Option<&Resume<'_>>| {
            let mut out = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            let cancel = AtomicBool::new(false);
            let fill = |s: &mut Stream<'_>| {
                for c in raw.chunks(10_007) {
                    s.write_all(c)?;
                }
                Ok(())
            };
            let o = opts(threads);
            match resume {
                Some(r) => wrap_resumable("x.exfat", n, &mut out, &o, &cancel, r, fill),
                None => wrap("x.exfat", n, &mut out, &o, &cancel, fill),
            }
            .map(|((), r)| r)
        };
        let report = make(1, &d.join("one"), None).unwrap();
        assert!(report.zero_blocks > 0 && report.compressed_blocks > report.zero_blocks);
        assert!(
            report.blocks > report.compressed_blocks,
            "some blocks stored raw"
        );
        make(8, &d.join("many"), None).unwrap();
        let want = std::fs::read(d.join("one")).unwrap();
        assert!(std::fs::read(d.join("many")).unwrap() == want);

        // Stopped after a checkpoint, as a crash would; then going on from it.
        let last = Mutex::new((0u64, 0u64));
        let stop = |blocks: u64, bytes: u64| {
            *last.lock().unwrap() = (blocks, bytes);
            if blocks >= 15 {
                return Err(io::Error::other("stopped"));
            }
            Ok(())
        };
        let path = d.join("resumed");
        let crash = Resume {
            blocks: 0,
            every: 4 * BLOCK,
            saved: &stop,
        };
        assert!(make(4, &path, Some(&crash)).is_err());
        let (blocks, bytes) = *last.lock().unwrap();
        assert!(blocks >= 15 && blocks < n / BLOCK, "{blocks}");
        assert!(std::fs::metadata(&path).unwrap().len() >= bytes);
        let go_on = Resume {
            blocks,
            every: 4 * BLOCK,
            saved: &|_, _| Ok(()),
        };
        make(4, &path, Some(&go_on)).unwrap();
        assert!(std::fs::read(&path).unwrap() == want, "the same file");

        // A checkpoint the file's table cannot back is refused, not gone on from.
        let damaged = Resume {
            blocks: n / BLOCK + 5,
            every: 4 * BLOCK,
            saved: &|_, _| Ok(()),
        };
        assert!(make(4, &path, Some(&damaged)).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
