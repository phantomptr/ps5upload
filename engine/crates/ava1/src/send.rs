//! The sender (SPEC.md §11–§12, §16): manifest, map, readers, bundles, chunks, lanes,
//! credit, requeues and the governor.
//!
//! Memory (correction 1): every byte between the blocking readers and a lane carries a
//! read-ahead permit. The permit is acquired before the read and released only when the
//! frame that holds the bytes is finally dropped — acknowledged, discarded, or lost with
//! the job — so a fast source can never buffer more than the read-ahead budget in any
//! queue, however slowly the receiver acknowledges.
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use crate::conn::Frame;
use crate::gen::{
    self, Bundle, BundleRecord, Chunk, Credit, Durable, FileRetry, FileRoot, JobDone, JobMap,
    JobOpen, JobOpenAck, ManifestEnd, Received, Status,
};
use crate::governor::{self, Class, Governor, GovernorOptions, JobSummary, Mode, Sample};
use crate::manifest::Manifest;
use crate::ranges::{from_runs, Need, RangeSet};
use crate::router::{ConnTx, Inbound, JobLink, LaneTx};
use crate::source::{read_full_at, Source};
use crate::verify::{self, FileHasher, Outboard, GROUP};
use crate::wire::{FrameMessage, Message};

/// What an upload still has to put on the receiver's drive, from the manifest and the
/// receiver's JobMap (design 015/02). Only bytes the receiver reported are credited: files it
/// has in place, ranges it has made durable, and the blocks its part files already hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpaceFigures {
    /// Every file byte in the manifest.
    pub job_bytes: u64,
    /// Bytes the receiver proved durable: whole files in place plus durable ranges.
    pub durable_bytes: u64,
    /// Bytes the drive already holds for unfinished large files (JobMap `held`), clamped to
    /// what those files can hold; 0 when the receiver reports none.
    pub held_bytes: u64,
    /// The sizes of the files that are not in place yet.
    pub unfinished_bytes: u64,
    pub files_total: u64,
    pub files_done: u64,
}

impl SpaceFigures {
    /// `cutoff` is the small/large split: only a large file has a part file to be held.
    pub fn of(m: &Manifest, need: &Need, cutoff: u64) -> Result<Self, SendError> {
        let mut f = SpaceFigures::default();
        let mut large_unfinished = 0u64;
        let mut partial_durable = 0u64;
        for (i, e) in m.entries.iter().enumerate() {
            if e.kind != gen::ENTRY_FILE {
                continue;
            }
            f.files_total += 1;
            f.job_bytes = f.job_bytes.saturating_add(e.size);
            if need.done.contains(&(i as u32)) {
                f.files_done += 1;
                f.durable_bytes = f.durable_bytes.saturating_add(e.size);
                continue;
            }
            f.unfinished_bytes = f.unfinished_bytes.saturating_add(e.size);
            if e.size >= cutoff {
                large_unfinished = large_unfinished.saturating_add(e.size);
                if let Some(d) = need.partial.get(&(i as u32)) {
                    large_remaining(e.size, d)?;
                    partial_durable = partial_durable.saturating_add(d.covered());
                }
            }
        }
        f.durable_bytes = f.durable_bytes.saturating_add(partial_durable);
        // Durable bytes are on the drive; so is whatever else the part files hold.
        f.held_bytes = need.held.min(large_unfinished).max(partial_durable);
        Ok(f)
    }

    /// What the drive must still find room for: the unfinished files less what it holds.
    pub fn to_allocate(&self) -> u64 {
        self.unfinished_bytes.saturating_sub(self.held_bytes)
    }
}

/// The free-space check an upload runs once the receiver has said what it already has
/// (before any byte is sent). `Err` carries the text the user reads; the job ends with a
/// `JobCancel` (the journal stays: freeing room and retrying resumes it).
pub type SpaceGate = Arc<dyn Fn(&SpaceFigures) -> Result<(), String> + Send + Sync>;

pub struct SendOptions {
    pub kind: u8,
    pub policy: u8,
    pub flags: u32,
    pub root: String,
    /// Small/large. The protocol constant `gen::LARGE_CUTOFF` (§12.2): receivers enforce
    /// it (a Chunk for a file below it, or a BundleRecord for one at or above it, ends the
    /// job with ERR_PROTOCOL), so only tests may set another value — against a receiver
    /// configured the same way.
    pub cutoff: u64,
    pub readers: usize,
    /// Sender outboards (engine restart without re-reading).
    pub persist: Option<PathBuf>,
    pub progress: Arc<Progress>,
    pub cancel: Arc<AtomicBool>,
    /// Bytes/s the sender paces its lanes to, when the link itself is not the limit.
    pub bandwidth_cap: Option<u64>,
    /// A forward-only source (7z, solid RAR; SPEC.md §17). When set, one decode thread
    /// replaces the random-access readers and `run_upload`'s `Source` is used only for
    /// its `close()`.
    pub seq: Option<Arc<dyn crate::seq::SeqSource>>,
    /// How long to wait for a receiver's files to settle after its JobDone (SPEC.md §15.7); `None` =
    /// `SETTLE_MAX`. Tests shorten it.
    pub settle_max: Option<Duration>,
    /// How long `open_upload` waits for the JobOpenAck; `None` = `OPEN_ACK_TIMEOUT`. Tests shorten it.
    pub open_ack_timeout: Option<Duration>,
    /// The up-front free-space check (see [`SpaceGate`]); `None` = no check.
    pub space_gate: Option<SpaceGate>,
}

impl SendOptions {
    pub fn upload(root: &str) -> Self {
        Self {
            kind: gen::JOB_UPLOAD,
            policy: gen::POLICY_REPLACE,
            flags: 0,
            root: root.into(),
            cutoff: gen::LARGE_CUTOFF as u64,
            readers: 8,
            persist: None,
            progress: Arc::default(),
            cancel: Arc::default(),
            bandwidth_cap: None,
            seq: None,
            settle_max: None,
            open_ack_timeout: None,
            space_gate: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Progress {
    pub bytes_total: AtomicU64,
    pub files_total: AtomicU64,
    /// Received-acknowledged payload bytes.
    pub bytes_sent: AtomicU64,
    pub bytes_durable: AtomicU64,
    pub files_durable: AtomicU64,
    /// Payload bytes sent more than once.
    pub resent_bytes: AtomicU64,
    pub lanes: AtomicU8,
    pub bottleneck: AtomicU8,
    pub sequential: AtomicBool,
    /// Files and bytes the receiver already had when the job first opened (its map's
    /// done files): "skipped", not sent. Recorded by the first attempt only, so a
    /// reconnect that finds the earlier attempt's files done does not call them skipped.
    pub skipped_files: AtomicU64,
    pub skipped_bytes: AtomicU64,
    pub skip_recorded: AtomicBool,
    /// Sequential sources (7z/RAR resume): 1 while the decoder discards data the receiver
    /// already holds (`skip_done_bytes` of `skip_total_bytes`), else 0.
    pub phase: AtomicU8,
    pub skip_done_bytes: AtomicU64,
    pub skip_total_bytes: AtomicU64,
    /// Files are still settling on the receiver after the job's last byte (it reports
    /// unswept files); false until the receiver says so.
    pub settling: AtomicBool,
    /// The receiver's last reported `unswept` count while the sender waits for it to settle.
    pub unswept: AtomicU32,
    /// Times the sender ran for this job (more than one: it reconnected and resumed).
    pub attempts: AtomicU32,
    /// The most files the receiver reported unswept while the sender waited for it to settle.
    pub unswept_peak: AtomicU32,
    /// Milliseconds spent waiting for the receiver to settle, over all attempts.
    pub settle_ms: AtomicU64,
    /// What the per-job telemetry record keeps beyond the counters above (review 009 #4).
    pub telemetry: Mutex<Telemetry>,
}

/// The parts of a job's history no counter holds: where its time went, the console's own
/// end-of-job line and which console it was. Written by the sender, read by the engine's
/// telemetry record.
#[derive(Debug, Default, Clone)]
pub struct Telemetry {
    /// The governor's time shares and lane/chunk series, merged over every attempt.
    pub shares: JobSummary,
    /// The receiver's `JobDone` message, verbatim.
    pub console_line: Option<String>,
    /// The console's public key (the engine records a hash of it, never an address).
    pub peer_key: Option<[u8; 32]>,
}

#[derive(Debug, Clone)]
pub struct SendReport {
    pub status: u16,
    pub message: Option<String>,
    pub files: u32,
    pub bytes: u64,
    pub resent: u64,
    pub max_lanes: u8,
    pub bottleneck: u8,
    pub sequential: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("connection lost: {0}")]
    Disconnected(String),
    #[error("refused ({status}): {message}")]
    Refused { status: u16, message: String },
    #[error("cancelled")]
    Cancelled,
    /// The receiver did not answer a JobOpen in time (SPEC.md: it must answer OK or BUSY). The sender
    /// retries like a BUSY answer, within the same bound.
    #[error("the console did not answer the job open within {0:?}")]
    OpenTimeout(Duration),
    #[error("reading the source: {0}")]
    Source(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(String),
    /// The up-front free-space check said the rest of the job does not fit (design 015/02).
    #[error("not enough free space: {0}")]
    NoRoom(String),
}

impl From<crate::Ava1Error> for SendError {
    fn from(e: crate::Ava1Error) -> Self {
        SendError::Disconnected(e.to_string())
    }
}

// ---- pure helpers ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub offset: u64,
    pub len: u64,
    /// false: read only to hash it (durable at the receiver, CV unknown here).
    pub send: bool,
}

/// What to read of a large file, in offset order: groups the receiver lacks (sent, in
/// runs of at most `chunk`) and durable groups whose CV this side does not know (hashed).
///
/// Pieces with `send = false` never reach a lane: their bytes are read only to finish the
/// file's root, so a consumer must never wait for them — a relay feeds only `send` pieces
/// into the pipe (Task 24).
pub fn pieces(
    size: u64,
    durable: &RangeSet,
    have_cv: &dyn Fn(u64) -> bool,
    chunk: u64,
) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let chunk = chunk.max(GROUP) / GROUP * GROUP;
    for g in 0..verify::groups(size) {
        let off = g * GROUP;
        let len = (size - off).min(GROUP);
        let send = !durable.covers(off, off + len);
        if !send && have_cv(g) {
            continue;
        }
        match out.last_mut() {
            Some(p)
                if p.send == send
                    && p.offset + p.len == off
                    && p.len + len <= chunk
                    && (send || p.len < chunk) =>
            {
                p.len += len
            }
            _ => out.push(Piece {
                offset: off,
                len,
                send,
            }),
        }
    }
    out
}

/// Bytes a relay can withhold from its source: the destination has them durably
/// and this sender already has their CV. This is the complement of the bytes
/// `run_upload` reads. If A withholds a byte B reads, B waits forever; if A
/// sends a byte B never reads, the bounded relay buffer can fill and stall A.
/// A fresh relay has no outboards and therefore rereads all unfinished files.
pub fn skip_set(m: &Manifest, durable: &Need, persist: Option<&std::path::Path>) -> Need {
    let mut skip = Need::default();
    for (i, e) in m.entries.iter().enumerate() {
        if e.kind != gen::ENTRY_FILE {
            continue;
        }
        let id = i as u32;
        if durable.done.contains(&id) {
            skip.done.insert(id);
            continue;
        }
        // Small files are sent whole. Only large files have group outboards.
        if e.size < gen::LARGE_CUTOFF as u64 {
            continue;
        }
        let Some(ranges) = durable.partial.get(&id) else {
            continue;
        };
        let ob = persist.and_then(|dir| {
            Outboard::open(&dir.join(format!("{id}.ob")), verify::groups(e.size)).ok()
        });
        let Some(ob) = ob else { continue };
        let mut known = RangeSet::new();
        for g in 0..verify::groups(e.size) {
            let off = g * GROUP;
            let end = off + (e.size - off).min(GROUP);
            if ranges.covers(off, end) && ob.get(g).is_some() {
                known.insert(off, end);
            }
        }
        if known.is_full(e.size) {
            skip.done.insert(id);
        } else if known.covered() > 0 {
            skip.partial.insert(id, known);
        }
    }
    skip
}

type LaneFrames = HashMap<u16, (u64, BTreeMap<u32, u64>)>;

/// Credit and per-lane in-flight accounting (SPEC.md §12.3–§12.5). All arithmetic is
/// checked: `sent` refuses a frame larger than the credit instead of underflowing.
pub struct Window {
    credit: u64,
    /// The window as first granted: `initial - credit` is what the receiver still holds
    /// (frames sent and not yet returned as `Credit`).
    initial: u64,
    /// lane -> (unreceived bytes, seq -> frame length)
    lanes: LaneFrames,
    /// seq -> len, frames of dead lanes whose charge is held (I3): a lane's death
    /// releases no window credit — the receiver may still charge these frames if it
    /// admitted them — so their bytes stay charged in `credit` until they are
    /// accounted for. An entry is removed by a late `Received` (the receiver's books
    /// confirm the frame; its bytes return with the receiver's `Credit` after its
    /// apply), by `release` (the frame provably never left this process — the writer
    /// died without taking it), or the job's end (the whole window dies with the
    /// job). A `Credit` removes nothing: it enlarges the window, which is where the
    /// charge lives. Frames that did leave the process but are never confirmed stay
    /// held until the job ends — SPEC §12.3's accepted tradeoff.
    refunded: HashMap<u32, u64>,
}

impl Window {
    pub fn new(credit: u64) -> Self {
        Self {
            credit,
            initial: credit,
            lanes: HashMap::new(),
            refunded: HashMap::new(),
        }
    }
    pub fn can_send(&self, lane: u16, len: u64, cap: u64) -> bool {
        let inflight = self.lanes.get(&lane).map_or(0, |l| l.0);
        len <= self.credit && (inflight == 0 || inflight + len <= cap)
    }
    /// Charges the frame, or refuses it when it does not fit the credit.
    pub fn sent(&mut self, lane: u16, seq: u32, len: u64) -> bool {
        if len > self.credit {
            return false;
        }
        self.credit -= len;
        let l = self.lanes.entry(lane).or_default();
        l.0 += len;
        l.1.insert(seq, len);
        true
    }
    /// Returns the frame's length when it was in flight here.
    pub fn received(&mut self, seq: u32) -> Option<u64> {
        for l in self.lanes.values_mut() {
            if let Some(len) = l.1.remove(&seq) {
                l.0 -= len;
                return Some(len);
            }
        }
        // I3: a late Received proves the receiver admitted the frame and charged its
        // window for it. This side held the charge all along (a lane's death releases
        // nothing), so there is nothing to re-charge: the receiver returns the bytes
        // with a Credit when it applies the frame.
        self.refunded.remove(&seq);
        None
    }
    /// More window from the receiver (peer-controlled bytes): saturating (M4) — a plain
    /// `+=` wraps on overflow, which would shrink the window (and panic in debug builds).
    pub fn credit(&mut self, n: u64) {
        self.credit = self.credit.saturating_add(n);
    }
    /// Unreceived frames of a dead lane (I3): a lane's death does NOT release window
    /// credit. The frames are requeued for a re-send and their bytes stay charged here —
    /// the receiver may still charge them if it admitted them — so the sender can never
    /// spend the same window twice. The charge is released only when the receiver
    /// accounts for the frames (its `Credit` after the apply) or the job ends.
    pub fn lane_down(&mut self, lane: u16) -> Vec<u32> {
        let Some((_, frames)) = self.lanes.remove(&lane) else {
            return Vec::new();
        };
        frames
            .into_iter()
            .map(|(seq, len)| {
                self.refunded.insert(seq, len);
                seq
            })
            .collect()
    }
    /// Releases a dead lane's held charge for a frame the writer provably never took
    /// (its queue died with the writer): the receiver's window was never charged for
    /// it, so the bytes return to this window here. Frames that may have reached the
    /// receiver stay held until their `Received`/`Credit` or the job's end. `lane_down`
    /// must have run first (the seq is in `refunded`). Returns the length released.
    pub fn release(&mut self, seq: u32) -> Option<u64> {
        let len = self.refunded.remove(&seq)?;
        self.credit = self.credit.saturating_add(len);
        Some(len)
    }
    pub fn available(&self) -> u64 {
        self.credit
    }
    /// Window bytes the receiver is holding: sent (or charged) and not yet returned as
    /// `Credit`. Zero means the receiver has nothing it could still apply and return.
    pub fn outstanding(&self) -> u64 {
        self.initial.saturating_sub(self.credit)
    }
}

// ---- the job ---------------------------------------------------------------------------

/// A frame waiting for a lane. Its read-ahead permits (correction 1) are released when
/// the frame is dropped — acknowledged, requeued and lost, or discarded with the job — so
/// every queue between the readers and the lanes stays under the read-ahead budget.
struct OutFrame {
    ty: u8,
    body: Arc<Vec<u8>>,
    class: Class,
    /// Payload bytes (without frame and message headers), for progress.
    payload: u64,
    resend: bool,
    /// Read-ahead permits (correction 1). Never read: the drop is the accounting — the
    /// permits are released when the frame is finally disposed of.
    _budget: Vec<OwnedSemaphorePermit>,
    /// The take-marker (I3's precise form), set at pick: the lane's writer flips it
    /// the moment it takes the frame out of its queue. Un-taken at the writer's death,
    /// the frame provably never left this process, so a lane death releases its window
    /// charge instead of holding it.
    taken: Option<Arc<AtomicBool>>,
}

#[derive(Default)]
struct Sched {
    bundles: VecDeque<OutFrame>,
    chunks: VecDeque<OutFrame>,
    requeue: VecDeque<OutFrame>,
    inflight: HashMap<u32, (u16, OutFrame)>,
    bundles_inflight: usize,
    next_seq: u32,
    decision: Option<governor::Decision>,
    floor: usize,
    /// Smoothed bytes/s per lane (EWMA over ticks): lanes size their in-flight cap by it.
    lane_rate: HashMap<u16, f64>,
    /// Raw bytes acked this tick per lane, normalised into `lane_rate` at the tick.
    lane_bytes: HashMap<u16, u64>,
    credit_starved: bool,
    source_starved: bool,
    acked_tick: u64,
    small_durable_tick: u64,
    large_durable_tick: u64,
    stalls: u32,
}

/// A genuine credit stall: frames are queued and the window cannot hold the smallest
/// one, so no lane can send anything until the receiver grants Credit. When it persists
/// — nothing sent, Received or credited anywhere — the receiver is applying nothing and
/// will grant nothing, so the control loop fails the job loudly instead of parking it
/// forever (I2). A lane that is blocked only by its own in-flight cap — a frame it sent
/// larger than its cap, or a frame charged before its rate warmed — is ordinary
/// backpressure and never marks a stall: the `Received` for those bytes unblocks the
/// lane by itself.
///
/// That in-flight-cap clause is an accepted tradeoff: a receiver that keeps the link
/// alive (pings, SPEC §6 — liveness only fires on total silence) but never sends
/// `Received` or `Credit` can park the job indefinitely, because such a lane is never
/// marked stalled. SPEC §12.3 obliges the receiver to acknowledge every frame the
/// moment it has it in memory, so a receiver that breaks that obligation is outside
/// the protocol; the sender's failure mode for it is a parked job, not a false stall.
/// Recorded here so a future reader does not mistake the hang for a missing rule.
#[derive(Clone, Copy)]
struct Stall {
    since: Instant,
    grant: u64,
    smallest: u64,
}

struct Shared {
    sched: Mutex<Sched>,
    window: Mutex<Window>,
    /// The lanes' wake signal (correction 4): a versioned channel. A waiter marks the
    /// current version seen *before* checking its state, then awaits `changed()` — a wake
    /// between the check and the wait is a version bump and is never lost.
    wake_tx: watch::Sender<u64>,
    chunk: AtomicU32,
    bundle: AtomicU32,
    /// Read-ahead, in KiB permits: readers acquire before reading, frames carry the
    /// permit until they are finally dropped.
    bytes_budget: Arc<Semaphore>,
    /// The lanes' credit stall (I2): recorded when the window cannot hold the smallest
    /// queued frame, cleared by any progress (a send, Received or Credit) or by a
    /// re-check that finds the smallest queued frame fits the window again.
    stall: Mutex<Option<Stall>>,
}

impl Shared {
    fn wake(&self) {
        let v = *self.wake_tx.borrow();
        self.wake_tx.send_replace(v.wrapping_add(1));
    }
}

pub(crate) enum Read {
    Record {
        file_id: u32,
        root: [u8; 32],
        data: Vec<u8>,
        budget: OwnedSemaphorePermit,
    },
    Chunk {
        file_id: u32,
        offset: u64,
        data: Vec<u8>,
        budget: OwnedSemaphorePermit,
    },
    Root {
        file_id: u32,
        root: [u8; 32],
    },
    Failed(std::io::Error),
}

const READ_AHEAD_KIB: u32 = 96 * 1024;

/// How often the large-file reader persists its outboard (a resume cache; see its use).
const OUTBOARD_SYNC_EVERY: Duration = Duration::from_secs(1);

/// The Chunk message overhead over its data (job 16 + file 4 + offset 8 + length 4 +
/// extension count 2): the window counts lane-frame *body* bytes, so a piece's data must
/// leave room for these (I2).
const CHUNK_HDR: u64 = 34;

/// I2: a credit stall with zero progress for this long — nothing sent, Received or
/// credited — is a job the receiver can never advance (its window cannot hold the
/// smallest queued frame and it applies nothing): the control loop fails it loudly
/// instead of letting it park forever.
const STALL_FATAL: Duration = Duration::from_secs(10);

/// A credit stall while the receiver still holds window bytes is a slow receiver (a USB
/// drive in a long flush), not a dead one: it returns the credit when it has applied them,
/// and a receiver that really died is caught by the session's liveness. Only a stall with
/// no progress at all for this long fails the job, so a drive that stalls for minutes
/// does not fail an upload that a sender which never waits on the drive would finish.
const STALL_SLOW_RECEIVER_FATAL: Duration = Duration::from_secs(600);

/// How long a credit stall may last before the job fails, given the window bytes the
/// receiver still holds.
fn stall_limit(outstanding: u64) -> Duration {
    if outstanding == 0 {
        STALL_FATAL
    } else {
        STALL_SLOW_RECEIVER_FATAL
    }
}

/// The next control frame, Status frames skipped (they are advisory). Shared with the
/// receiver (`recv.rs`), which reads its manifest the same way.
pub(crate) async fn next_ctl(link: &mut JobLink) -> Result<Frame, SendError> {
    loop {
        match link.rx.recv().await {
            // M6: a Status is skipped by type, but only after its decode succeeds — a
            // malformed Status in the open window is the same protocol error as one
            // mid-transfer (the control loop decodes and errors), not something to
            // shrug off.
            Some(Inbound::Control(f)) if f.ty == Status::TYPE => {
                f.decode::<Status>()
                    .map_err(|e| SendError::Protocol(e.to_string()))?;
                continue;
            }
            Some(Inbound::Control(f)) => return Ok(f),
            Some(Inbound::Closed(why)) => return Err(SendError::Disconnected(why)),
            None => return Err(SendError::Disconnected("the session ended".into())),
            Some(_) => {} // lane events before data starts: lanes are read from the router
        }
    }
}

/// How long a sender waits for a JobOpenAck before it treats the open as lost. Generous: a receiver replays a
/// journal and may hold the open behind other work, but it answers.
pub const OPEN_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// JobOpen → ack → manifest pages → ManifestEnd → map pages. Returns (credit, need).
pub async fn open_upload(
    link: &mut JobLink,
    m: &Manifest,
    o: &SendOptions,
) -> Result<(u64, Need), SendError> {
    let job_id = link.job_id;
    link.control
        .send(&JobOpen {
            job_id,
            kind: o.kind,
            policy: o.policy,
            flags: o.flags,
            root: o.root.clone(),
            src: None,
            credit: None,
        })
        .await?;
    // A receiver answers a JobOpen promptly, OK or BUSY. One that stays silent (the open lost behind
    // a job it was closing) must not hold the sender forever: bounded wait, cancel honoured.
    let wait = o.open_ack_timeout.unwrap_or(OPEN_ACK_TIMEOUT);
    let deadline = Instant::now() + wait;
    let ack: JobOpenAck = loop {
        if o.cancel.load(Ordering::Relaxed) {
            return Err(SendError::Cancelled);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(SendError::OpenTimeout(wait));
        }
        match tokio::time::timeout(left.min(Duration::from_millis(100)), next_ctl(link)).await {
            Err(_) => continue, // re-check the cancel flag and the deadline
            Ok(f) => {
                let f = f?;
                if f.ty == JobOpenAck::TYPE {
                    break f.decode().map_err(|e| SendError::Protocol(e.to_string()))?;
                }
            }
        }
    };
    if ack.status != gen::STATUS_OK {
        return Err(SendError::Refused {
            status: ack.status,
            message: ack.message.unwrap_or_default(),
        });
    }
    for p in m.pages(job_id) {
        link.control.send(&p).await?;
    }
    link.control
        .send(&ManifestEnd {
            job_id,
            files: m.files(),
            bytes: m.bytes(),
            manifest_hash: m.hash(),
        })
        .await?;
    let mut need = Need::default();
    let mut credit = ack.credit;
    loop {
        let f = next_ctl(link).await?;
        if f.ty == JobDone::TYPE {
            // A job that failed in prepare: the map carried the status already, or it is this.
            let d: JobDone = f.decode().map_err(|e| SendError::Protocol(e.to_string()))?;
            return Err(SendError::Refused {
                status: d.status,
                message: d.message.unwrap_or_default(),
            });
        }
        if f.ty == Credit::TYPE {
            // M5: a granting receiver may send Credit between the ack and the map —
            // fold it into the window instead of discarding it (an under-granted sender).
            let c: Credit = f.decode().map_err(|e| SendError::Protocol(e.to_string()))?;
            credit = credit.saturating_add(c.bytes);
            continue;
        }
        if f.ty != JobMap::TYPE {
            continue;
        }
        let map: JobMap = f.decode().map_err(|e| SendError::Protocol(e.to_string()))?;
        if map.status != gen::STATUS_OK {
            return Err(SendError::Refused {
                status: map.status,
                message: map.message.unwrap_or_default(),
            });
        }
        need.add_page(&map);
        if map.last == 1 {
            return Ok((credit, need));
        }
    }
}

/// The blocking readers. Small files: `readers` threads over a shared queue. Large files:
/// one thread, file by file, piece by piece, hashing every group it reads. Every thread
/// checks the job's shutdown flag each iteration (I1) and returns its handle, so the
/// control loop can close the budget and join them on teardown.
#[allow(clippy::too_many_arguments)]
fn spawn_readers(
    m: Arc<Manifest>,
    src: Arc<dyn Source>,
    small: Arc<Mutex<VecDeque<u32>>>,
    large: Arc<Mutex<VecDeque<(u32, RangeSet)>>>,
    sh: Arc<Shared>,
    o: &SendOptions,
    tx: mpsc::UnboundedSender<Read>,
    stop: Arc<AtomicBool>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let rt = tokio::runtime::Handle::current();
    let mut handles = Vec::new();
    let cancel = o.cancel.clone();
    for _ in 0..o.readers.max(1) {
        let (m, src, small, sh, tx, rt, stop, cancel) = (
            m.clone(),
            src.clone(),
            small.clone(),
            sh.clone(),
            tx.clone(),
            rt.clone(),
            stop.clone(),
            cancel.clone(),
        );
        handles.push(tokio::task::spawn_blocking(move || loop {
            if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                return;
            }
            let Some(id) = small.lock().unwrap().pop_front() else {
                return;
            };
            let Some(e) = m.entry(id) else {
                return;
            };
            let kib = (e.size / 1024 + 1).min(READ_AHEAD_KIB as u64) as u32;
            // I1: a closed budget (the job ended) wakes a parked reader; it must leave,
            // not panic on the acquire or park forever.
            let Ok(budget) = rt.block_on(sh.bytes_budget.clone().acquire_many_owned(kib)) else {
                return;
            };
            let mut data = vec![0u8; e.size as usize];
            let r = src
                .open(&e.path)
                .and_then(|mut f| read_full_at(f.as_mut(), 0, &mut data));
            match r {
                Ok(n) if n as u64 == e.size => {
                    let root = *blake3::hash(&data).as_bytes();
                    let _ = tx.send(Read::Record {
                        file_id: id,
                        root,
                        data,
                        budget,
                    });
                }
                Ok(_) => {
                    let _ = tx.send(Read::Failed(std::io::Error::other(format!(
                        "{} changed while it was being sent",
                        e.path
                    ))));
                    return;
                }
                Err(err) => {
                    let _ = tx.send(Read::Failed(std::io::Error::new(
                        err.kind(),
                        format!("{}: {err}", e.path),
                    )));
                    return;
                }
            }
        }));
    }
    let persist = o.persist.clone();
    handles.push(tokio::task::spawn_blocking(move || loop {
        if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return;
        }
        let Some((id, durable)) = large.lock().unwrap().pop_front() else {
            return;
        };
        let Some(e) = m.entry(id).cloned() else {
            return;
        };
        let mut hasher = FileHasher::new(e.size);
        let mut ob = persist.as_ref().and_then(|d| {
            std::fs::create_dir_all(d).ok()?;
            Outboard::open(&d.join(format!("{id}.ob")), verify::groups(e.size)).ok()
        });
        if let Some(ob) = &ob {
            for g in 0..verify::groups(e.size) {
                if let Some(cv) = ob.get(g) {
                    hasher.set_cv(g, cv);
                }
            }
        }
        let mut f = match src.open(&e.path) {
            Ok(f) => f,
            Err(err) => {
                let _ = tx.send(Read::Failed(err));
                return;
            }
        };
        let chunk = sh.chunk.load(Ordering::Relaxed) as u64;
        // I2: a piece is never larger than the credit the receiver has already granted.
        // The window counts frame *body* bytes, so the piece data is capped at
        // floor((grant − CHUNK_HDR) / GROUP) groups — one group minimum (`pieces` floors
        // again, idempotently). A grant that cannot hold one group leaves the lane-side
        // stall to fail the job loudly instead of parking forever.
        let grant = sh.window.lock().unwrap().available();
        let chunk = (chunk.min(grant.saturating_sub(CHUNK_HDR)) / GROUP * GROUP).max(GROUP);
        let plan = pieces(e.size, &durable, &|g| hasher.cv(g).is_some(), chunk);
        let last_piece = plan.len().saturating_sub(1);
        let mut ob_synced = Instant::now();
        for (pi, p) in plan.into_iter().enumerate() {
            if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                return;
            }
            // The permit is acquired before the read and rides with the frame: only the
            // frame's final drop releases it (correction 1). Hash-only pieces are read
            // into one transient buffer and never enter a queue, so they hold none.
            let budget = if p.send {
                match rt.block_on(
                    sh.bytes_budget
                        .clone()
                        .acquire_many_owned((p.len / 1024 + 1) as u32),
                ) {
                    // I1: the job ended while this reader was parked on the budget.
                    Ok(b) => Some(b),
                    Err(_) => return,
                }
            } else {
                None
            };
            let mut data = vec![0u8; p.len as usize];
            match read_full_at(f.as_mut(), p.offset, &mut data) {
                Ok(n) if n as u64 == p.len => {}
                Ok(_) => {
                    let _ = tx.send(Read::Failed(std::io::Error::other(format!(
                        "{} changed while it was being sent",
                        e.path
                    ))));
                    return;
                }
                Err(err) => {
                    let _ = tx.send(Read::Failed(err));
                    return;
                }
            }
            for (k, g) in data.chunks(GROUP as usize).enumerate() {
                let gi = p.offset / GROUP + k as u64;
                hasher.add_group(gi, g);
                if let (Some(ob), Some(cv)) = (ob.as_mut(), hasher.cv(gi)) {
                    let _ = ob.put(gi, &cv);
                }
            }
            // The outboard is a resume cache: a sync costs a whole-image rewrite, an fsync
            // (F_FULLFSYNC on macOS, ~10 ms) and a rename, so it runs about once a second and
            // at the file's last piece, not once per 4 MiB piece. A crash between syncs only
            // loses the CVs since the last one, which the resume re-hashes.
            if pi == last_piece || ob_synced.elapsed() >= OUTBOARD_SYNC_EVERY {
                if let Some(ob) = ob.as_mut() {
                    let _ = ob.sync();
                }
                ob_synced = Instant::now();
            }
            if p.send {
                let budget = budget.expect("send pieces hold a read-ahead permit");
                let _ = tx.send(Read::Chunk {
                    file_id: id,
                    offset: p.offset,
                    data,
                    budget,
                });
            }
        }
        if let Some(root) = hasher.root() {
            let _ = tx.send(Read::Root { file_id: id, root });
        }
    }));
    handles
}

/// The decode thread of a sequential source (SPEC.md §17).
fn spawn_decoder(ctx: crate::seq::DecodeCtx) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || crate::seq::run(ctx))
}

/// Packs records into bundles; flushes a partial bundle when no record is waiting.
fn bundle_frame(job_id: [u8; 16], recs: Vec<(BundleRecord, OwnedSemaphorePermit)>) -> OutFrame {
    let payload = recs.iter().map(|r| r.0.data.len() as u64).sum();
    let body = Bundle {
        job_id,
        records: recs.iter().map(|r| r.0.clone()).collect(),
    }
    .to_bytes()
    .expect("bundle encodes");
    let budget = recs.into_iter().map(|r| r.1).collect();
    OutFrame {
        ty: Bundle::TYPE,
        body: Arc::new(body),
        class: Class::Bundle,
        payload,
        resend: false,
        _budget: budget,
        taken: None,
    }
}

fn chunk_frame(
    job_id: [u8; 16],
    file_id: u32,
    offset: u64,
    data: Vec<u8>,
    budget: OwnedSemaphorePermit,
) -> OutFrame {
    let payload = data.len() as u64;
    let body = Chunk {
        job_id,
        file_id,
        offset,
        data,
    }
    .to_bytes()
    .expect("chunk encodes");
    OutFrame {
        ty: Chunk::TYPE,
        body: Arc::new(body),
        class: Class::Stream,
        payload,
        resend: false,
        _budget: vec![budget],
        taken: None,
    }
}

/// Takes the first frame a lane can send, in the scheduler's order (the requeue first,
/// then the governor's class preference). I2: `can_send` judges exactly the frame that
/// leaves the queue — but a front frame that does not fit no longer blocks the frames
/// behind it (the head-of-line credit stall): the scan skips it and takes a later one
/// that fits. `None` with a non-empty queue means nothing fits this lane at all — the
/// caller tells the window's reason (credit) from the lane's own in-flight cap and
/// records a stall only for the former.
fn pick_any(s: &mut Sched, lane: u16, w: &Window, cap: u64) -> Option<OutFrame> {
    let fits = |f: &OutFrame| w.can_send(lane, f.body.len() as u64, cap);
    if !s.requeue.is_empty() {
        if let Some(i) = s.requeue.iter().position(fits) {
            return s.requeue.remove(i);
        }
    }
    let d = s.decision?;
    let bundle_first = match d.mode {
        Mode::BundleOnly => true,
        Mode::StreamOnly => false,
        Mode::Mixed => s.bundles_inflight < s.floor || d.prefer == Class::Bundle,
    };
    let order: [&mut VecDeque<OutFrame>; 2] = if bundle_first {
        [&mut s.bundles, &mut s.chunks]
    } else {
        [&mut s.chunks, &mut s.bundles]
    };
    for q in order {
        if let Some(i) = q.iter().position(&fits) {
            return q.remove(i);
        }
    }
    None
}

/// The smallest queued frame's body length, or `None` when no frame is queued. The
/// stall rule (I2) compares this against the window: a frame the window cannot hold
/// means no lane can send anything (a credit stall); one that fits the window but not
/// a lane's in-flight cap is that lane's backpressure, never a stall.
fn smallest_queued(s: &Sched) -> Option<u64> {
    s.requeue
        .iter()
        .chain(s.bundles.iter())
        .chain(s.chunks.iter())
        .map(|f| f.body.len() as u64)
        .min()
}

/// One tick's lane-rate update: EWMA-smoothed bytes/s per lane, from the raw bytes acked
/// this tick. Correction 3: the smoothed rate persists across ticks (a lane with no bytes
/// this tick keeps its rate), and lanes size their in-flight cap by it.
fn smooth_rates(lane_bytes: &mut HashMap<u16, u64>, lane_rate: &mut HashMap<u16, f64>, secs: f64) {
    let secs = secs.max(0.001);
    for (lane, bytes) in lane_bytes.drain() {
        let rate = bytes as f64 / secs;
        let slot = lane_rate.entry(lane).or_default();
        *slot += (rate - *slot) * 0.5;
    }
}

/// One per lane: take the next frame the window allows, send it, repeat. Exits when the
/// lane's send fails (the control loop sees LaneDown and requeues) or `stop` is set.
async fn lane_task(lane: LaneTx, sh: Arc<Shared>, cap_bps: Option<u64>, stop: Arc<AtomicBool>) {
    let started = Instant::now();
    let mut sent_bytes = 0u64;
    let mut wake = sh.wake_tx.subscribe();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // The version is marked seen before the state check: a wake between the check and
        // the wait below lands as a version bump and is not lost (correction 4).
        let _ = *wake.borrow_and_update();
        let next = {
            let mut s = sh.sched.lock().unwrap();
            let mut w = sh.window.lock().unwrap();
            let chunk = sh.chunk.load(Ordering::Relaxed);
            let rate = s.lane_rate.get(&lane.id).copied().unwrap_or(0.0);
            let cap = governor::inflight_cap(chunk, rate);
            match pick_any(&mut s, lane.id, &w, cap) {
                Some(mut f) => {
                    let len = f.body.len() as u64;
                    s.next_seq += 1;
                    let seq = s.next_seq;
                    let ty = f.ty;
                    // Shared, not copied: the frame stays in `inflight` for a resend.
                    let body = f.body.clone();
                    if f.class == Class::Bundle {
                        s.bundles_inflight += 1;
                    }
                    assert!(w.sent(lane.id, seq, len), "can_send passed");
                    // The take-marker: the writer flips it the moment it takes the
                    // frame out of its queue (see `LaneDown` in `run_upload`).
                    let taken = Arc::new(AtomicBool::new(false));
                    f.taken = Some(taken.clone());
                    s.inflight.insert(seq, (lane.id, f));
                    drop(w);
                    drop(s);
                    // Progress: a stall another lane observed is not a deadlock.
                    *sh.stall.lock().unwrap() = None;
                    Some((seq, ty, body, taken))
                }
                None => {
                    let pending =
                        !s.requeue.is_empty() || !s.bundles.is_empty() || !s.chunks.is_empty();
                    if pending {
                        let grant = w.available();
                        let smallest = smallest_queued(&s).unwrap_or(0);
                        let mut stall = sh.stall.lock().unwrap();
                        if smallest > grant {
                            // I2: a genuine credit stall — the window cannot hold the
                            // smallest queued frame, so nothing can be sent on any lane
                            // until the receiver grants Credit. When that persists with
                            // no send, Received or Credit anywhere, the receiver is
                            // applying nothing and will grant nothing — the control loop
                            // fails the job loudly instead of parking it forever. Record
                            // the window and the smallest queued frame once (progress
                            // clears it; the earliest mark wins).
                            s.credit_starved = true;
                            if stall.is_none() {
                                *stall = Some(Stall {
                                    since: Instant::now(),
                                    grant,
                                    smallest,
                                });
                            }
                        } else {
                            // The smallest frame fits the window but not this lane's
                            // in-flight cap (a frame larger than the cap, or charged
                            // before its rate warmed): ordinary per-lane backpressure.
                            // The Received for those bytes — or another lane's send —
                            // moves the job by itself; it must never mark a stall, and
                            // a stale mark from a window that has since changed is
                            // cleared here. Accepted tradeoff (see `Stall`): a receiver
                            // that keeps the link alive but never acknowledges can
                            // park such a lane forever — SPEC §12.3 obliges the
                            // receiver to acknowledge promptly, so the parking receiver
                            // is the one outside the protocol.
                            *stall = None;
                        }
                    } else {
                        s.source_starved = true;
                    }
                    None
                }
            }
        };
        let Some((seq, ty, body, taken)) = next else {
            if wake.changed().await.is_err() {
                return;
            }
            continue;
        };
        if let Some(bps) = cap_bps {
            sent_bytes += body.len() as u64; // plaintext bytes, as before
            let due = Duration::from_secs_f64(sent_bytes as f64 / bps as f64);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                tokio::time::sleep(wait).await;
            }
        }
        // Queued whole or not at all (the outbox), so this task may be cancelled here
        // without leaving half a sealed frame on the lane. A failed send is not
        // accounted here: the lane's death holds or releases the charge through the
        // take-marker (a send that failed was never taken).
        if lane
            .tx
            .send_raw_marked(ty, 0, seq, body, taken)
            .await
            .is_err()
        {
            return;
        }
    }
}

/// The `LaneDown` handling (SPEC §12.3, I3's precise form): the dead lane's frames are
/// requeued with their charge held, and the charge of exactly the frames the writer
/// provably never took is released. The requeue sweep runs immediately (frames can be
/// re-picked by a live lane without waiting for the writer's end); the release runs
/// only once the writer's end is confirmed by `tx.writer_dead()`. The take-markers are
/// read twice: the sweep never reads their values — a release is decided only by the
/// final read after the writer's end. The writer may still be mid-poll while the sweep
/// runs (the lane's stop flag takes effect at its next check, and an abort at its next
/// yield), so it can take a frame — flip the marker, write it to the socket — after
/// the sweep: releasing such a frame's charge would spend the window twice and the
/// receiver's ERR_CREDIT fails a healthy job. The bounded wait cannot deadlock: the
/// writer's death is driven by the lane's own link tasks (its write fails, or the
/// lane's close aborts it), never by this loop.
async fn lane_death(sh: Arc<Shared>, id: u16, tx: ConnTx) {
    let seqs = sh.window.lock().unwrap().lane_down(id);
    // The sched guard is scoped: it must not live across the bounded wait below
    // (the job's future is spawned and Send).
    let untaken: Vec<(u32, Option<Arc<AtomicBool>>)> = {
        let mut s = sh.sched.lock().unwrap();
        let mut untaken = Vec::new();
        for seq in seqs {
            if let Some((_, mut f)) = s.inflight.remove(&seq) {
                if f.class == Class::Bundle {
                    s.bundles_inflight -= 1;
                }
                f.resend = true;
                // The marker's value is deliberately not read here: the writer may
                // still take the frame after this sweep, so a read now is stale by
                // construction. Only the marker itself is kept for the final read.
                untaken.push((seq, f.taken.clone()));
                s.requeue.push_back(f);
            }
        }
        s.stalls += 1;
        untaken
    };
    // The writer must have ended before the markers are final — a frame still
    // queued when it ends was never taken. Wait (bounded) for its end; a writer
    // that lingers leaves the charge held (the conservative I3 rule).
    let deadline = Instant::now() + Duration::from_millis(250);
    while !tx.writer_dead() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if tx.writer_dead() {
        let mut w = sh.window.lock().unwrap();
        for (seq, taken) in untaken {
            // The final read, after the writer's end: no further flip is possible, so
            // a marker still unset here proves the frame never left this process and
            // its charge is released; a marker the writer set after the sweep means
            // the frame may have reached the receiver — its charge stays held.
            if taken.as_ref().is_none_or(|t| !t.load(Ordering::SeqCst)) {
                let _ = w.release(seq);
            }
        }
    }
    // A pick that slipped past the stop flag after the sweep charged a frame to this
    // dead lane; its send fails against the closed queue (the writer has ended), so it
    // would sit charged in `inflight` forever — lost bytes, not just a leak. Sweep once
    // more now that the markers are final: the straggler is requeued like the rest, and
    // its charge is released too (its send failed, so it was never taken).
    let strays = sh.window.lock().unwrap().lane_down(id);
    if !strays.is_empty() {
        let mut s = sh.sched.lock().unwrap();
        for seq in strays {
            if let Some((_, mut f)) = s.inflight.remove(&seq) {
                if f.class == Class::Bundle {
                    s.bundles_inflight -= 1;
                }
                f.resend = true;
                let taken = f.taken.as_ref().is_some_and(|t| t.load(Ordering::SeqCst));
                if !taken {
                    let _ = sh.window.lock().unwrap().release(seq);
                }
                s.requeue.push_back(f);
            }
        }
    }
}

/// The bytes of a file of `size` still to send when the receiver says `have` is durable. A
/// range reaching past the end of the file (or more bytes than the file has) is the receiver
/// talking nonsense, not something to subtract: a protocol error.
fn large_remaining(size: u64, have: &RangeSet) -> Result<u64, SendError> {
    if have.iter().any(|(_, end)| end > size) || have.covered() > size {
        return Err(SendError::Protocol(
            "the receiver reports durable ranges beyond the end of a file".into(),
        ));
    }
    Ok(size - have.covered())
}

/// The data phase of an upload: what `open_upload` returned (credit, need) in, a report
/// (or the error that ended the job) out. Every lane task is cancelled and joined before
/// this returns, on every exit (correction 5).
pub async fn run_upload(
    link: &mut JobLink,
    manifest: Arc<Manifest>,
    source: Arc<dyn Source>,
    opts: SendOptions,
    opened: (u64, Need),
) -> Result<SendReport, SendError> {
    let (credit, need) = opened;
    let job_id = link.job_id;
    let pg = opts.progress.clone();
    pg.attempts.fetch_add(1, Ordering::Relaxed);
    pg.bytes_total.store(manifest.bytes(), Ordering::Relaxed);
    pg.files_total
        .store(manifest.files() as u64, Ordering::Relaxed);

    // What is left to send.
    let (mut small, mut large) = (VecDeque::new(), VecDeque::new());
    let (mut small_left, mut large_left) = (0u64, 0u64);
    let mut durable_files: HashSet<u32> = need.done.iter().copied().collect();
    for (i, e) in manifest.entries.iter().enumerate() {
        let id = i as u32;
        if e.kind != gen::ENTRY_FILE || durable_files.contains(&id) {
            continue;
        }
        if e.size < opts.cutoff {
            small.push_back(id);
            small_left += e.size;
        } else {
            let d = need.partial.get(&id).cloned().unwrap_or_default();
            large_left = large_left.saturating_add(large_remaining(e.size, &d)?);
            large.push_back((id, d));
        }
    }
    let done_bytes: u64 = durable_files
        .iter()
        .map(|i| manifest.entry(*i).map_or(0, |e| e.size))
        .sum::<u64>()
        + need
            .partial
            .values()
            .fold(0u64, |a, r| a.saturating_add(r.covered()));
    // The up-front free-space check, before any byte is sent: what the receiver reported it
    // already has is credited (design 015/02), the rest must fit.
    if let Some(gate) = opts.space_gate.clone() {
        let figures = SpaceFigures::of(&manifest, &need, opts.cutoff)?;
        let verdict = tokio::task::spawn_blocking(move || gate(&figures))
            .await
            .map_err(|e| SendError::Protocol(format!("the space check failed: {e}")))?;
        if let Err(why) = verdict {
            let _ = link
                .control
                .send(&gen::JobCancel {
                    job_id,
                    reason: gen::ERR_NO_SPACE,
                })
                .await;
            return Err(SendError::NoRoom(why));
        }
    }
    if !pg.skip_recorded.swap(true, Ordering::Relaxed) {
        let done_files = need
            .done
            .iter()
            .filter_map(|i| manifest.entry(*i))
            .filter(|e| e.kind == gen::ENTRY_FILE);
        let (n, b) = done_files.fold((0u64, 0u64), |(n, b), e| (n + 1, b + e.size));
        pg.skipped_files.store(n, Ordering::Relaxed);
        pg.skipped_bytes.store(b, Ordering::Relaxed);
    }
    pg.bytes_durable.store(done_bytes, Ordering::Relaxed);
    pg.files_durable
        .store(durable_files.len() as u64, Ordering::Relaxed);

    let sh = Arc::new(Shared {
        sched: Mutex::new(Sched {
            floor: 4,
            ..Default::default()
        }),
        window: Mutex::new(Window::new(credit)),
        wake_tx: watch::channel(0).0,
        chunk: AtomicU32::new(governor::START_CHUNK),
        bundle: AtomicU32::new(governor::START_BUNDLE),
        bytes_budget: Arc::new(Semaphore::new(READ_AHEAD_KIB as usize)),
        stall: Mutex::new(None),
    });
    // `PS5UPLOAD_AVA1_LANES` / `_CHUNK` pin the governor (benchmarking only).
    let mut gov = Governor::with_options(GovernorOptions::from_env());
    let first = gov.tick(&Sample::default());
    sh.chunk.store(first.chunk, Ordering::Relaxed);
    let mut summary = JobSummary::default();
    sh.sched.lock().unwrap().decision = Some(first);
    let small_q = Arc::new(Mutex::new(small));
    let large_q = Arc::new(Mutex::new(large));
    let (rtx, mut rrx) = mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let seq_retries = Arc::new(crate::seq::Retries::default());
    let budget_wait: Arc<crate::seq::BudgetWait> = Arc::default();
    let mut budget_wait_seen = 0u64;
    let mut readers = if let Some(seq) = opts.seq.clone() {
        // One decode thread for a forward-only source (SPEC.md §17).
        let chunk_sh = sh.clone();
        vec![spawn_decoder(crate::seq::DecodeCtx {
            seq,
            manifest: manifest.clone(),
            need: need.clone(),
            cutoff: opts.cutoff,
            persist: opts.persist.clone(),
            budget: sh.bytes_budget.clone(),
            budget_wait: budget_wait.clone(),
            chunk: Box::new(move || {
                // I2: never larger than the credit already granted (see the large reader).
                let grant = chunk_sh.window.lock().unwrap().available();
                let chunk = chunk_sh.chunk.load(Ordering::Relaxed) as u64;
                (chunk.min(grant.saturating_sub(CHUNK_HDR)) / GROUP * GROUP).max(GROUP)
            }),
            tx: rtx.clone(),
            stop: stop.clone(),
            cancel: opts.cancel.clone(),
            retries: seq_retries.clone(),
            rt: tokio::runtime::Handle::current(),
        })]
    } else {
        spawn_readers(
            manifest.clone(),
            source.clone(),
            small_q.clone(),
            large_q.clone(),
            sh.clone(),
            &opts,
            rtx.clone(),
            stop.clone(),
        )
    };

    // Lanes: the governor's starting count is opened at the top of the loop below
    // (client side), so an open failure breaks into the teardown like every other
    // exit (I1); tasks adopt any already up. Each lane has its own stop flag (a lane
    // death stops only its own task) and keeps its outbox clone so the control loop
    // can learn when the dead lane's writer has ended (I3's precise form).
    let mut lane_tasks: HashMap<u16, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut lane_links: HashMap<u16, (Arc<AtomicBool>, ConnTx)> = HashMap::new();
    let spawn_lane = |id: u16,
                      tasks: &mut HashMap<u16, tokio::task::JoinHandle<()>>,
                      links: &mut HashMap<u16, (Arc<AtomicBool>, ConnTx)>,
                      link: &JobLink| {
        if tasks.contains_key(&id) {
            return;
        }
        if let Some(l) = link.lane(id) {
            let stop = Arc::new(AtomicBool::new(false));
            let h = tokio::spawn(lane_task(
                l.clone(),
                sh.clone(),
                opts.bandwidth_cap,
                stop.clone(),
            ));
            tasks.insert(id, h);
            links.insert(id, (stop, l.tx.clone()));
        }
    };
    for l in link.lanes() {
        spawn_lane(l.id, &mut lane_tasks, &mut lane_links, link);
    }

    let mut pending: Vec<(BundleRecord, OwnedSemaphorePermit)> = Vec::new();
    let mut pending_bytes = 0usize;
    let mut retries: HashMap<u32, u32> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_tick = Instant::now();
    let (mut max_lanes, mut receiver_bn, mut sequential) = (0u8, gen::BN_NONE, false);
    let mut last_bn = gen::BN_NONE;
    let mut root_next: VecDeque<(u32, [u8; 32])> = VecDeque::new();
    // A reader failure: the first error wins; its JobCancel is sent on the dedicated
    // arm below (awaiting it inline deadlocks against the receiver's backpressure).
    let mut io_fail: Option<std::io::Error> = None;
    // The starting lane count is opened once, on the first pass (the tick below owns
    // every later open/close against the governor's live target — re-opening here on
    // every pass would fight a target the governor has since lowered, one open and one
    // close per tick).
    let mut opened_start = false;
    // The receiver's JobDone carried `settling`: files are still being made durable in place.
    let mut settle_after = false;
    let result = 'job: loop {
        if opts.cancel.load(Ordering::Relaxed) {
            let _ = link
                .control
                .send(&gen::JobCancel {
                    job_id,
                    reason: gen::ERR_CANCELLED,
                })
                .await;
            break Err(SendError::Cancelled);
        }
        if !opened_start {
            opened_start = true;
            // Open the governor's starting lane count (client side). A failure here must
            // break into the teardown below like every other exit (I1): a `?` return
            // from `run_upload` skips the stop flag, the budget close and the reader
            // joins, so a reader parked on the read-ahead semaphore would stay parked
            // forever and the readers would keep reading the source.
            if let Some(op) = link.opener().cloned() {
                while link.lanes().len() < first.lanes as usize {
                    if let Err(e) = op.open().await {
                        break 'job Err(SendError::Disconnected(e.to_string()));
                    }
                }
            }
        }
        tokio::select! {
            r = rrx.recv() => match r {
                Some(Read::Record { file_id, root, data, budget }) => {
                    pending_bytes += data.len() + 48;
                    pending.push((BundleRecord { file_id, root, data }, budget));
                    // A slow source never holds a half-full bundle back: flush when the
                    // record channel is momentarily empty; a fast source keeps it full and
                    // the bundles reach the governor's size.
                    let flush = pending_bytes >= sh.bundle.load(Ordering::Relaxed) as usize || rrx.is_empty();
                    if flush {
                        let f = bundle_frame(job_id, std::mem::take(&mut pending));
                        pending_bytes = 0;
                        sh.sched.lock().unwrap().bundles.push_back(f);
                        sh.wake();
                    }
                }
                Some(Read::Chunk { file_id, offset, data, budget }) => {
                    let f = chunk_frame(job_id, file_id, offset, data, budget);
                    sh.sched.lock().unwrap().chunks.push_back(f);
                    sh.wake();
                }
                Some(Read::Root { file_id, root }) => {
                    // Queued here, awaited on the select arm below: the loop must keep
                    // draining the inbox while the control outbox is full, or the sender's
                    // and the receiver's backpressure deadlock against each other — both
                    // sides awaiting room on full control queues. The queue is bounded by
                    // the file count (the roots already waiting in `rtx`).
                    root_next.push_back((file_id, root));
                }
                Some(Read::Failed(e)) if io_fail.is_none() => {
                    // Queued like the roots: awaiting the JobCancel inline stops the
                    // loop's drain of the inbox while the control outbox is full, and
                    // the sender's and the receiver's backpressure then deadlock
                    // against each other — both sides awaiting room on full control
                    // queues (the shape the FileRoot fix removed). The cancel is sent
                    // on the dedicated arm below; the first failure wins.
                    io_fail = Some(e);
                }
                Some(Read::Failed(_)) => {} // a failure is already queued: it wins
                // The retry path holds `rtx` alive, so the channel never closes mid-job.
                None => {}
            },
            root_sent = async {
                let (file_id, root) = root_next
                    .front()
                    .copied()
                    .expect("guarded by `if !root_next.is_empty()`");
                link.control.send(&FileRoot { job_id, file_id, root }).await
            }, if !root_next.is_empty() => {
                if let Err(e) = root_sent {
                    break Err(SendError::Disconnected(e.to_string()));
                }
                root_next.pop_front();
            },
            cancel_sent = async {
                link.control
                    .send(&gen::JobCancel { job_id, reason: gen::ERR_IO })
                    .await
            }, if io_fail.is_some() => {
                // The JobCancel is on the wire (or the connection refused it, which
                // the inline version ignored too): end the job with the reader's
                // error.
                let _ = cancel_sent;
                break Err(SendError::Source(
                    io_fail.take().expect("guarded by `if io_fail.is_some()`"),
                ));
            },
            ev = link.rx.recv() => match ev {
                None => break Err(SendError::Disconnected("the session ended".into())),
                Some(Inbound::Closed(why)) => break Err(SendError::Disconnected(why)),
                Some(Inbound::LaneUp(id)) => {
                    spawn_lane(id, &mut lane_tasks, &mut lane_links, link)
                }
                Some(Inbound::LaneDown(id)) => {
                    // I3's precise form: the dead lane's frames are requeued with
                    // their charge held — but a frame whose take-marker is still
                    // unset provably never left this process (it was still in the
                    // writer's queue when the writer died), so the receiver's window
                    // was never charged for it and its charge is released here.
                    // Holding the charge of every killed frame instead — the blunt
                    // rule — leaks window credit forever on frames the receiver
                    // never saw, and after enough churn the window cannot hold the
                    // smallest queued frame: the stall detector then fails a healthy
                    // job. See `a_dead_lane_releases_only_frames_the_writer_never_took`
                    // and `lane_churn_never_wedges_the_window`.
                    if let Some((stop, tx)) = lane_links.remove(&id) {
                        // Stop (never abort) the lane's task: a send already in
                        // flight must resolve against the closed queue so its marker
                        // is final. The task is detached here so the id can be
                        // re-joined; it exits at its next check or failed send.
                        stop.store(true, Ordering::Relaxed);
                        lane_tasks.remove(&id);
                        lane_death(sh.clone(), id, tx).await;
                    }
                    sh.wake();
                }
                Some(Inbound::Lane { .. }) => {} // an uploader receives nothing on lanes
                Some(Inbound::Control(f)) => match f.ty {
                    Received::TYPE => match f.decode::<Received>() {
                        Ok(r) => {                            let got = sh.window.lock().unwrap().received(r.seq);
                            let mut s = sh.sched.lock().unwrap();
                            if let (Some(len), Some((lane, fr))) = (got, s.inflight.remove(&r.seq)) {
                                if fr.class == Class::Bundle {
                                    s.bundles_inflight -= 1;
                                }
                                s.acked_tick += len;
                                *s.lane_bytes.entry(lane).or_default() += len;
                                pg.bytes_sent.fetch_add(fr.payload, Ordering::Relaxed);
                                if fr.resend {
                                    pg.resent_bytes.fetch_add(fr.payload, Ordering::Relaxed);
                                }
                            }
                            drop(s);
                            *sh.stall.lock().unwrap() = None; // bytes moved: not a deadlock
                            sh.wake();
                        }
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    Credit::TYPE => match f.decode::<Credit>() {
                        Ok(c) => {                            sh.window.lock().unwrap().credit(c.bytes);
                            *sh.stall.lock().unwrap() = None; // the window moved: not a deadlock
                            sh.wake();
                        }
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    Durable::TYPE => match f.decode::<Durable>() {
                        Ok(d) => {
                            let mut s = sh.sched.lock().unwrap();
                            for id in from_runs(&d.files) {
                                if durable_files.insert(id) {
                                    let size = manifest.entry(id).map_or(0, |e| e.size);
                                    pg.files_durable.fetch_add(1, Ordering::Relaxed);
                                    if size < opts.cutoff {
                                        pg.bytes_durable.fetch_add(size, Ordering::Relaxed);
                                        s.small_durable_tick += size;
                                        small_left = small_left.saturating_sub(size);
                                    }
                                }
                            }
                            for r in &d.ranges {
                                pg.bytes_durable.fetch_add(r.len, Ordering::Relaxed);
                                s.large_durable_tick += r.len;
                                large_left = large_left.saturating_sub(r.len);
                            }
                        }
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    FileRetry::TYPE => match f.decode::<FileRetry>() {
                        Ok(r) => {
                            // M7: an id outside the manifest would silently land in the
                            // large queue, where the reader exits and the job stalls.
                            let Some(entry) = manifest.entry(r.file_id) else {
                                break Err(SendError::Protocol(format!(
                                    "FileRetry for a file not in the manifest: id {}",
                                    r.file_id
                                )));
                            };
                            let n = retries.entry(r.file_id).or_default();
                            *n += 1;
                            if *n > 3 {
                                let _ = link.control.send(&gen::JobCancel { job_id, reason: gen::ERR_VERIFY }).await;
                                break Err(SendError::Protocol(format!(
                                    "{} failed verification 3 times",
                                    entry.path
                                )));
                            }
                            if let Some(d) = &opts.persist {
                                let _ = std::fs::remove_file(d.join(format!("{}.ob", r.file_id)));
                            }
                            if opts.seq.is_some() {
                                // The decode thread runs a further pass for this file.
                                seq_retries.push(r.file_id);
                            } else if entry.size < opts.cutoff {
                                small_q.lock().unwrap().push_back(r.file_id);
                            } else {
                                large_q.lock().unwrap().push_back((r.file_id, RangeSet::new()));
                            }
                            // The reader threads may have exited; start a fresh set for the retried file.
                            if opts.seq.is_none() {
                                readers.extend(spawn_readers(manifest.clone(), source.clone(), small_q.clone(), large_q.clone(), sh.clone(), &opts, rtx.clone(), stop.clone()));
                            }
                        }
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    Status::TYPE => match f.decode::<Status>() {
                        Ok(st) => {
                            receiver_bn = st.bottleneck;
                            sh.sched.lock().unwrap().floor = st.workers.max(1) as usize;
                        }
                        // M6: like every other control frame, a malformed Status is a
                        // protocol error, not something to shrug off.
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    JobDone::TYPE => match f.decode::<JobDone>() {
                        Ok(d) => {
                            settle_after = d.settling == Some(1);
                            pg.telemetry.lock().unwrap().console_line = d.message.clone();
                            break Ok(SendReport {
                            status: d.status,
                            message: d.message,
                            files: d.files,
                            bytes: d.bytes,
                            resent: pg.resent_bytes.load(Ordering::Relaxed),
                            max_lanes,
                            bottleneck: last_bn,
                            sequential,
                            });
                        }
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    // The receiver ended the job (cancel, disk full, verify...): its
                    // session may stay open, so nothing else would tell this task.
                    gen::JobCancel::TYPE => match f.decode::<gen::JobCancel>() {
                        Ok(c) => break Err(SendError::Refused {
                            status: c.reason,
                            message: "the receiver ended the job".into(),
                        }),
                        Err(e) => break Err(SendError::Protocol(e.to_string())),
                    },
                    _ => {}
                },
            },
            _ = tick.tick() => {
                // I2: a stall that has persisted — nothing sent, Received or credited
                // since it began — is a job the receiver can never advance: fail it
                // loudly instead of parking forever.
                // The receiver holding window bytes is a slow disk, not a window that can never
                // fit a frame: only a stall with nothing outstanding fails quickly.
                let stalled = *sh.stall.lock().unwrap();
                if let Some(st) = stalled {
                    let limit = stall_limit(sh.window.lock().unwrap().outstanding());
                    if st.since.elapsed() >= limit {
                        break Err(SendError::Protocol(format!(
                            "no queued frame fits the receiver's window ({} bytes granted, the smallest queued frame is {} bytes) and nothing was sent, received or credited for {:?}",
                            st.grant, st.smallest, limit
                        )));
                    }
                }
                let secs = last_tick.elapsed().as_secs_f64();
                last_tick = Instant::now();
                let lanes_now = link.lanes().len() as u8;
                // Read every tick (never behind a short-circuit) so `seen` always tracks the
                // previous tick: a park from an earlier tick must not hide this tick's starvation.
                let budget_waited = budget_wait.waited_since(&mut budget_wait_seen);
                let sample = {
                    let mut s = sh.sched.lock().unwrap();
                    let mut lane_bytes = std::mem::take(&mut s.lane_bytes);
                    smooth_rates(&mut lane_bytes, &mut s.lane_rate, secs);
                    Sample {
                        secs,
                        bytes_acked: std::mem::take(&mut s.acked_tick),
                        lanes: lanes_now,
                        stalls: std::mem::take(&mut s.stalls),
                        credit_starved: std::mem::take(&mut s.credit_starved),
                        // A decode thread parked on the read-ahead budget means the lanes
                        // are the limit (SPEC.md 17.4): an empty queue is not the source.
                        source_starved: std::mem::take(&mut s.source_starved)
                            && s.requeue.is_empty()
                            && !budget_waited,
                        receiver_bottleneck: receiver_bn,
                        small_durable: std::mem::take(&mut s.small_durable_tick),
                        large_durable: std::mem::take(&mut s.large_durable_tick),
                        small_left,
                        large_left,
                    }
                };
                let d = gov.tick(&sample);
                summary.observe(&sample, &d);
                sh.chunk.store(d.chunk, Ordering::Relaxed);
                sh.bundle.store(d.bundle, Ordering::Relaxed);
                sh.sched.lock().unwrap().decision = Some(d);
                sequential = d.sequential;
                last_bn = d.bottleneck;
                pg.bottleneck.store(d.bottleneck, Ordering::Relaxed);
                pg.sequential.store(d.sequential, Ordering::Relaxed);
                pg.lanes.store(lanes_now, Ordering::Relaxed);
                max_lanes = max_lanes.max(lanes_now);
                if let Some(op) = link.opener().cloned() {
                    if lanes_now < d.lanes {
                        let _ = op.open().await; // LaneUp follows
                    } else if lanes_now > d.lanes {
                        if let Some(l) = link.lanes().last() {
                            op.close(l.id); // LaneDown follows and requeues
                        }
                    }
                }
                sh.wake();
            }
        }
    };
    let mut result = result;
    if settle_after && result.is_ok() {
        let max = opts.settle_max.unwrap_or(SETTLE_MAX);
        let why = settle_wait(link, &pg, &opts.cancel, max).await;
        match (&why, result.as_mut()) {
            (Settled::Failed(code, msg), Ok(_)) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "[ava1] upload: the console cannot make its files durable: {msg}"
                );
                result = Err(SendError::Refused {
                    status: *code,
                    message: if msg.is_empty() {
                        "the console cannot make its files durable".into()
                    } else {
                        msg.clone()
                    },
                });
            }
            (_, Ok(rep)) => {
                if let Some(w) = settle_warning(&why) {
                    rep.message = Some(match rep.message.take() {
                        Some(m) => format!("{m}; {w}"),
                        None => w,
                    });
                }
            }
            _ => {}
        }
    }
    // Every exit: stop the readers' flag, wake the sleepers, cancel and join every
    // lane task still held (correction 5; a dead lane's task was detached at its
    // LaneDown and exits on its own) — and then the readers (I1): close the read-ahead
    // budget so a reader parked on it wakes, drain the queues so the frames' permits
    // are released, and join every reader thread before the job's future returns.
    // Nothing of this job keeps running after the return.
    stop.store(true, Ordering::Relaxed);
    sh.wake();
    source.close();
    if let Some(seq) = &opts.seq {
        seq.close();
    }
    for (_, h) in lane_tasks.drain() {
        h.abort();
        let _ = h.await;
    }
    sh.bytes_budget.close();
    {
        let mut s = sh.sched.lock().unwrap();
        s.bundles.clear();
        s.chunks.clear();
        s.requeue.clear();
        s.inflight.clear();
        s.bundles_inflight = 0;
    }
    for h in readers {
        let _ = h.await;
    }
    pg.telemetry.lock().unwrap().shares.merge(&summary);
    if let Some(line) = summary.line() {
        // writeln!, not eprintln!: a dead parent's closed stderr must not panic the engine.
        let _ = writeln!(std::io::stderr(), "{line}");
    }
    result
}

/// How long a sender waits for a receiver's files to finish settling (SPEC.md §15.7) before it
/// reports anyway: the report is true either way (every byte is durable through the receiver's log).
pub const SETTLE_MAX: Duration = Duration::from_secs(30);

/// How a settle wait ended.
#[derive(Debug, PartialEq, Eq)]
enum Settled {
    /// The receiver reported `unswept` = 0.
    Done,
    /// `settle_max` passed with files still unswept.
    TimedOut,
    /// The job was cancelled while waiting.
    Cancelled,
    /// The session closed first.
    Closed,
    /// The receiver reported it cannot make the files durable (Status `code`).
    Failed(u16, String),
}

/// Adds a settle wait's length to `Progress::settle_ms` however the wait ends.
struct SettleTimer<'a>(&'a Progress, Instant);

impl Drop for SettleTimer<'_> {
    fn drop(&mut self) {
        let ms = self.1.elapsed().as_millis() as u64;
        self.0.settle_ms.fetch_add(ms, Ordering::Relaxed);
    }
}

/// The receiver said files are still settling: keep the job alive while its `Status` reports `unswept`, so
/// the engine can show "finishing on the console". Ends when it reaches 0, the receiver reports a failure
/// (Status `code`, the sweep's sticky error), the job is cancelled, the session closes, or after `max`.
/// Polls `cancel` every 100 ms.
async fn settle_wait(
    link: &mut JobLink,
    pg: &Progress,
    cancel: &AtomicBool,
    max: Duration,
) -> Settled {
    pg.settling.store(true, Ordering::Relaxed);
    let t0 = Instant::now();
    let _timed = SettleTimer(pg, t0);
    let out = loop {
        if cancel.load(Ordering::Relaxed) {
            break Settled::Cancelled;
        }
        let Some(left) = max.checked_sub(t0.elapsed()) else {
            break Settled::TimedOut;
        };
        match tokio::time::timeout(left.min(Duration::from_millis(100)), link.rx.recv()).await {
            Ok(Some(Inbound::Control(f))) if f.ty == Status::TYPE => {
                let Ok(st) = f.decode::<Status>() else {
                    continue;
                };
                if let Some(code) = st.code.filter(|c| *c != 0) {
                    break Settled::Failed(code, st.current.unwrap_or_default());
                }
                let n = st.unswept.unwrap_or(0);
                pg.unswept.store(n, Ordering::Relaxed);
                pg.unswept_peak.fetch_max(n, Ordering::Relaxed);
                if n == 0 {
                    break Settled::Done;
                }
            }
            Ok(Some(Inbound::Closed(_))) | Ok(None) => break Settled::Closed,
            Ok(Some(_)) | Err(_) => {}
        }
    };
    pg.unswept.store(0, Ordering::Relaxed);
    pg.settling.store(false, Ordering::Relaxed);
    out
}

/// The warning a report carries when the receiver's files were not confirmed durable in place: the bytes
/// are safe in its log (an upload is never resent for this), but the job did not end clean.
fn settle_warning(why: &Settled) -> Option<String> {
    let w = match why {
        Settled::Done => return None,
        Settled::TimedOut => "files are still being made durable on the console",
        Settled::Cancelled => {
            "cancelled while files were still being made durable on the console (they are safe in its log)"
        }
        Settled::Closed => {
            "the console closed the session while files were still being made durable (they are safe in its log)"
        }
        Settled::Failed(..) => return None,
    };
    let _ = writeln!(std::io::stderr(), "[ava1] upload: {w}");
    Some(w.to_string())
}

/// One upload from open to report: `open_upload` then `run_upload`.
pub async fn send_job(
    link: &mut JobLink,
    manifest: Arc<Manifest>,
    source: Arc<dyn Source>,
    opts: SendOptions,
) -> Result<SendReport, SendError> {
    let opened = open_upload(link, &manifest, &opts).await?;
    run_upload(link, manifest, source, opts, opened).await
}

/// The responder's half of a download: ack, manifest, then the opener's map pages.
async fn open_as_responder(
    link: &mut JobLink,
    open: &JobOpen,
    m: &Manifest,
) -> Result<(u64, Need), SendError> {
    let job_id = link.job_id;
    link.control
        .send(&JobOpenAck {
            job_id,
            status: gen::STATUS_OK,
            credit: 0,
            staged: 0,
            workers: 0,
            message: None,
        })
        .await
        .map_err(|e| SendError::Disconnected(e.to_string()))?;
    for p in m.pages(job_id) {
        link.control
            .send(&p)
            .await
            .map_err(|e| SendError::Disconnected(e.to_string()))?;
    }
    link.control
        .send(&ManifestEnd {
            job_id,
            files: m.files(),
            bytes: m.bytes(),
            manifest_hash: m.hash(),
        })
        .await
        .map_err(|e| SendError::Disconnected(e.to_string()))?;
    let mut need = Need::default();
    loop {
        let f = next_ctl(link).await?;
        if f.ty != JobMap::TYPE {
            continue;
        }
        let map: JobMap = f.decode().map_err(|e| SendError::Protocol(e.to_string()))?;
        if map.status != gen::STATUS_OK {
            return Err(SendError::Refused {
                status: map.status,
                message: map.message.unwrap_or_default(),
            });
        }
        need.add_page(&map);
        if map.last == 1 {
            return Ok((open.credit.unwrap_or(16 << 20), need));
        }
    }
}

/// The responder's half of a download (a host for downloads): the JobOpen already arrived
/// and no lane is opened — the opener joined the lanes. With `JF_ORDERED` one reader
/// feeds one FIFO in file order: every file is large (`cutoff = 0`), so `run_upload` draws
/// the queue in order once `small` is empty and the frames leave in (file, offset) order.
pub async fn serve_download(
    link: &mut JobLink,
    open: JobOpen,
    manifest: Arc<Manifest>,
    source: Arc<dyn Source>,
    mut opts: SendOptions,
) -> Result<SendReport, SendError> {
    if open.flags & gen::JF_ORDERED != 0 {
        opts.readers = 1;
        opts.cutoff = 0;
    }
    let opened = open_as_responder(link, &open, &manifest).await?;
    run_upload(link, manifest, source, opts, opened).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{FrameReader, FrameWriter};
    use crate::manifest::Entry;
    use crate::router::{is_data_type, BoxFut, ConnTx, LaneOpener, Router};
    use crate::session::Timing;
    use crate::source::SourceMeta;
    use crate::wire::SplitMix;
    use std::io;
    use tokio::io::{duplex, split};

    fn mf(sizes: &[u64]) -> Manifest {
        let mut m = Manifest::default();
        for (i, s) in sizes.iter().enumerate() {
            m.entries.push(crate::manifest::Entry {
                path: format!("f{i}"),
                size: *s,
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                mtime: 1,
                root: None,
            });
        }
        m
    }

    #[test]
    fn a_fresh_job_needs_every_byte_and_a_resumed_one_only_the_rest() {
        let cutoff = 1 << 20;
        let m = mf(&[100, 5 << 20, 8 << 20]);
        let total = 100 + (13 << 20);
        let f = SpaceFigures::of(&m, &Need::default(), cutoff).unwrap();
        assert_eq!((f.job_bytes, f.durable_bytes, f.held_bytes), (total, 0, 0));
        assert_eq!(f.to_allocate(), total);

        // File 0 done; file 1 has 2 MiB durable; file 2 untouched. Nothing else held.
        let mut need = Need::default();
        need.done.insert(0);
        need.partial.entry(1).or_default().insert(0, 2 << 20);
        let f = SpaceFigures::of(&m, &need, cutoff).unwrap();
        assert_eq!(f.durable_bytes, 100 + (2 << 20));
        assert_eq!(f.to_allocate(), (3 << 20) + (8 << 20));
    }

    #[test]
    fn a_preallocated_part_is_credited_by_what_the_console_reports_held() {
        // The part file of file 1 was preallocated at its full 5 MiB: only 2 MiB is durable
        // but the drive already holds all of it, so only file 2 still needs room.
        let cutoff = 1 << 20;
        let m = mf(&[5 << 20, 8 << 20]);
        let mut need = Need::default();
        need.partial.entry(0).or_default().insert(0, 2 << 20);
        need.held = 5 << 20;
        let f = SpaceFigures::of(&m, &need, cutoff).unwrap();
        assert_eq!(f.to_allocate(), 8 << 20);
        // `held` below the durable bytes never credits less than what is durable.
        need.held = 1 << 20;
        assert_eq!(
            SpaceFigures::of(&m, &need, cutoff).unwrap().to_allocate(),
            (3 << 20) + (8 << 20)
        );
    }

    #[test]
    fn a_held_figure_larger_than_the_unfinished_large_files_is_clamped_not_believed() {
        let cutoff = 1 << 20;
        let m = mf(&[100, 5 << 20]);
        let need = Need {
            held: u64::MAX,
            ..Need::default()
        };
        let f = SpaceFigures::of(&m, &need, cutoff).unwrap();
        // The small file is never "held" (it is written whole); the large one is fully held.
        assert_eq!(f.to_allocate(), 100);
        // Durable ranges beyond a file are the protocol error `large_remaining` names.
        let mut bad = Need::default();
        bad.partial.entry(1).or_default().insert(0, 9 << 20);
        assert!(SpaceFigures::of(&m, &bad, cutoff).is_err());
    }

    #[test]
    fn durable_ranges_beyond_a_file_are_a_protocol_error_not_an_underflow() {
        let mut have = RangeSet::new();
        have.insert(0, 100);
        assert_eq!(large_remaining(300, &have).unwrap(), 200);
        assert!(matches!(
            large_remaining(50, &have),
            Err(SendError::Protocol(_))
        ));
        let mut far = RangeSet::new();
        far.insert(1000, 1010);
        assert!(matches!(
            large_remaining(500, &far),
            Err(SendError::Protocol(_))
        ));
        let mut all = RangeSet::new();
        all.insert(0, u64::MAX);
        assert!(large_remaining(10, &all).is_err());
    }

    #[test]
    fn a_stall_with_bytes_outstanding_is_a_slow_receiver_not_a_dead_one() {
        // The Phat's USB drive stalled for over ten seconds with 64 MiB of window held:
        // 18,260 bytes were left, the smallest queued frame was 26,806, and the job was
        // failed although the receiver was only flushing. With nothing outstanding the
        // window can never fit the frame, and that still fails fast.
        let mut w = Window::new(64 << 20);
        assert_eq!(w.outstanding(), 0);
        assert_eq!(stall_limit(w.outstanding()), STALL_FATAL);
        assert!(w.sent(0, 1, (64 << 20) - 18_260));
        assert_eq!(w.available(), 18_260);
        assert_eq!(w.outstanding(), (64 << 20) - 18_260);
        assert!(stall_limit(w.outstanding()) >= Duration::from_secs(60));
        w.received(1); // the frame reached the receiver; its bytes are still held there
        assert!(stall_limit(w.outstanding()) >= Duration::from_secs(60));
        w.credit((64 << 20) - 18_260); // applied and returned
        assert_eq!(w.outstanding(), 0);
        assert_eq!(stall_limit(w.outstanding()), STALL_FATAL);
    }

    #[test]
    fn skip_set_is_the_complement_of_the_read_set() {
        let dir = std::env::temp_dir().join(format!(
            "ava1-skip-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let size = gen::LARGE_CUTOFF as u64 + 3 * GROUP;
        let m = Manifest {
            entries: vec![
                Entry {
                    kind: gen::ENTRY_FILE,
                    mode: 0o644,
                    size,
                    mtime: 0,
                    path: "partial".into(),
                    root: None,
                },
                Entry {
                    kind: gen::ENTRY_FILE,
                    mode: 0o644,
                    size: 3,
                    mtime: 0,
                    path: "done".into(),
                    root: None,
                },
            ],
        };
        let mut durable = Need::default();
        durable.done.insert(1);
        durable.partial.entry(0).or_default().insert(0, 2 * GROUP);
        let mut ob = Outboard::open(&dir.join("0.ob"), verify::groups(size)).unwrap();
        ob.put(0, &[1; 32]).unwrap();
        ob.sync().unwrap();
        let skip = skip_set(&m, &durable, Some(&dir));
        assert!(skip.done.contains(&1));
        assert_eq!(
            skip.partial[&0].iter().collect::<Vec<_>>(),
            vec![(0, GROUP)]
        );
        for chunk in [GROUP, 4 * GROUP] {
            let plan = pieces(size, &durable.partial[&0], &|g| g == 0, chunk);
            for g in 0..verify::groups(size) {
                let off = g * GROUP;
                let skipped = skip.partial[&0].covers(off, off + (size - off).min(GROUP));
                let read = plan
                    .iter()
                    .any(|p| p.offset <= off && off < p.offset + p.len);
                assert_ne!(skipped, read, "group {g}, chunk {chunk}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    const G: u64 = crate::verify::GROUP;

    #[test]
    fn pieces_send_the_missing_groups_and_hash_the_unknown_ones() {
        let mut durable = RangeSet::new();
        durable.insert(0, 2 * G);
        durable.insert(4 * G, 5 * G);
        let size = 6 * G + 7;
        // CVs known for group 0 only: group 1 and 4 are durable but must be read for the root.
        let p = pieces(size, &durable, &|g| g == 0, 2 * G);
        let got: Vec<(u64, u64, bool)> = p.iter().map(|x| (x.offset / G, x.len, x.send)).collect();
        assert_eq!(
            got,
            vec![
                (1, G, false),
                (2, 2 * G, true),
                (4, G, false),
                (5, G + 7, true)
            ]
        );
    }

    #[test]
    fn pieces_never_cross_a_chunk_limit_or_a_group_boundary() {
        let p = pieces(10 * G + 1, &RangeSet::new(), &|_| false, 3 * G);
        assert!(p
            .iter()
            .all(|x| x.offset % G == 0 && x.len <= 3 * G && x.send));
        assert_eq!(p.iter().map(|x| x.len).sum::<u64>(), 10 * G + 1);
    }

    #[test]
    fn every_chunk_but_a_files_last_is_whole_groups() {
        // Correction 6, the wire contract: the receiver answers ERR_PROTOCOL on a mid-file
        // chunk that is not a whole number of 1 MiB groups.
        let mut rng = SplitMix(11);
        for _ in 0..300 {
            let size = rng.below(16 * G) + 1;
            let mut durable = RangeSet::new();
            let mut left = rng.below(6);
            while left > 0 {
                let s = rng.below(size);
                durable.insert(s, (s + rng.below(size - s) + 1).min(size));
                left -= 1;
            }
            let chunk = (rng.below(5) + 1) * G;
            let cv_at = rng.below(16);
            let p = pieces(size, &durable, &|g| g == cv_at, chunk);
            for x in &p {
                assert!(x.len <= chunk && x.offset % G == 0, "{x:?} of {size}");
                if x.offset + x.len < size {
                    assert_eq!(x.len % G, 0, "a mid-file piece is not whole groups: {x:?}");
                }
            }
        }
    }

    #[test]
    fn a_lane_death_keeps_the_window_charged_until_the_receiver_accounts() {
        // I3: a lane's death does not release window credit. The un-Received frame keeps
        // its charge, so the sender can never re-spend the same window (the receiver may
        // still charge the frame if it admitted it). A late Received only confirms the
        // receiver's books — its Credit returns the bytes when it applies the frame.
        let mut w = Window::new(100);
        assert!(w.can_send(1, 60, 1000));
        w.sent(1, 7, 60);
        assert!(!w.can_send(2, 60, 1000), "credit");
        w.lane_down(1); // frame 7 never acknowledged: the charge stays
        assert_eq!(w.refunded.len(), 1, "the dead frame is held");
        assert_eq!(w.available(), 40, "a lane death releases no window credit");
        assert!(
            !w.can_send(2, 60, 1000),
            "the dead lane's bytes could be spent twice"
        );
        w.credit(60); // the receiver applied it and returned the space
        assert_eq!(
            w.refunded.len(),
            1,
            "a Credit closes nothing out: the charge lives in the window"
        );
        w.received(7); // ...but it had arrived after all: the receiver did charge it
        assert_eq!(
            w.refunded.len(),
            0,
            "the late Received closed the frame out"
        );
        assert_eq!(
            w.available(),
            100,
            "a late Received double-charged the sender"
        );
        w.received(7); // a duplicated Received closes nothing out...
        assert_eq!(w.refunded.len(), 0, "a duplicated Received changes nothing");
        assert!(w.can_send(2, 60, 1000));
        w.sent(2, 8, 60);
        assert!(!w.can_send(2, 30, 70), "the lane's in-flight cap");
        assert_eq!(w.received(8), Some(60));
    }

    #[test]
    fn a_dead_lane_releases_only_frames_the_writer_never_took() {
        // I3's precise form: the kill holds the charge of frames that may have
        // reached the receiver (the writer took them) and releases the rest — the
        // blunt rule held everything, and frames the receiver never saw could never
        // be credited back, so the window shrank with every kill until the stall
        // detector failed a healthy job.
        let mut w = Window::new(8 << 20);
        assert!(w.sent(1, 1, 4 << 20)); // written: the receiver may have it
        assert!(w.sent(1, 2, 4 << 20)); // still in the writer's queue: never left
        assert_eq!(w.lane_down(1), vec![1, 2]);
        assert_eq!(
            w.available(),
            0,
            "a lane death must not release credit itself"
        );
        // The writer died without taking frame 2: its charge is released here —
        // the receiver's window was never charged for it.
        assert_eq!(w.release(2), Some(4 << 20));
        assert_eq!(w.available(), 4 << 20);
        assert_eq!(w.release(2), None, "released twice");
        // Frame 1's charge stays held until the receiver accounts for it.
        assert_eq!(w.received(1), None);
        assert_eq!(w.available(), 4 << 20, "a late Received releases nothing");
        w.credit(4 << 20); // the receiver applied it and returned the bytes
        assert_eq!(w.available(), 8 << 20);
        assert_eq!(
            w.received(2),
            None,
            "a late Received for a released frame is nothing"
        );
        assert_eq!(w.available(), 8 << 20);
    }

    #[test]
    fn lane_churn_never_wedges_the_window_when_the_untaken_frames_are_released() {
        // The wedge, pinned: each round kills a lane with one written frame (whose
        // bytes the receiver returns with a Credit after its apply) and one frame
        // the writer never took (released at the kill). The window returns to its
        // full grant every round. Under the blunt rule — no `release` — every round
        // leaks the untaken frame's charge and the window is empty after two rounds:
        // nothing fits, and the stall detector fails a healthy job.
        for _ in 0..200 {
            let mut w = Window::new(8 << 20);
            assert!(w.sent(1, 1, 4 << 20)); // written
            assert!(w.sent(1, 2, 4 << 20)); // never left the writer's queue
            w.lane_down(1);
            w.release(2);
            assert_eq!(w.received(1), None); // the receiver confirms the written frame
            w.credit(4 << 20); // ...and returns its bytes after the apply
            assert_eq!(
                w.available(),
                8 << 20,
                "the window shrank: an untaken frame's charge leaked"
            );
        }
    }

    #[test]
    fn peer_credit_overflow_saturates_instead_of_wrapping() {
        // M4: `Credit.bytes` is peer-controlled; a plain `+=` wraps on overflow — which
        // shrinks the window — and panics in debug builds.
        let mut w = Window::new(u64::MAX / 2);
        w.credit(u64::MAX);
        assert_eq!(w.available(), u64::MAX);
        assert!(w.can_send(1, u64::MAX, u64::MAX));
    }

    #[test]
    fn a_lane_skips_a_front_frame_that_does_not_fit_and_takes_one_behind_it() {
        // I2: the head-of-line credit stall — every lane re-picking the same oversized
        // front frame while a smaller frame behind it fits. `pick_any` scans for a frame
        // that fits and leaves the oversized front in place.
        let mut s = Sched {
            decision: Some(governor::Decision {
                lanes: 1,
                chunk: 4 << 20,
                bundle: 1 << 20,
                bottleneck: gen::BN_NETWORK,
                mode: Mode::StreamOnly,
                prefer: Class::Stream,
                sequential: false,
            }),
            ..Default::default()
        };
        s.chunks.push_back(test_frame(Chunk::TYPE, 15 << 20));
        s.chunks.push_back(test_frame(Chunk::TYPE, 4 << 20));
        let w = Window::new(8 << 20);
        let f = pick_any(&mut s, 1, &w, 64 << 20).unwrap();
        assert_eq!(
            f.payload,
            4 << 20,
            "the fitting frame behind the oversized front was taken"
        );
        assert_eq!(s.chunks.len(), 1, "the oversized front frame stayed queued");
        assert_eq!(s.chunks.front().unwrap().payload, 15 << 20);
        // Nothing fits at all: None, and the queues are untouched.
        let tiny = Window::new(3 << 20);
        assert!(pick_any(&mut s, 1, &tiny, 64 << 20).is_none());
        assert_eq!(s.chunks.len(), 1);
    }

    #[test]
    fn sent_never_takes_more_than_the_credit() {
        let mut w = Window::new(50);
        assert!(
            !w.sent(1, 1, 60),
            "a frame larger than the credit is refused"
        );
        assert_eq!(w.available(), 50);
        assert!(w.sent(1, 2, 50));
        assert_eq!(w.available(), 0);
    }

    #[test]
    fn the_window_never_underflows_under_mixed_sizes() {
        // Correction 2: checked arithmetic; mixed sizes under low credit never panic and
        // the credit stays within its initial grant plus what was credited.
        let mut rng = SplitMix(3);
        let mut w = Window::new(500);
        let mut credited = 0u64;
        for _ in 0..20_000 {
            match rng.below(6) {
                0 => {
                    let lane = (rng.below(3) + 1) as u16;
                    let len = rng.below(600) + 1;
                    if w.can_send(lane, len, 1000) {
                        assert!(w.sent(lane, rng.below(500) as u32, len));
                    }
                }
                1 => {
                    let _ = w.received(rng.below(500) as u32);
                }
                2 => {
                    let lane = (rng.below(3) + 1) as u16;
                    let _ = w.lane_down(lane);
                }
                3 => {
                    let n = rng.below(400);
                    w.credit(n);
                    credited += n;
                }
                _ => {
                    let _ = w.available();
                }
            }
            assert!(w.available() <= 500 + credited, "credit over-accounted");
        }
    }

    #[test]
    fn lane_rates_are_smoothed_and_survive_idle_ticks() {
        // Correction 3: lanes size their in-flight cap by a real, persistent rate — not a
        // per-tick byte count cleared before the lanes read it.
        let mut bytes: HashMap<u16, u64> = HashMap::new();
        let mut rate: HashMap<u16, f64> = HashMap::new();
        for _ in 0..8 {
            bytes.insert(1, 10_000_000);
            smooth_rates(&mut bytes, &mut rate, 1.0);
        }
        assert!(
            rate[&1] > 9.9e6,
            "the EWMA converges to the true rate: {}",
            rate[&1]
        );
        let before = rate[&1];
        smooth_rates(&mut bytes, &mut rate, 1.0);
        assert_eq!(rate[&1], before, "an idle tick must not reset the rate");
        assert_eq!(
            governor::inflight_cap(4 << 20, rate[&1]),
            (rate[&1] * 2.0) as u64,
            "the cap is two seconds of the lane's rate"
        );
    }

    fn test_shared(chunk: u32, budget_kib: usize) -> Arc<Shared> {
        Arc::new(Shared {
            sched: Mutex::new(Sched {
                floor: 4,
                ..Default::default()
            }),
            window: Mutex::new(Window::new(1 << 20)),
            wake_tx: watch::channel(0).0,
            chunk: AtomicU32::new(chunk),
            bundle: AtomicU32::new(governor::START_BUNDLE),
            bytes_budget: Arc::new(Semaphore::new(budget_kib)),
            stall: Mutex::new(None),
        })
    }

    fn test_frame(ty: u8, payload: u64) -> OutFrame {
        OutFrame {
            ty,
            body: Arc::new(
                Chunk {
                    job_id: [0x78; 16],
                    file_id: 0,
                    offset: 0,
                    data: vec![0x22; payload as usize],
                }
                .to_bytes()
                .unwrap(),
            ),
            class: Class::Stream,
            payload,
            resend: false,
            _budget: Vec::new(),
            taken: None,
        }
    }

    #[tokio::test]
    async fn a_wake_between_the_check_and_the_wait_is_not_lost() {
        // Correction 4: the exact interleaving a lost wake needs — the waiter has checked
        // (nothing to do) and is about to wait when the producer queues work and wakes.
        // The versioned channel sees the bump; a bare Notify would leave the waiter asleep.
        let sh = test_shared(governor::START_CHUNK, READ_AHEAD_KIB as usize);
        let (armed, fire) = tokio::sync::oneshot::channel();
        let waiter = {
            let sh = sh.clone();
            tokio::spawn(async move {
                let mut rx = sh.wake_tx.subscribe();
                let mut armed = Some(armed);
                loop {
                    let _ = *rx.borrow_and_update(); // the version is seen first
                    let work = {
                        let s = sh.sched.lock().unwrap();
                        !s.chunks.is_empty() || !s.bundles.is_empty() || !s.requeue.is_empty()
                    };
                    if work {
                        return;
                    }
                    if let Some(a) = armed.take() {
                        if a.send(()).is_err() {
                            return;
                        }
                    }
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            })
        };
        fire.await.unwrap(); // the waiter checked, found nothing, and is (about to be) waiting
        sh.sched
            .lock()
            .unwrap()
            .chunks
            .push_back(test_frame(Chunk::TYPE, 64));
        sh.wake();
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("the wake between the check and the wait was lost")
            .unwrap();
    }

    /// A `ReadAt` over memory, and a `Source` that serves it as one file.
    struct MemRead {
        data: Vec<u8>,
    }
    impl crate::source::ReadAt for MemRead {
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.data.len().saturating_sub(off as usize));
            if n > 0 {
                buf[..n].copy_from_slice(&self.data[off as usize..off as usize + n]);
            }
            Ok(n)
        }
    }
    struct MemSource(Vec<u8>);
    impl Source for MemSource {
        fn open(&self, _rel: &str) -> io::Result<Box<dyn crate::source::ReadAt>> {
            Ok(Box::new(MemRead {
                data: self.0.clone(),
            }))
        }
        fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
            Ok(Vec::new())
        }
        fn stat(&self, _rel: &str) -> io::Result<SourceMeta> {
            Ok(SourceMeta {
                size: self.0.len() as u64,
                mtime: 0,
                mode: 0o644,
                is_dir: false,
            })
        }
    }

    #[tokio::test]
    async fn reader_bytes_stay_under_the_read_ahead_budget() {
        // Correction 1: the permit is acquired before the read and rides with the frame,
        // so a fast source can never buffer more than the budget in the queues.
        let g = G as usize;
        let size = 16 * g;
        let sh = test_shared(g as u32, g / 1024 + 8); // one piece plus the per-piece slack
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "f".into(),
                root: None,
            }],
        });
        let src: Arc<dyn Source> = Arc::new(MemSource(vec![0x5a; size]));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let small = Arc::new(Mutex::new(VecDeque::new()));
        let large = Arc::new(Mutex::new(VecDeque::from([(0u32, RangeSet::new())])));
        let _handles = spawn_readers(
            m,
            src,
            small,
            large,
            sh.clone(),
            &SendOptions::upload(""),
            tx,
            Arc::new(AtomicBool::new(false)),
        );
        let budget = sh.bytes_budget.clone();
        let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match &first {
            Read::Chunk { data, .. } => assert!(data.len() <= g),
            _ => panic!("expected a chunk"),
        }
        // The read bytes still hold the budget: less than one piece's worth is left.
        assert!(
            budget.available_permits() < g / 1024 + 1,
            "{}",
            budget.available_permits()
        );
        // ...and nothing more can be read while the piece waits in the queue.
        assert!(tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err());
        // The permit travels with the frame: dropped, the budget returns and the next
        // piece is read.
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(second, Read::Chunk { .. }));
    }

    #[tokio::test]
    async fn a_lane_task_stops_promptly_when_the_job_ends() {
        // The teardown's other half (correction 5): a lane task sleeping on the wake
        // signal sees `stop` and exits, so the join never hangs.
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(1),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let (a, b) = duplex(1 << 20);
        let (ar, aw) = split(a);
        let (tx, _rx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (_link, outbox) =
            crate::link::drive(FrameReader::new(ar), FrameWriter::new(aw), timing, tx);
        let mut peer = FrameReader::new(b);
        peer.set_max_body(crate::frame::MAX_BODY);
        // The lane's frame must be taken or the send would block.
        let drain = tokio::spawn(async move {
            let _ = peer.recv().await;
        });
        let router = Arc::new(Router::default());
        router.lane_up(1, outbox);
        let lane = router.lane(1).unwrap();
        let sh = test_shared(governor::START_CHUNK, READ_AHEAD_KIB as usize);
        sh.sched
            .lock()
            .unwrap()
            .chunks
            .push_back(test_frame(Chunk::TYPE, 1 << 20));
        let stop = Arc::new(AtomicBool::new(false));
        let h = tokio::spawn(lane_task(lane, sh.clone(), None, stop.clone()));
        tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .unwrap()
            .unwrap();
        stop.store(true, Ordering::Relaxed);
        sh.wake();
        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .expect("the lane task did not stop")
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_frame_taken_after_the_sweep_but_before_the_writers_death_keeps_its_charge() {
        // I3's precise form, the stale-read corner: the first sweep's marker read must
        // never decide a release. The writer may still be mid-poll when the sweep runs
        // (the lane's stop flag takes effect at its next check, and an abort at its
        // next yield), so it can take a frame — flip the marker, write it to the
        // socket — after the sweep read it as untaken. Releasing that frame's charge
        // would spend the window twice and the receiver's ERR_CREDIT fails a healthy
        // job. This test flips the marker through the real link writer between the
        // sweep and the writer's death: the release must re-read the marker once the
        // death is confirmed and keep the charge.
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(1),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let (a, b) = duplex(1 << 20);
        let (ar, aw) = split(a);
        let (br, _bw) = split(b);
        let (tx, _rx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (link, outbox) =
            crate::link::drive(FrameReader::new(ar), FrameWriter::new(aw), timing, tx);
        // The peer drains whatever the writer flushes, so the frame below is taken and
        // written out rather than parked in the outbox.
        tokio::spawn(async move {
            let mut peer = FrameReader::new(br);
            peer.set_max_body(crate::frame::MAX_BODY);
            while peer.recv().await.is_ok() {}
        });
        let sh = Arc::new(Shared {
            sched: Mutex::new(Sched {
                floor: 4,
                ..Default::default()
            }),
            window: Mutex::new(Window::new(1 << 20)),
            wake_tx: watch::channel(0).0,
            chunk: AtomicU32::new(governor::START_CHUNK),
            bundle: AtomicU32::new(governor::START_BUNDLE),
            bytes_budget: Arc::new(Semaphore::new(READ_AHEAD_KIB as usize)),
            stall: Mutex::new(None),
        });
        // One frame charged to lane 1 and in flight, its take-marker shared with the test.
        let taken = Arc::new(AtomicBool::new(false));
        let mut f = test_frame(Chunk::TYPE, 1024);
        let len = f.body.len() as u64;
        f.taken = Some(taken.clone());
        assert!(sh.window.lock().unwrap().sent(1, 7, len));
        sh.sched.lock().unwrap().inflight.insert(7, (1, f));
        let tx = ConnTx::new(outbox);
        let death = tokio::spawn(lane_death(sh.clone(), 1, tx.clone()));
        // The first sweep: the frame leaves `inflight` for the requeue (its charge is
        // held either way — `lane_down` moved it to `refunded`).
        tokio::time::timeout(Duration::from_secs(10), async {
            while !sh.sched.lock().unwrap().inflight.is_empty() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the first sweep ran within the bound");
        // The writer takes the frame now — after the sweep, before its death: exactly
        // the stale-read window. The real link writer flips the marker as it dequeues.
        tx.send_raw_marked(Chunk::TYPE, 0, 99, vec![0x11; len as usize], taken.clone())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !taken.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the writer took the frame within the bound");
        // The writer's death: dropping the link aborts its tasks.
        drop(link);
        tokio::time::timeout(Duration::from_secs(10), death)
            .await
            .expect("the lane death handling finished within the bound")
            .expect("the lane death handling did not panic");
        assert_eq!(
            sh.window.lock().unwrap().available(),
            (1 << 20) - len,
            "the frame's charge was released although the writer took it: the window was spent twice"
        );
    }

    /// A lane task over an in-memory pipe with a custom window: the task, the shared
    /// state, what the peer reads off the wire, and the stop flag. The caller keeps the
    /// `Link` alive for the connection.
    type LaneHarness = (
        tokio::task::JoinHandle<()>,
        Arc<Shared>,
        tokio::sync::mpsc::UnboundedReceiver<Result<u32, ()>>,
        Arc<AtomicBool>,
        crate::link::Link,
    );

    fn lane_harness(credit: u64, chunk: u32) -> LaneHarness {
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(1),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let (a, b) = duplex(1 << 20);
        let (ar, aw) = split(a);
        let (tx, _rx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (link, outbox) =
            crate::link::drive(FrameReader::new(ar), FrameWriter::new(aw), timing, tx);
        let mut peer = FrameReader::new(b);
        peer.set_max_body(crate::frame::MAX_BODY);
        let (drain_tx, drain_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok(f) = peer.recv().await {
                // Only lane data frames count: the link's heartbeat Ping (channel 0)
                // lands here too, and it is not a send.
                if is_data_type(f.ty) {
                    let _ = drain_tx.send(Ok(f.channel));
                }
            }
            let _ = drain_tx.send(Err(()));
        });
        let router = Arc::new(Router::default());
        router.lane_up(1, outbox);
        let sh = Arc::new(Shared {
            sched: Mutex::new(Sched {
                floor: 4,
                decision: Some(governor::Decision {
                    lanes: 1,
                    chunk: 4 << 20,
                    bundle: 1 << 20,
                    bottleneck: gen::BN_NETWORK,
                    mode: Mode::StreamOnly,
                    prefer: Class::Stream,
                    sequential: false,
                }),
                ..Default::default()
            }),
            window: Mutex::new(Window::new(credit)),
            wake_tx: watch::channel(0).0,
            chunk: AtomicU32::new(chunk),
            bundle: AtomicU32::new(governor::START_BUNDLE),
            bytes_budget: Arc::new(Semaphore::new(READ_AHEAD_KIB as usize)),
            stall: Mutex::new(None),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let h = tokio::spawn(lane_task(
            router.lane(1).unwrap(),
            sh.clone(),
            None,
            stop.clone(),
        ));
        (h, sh, drain_rx, stop, link)
    }

    #[tokio::test]
    async fn a_lane_blocked_by_its_inflight_cap_is_backpressure_never_a_stall() {
        // Fix round 2: `can_send`'s in-flight cap clause is per-lane backpressure. A lane
        // that sent a frame larger than its cap (or charged one before its rate warmed)
        // can send nothing further until the `Received` for those bytes arrives — the job
        // resumes by itself. The stall detector must never mark that: the tick would fail
        // a healthy job at STALL_FATAL.
        let (h, sh, mut wire, stop, _link) = lane_harness(16 << 20, governor::START_CHUNK);
        // 8 MiB in flight on lane 1 (a frame charged before the rate warmed), 8 MiB of the
        // window left, and a queued frame that fits the window but not the cap: the lane's
        // rate is still 0, so `cap = chunk = 4 MiB < 8 MiB + the frame`.
        assert!(sh.window.lock().unwrap().sent(1, 77, 8 << 20));
        sh.sched
            .lock()
            .unwrap()
            .chunks
            .push_back(test_frame(Chunk::TYPE, 1 << 20));
        sh.wake();
        // The lane gets every chance to (wrongly) mark the stall: nothing fits the cap.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            sh.stall.lock().unwrap().is_none(),
            "a lane blocked by its in-flight cap marked a fatal stall"
        );
        let got = wire.try_recv();
        assert!(got.is_err(), "the cap-blocked lane sent a frame: {got:?}");
        // The Received for the charged bytes alone (no Credit) unblocks the lane, which
        // then sends the queued frame — the job resumes by itself.
        assert_eq!(sh.window.lock().unwrap().received(77), Some(8 << 20));
        sh.wake();
        tokio::time::timeout(Duration::from_secs(5), wire.recv())
            .await
            .expect("the lane resumed once the Received arrived")
            .expect("a frame arrived")
            .expect("the wire carried a frame");
        stop.store(true, Ordering::Relaxed);
        sh.wake();
        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .expect("the lane task stopped")
            .unwrap();
    }

    #[tokio::test]
    async fn a_frame_the_window_cannot_hold_marks_a_credit_stall() {
        // The detector's real case: the smallest queued frame is larger than the whole
        // window, so nothing can be sent on any lane until the receiver grants Credit.
        // That — and only that — records the stall the control loop's tick fails on.
        let (h, sh, _wire, stop, _link) = lane_harness(512 << 10, governor::START_CHUNK);
        sh.sched
            .lock()
            .unwrap()
            .chunks
            .push_back(test_frame(Chunk::TYPE, 1 << 20)); // one group + the header
        sh.wake();
        let st = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let st = *sh.stall.lock().unwrap();
                if let Some(st) = st {
                    return st;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the credit stall was recorded within the bound");
        assert_eq!(st.grant, 512 << 10);
        assert!(st.smallest >= 1 << 20, "smallest: {}", st.smallest);
        stop.store(true, Ordering::Relaxed);
        sh.wake();
        tokio::time::timeout(Duration::from_secs(1), h)
            .await
            .expect("the lane task stopped")
            .unwrap();
    }

    #[tokio::test]
    async fn an_injected_receiver_error_ends_the_job_promptly_with_no_lane_left_sending() {
        // Correction 5: no early return from a select arm — on any error every lane task
        // is cancelled and joined before `send_job` reports it. The fake receiver answers
        // the open, answers the map, acknowledges the first chunk, then sends a malformed
        // Received; the job must end with Protocol and nothing may keep sending on the lane.
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(5),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let job = [0x79u8; 16];
        // The control connection.
        let (ca, cb) = duplex(1 << 20);
        let (car, caw) = split(ca);
        let (cbr, cbw) = split(cb);
        let (ctx, mut crx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (_clink, coutbox) =
            crate::link::drive(FrameReader::new(car), FrameWriter::new(caw), timing, ctx);
        // One lane.
        let (la, lb) = duplex(1 << 20);
        let (lar, law) = split(la);
        let (lbr, _lbw) = split(lb);
        let (ltx, _lrx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (_llink, loutbox) =
            crate::link::drive(FrameReader::new(lar), FrameWriter::new(law), timing, ltx);
        let router = Arc::new(Router::default());
        router.lane_up(1, loutbox);
        let mut link = JobLink::new(job, router.clone(), ConnTx::new(coutbox), None);
        // Route the control connection's frames into the job, like the session dispatcher.
        let r2 = router.clone();
        tokio::spawn(async move {
            while let Some(f) = crx.recv().await {
                if is_data_type(f.ty) {
                    let _ = r2.route_control(f).await;
                }
            }
            r2.close("the session ended");
        });
        // The fake receiver: answers the open and the map, acknowledges lane frames, and
        // after the first acknowledgement injects the malformed Received.
        let lane_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ls = lane_seen.clone();
        tokio::spawn(async move {
            let mut peer = FrameReader::new(cbr);
            peer.set_max_body(crate::frame::MAX_BODY);
            let mut lane_peer = FrameReader::new(lbr);
            lane_peer.set_max_body(crate::frame::MAX_BODY);
            let mut peer_w = FrameWriter::new(cbw);
            loop {
                tokio::select! {
                    f = peer.recv() => match f {
                        Ok(f) if f.ty == JobOpen::TYPE => {
                            let open: JobOpen = f.decode().unwrap();
                            peer_w.send_msg(0, &JobOpenAck {
                                job_id: open.job_id,
                                status: 0,
                                credit: 64 << 20,
                                staged: 1,
                                workers: 4,
                                message: None,
                            }).await.unwrap();
                        }
                        Ok(f) if f.ty == ManifestEnd::TYPE => {
                            peer_w.send_msg(0, &JobMap {
                                job_id: job,
                                status: 0,
                                last: 1,
                                done: vec![],
                                partial: vec![],
                                message: None,
                                held: None,
                            }).await.unwrap();
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    },
                    f = lane_peer.recv() => match f {
                        Ok(f) if is_data_type(f.ty) => {
                            let n = ls.fetch_add(1, Ordering::Relaxed);
                            let _ = peer_w.send_msg(0, &Received { job_id: job, lane: 1, seq: f.channel }).await;
                            if n == 0 {
                                // The first chunk is acknowledged; now the injected error: a
                                // Received whose body is just the job id (its decode fails).
                                peer_w.send(Received::TYPE, 0, &job).await.unwrap();
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    },
                }
            }
        });

        let dir = std::env::temp_dir().join(format!("ava1-send-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), vec![0x33u8; 64 << 20]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 64 << 20,
                mtime: 1,
                path: "f".into(),
                root: None,
            }],
        });
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await;
        let err = result
            .expect("the job ended promptly")
            .expect_err("the injected error surfaces");
        assert!(matches!(err, SendError::Protocol(_)), "{err:?}");
        // Nothing was left running: no frame arrives on the lane after the error.
        let n = lane_seen.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            lane_seen.load(Ordering::Relaxed),
            n,
            "a lane task kept sending after the error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    struct FakeReceiver {
        credit: u64,
        credit_on_apply: bool,
        done_when_complete: bool,
        /// A `Credit` sent right after the ack, before the map (M5).
        credit_after_ack: Option<u64>,
        /// A malformed `Status` sent right after the first `Received` (M6).
        malformed_status: bool,
        /// A `FileRetry` for an id not in the manifest, right after the map (M7).
        retry_unknown: bool,
        /// A malformed `Status` right after the ack, in the open window (M6's open arm).
        malformed_status_open: bool,
        /// A well-formed `Status` right after the ack (absorbed without breaking the open).
        wellformed_status_open: bool,
        /// Pause reading the control connection this long after the map: the sender's
        /// control outbox fills with the file roots (the JobCancel arm's full-outbox
        /// case). While paused, a Status flood keeps the receiver's own writes flowing,
        /// so a sender that stops draining its inbox (awaiting the cancel inline)
        /// deadlocks against it.
        pause_control: Option<Duration>,
        /// The control pipe's capacity (a small one makes the flood's backpressure bite).
        control_pipe: usize,
        /// The last JobCancel's reason (0: none seen).
        cancel_seen: Arc<AtomicU32>,
    }

    impl Default for FakeReceiver {
        fn default() -> Self {
            Self {
                credit: 0,
                credit_on_apply: false,
                done_when_complete: false,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                malformed_status_open: false,
                wellformed_status_open: false,
                pause_control: None,
                control_pipe: 1 << 20,
                cancel_seen: Arc::new(AtomicU32::new(0)),
            }
        }
    }

    /// A `JobLink` over in-memory pipes: one lane and a fake receiver that answers the
    /// open (granting `rcv.credit`), the map, and every lane frame with `Received` (and
    /// `Credit` when `credit_on_apply`). `done_when_complete` ends the job (`JobDone`)
    /// once the payload bytes reach the manifest's total. Returns the link, how many
    /// frames the lane carried, and the background tasks — the caller must keep the
    /// tasks alive (dropping the link drives closes the session).
    fn fake_link(
        rcv: FakeReceiver,
        opener: Option<Arc<dyn LaneOpener>>,
    ) -> (
        JobLink,
        Arc<std::sync::atomic::AtomicUsize>,
        Vec<crate::link::Link>,
    ) {
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(5),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let job = [0x7d; 16];
        // The control connection.
        let (ca, cb) = duplex(rcv.control_pipe);
        let (car, caw) = split(ca);
        let (cbr, cbw) = split(cb);
        let (ctx, mut crx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (clink, coutbox) =
            crate::link::drive(FrameReader::new(car), FrameWriter::new(caw), timing, ctx);
        // One lane.
        let (la, lb) = duplex(1 << 20);
        let (lar, law) = split(la);
        let (lbr, _lbw) = split(lb);
        let (ltx, _lrx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (llink, loutbox) =
            crate::link::drive(FrameReader::new(lar), FrameWriter::new(law), timing, ltx);
        let keep: Vec<crate::link::Link> = vec![clink, llink];
        let router = Arc::new(Router::default());
        router.lane_up(1, loutbox);
        let link = JobLink::new(job, router.clone(), ConnTx::new(coutbox), opener);
        // Route the control connection's frames into the job, like the session dispatcher.
        let r2 = router.clone();
        tokio::spawn(async move {
            while let Some(f) = crx.recv().await {
                if is_data_type(f.ty) {
                    let _ = r2.route_control(f).await;
                }
            }
            r2.close("the session ended");
        });
        let lane_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ls = lane_seen.clone();
        tokio::spawn(async move {
            // `FrameReader::recv` is not cancel-safe (a cancelled read loses the bytes it
            // already took, and the next frame starts mid-body: BadMagic). The select!
            // below cancels whichever branch loses, so each reader runs in its own task and
            // the branches wait on a channel, which is. Capacity 1 keeps the backpressure the
            // full-outbox test relies on while the control read is held.
            fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
                r: R,
            ) -> mpsc::Receiver<Result<Frame, ()>> {
                let (tx, rx) = mpsc::channel(1);
                tokio::spawn(async move {
                    let mut reader = FrameReader::new(r);
                    reader.set_max_body(crate::frame::MAX_BODY);
                    loop {
                        let f = reader.recv().await.map_err(|_| ());
                        let end = f.is_err();
                        if tx.send(f).await.is_err() || end {
                            return;
                        }
                    }
                });
                rx
            }
            let mut peer = pump(cbr);
            let mut lane_peer = pump(lbr);
            let mut peer_w = FrameWriter::new(cbw);
            let cancel_seen = rcv.cancel_seen.clone();
            // The control-read hold (the full-outbox case) and the Status flood that
            // runs while it lasts. Both are disarmed for a receiver that does not
            // pause: the hold is already completed, and the flood never fires.
            let mut control_hold = Box::pin(tokio::time::sleep(Duration::ZERO));
            let mut flood = Box::pin(tokio::time::sleep(Duration::from_secs(3600)));
            let mut flood_started = false;
            let (mut total, mut manifest_bytes, mut manifest_files) = (0u64, 0u64, 0u32);
            loop {
                tokio::select! {
                    f = async {
                        (&mut control_hold).await;
                        peer.recv().await.unwrap_or(Err(()))
                    } => match f {
                        Ok(f) if f.ty == JobOpen::TYPE => {
                            let open: JobOpen = f.decode().unwrap();
                            peer_w.send_msg(0, &JobOpenAck {
                                job_id: open.job_id,
                                status: 0,
                                credit: rcv.credit,
                                staged: 1,
                                workers: 4,
                                message: None,
                            }).await.unwrap();
                            if let Some(bytes) = rcv.credit_after_ack {
                                let _ = peer_w.send_msg(0, &Credit { job_id: job, bytes }).await;
                            }
                            if rcv.malformed_status_open {
                                // A Status whose body is just the job id (its decode
                                // fails), in the open window: after the ack, before the map.
                                let _ = peer_w.send(Status::TYPE, 0, &job).await;
                            }
                            if rcv.wellformed_status_open {
                                let _ = peer_w.send_msg(0, &Status {
                                    job_id: job,
                                    ..Default::default()
                                }).await;
                            }
                        }
                        Ok(f) if f.ty == ManifestEnd::TYPE => {
                            let me: ManifestEnd = f.decode().unwrap();
                            manifest_bytes = me.bytes;
                            manifest_files = me.files;
                            peer_w.send_msg(0, &JobMap {
                                job_id: job,
                                status: 0,
                                last: 1,
                                done: vec![],
                                partial: vec![],
                                message: None,
                                held: None,
                            }).await.unwrap();
                            if let Some(p) = rcv.pause_control {
                                control_hold.as_mut().reset(tokio::time::Instant::now() + p);
                                flood.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(1));
                                flood_started = true;
                            }
                            if rcv.retry_unknown {
                                let _ = peer_w.send_msg(0, &FileRetry {
                                    job_id: job,
                                    file_id: 99,
                                    reason: 1,
                                }).await;
                            }
                        }
                        Ok(f) if f.ty == gen::JobCancel::TYPE => {
                            let c: gen::JobCancel = f.decode().unwrap();
                            cancel_seen.store(c.reason as u32, Ordering::SeqCst);
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    },
                    _ = &mut flood, if flood_started => {
                        // While the control read is paused, keep the receiver's own
                        // writes flowing: a sender that stopped draining its inbox
                        // (awaiting the JobCancel inline) blocks here, and the two
                        // sides then deadlock against each other's full outboxes.
                        let _ = peer_w.send_msg(0, &Status {
                            job_id: job,
                            current: Some("s".repeat(32 << 10)),
                            ..Default::default()
                        }).await;
                        flood.as_mut().reset(tokio::time::Instant::now() + Duration::from_millis(1));
                    }
                    f = async { lane_peer.recv().await.unwrap_or(Err(())) } => match f {
                        Ok(f) if is_data_type(f.ty) => {
                            let n = ls.fetch_add(1, Ordering::Relaxed);
                            let payload = f.decode::<Chunk>().map(|c| c.data.len() as u64).unwrap_or(0);
                            let _ = peer_w.send_msg(0, &Received { job_id: job, lane: 1, seq: f.channel }).await;
                            if rcv.credit_on_apply {
                                let _ = peer_w.send_msg(0, &Credit { job_id: job, bytes: f.body.len() as u64 }).await;
                            }
                            if rcv.malformed_status && n == 0 {
                                // A Status whose body is just the job id (its decode fails).
                                let _ = peer_w.send(Status::TYPE, 0, &job).await;
                            }
                            total += payload;
                            if rcv.done_when_complete && total >= manifest_bytes {
                                let _ = peer_w.send_msg(0, &JobDone {
                                    job_id: job,
                                    status: 0,
                                    files: manifest_files,
                                    bytes: manifest_bytes,
                                    message: None, settling: None,}).await;
                            }
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    },
                }
            }
        });
        (link, lane_seen, keep)
    }

    /// A `Source` whose reads are counted and slow, and whose open sessions are counted —
    /// a parked reader still holds its session, so a leaked thread shows as `active > 0`.
    struct CountingRead {
        data: Arc<Vec<u8>>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        slow: bool,
    }
    impl crate::source::ReadAt for CountingRead {
        fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            if self.slow {
                std::thread::sleep(Duration::from_millis(5)); // a slow source: reads are observable
            }
            let n = buf
                .len()
                .min(64 * 1024)
                .min(self.data.len().saturating_sub(off as usize));
            if n > 0 {
                buf[..n].copy_from_slice(&self.data[off as usize..off as usize + n]);
            }
            self.reads.fetch_add(n, Ordering::Relaxed);
            Ok(n)
        }
    }
    impl Drop for CountingRead {
        fn drop(&mut self) {
            self.active.fetch_sub(1, Ordering::Relaxed);
        }
    }
    struct CountingSource {
        data: Arc<Vec<u8>>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        slow: bool,
    }
    impl Source for CountingSource {
        fn open(&self, _rel: &str) -> io::Result<Box<dyn crate::source::ReadAt>> {
            self.active.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(CountingRead {
                data: self.data.clone(),
                reads: self.reads.clone(),
                active: self.active.clone(),
                slow: self.slow,
            }))
        }
        fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
            Ok(Vec::new())
        }
        fn stat(&self, _rel: &str) -> io::Result<SourceMeta> {
            Ok(SourceMeta {
                size: self.data.len() as u64,
                mtime: 0,
                mode: 0o644,
                is_dir: false,
            })
        }
    }

    /// A `Source` whose first `fail_after` opens succeed (each after a small delay, so
    /// the failure lands mid-transfer, after the earlier files' frames are sent) and
    /// whose next open fails: the deterministic reader failure. Open sessions are
    /// counted, so a leaked reader shows as `active > 0`.
    struct FailingAfter {
        data: Vec<u8>,
        opens: Arc<std::sync::atomic::AtomicUsize>,
        active: Arc<std::sync::atomic::AtomicUsize>,
        fail_after: usize,
    }
    impl Source for FailingAfter {
        fn open(&self, _rel: &str) -> io::Result<Box<dyn crate::source::ReadAt>> {
            if self.opens.fetch_add(1, Ordering::SeqCst) >= self.fail_after {
                return Err(std::io::Error::other("the test source failed"));
            }
            std::thread::sleep(Duration::from_millis(1)); // a slow source: reads are observable
            self.active.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(CountingRead {
                data: Arc::new(self.data.clone()),
                reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                active: self.active.clone(),
                slow: false,
            }))
        }
        fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
            Ok(Vec::new())
        }
        fn stat(&self, _rel: &str) -> io::Result<SourceMeta> {
            Ok(SourceMeta {
                size: self.data.len() as u64,
                mtime: 0,
                mode: 0o644,
                is_dir: false,
            })
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_job_stops_the_readers_and_leaves_none_parked() {
        // I1: teardown wakes every blocked reader (the budget closes), drains the queues
        // so the frames' read-ahead permits are released, and joins the reader threads
        // before the job's future returns; the readers also check the shutdown flag each
        // iteration. Against the unfixed sender the reader keeps reading for seconds
        // after the return and then leaks, parked on the budget.
        let size = 160usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 4 << 20,
                credit_on_apply: true,
                done_when_complete: false,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let src: Arc<dyn Source> = Arc::new(CountingSource {
            data: Arc::new(vec![0x5a; size]),
            reads: reads.clone(),
            active: active.clone(),
            slow: true,
        });
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let mut opts = SendOptions::upload("dest");
        opts.cancel = cancel.clone();
        let job_task = tokio::spawn(async move { send_job(&mut link, m, src, opts).await });
        // Let the reader get going: two pieces' worth of reads.
        tokio::time::timeout(Duration::from_secs(12), async {
            while reads.load(Ordering::Relaxed) < 2 * (4 << 20) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the reader made progress within the bound");
        cancel.store(true, Ordering::Relaxed);
        let result = tokio::time::timeout(Duration::from_secs(15), job_task)
            .await
            .expect("the sender task completed within the bound")
            .expect("the sender task did not panic");
        assert!(matches!(result, Err(SendError::Cancelled)), "{result:?}");
        // The reads stopped with the job...
        let at = reads.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            reads.load(Ordering::Relaxed),
            at,
            "a reader kept reading after the job returned"
        );
        // ...and no reader thread is still inside the source (parked on the read-ahead
        // budget or otherwise).
        assert_eq!(active.load(Ordering::Relaxed), 0, "a reader thread leaked");
    }

    /// A source whose reads park on something outside the disk (a relay waiting for
    /// another connection) until `close` wakes them.
    struct ParkedSource {
        closed: Arc<AtomicBool>,
        parked: Arc<AtomicBool>,
    }
    struct ParkedRead(Arc<AtomicBool>, Arc<AtomicBool>);
    impl crate::source::ReadAt for ParkedRead {
        fn read_at(&mut self, _off: u64, _buf: &mut [u8]) -> std::io::Result<usize> {
            self.1.store(true, Ordering::Relaxed);
            while !self.0.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
    }
    impl Source for ParkedSource {
        fn open(&self, _rel: &str) -> std::io::Result<Box<dyn crate::source::ReadAt>> {
            Ok(Box::new(ParkedRead(
                self.closed.clone(),
                self.parked.clone(),
            )))
        }
        fn list(&self, _rel: &str) -> std::io::Result<Vec<(String, SourceMeta)>> {
            Ok(Vec::new())
        }
        fn stat(&self, _rel: &str) -> std::io::Result<SourceMeta> {
            Ok(SourceMeta {
                size: 64 << 20,
                mtime: 0,
                mode: 0o644,
                is_dir: false,
            })
        }
        fn close(&self) {
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn teardown_closes_the_source_so_a_parked_reader_cannot_hold_the_job() {
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 4 << 20,
                credit_on_apply: true,
                done_when_complete: false,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let (closed, parked) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let src: Arc<dyn Source> = Arc::new(ParkedSource {
            closed: closed.clone(),
            parked: parked.clone(),
        });
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: 64 << 20,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let cancel = Arc::new(AtomicBool::new(false));
        let mut opts = SendOptions::upload("dest");
        opts.cancel = cancel.clone();
        let job_task = tokio::spawn(async move { send_job(&mut link, m, src, opts).await });
        tokio::time::timeout(Duration::from_secs(10), async {
            while !parked.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a reader parked in the source");
        cancel.store(true, Ordering::Relaxed);
        let result = tokio::time::timeout(Duration::from_secs(5), job_task)
            .await
            .expect("teardown waited on a parked reader")
            .unwrap();
        assert!(matches!(result, Err(SendError::Cancelled)), "{result:?}");
        assert!(closed.load(Ordering::Relaxed));
    }

    /// An opener whose `open()` waits for the test's signal and then fails: the
    /// deterministic lane-open failure for the I1 open-failure path. The test fires the
    /// signal only once a reader is parked on the read-ahead budget, so the failure
    /// lands exactly while a reader is parked — the leak the teardown must clean up.
    struct FailOpenOnSignal(Arc<tokio::sync::Notify>);
    impl LaneOpener for FailOpenOnSignal {
        fn open(&self) -> BoxFut<'_, Result<u16, crate::Ava1Error>> {
            let n = self.0.clone();
            Box::pin(async move {
                n.notified().await;
                Err(crate::Ava1Error::Io(std::io::Error::other(
                    "the test opener refuses the lane",
                )))
            })
        }
        fn close(&self, _id: u16) {}
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lane_open_failure_still_runs_the_teardown_and_joins_the_readers() {
        // I1's other exit: the lane-open failure must break into the teardown like any
        // other exit — the unfixed `?` returns from `run_upload` before the stop flag,
        // the budget close and the reader joins, so a reader parked on the read-ahead
        // semaphore stays parked (holding its source session) forever after the job's
        // future has already returned.
        let size = 160usize << 20;
        let signal = Arc::new(tokio::sync::Notify::new());
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 4 << 20,
                credit_on_apply: true,
                done_when_complete: false,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            Some(Arc::new(FailOpenOnSignal(signal.clone())) as Arc<dyn LaneOpener>),
        );
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let src: Arc<dyn Source> = Arc::new(CountingSource {
            data: Arc::new(vec![0x5b; size]),
            reads: reads.clone(),
            active: active.clone(),
            slow: true,
        });
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let job_task =
            tokio::spawn(
                async move { send_job(&mut link, m, src, SendOptions::upload("dest")).await },
            );
        // Deterministic failure: wait until a reader is parked on the read-ahead budget
        // — its reads stop advancing while it still holds its source session — and only
        // then let the opener fail, so the failure lands with a reader parked.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let a = reads.load(Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(150)).await;
                if a > 0 && reads.load(Ordering::Relaxed) == a && active.load(Ordering::Relaxed) > 0
                {
                    break;
                }
            }
        })
        .await
        .expect("a reader parked on the budget within the bound");
        // `notify_one` stores a permit if the opener is not yet waiting, so the failure
        // lands however the two interleave.
        signal.notify_one();
        let result = tokio::time::timeout(Duration::from_secs(15), job_task)
            .await
            .expect("the job ended promptly after the open failure")
            .expect("the sender task did not panic");
        assert!(
            matches!(result, Err(SendError::Disconnected(_))),
            "{result:?}"
        );
        // The teardown ran: no reader keeps reading...
        let at = reads.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            reads.load(Ordering::Relaxed),
            at,
            "a reader kept reading after the job returned"
        );
        // ...and no reader thread is still parked inside the source.
        assert_eq!(active.load(Ordering::Relaxed), 0, "a reader thread leaked");
    }

    /// An opener whose `open()` asks the lane factory (the test) for a new lane: each
    /// request is answered by spinning up a fresh lane over an in-memory pipe.
    struct LaneFactory {
        reqs: mpsc::UnboundedSender<tokio::sync::oneshot::Sender<u16>>,
    }
    impl LaneOpener for LaneFactory {
        fn open(&self) -> BoxFut<'_, Result<u16, crate::Ava1Error>> {
            let reqs = self.reqs.clone();
            Box::pin(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if reqs.send(tx).is_err() {
                    return Err(crate::Ava1Error::Io(std::io::Error::other(
                        "the test's lane factory ended",
                    )));
                }
                rx.await
                    .map_err(|_| crate::Ava1Error::Lost("the test's lane factory ended".into()))
            })
        }
        fn close(&self, _id: u16) {}
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repeated_lane_kills_with_frames_stuck_in_the_outbox_never_wedge_the_window() {
        // The wedge (I3's blunt form), pinned end to end. The source is fast and the
        // acks are throttled, so the warm-up runs the pipeline at a controlled
        // ~40 MiB/s: the governor's per-lane rate settles around 15 MB/s, which sizes
        // each warm lane's in-flight cap at ~30 MiB — seven 4 MiB frames — and the
        // 32 MiB grant then charges the whole window across the two warm lanes. The
        // peer stops reading, so each lane's writer blocks on the 1 MiB pipe mid-frame
        // with the lane's later frames still queued in its outbox — frames the writer
        // never took and the receiver never saw. The blunt rule held every dead lane's
        // charge forever (the receiver credits only what it applied), so after enough
        // kills the whole grant was held and nothing could be charged: the stall
        // detector failed a healthy job. The refined rule releases exactly the
        // never-taken frames' charge at each kill, so three kills must still complete.
        // The kill is deterministic: dropping the lane's `Link` ends its connection
        // (its writer aborts, so the take-markers are final) — and the stall
        // detector's 10 s bound is never near.
        let size = 256usize << 20;
        let credit: u64 = 32 << 20;
        let reads_on = Arc::new(AtomicBool::new(true));
        let (ack_tx, mut ack_rx) = mpsc::unbounded_channel::<(u32, u64, u32, u64, u64)>();
        let timing = Timing {
            ping_every: Duration::from_secs(3600),
            dead_after: Duration::from_secs(3600),
            handshake: Duration::from_secs(5),
            min_frame_rate: crate::link::MIN_FRAME_RATE,
        };
        let job = [0x7eu8; 16];
        // The control connection.
        let (ca, cb) = duplex(1 << 20);
        let (car, caw) = split(ca);
        let (cbr, cbw) = split(cb);
        let (ctx, mut crx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (clink, coutbox) =
            crate::link::drive(FrameReader::new(car), FrameWriter::new(caw), timing, ctx);
        let router = Arc::new(Router::default());
        // Lane 1, up before the job; the test holds its Link for the first kill. Its
        // peer reader is gated on `reads_on` (off: the lane backs up).
        let (la, lb) = duplex(1 << 20);
        let (lar, law) = split(la);
        let (lbr, _lbw) = split(lb);
        let (ltx, mut lrx) = mpsc::channel(crate::link::DELIVER_DEPTH);
        let (llink1, loutbox) =
            crate::link::drive(FrameReader::new(lar), FrameWriter::new(law), timing, ltx);
        let gen1 = router.lane_up(1, loutbox);
        let r1 = router.clone();
        tokio::spawn(async move {
            while lrx.recv().await.is_some() {}
            r1.lane_down(1, gen1);
        });
        {
            let reads_on = reads_on.clone();
            let ack_tx = ack_tx.clone();
            tokio::spawn(async move {
                let mut peer = FrameReader::new(lbr);
                peer.set_max_body(crate::frame::MAX_BODY);
                loop {
                    while !reads_on.load(Ordering::Relaxed) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    match peer.recv().await {
                        Ok(f) if is_data_type(f.ty) => {
                            let c: Chunk = f.decode().unwrap();
                            let _ = ack_tx.send((
                                f.channel,
                                f.body.len() as u64,
                                c.file_id,
                                c.offset,
                                c.data.len() as u64,
                            ));
                        }
                        Ok(_) => {} // the lane's heartbeat Ping
                        Err(_) => return,
                    }
                }
            });
        }
        // The fake receiver: answers the open and the map, turns every forwarded lane
        // frame into Received + Credit (what the C receiver does on apply), and ends
        // the job once the received ranges cover the file (re-sent frames overlap).
        tokio::spawn(async move {
            let mut peer = FrameReader::new(cbr);
            peer.set_max_body(crate::frame::MAX_BODY);
            let mut peer_w = FrameWriter::new(cbw);
            let (mut manifest_bytes, mut manifest_files) = (0u64, 0u32);
            let mut covered = RangeSet::new();
            loop {
                tokio::select! {
                    f = peer.recv() => match f {
                        Ok(f) if f.ty == JobOpen::TYPE => {
                            let open: JobOpen = f.decode().unwrap();
                            peer_w.send_msg(0, &JobOpenAck {
                                job_id: open.job_id,
                                status: 0,
                                credit,
                                staged: 1,
                                workers: 4,
                                message: None,
                            }).await.unwrap();
                        }
                        Ok(f) if f.ty == ManifestEnd::TYPE => {
                            let me: ManifestEnd = f.decode().unwrap();
                            manifest_bytes = me.bytes;
                            manifest_files = me.files;
                            peer_w.send_msg(0, &JobMap {
                                job_id: job,
                                status: 0,
                                last: 1,
                                done: vec![],
                                partial: vec![],
                                message: None,
                                held: None,
                            }).await.unwrap();
                        }
                        Ok(_) => {}
                        Err(_) => return,
                    },
                    ack = ack_rx.recv() => match ack {
                        Some((seq, len, _file_id, offset, data_len)) => {
                            // The ack throttle: one apply every 100 ms paces the whole
                            // pipeline at ~40 MiB/s, so the governor's per-lane rate
                            // settles near 15 MB/s during the warm-up — the wedge
                            // needs each warm lane's in-flight cap at ~seven frames.
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            let _ = peer_w.send_msg(0, &Received { job_id: job, lane: 1, seq }).await;
                            let _ = peer_w.send_msg(0, &Credit { job_id: job, bytes: len }).await;
                            covered.insert(offset, offset + data_len);
                            if manifest_bytes > 0 && covered.covered() >= manifest_bytes {
                                let _ = peer_w.send_msg(0, &JobDone {
                                    job_id: job,
                                    status: 0,
                                    files: manifest_files,
                                    bytes: manifest_bytes,
                                    message: None, settling: None,}).await;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
        // Route the control connection's frames into the job, like the session dispatcher.
        let r2 = router.clone();
        tokio::spawn(async move {
            while let Some(f) = crx.recv().await {
                if is_data_type(f.ty) {
                    let _ = r2.route_control(f).await;
                }
            }
            r2.close("the session ended");
        });
        // The opener asks the lane factory for a new lane on demand; the factory spins
        // one up (peer reader gated on `reads_on`) and hands its Link to the test as
        // the kill switch — dropping the Link ends the lane's connection.
        let (req_tx, mut req_rx) = mpsc::unbounded_channel::<tokio::sync::oneshot::Sender<u16>>();
        let (new_tx, mut new_rx) = mpsc::unbounded_channel::<(u16, crate::link::Link)>();
        let opener = Arc::new(LaneFactory { reqs: req_tx }) as Arc<dyn LaneOpener>;
        let mut link = JobLink::new(job, router.clone(), ConnTx::new(coutbox), Some(opener));
        let (reads_on_f, ack_tx_f) = (reads_on.clone(), ack_tx.clone());
        tokio::spawn(async move {
            let mut next_id = 2u16;
            while let Some(tx) = req_rx.recv().await {
                let id = next_id;
                next_id += 1;
                let (la, lb) = duplex(1 << 20);
                let (lar, law) = split(la);
                let (lbr, _lbw) = split(lb);
                let (ltx, mut lrx) = mpsc::channel(crate::link::DELIVER_DEPTH);
                let (llink, loutbox) =
                    crate::link::drive(FrameReader::new(lar), FrameWriter::new(law), timing, ltx);
                let gen = router.lane_up(id, loutbox);
                let r3 = router.clone();
                tokio::spawn(async move {
                    while lrx.recv().await.is_some() {}
                    r3.lane_down(id, gen);
                });
                tokio::spawn({
                    let reads_on = reads_on_f.clone();
                    let ack_tx = ack_tx_f.clone();
                    async move {
                        let mut peer = FrameReader::new(lbr);
                        peer.set_max_body(crate::frame::MAX_BODY);
                        loop {
                            while !reads_on.load(Ordering::Relaxed) {
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            match peer.recv().await {
                                Ok(f) if is_data_type(f.ty) => {
                                    let c: Chunk = f.decode().unwrap();
                                    let _ = ack_tx.send((
                                        f.channel,
                                        f.body.len() as u64,
                                        c.file_id,
                                        c.offset,
                                        c.data.len() as u64,
                                    ));
                                }
                                Ok(_) => {} // the lane's heartbeat Ping
                                Err(_) => return,
                            }
                        }
                    }
                });
                let _ = new_tx.send((id, llink));
                let _ = tx.send(id);
            }
        });
        // The job over a fast source; the kill driver runs alongside. The source
        // keeps up with anything, so the credit is what throttles the pieces — the
        // whole grant is charged while the writers block, exactly the wedge state.
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let src: Arc<dyn Source> = Arc::new(CountingSource {
            data: Arc::new(vec![0x5c; size]),
            reads: reads.clone(),
            active: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            slow: false,
        });
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let job_task =
            tokio::spawn(
                async move { send_job(&mut link, m, src, SendOptions::upload("dest")).await },
            );
        // Warm-up: the peer reads, so each lane's rate (and the seven-frame in-flight
        // cap that follows it) is established. The wait also spans at least two
        // governor ticks — the EWMA needs both to reach the cap the wedge relies on.
        tokio::time::timeout(Duration::from_secs(30), async {
            let start = Instant::now();
            while reads.load(Ordering::Relaxed) < 24 * (1 << 20)
                || start.elapsed() < Duration::from_secs(3)
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the job warmed up within the bound");
        // The wedge state: the peer stops reading, so each lane's writer blocks on the
        // 1 MiB pipe while the pieces keep arriving — the lanes keep charging them
        // until the whole grant is out (the source is fast, so this takes moments and
        // each open lane is left holding several queued frames the writer never took).
        reads_on.store(false, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(700)).await;
        // Three kills: lane 1 (held above), then lane 2's and lane 3's Links as the
        // factory hands them over — lane 3 (the replacement) backs up too before it
        // dies.
        drop(llink1);
        let (_, kill2) = tokio::time::timeout(Duration::from_secs(15), new_rx.recv())
            .await
            .expect("lane 2's Link arrived within the bound")
            .expect("the lane factory is alive");
        tokio::time::sleep(Duration::from_millis(700)).await;
        drop(kill2);
        let (_, kill3) = tokio::time::timeout(Duration::from_secs(15), new_rx.recv())
            .await
            .expect("a replacement lane came up within the bound")
            .expect("the lane factory is alive");
        tokio::time::sleep(Duration::from_millis(700)).await;
        drop(kill3);
        // The readers come on: the job must complete.
        reads_on.store(true, Ordering::Relaxed);
        let report = tokio::time::timeout(Duration::from_secs(90), job_task)
            .await
            .expect("the job completed within the bound")
            .expect("the sender task did not panic")
            .expect("the upload completed");
        assert_eq!(report.status, 0, "{:?}", report.message);
        assert_eq!(report.bytes, size as u64);
        assert!(
            report.resent > 0,
            "the dead lanes' frames were requeued and re-sent"
        );
        let _keep_control = clink;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_job_completes_when_the_grant_is_smaller_than_the_chunk() {
        // I2: the receiver grants less than the sender's chunk. The unfixed sender reads
        // chunk-sized pieces that can never fit the grant — the bounded wait here fails
        // by hanging. With the read-time grant cap every piece fits and the job completes.
        let size = 12usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 3 << 20,
                credit_on_apply: true,
                done_when_complete: true,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let dir = std::env::temp_dir().join(format!("ava1-send-grant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x44u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let report = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job completed within the bound (a grant below the chunk must not hang it)")
        .expect("the upload completed");
        assert_eq!(report.status, 0, "{:?}", report.message);
        assert_eq!(report.bytes, size as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_finished_job_leaves_its_telemetry_on_the_progress() {
        // Review 009 #4: the engine's per-job record reads these after the job returns.
        let size = 3usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 16 << 20,
                credit_on_apply: true,
                done_when_complete: true,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let dir = std::env::temp_dir().join(format!("ava1-send-telem-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("f"), vec![0x11u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "f".into(),
                root: None,
            }],
        });
        let opts = SendOptions::upload("dest");
        let progress = opts.progress.clone();
        let report = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                opts,
            ),
        )
        .await
        .expect("bounded")
        .expect("the upload completed");
        assert_eq!(report.status, 0, "{:?}", report.message);
        assert_eq!(progress.attempts.load(Ordering::Relaxed), 1);
        let t = progress.telemetry.lock().unwrap().clone();
        assert_eq!(
            t.shares.history.len() as u32,
            t.shares.ticks.min(super::governor::HISTORY_MAX as u32)
        );
        assert_eq!(progress.unswept_peak.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_smaller_than_one_group_fails_loudly_instead_of_hanging() {
        // I2: with a window below one verification group, even the smallest legal piece
        // (one group) can never fit and no Credit can arrive — the job must fail with a
        // Protocol error naming the grant, not park forever.
        let size = 8usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 512 << 10,
                credit_on_apply: true,
                done_when_complete: true,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let dir = std::env::temp_dir().join(format!("ava1-send-tinygrant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x45u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let err = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job ended within the bound (it must not hang)")
        .expect_err("a window below one group cannot send anything");
        assert!(
            matches!(&err, SendError::Protocol(msg) if msg.contains("524288") && msg.contains("window")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_credit_between_the_ack_and_the_map_is_not_discarded() {
        // M5: the ack grants nothing and the receiver returns Credit before the map. The
        // unfixed open_upload discards it, leaving the window empty forever (the bounded
        // wait here fails); folded in, the window is usable and the job completes.
        let size = 8usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 0,
                credit_on_apply: true,
                done_when_complete: true,
                credit_after_ack: Some(4 << 20),
                malformed_status: false,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let dir =
            std::env::temp_dir().join(format!("ava1-send-earlycredit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x46u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let report = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job completed within the bound")
        .expect("the upload completed");
        assert_eq!(report.status, 0, "{:?}", report.message);
        assert_eq!(report.bytes, size as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_status_is_a_protocol_error() {
        // M6: like every other control frame, a Status whose decode fails ends the job
        // with a Protocol error — the unfixed sender shrugs it off and completes.
        let size = 4usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 64 << 20,
                credit_on_apply: true,
                done_when_complete: true,
                credit_after_ack: None,
                malformed_status: true,
                retry_unknown: false,
                ..Default::default()
            },
            None,
        );
        let dir = std::env::temp_dir().join(format!("ava1-send-badstatus-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x47u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let err = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job ended within the bound")
        .expect_err("a malformed Status ends the job");
        assert!(matches!(err, SendError::Protocol(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_status_between_the_ack_and_the_map_is_a_protocol_error() {
        // M6's open-window arm: `next_ctl` skips a Status in the open window only
        // after its decode succeeds — a malformed one there is the same protocol
        // error as one mid-transfer, not something to shrug off.
        let size = 4usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 64 << 20,
                credit_on_apply: true,
                done_when_complete: true,
                malformed_status_open: true,
                ..Default::default()
            },
            None,
        );
        let dir =
            std::env::temp_dir().join(format!("ava1-send-badstatusopen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x49u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let err = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job ended within the bound")
        .expect_err("a malformed Status in the open window ends the job");
        assert!(matches!(err, SendError::Protocol(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_well_formed_status_between_the_ack_and_the_map_is_absorbed() {
        // M6's open-window arm, the other half: a Status that decodes is skipped and
        // the open completes (it must not break the JobOpenAck-to-JobMap wait).
        let size = 4usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 64 << 20,
                credit_on_apply: true,
                done_when_complete: true,
                wellformed_status_open: true,
                ..Default::default()
            },
            None,
        );
        let dir =
            std::env::temp_dir().join(format!("ava1-send-okstatusopen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x4au8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let report = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job completed within the bound")
        .expect("the upload completed");
        assert_eq!(report.status, 0, "{:?}", report.message);
        assert_eq!(report.bytes, size as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reader_failure_sends_the_cancel_and_ends_the_job_without_deadlocking_on_a_full_outbox(
    ) {
        // The Read::Failed arm: the first failure wins and its JobCancel is sent on the
        // dedicated select arm — awaiting it inline would stop the inbox drain while the
        // control outbox is full, and the sender's and the receiver's backpressure then
        // deadlock against each other (the shape the FileRoot fix removed). Here the
        // receiver answers the open and the map, then pauses reading the control
        // connection — the sender's control outbox fills with the file roots — while it
        // keeps flooding status frames on the control connection (its sends only keep
        // flowing because the sender keeps draining its inbox). The source fails on its
        // 150th open, mid-transfer. The job must end with the reader's error and the
        // JobCancel must reach the receiver, all within the bound.
        let cancel_seen = Arc::new(AtomicU32::new(0));
        let (mut link, lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 64 << 20,
                credit_on_apply: true,
                done_when_complete: false,
                pause_control: Some(Duration::from_millis(1200)),
                control_pipe: 4 << 10,
                cancel_seen: cancel_seen.clone(),
                ..Default::default()
            },
            None,
        );
        // 300 large files (at the cutoff: each sends a FileRoot), so the control pipe
        // (4 KiB) and the outbox (64 slots) fill with the roots once the receiver
        // stops reading, and the failing open lands while they are full.
        let opens = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let src: Arc<dyn Source> = Arc::new(FailingAfter {
            data: vec![0x5d; 256 << 10],
            opens: opens.clone(),
            active: active.clone(),
            fail_after: 150,
        });
        let m = Arc::new(Manifest {
            entries: (0..300)
                .map(|i| Entry {
                    kind: gen::ENTRY_FILE,
                    mode: 0o644,
                    size: 256 << 10,
                    mtime: 1,
                    path: format!("f{i:03}"),
                    root: None,
                })
                .collect(),
        });
        let job_task =
            tokio::spawn(
                async move { send_job(&mut link, m, src, SendOptions::upload("dest")).await },
            );
        let err = tokio::time::timeout(Duration::from_secs(30), job_task)
            .await
            .expect("the job ended promptly after the reader failure")
            .expect("the sender task did not panic")
            .expect_err("a reader failure ends the job");
        assert!(matches!(err, SendError::Source(_)), "{err:?}");
        // The failure landed mid-transfer: the earlier files' chunks were already sent.
        assert!(
            lane_seen.load(Ordering::Relaxed) > 0,
            "no frame was sent before the failure"
        );
        // The JobCancel reached the receiver through the once-full outbox.
        tokio::time::timeout(Duration::from_secs(10), async {
            while cancel_seen.load(Ordering::SeqCst) != gen::ERR_IO as u32 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the receiver saw the JobCancel (reason ERR_IO) within the bound");
        // The teardown joined the readers: none is still inside the source.
        tokio::time::timeout(Duration::from_secs(10), async {
            while active.load(Ordering::SeqCst) != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a reader thread leaked past the job's end");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_retry_for_an_unknown_file_is_a_protocol_error() {
        // M7: a FileRetry for an id not in the manifest must fail the job — the unfixed
        // sender silently queues it for the large reader, which exits, and the job
        // completes without the retried file.
        let size = 4usize << 20;
        let (mut link, _lane_seen, _keep) = fake_link(
            FakeReceiver {
                credit: 64 << 20,
                credit_on_apply: true,
                done_when_complete: true,
                credit_after_ack: None,
                malformed_status: false,
                retry_unknown: true,
                ..Default::default()
            },
            None,
        );
        let dir = std::env::temp_dir().join(format!("ava1-send-badretry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("big"), vec![0x48u8; size]).unwrap();
        let m = Arc::new(Manifest {
            entries: vec![Entry {
                kind: gen::ENTRY_FILE,
                mode: 0o644,
                size: size as u64,
                mtime: 1,
                path: "big".into(),
                root: None,
            }],
        });
        let err = tokio::time::timeout(
            Duration::from_secs(20),
            send_job(
                &mut link,
                m,
                Arc::new(crate::source::LocalSource::new(dir.clone())),
                SendOptions::upload("dest"),
            ),
        )
        .await
        .expect("the job ended within the bound")
        .expect_err("a FileRetry for an unknown file ends the job");
        assert!(
            matches!(&err, SendError::Protocol(msg) if msg.contains("FileRetry")),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
