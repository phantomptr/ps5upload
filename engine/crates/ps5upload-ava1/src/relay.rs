//! Bounded engine relay: ordered download from A feeds an upload to B in RAM.
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use ava1::gen;
use ava1::manifest::Manifest;
use ava1::ranges::Need;
use ava1::recv::{download_open, download_run, RecvOptions, Sink};
use ava1::send::{open_upload, run_upload, Progress, SendError, SendOptions, SendReport};
use ava1::source::{ReadAt, Source, SourceMeta};

use crate::pool::{pool, Pool};
use crate::upload::{rearm, refusal, ConsoleFailure, SessionGate};

const RELAY_CAP: usize = 64 << 20;
// A blocked source or lane may leave both halves alive without progress.
const RELAY_WAIT_DEFAULT: Duration = Duration::from_secs(120);
static RELAY_WAIT_MS: AtomicU64 = AtomicU64::new(120_000);

/// How long one `put`/`take` may make no progress. Never a bound on how long a
/// file may take: only a *stalled* hand-off fails, a slow one does not.
fn relay_wait() -> Duration {
    Duration::from_millis(RELAY_WAIT_MS.load(Ordering::Relaxed))
}

/// Test knob: shrink the no-progress bound so a stall test does not take minutes.
#[doc(hidden)]
pub fn set_wait_for_tests(d: Duration) {
    RELAY_WAIT_MS.store(d.as_millis() as u64, Ordering::Relaxed);
}

#[doc(hidden)]
pub fn reset_wait_for_tests() {
    RELAY_WAIT_MS.store(RELAY_WAIT_DEFAULT.as_millis() as u64, Ordering::Relaxed);
}
/// How long a finished destination waits for the source's closing handshake.
const A_GRACE: Duration = Duration::from_secs(15);
const STALL_LIMIT: Duration = Duration::from_secs(600);

struct State {
    chunks: BTreeMap<(u32, u64), Vec<u8>>,
    bytes: usize,
    failed: bool,
    /// A's download ended cleanly: nothing more will arrive, nothing is wrong.
    source_done: bool,
    /// Last put/take/leave: a file waiting its turn gives up only after this long idle.
    last_activity: Instant,
    /// Offsets a reader is parked on right now. A put that supplies one is admitted
    /// even into a full buffer (see `put`).
    wanted: Vec<(u32, u64)>,
}

struct Relay {
    /// A's receiver ran `finish`: every byte arrived and was verified on A's side.
    finished: AtomicBool,
    cap: usize,
    count: usize,
    expected: Need,
    state: Mutex<State>,
    changed: Condvar,
}

impl Relay {
    fn new(m: &Manifest, skip: &Need) -> Self {
        let mut expected = Need::default();
        for (i, e) in m.entries.iter().enumerate() {
            if e.kind != gen::ENTRY_FILE || skip.done.contains(&(i as u32)) {
                continue;
            }
            let id = i as u32;
            let skipped = skip.partial.get(&id).cloned().unwrap_or_default();
            let ranges = expected.partial.entry(id).or_default();
            for (start, end) in skipped.missing(e.size) {
                ranges.insert(start, end);
            }
        }
        Self {
            finished: AtomicBool::new(false),
            cap: RELAY_CAP,
            count: m.entries.len(),
            expected,
            state: Mutex::new(State {
                chunks: BTreeMap::new(),
                bytes: 0,
                failed: false,
                source_done: false,
                wanted: Vec::new(),
                last_activity: Instant::now(),
            }),
            changed: Condvar::new(),
        }
    }

    #[cfg(test)]
    fn with_cap(mut self, cap: usize) -> Self {
        self.cap = cap;
        self
    }

    fn idle(&self) -> Duration {
        self.state.lock().unwrap().last_activity.elapsed()
    }

    fn finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    fn fail(&self) {
        let mut s = self.state.lock().unwrap();
        s.failed = true;
        self.changed.notify_all();
    }

    /// A's download task ended. A clean end only means no more bytes are coming:
    /// what is already buffered is still B's to read, and B may not have opened
    /// its later (small) files yet. Only an error stops the relay.
    fn source_ended(&self, ok: bool) {
        if !ok {
            return self.fail();
        }
        let mut s = self.state.lock().unwrap();
        s.source_done = true;
        self.changed.notify_all();
    }

    fn permitted(&self, id: u32, off: u64) -> bool {
        self.expected
            .partial
            .get(&id)
            .is_some_and(|r| r.covers(off, off.saturating_add(1)))
    }

    fn put(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        // Ordered downloads synthesize empty-file writes before any data frame;
        // B's sender opens those files but reads zero bytes from them.
        if data.is_empty() && off == 0 && (id as usize) < self.count {
            return Ok(());
        }
        if id as usize >= self.count || !self.permitted(id, off) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected relay data {id}@{off}"),
            ));
        }
        if data.len() > self.cap {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "relay frame exceeds capacity",
            ));
        }
        let deadline = Instant::now() + relay_wait();
        let mut s = self.state.lock().unwrap();
        loop {
            if s.failed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "relay stopped"));
            }
            // The chunk a reader is parked on is admitted even into a full buffer: A's
            // sink writes run concurrently, so later chunks may have taken the room
            // while this one waited, and the reader cannot free any until it has this
            // one. The overshoot is one chunk per parked reader.
            let needed = s
                .wanted
                .iter()
                .any(|&(wid, woff)| wid == id && off <= woff && woff < off + data.len() as u64);
            if s.bytes + data.len() <= self.cap || needed {
                if let Some(old) = s.chunks.insert((id, off), data.to_vec()) {
                    s.bytes -= old.len();
                }
                s.bytes += data.len();
                s.last_activity = Instant::now();
                self.changed.notify_all();
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("relay put timed out at {id}@{off}"),
                ));
            }
            let (next, _) = self.changed.wait_timeout(s, deadline - now).unwrap();
            s = next;
        }
    }

    fn take(&self, id: u32, off: u64) -> io::Result<Vec<u8>> {
        if !self.permitted(id, off) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("relay read outside expected set at {id}@{off}"),
            ));
        }
        let deadline = Instant::now() + relay_wait();
        let mut s = self.state.lock().unwrap();
        s.wanted.push((id, off));
        // A put parked for room may be exactly the chunk this read needs.
        self.changed.notify_all();
        let result = loop {
            if let Some((&(key_id, start), _)) = s.chunks.range(..=(id, off)).next_back() {
                if key_id == id && s.chunks[&(key_id, start)].len() as u64 > off - start {
                    let data = s.chunks.remove(&(key_id, start)).unwrap();
                    s.bytes -= data.len();
                    s.last_activity = Instant::now();
                    self.changed.notify_all();
                    break Ok(if off == start {
                        data
                    } else {
                        data[(off - start) as usize..].to_vec()
                    });
                }
            }
            if s.failed || s.source_done {
                break Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    format!("relay ended before {id}@{off}"),
                ));
            }
            let now = Instant::now();
            if now >= deadline {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("relay take timed out at {id}@{off}"),
                ));
            }
            let (next, _) = self.changed.wait_timeout(s, deadline - now).unwrap();
            s = next;
        };
        if let Some(i) = s.wanted.iter().position(|w| *w == (id, off)) {
            s.wanted.swap_remove(i);
        }
        result
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Settle {
    Done,
    Cancelled,
    Retry,
    Fail,
}

/// What one attempt's outcome means. The destination decides: once it reports OK the
/// transfer is done whatever the source did afterwards (`a_dropped` is ignored);
/// a destination refusal is final even if the source dropped too; only a transport
/// loss on either side retries.
fn settle(b: &Result<SendReport, SendError>, a_dropped: bool) -> Settle {
    match b {
        Ok(r) if r.status == gen::STATUS_OK => Settle::Done,
        Ok(_) | Err(SendError::Refused { .. }) => Settle::Fail,
        Err(SendError::Cancelled) => Settle::Cancelled,
        Err(SendError::Disconnected(_)) => Settle::Retry,
        Err(_) if a_dropped => Settle::Retry,
        Err(_) => Settle::Fail,
    }
}

/// The source's task may be abandoned only when the destination is complete and the
/// source's receiver already finished (so nothing it could still say changes the data).
fn may_abandon_source(destination_ok: bool, source_finished: bool) -> bool {
    destination_ok && source_finished
}

struct RelaySink(Arc<Relay>);
impl Sink for RelaySink {
    fn prepare(&self, _m: &Manifest) -> io::Result<()> {
        Ok(())
    }
    fn write_at(&self, id: u32, off: u64, data: &[u8]) -> io::Result<()> {
        self.0.put(id, off, data)
    }
    fn write_whole(&self, id: u32, data: &[u8]) -> io::Result<()> {
        self.0.put(id, 0, data)
    }
    // B's journal is the durability authority. Losing this buffer only costs a reread.
    fn sync(&self, _ids: &[u32]) -> io::Result<()> {
        Ok(())
    }
    fn read_at(&self, _id: u32, _off: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn transient_relay(&self) -> bool {
        true
    }
    fn commit(&self, _id: u32) -> io::Result<()> {
        Ok(())
    }
    fn finish(&self) -> io::Result<()> {
        self.0.finished.store(true, Ordering::Relaxed);
        Ok(())
    }
}

struct RelayReader {
    relay: Arc<Relay>,
    id: u32,
    pending: Vec<u8>,
    pending_off: u64,
    turn: Arc<Turn>,
}
struct Turn {
    ids: Vec<u32>,
    next: Mutex<usize>,
    changed: Condvar,
    cancel: Arc<AtomicBool>,
    /// When the last file left; with `Relay::idle` it measures a stalled predecessor.
    last_leave: Mutex<Instant>,
}
impl Turn {
    /// Waits for the previous file to finish. There is no wall-clock bound on the
    /// predecessor (it may legitimately take hours) but there is a progress bound: if
    /// nothing at all happened (no chunk in or out, no file finished) for
    /// `relay_wait()`, the predecessor will never come (its reader exited because
    /// the destination's job ended while its session stayed open) and this wait
    /// fails instead of hanging the sender's teardown join.
    fn enter(&self, id: u32, relay: &Relay) -> io::Result<()> {
        let mut next = self.next.lock().unwrap();
        while self.ids.get(*next) != Some(&id) {
            if relay.state.lock().unwrap().failed {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "relay stopped"));
            }
            if *next >= self.ids.len() || self.ids[*next] > id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("relay read order passed file {id}"),
                ));
            }
            if self.cancel.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "relay cancelled",
                ));
            }
            let idle = relay.idle().min(self.last_leave.lock().unwrap().elapsed());
            if idle > relay_wait() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("relay read order stalled before file {id}: no progress for {idle:?}"),
                ));
            }
            let (n, _) = self
                .changed
                .wait_timeout(next, Duration::from_millis(100))
                .unwrap();
            next = n;
        }
        Ok(())
    }
    fn leave(&self, id: u32) {
        let mut next = self.next.lock().unwrap();
        if self.ids.get(*next) == Some(&id) {
            *self.last_leave.lock().unwrap() = Instant::now();
            *next += 1;
            self.changed.notify_all();
        }
    }
}
impl Drop for RelayReader {
    fn drop(&mut self) {
        self.turn.leave(self.id);
    }
}
impl ReadAt for RelayReader {
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if off < self.pending_off || off >= self.pending_off + self.pending.len() as u64 {
            self.pending = self.relay.take(self.id, off)?;
            self.pending_off = off;
        }
        if self.pending.is_empty() {
            return Ok(0);
        }
        let start = (off - self.pending_off) as usize;
        let n = buf.len().min(self.pending.len() - start);
        buf[..n].copy_from_slice(&self.pending[start..start + n]);
        Ok(n)
    }
}

struct RelaySource {
    relay: Arc<Relay>,
    manifest: Arc<Manifest>,
    ids: HashMap<String, u32>,
    turn: Arc<Turn>,
}
impl RelaySource {
    fn new(
        relay: Arc<Relay>,
        manifest: Arc<Manifest>,
        b_need: &Need,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        let ids = manifest
            .entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i as u32))
            .collect();
        let turns = manifest
            .entries
            .iter()
            .enumerate()
            .filter(|(i, e)| e.kind == gen::ENTRY_FILE && !b_need.done.contains(&(*i as u32)))
            .map(|(i, _)| i as u32)
            .collect();
        Self {
            relay,
            manifest,
            ids,
            turn: Arc::new(Turn {
                ids: turns,
                next: Mutex::new(0),
                changed: Condvar::new(),
                cancel,
                last_leave: Mutex::new(Instant::now()),
            }),
        }
    }
}
impl Source for RelaySource {
    fn open(&self, rel: &str) -> io::Result<Box<dyn ReadAt>> {
        let id = *self
            .ids
            .get(rel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))?;
        self.turn.enter(id, &self.relay)?;
        Ok(Box::new(RelayReader {
            relay: self.relay.clone(),
            id,
            pending: Vec::new(),
            pending_off: 0,
            turn: self.turn.clone(),
        }))
    }
    /// The destination's job is over: wake every reader parked for the source's bytes or
    /// its turn, so the sender's teardown join returns now, not after the stall bound.
    fn close(&self) {
        self.relay.fail();
    }
    fn list(&self, _rel: &str) -> io::Result<Vec<(String, SourceMeta)>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "relay manifest is already known",
        ))
    }
    fn stat(&self, rel: &str) -> io::Result<SourceMeta> {
        let id = *self
            .ids
            .get(rel)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))?;
        let e = self
            .manifest
            .entry(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, rel.to_owned()))?;
        Ok(SourceMeta {
            size: e.size,
            mtime: e.mtime,
            mode: e.mode,
            is_dir: e.kind == gen::ENTRY_DIR,
        })
    }
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn ps5_to_ps5(
    from: &str,
    src: &str,
    to: &str,
    dest: &str,
    job_id: [u8; 16],
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
) -> Result<SendReport> {
    ps5_to_ps5_between(
        pool(),
        from,
        src,
        pool(),
        to,
        dest,
        job_id,
        progress,
        cancel,
    )
}

/// Pool injection for local two-host tests and the same production relay body.
#[allow(clippy::too_many_arguments)]
pub fn ps5_to_ps5_between(
    from_pool: &Pool,
    from: &str,
    src: &str,
    to_pool: &Pool,
    to: &str,
    dest: &str,
    job_id: [u8; 16],
    progress: Arc<Progress>,
    cancel: Arc<AtomicBool>,
) -> Result<SendReport> {
    if std::ptr::eq(from_pool, to_pool) && from.split(':').next() == to.split(':').next() {
        return Err(anyhow!(
            "relay source and destination must be different consoles"
        ));
    }
    // The same terminal classification as an upload: no identity ends the job at once.
    SessionGate::identity(from_pool)?;
    SessionGate::identity(to_pool)?;
    let hex = ava1::hex::encode(&job_id);
    let persist = to_pool.ava_dir().join("send").join(&hex);
    let _live = to_pool.live_job(&job_id); // the journal sweep leaves a running job alone
    let scratch = from_pool.ava_dir().join("relay").join(&hex);
    let _scratch = Scratch(scratch.clone());
    crate::block_on(async {
        let mut backoff = Duration::from_millis(250);
        let mut busy = 0u32;
        let (mut gate_a, mut gate_b) = (SessionGate::default(), SessionGate::default());
        let (mut last_at, mut last_durable) = (Instant::now(), 0u64);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("transfer_cancelled"));
            }
            // A's journal is never authoritative. Each attempt derives its map
            // anew from B's durable map and the sender's persisted outboards.
            match std::fs::remove_dir_all(&scratch) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let durable = progress.bytes_durable.load(Ordering::Relaxed);
            let attempt_started = Instant::now();
            if durable > last_durable {
                last_at = Instant::now();
                last_durable = durable;
            } else if last_at.elapsed() > STALL_LIMIT {
                return Err(anyhow!("no durable progress for {STALL_LIMIT:?}"));
            }
            let sa = match from_pool.session(from).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(failure) = gate_a.failed(&e) {
                        return Err(ConsoleFailure::on(from, failure).into());
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    let _ = writeln!(std::io::stderr(), "ava1 relay: reconnecting source: {e}");
                    continue;
                }
            };
            gate_a.connected();
            let sb = match to_pool.session(to).await {
                Ok(s) => s,
                Err(e) => {
                    if let Some(failure) = gate_b.failed(&e) {
                        return Err(ConsoleFailure::on(to, failure).into());
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    let _ = writeln!(
                        std::io::stderr(),
                        "ava1 relay: reconnecting destination: {e}"
                    );
                    continue;
                }
            };
            gate_b.connected();
            let (mut la, mut lb) = (sa.job(job_id), sb.job(job_id));
            let m = match download_open(&mut la, src, gen::JF_ORDERED, RELAY_CAP as u64).await {
                Ok(m) => m,
                Err(SendError::Disconnected(e)) => {
                    let _ = writeln!(std::io::stderr(), "ava1 relay: source open dropped: {e}");
                    from_pool.forget_if(from, &sa).await;
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                Err(SendError::Refused { status, message }) if status == gen::ERR_BUSY => {
                    busy += 1;
                    if busy > from_pool.busy_tries() {
                        return Err(crate::upload::busy_failure(
                            from_pool.busy_tries(),
                            &message,
                        ));
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let mut o = SendOptions::upload(dest);
            o.progress = progress.clone();
            o.cancel = cancel.clone();
            o.persist = Some(persist.clone());
            // Must be one reader: parallel file reads can fill the cap with later files.
            o.readers = 1;
            let opened = match open_upload(&mut lb, &m, &o).await {
                Ok(opened) => opened,
                Err(SendError::Disconnected(e)) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "ava1 relay: destination open dropped: {e}"
                    );
                    to_pool.forget_if(to, &sb).await;
                    tokio::time::sleep(backoff).await;
                    continue;
                }
                // The destination console says BUSY (finishing a job's files): retry, bounded.
                Err(SendError::Refused { status, message }) if status == gen::ERR_BUSY => {
                    busy += 1;
                    if busy > to_pool.busy_tries() {
                        return Err(crate::upload::busy_failure(to_pool.busy_tries(), &message));
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
                Err(SendError::OpenTimeout(t)) => {
                    busy += 1;
                    if busy > to_pool.busy_tries() {
                        return Err(crate::upload::open_timeout_failure(to_pool.busy_tries(), t));
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let skip = ava1::send::skip_set(&m, &opened.1, Some(&persist));
            let relay = Arc::new(Relay::new(&m, &skip));
            let ro = RecvOptions {
                credit: RELAY_CAP as u64,
                flags: gen::JF_ORDERED,
                jobs_dir: scratch.clone(),
                ordered: true,
                progress: Arc::default(),
                cancel: cancel.clone(),
                progress_deadline: None,
            };
            let a_relay = relay.clone();
            let a_m = m.clone();
            let mut a = tokio::spawn(async move {
                let result = download_run(
                    &mut la,
                    a_m,
                    Some(skip),
                    Arc::new(RelaySink(a_relay.clone())),
                    ro,
                )
                .await;
                a_relay.source_ended(result.is_ok());
                result
            });
            let source = Arc::new(RelaySource::new(
                relay.clone(),
                m.clone(),
                &opened.1,
                cancel.clone(),
            ));
            // B's readers park inside `take` for A's data and the sender joins them
            // on every exit: a dead destination session must wake them, or the
            // attempt waits out the no-progress bound before it can reconnect.
            let watch = {
                let (relay, sb) = (relay.clone(), sb.clone());
                tokio::spawn(async move {
                    while !sb.is_closed() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    relay.fail();
                })
            };
            let b = run_upload(&mut lb, m, source, o, opened).await;
            watch.abort();
            relay.fail();
            // Once the destination has everything, only A's closing handshake can be
            // outstanding: do not hold a finished transfer for the full bound.
            let b_ok = matches!(&b, Ok(r) if r.status == gen::STATUS_OK);
            let grace = if b_ok { A_GRACE } else { relay_wait() };
            let a_result = match tokio::time::timeout(grace, &mut a).await {
                Ok(r) => r.map_err(|e| anyhow!(e))?.map(|_| ()),
                Err(_) if may_abandon_source(b_ok, relay.finished()) => {
                    a.abort();
                    let _ = writeln!(
                        std::io::stderr(),
                        "ava1 relay: source closed late; its data was complete and verified"
                    );
                    Ok(())
                }
                Err(_) => {
                    a.abort();
                    return Err(anyhow!("source relay did not stop within {grace:?}"));
                }
            };
            let a_dropped = matches!(a_result, Err(SendError::Disconnected(_)));
            match settle(&b, a_dropped) {
                Settle::Done => {
                    // Every file root is verified and committed on the destination: a
                    // late error on the source side cannot undo that, nor restart it.
                    if let Err(e) = a_result {
                        let _ = writeln!(
                            std::io::stderr(),
                            "ava1 relay: source ended with {e} after the destination committed"
                        );
                    }
                    let _ = std::fs::remove_dir_all(&persist);
                    return b.map_err(|e| anyhow!(e));
                }
                Settle::Cancelled => return Err(anyhow!("transfer_cancelled")),
                Settle::Retry => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "ava1 relay: attempt ended, retrying: destination {b:?}, source {a_result:?}"
                    );
                    from_pool.forget_if(from, &sa).await;
                    to_pool.forget_if(to, &sb).await;
                    rearm(
                        &mut backoff,
                        attempt_started,
                        durable,
                        progress.bytes_durable.load(Ordering::Relaxed),
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
                Settle::Fail => {
                    return Err(match b {
                        // The destination's own reason, whatever the source did after.
                        Ok(r) => refusal(r.status, r.message.unwrap_or_default()).into(),
                        Err(SendError::Refused { status, message }) => {
                            refusal(status, message).into()
                        }
                        Err(e) => anyhow!(e),
                    });
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava1::manifest::Entry;

    /// The no-progress knob is process-wide; tests that move it take this lock.
    static KNOB: Mutex<()> = Mutex::new(());

    fn file(path: &str, size: u64) -> Entry {
        Entry {
            kind: gen::ENTRY_FILE,
            mode: 0o644,
            size,
            mtime: 0,
            path: path.into(),
            root: None,
        }
    }

    fn rig(sizes: &[u64]) -> (Arc<Relay>, RelaySource) {
        let m = Arc::new(Manifest {
            entries: sizes
                .iter()
                .enumerate()
                .map(|(i, n)| file(&format!("f{i}"), *n))
                .collect(),
        });
        let relay = Arc::new(Relay::new(&m, &Need::default()));
        let src = RelaySource::new(
            relay.clone(),
            m,
            &Need::default(),
            Arc::new(AtomicBool::new(false)),
        );
        (relay, src)
    }

    fn read_all(src: &RelaySource, path: &str) -> io::Result<Vec<u8>> {
        let mut r = src.open(path)?;
        let size = src.stat(path)?.size as usize;
        let mut out = Vec::new();
        let mut buf = [0u8; 16];
        while out.len() < size {
            let n = r.read_at(out.len() as u64, &mut buf)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok(out)
    }

    // Flake root cause (Task 25): A finishing first used to mark the whole relay
    // failed, so B's readers, which had not opened their small files yet, died with
    // "relay stopped" depending on thread timing.
    #[test]
    fn a_source_that_finished_cleanly_still_lets_the_destination_read() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let (relay, src) = rig(&[3, 3]);
        relay.put(0, 0, b"abc").unwrap();
        relay.put(1, 0, b"def").unwrap();
        relay.source_ended(true);
        assert_eq!(read_all(&src, "f0").unwrap(), b"abc");
        assert_eq!(read_all(&src, "f1").unwrap(), b"def");
    }

    #[test]
    fn a_source_that_finishes_while_a_file_waits_its_turn_does_not_stop_it() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let (relay, src) = rig(&[3, 3]);
        relay.put(0, 0, b"abc").unwrap();
        relay.put(1, 0, b"def").unwrap();
        let src = Arc::new(src);
        let first = src.open("f0").unwrap();
        let waiter = {
            let src = src.clone();
            std::thread::spawn(move || read_all(&src, "f1"))
        };
        std::thread::sleep(Duration::from_millis(300));
        relay.source_ended(true);
        std::thread::sleep(Duration::from_millis(300));
        drop(first);
        assert_eq!(waiter.join().unwrap().unwrap(), b"def");
    }

    #[test]
    fn a_source_that_finished_cleanly_ends_a_read_of_bytes_it_never_sent() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let (relay, src) = rig(&[3, 3]);
        relay.put(0, 0, b"abc").unwrap();
        relay.source_ended(true);
        assert_eq!(read_all(&src, "f0").unwrap(), b"abc");
        let started = Instant::now();
        let e = read_all(&src, "f1").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
        assert!(started.elapsed() < Duration::from_secs(5), "{e}");
    }

    #[test]
    fn a_terminal_session_failure_names_the_console_it_came_from() {
        let mut gate = SessionGate::default();
        let mut failure = None;
        for _ in 0..crate::upload::TERMINAL_ATTEMPTS {
            failure = gate.failed(&ava1::Ava1Error::NotPaired);
        }
        let e: anyhow::Error = ConsoleFailure::on("10.0.0.2", failure.unwrap()).into();
        let cf = e.downcast_ref::<ConsoleFailure>().expect("typed");
        assert_eq!(cf.console, "10.0.0.2");
        assert_eq!(cf.failure.reason, "ava1_not_paired");
    }

    #[test]
    fn a_failed_source_stops_the_destination() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let (relay, src) = rig(&[3, 3]);
        relay.source_ended(false);
        let e = read_all(&src, "f0").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
    }

    // Task 24 review, Critical 1: waiting for the previous file has no wall-clock bound.
    #[test]
    fn a_slow_predecessor_never_times_out_the_next_file() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        set_wait_for_tests(Duration::from_millis(300));
        let (relay, src) = rig(&[3000, 3]);
        let src = Arc::new(src);
        let first = src.open("f0").unwrap();
        let waiter = {
            let src = src.clone();
            std::thread::spawn(move || src.open("f1").map(|_| ()))
        };
        // The predecessor is slow but alive: a chunk every 100 ms for 900 ms (3x the
        // bound), each consumed.
        for i in 0..9u64 {
            relay.put(0, i * 100, &[1u8; 100]).unwrap();
            relay.take(0, i * 100).unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(first);
        let r = waiter.join().unwrap();
        reset_wait_for_tests();
        r.expect("the wait for a live predecessor must not time out");
    }

    // Review round 2, Critical 1: a predecessor that never starts (its reader exited
    // because the destination's job ended while the session stayed open) must not hang
    // the sender's teardown join forever.
    #[test]
    fn a_predecessor_that_never_starts_stalls_out_the_waiting_file() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        set_wait_for_tests(Duration::from_millis(400));
        let (_relay, src) = rig(&[3, 3]);
        // f0 is never opened.
        let started = Instant::now();
        let e = src.open("f1").err().expect("must give up");
        reset_wait_for_tests();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_destination_decides_the_outcome() {
        let report = |status| SendReport {
            status,
            message: None,
            files: 1,
            bytes: 1,
            resent: 0,
            max_lanes: 1,
            bottleneck: 0,
            sequential: false,
        };
        let dropped = || SendError::Disconnected("x".into());
        // A late source error after the destination committed is still Done.
        assert_eq!(settle(&Ok(report(gen::STATUS_OK)), true), Settle::Done);
        assert_eq!(settle(&Ok(report(gen::STATUS_OK)), false), Settle::Done);
        // A refusal is final even if the source also dropped.
        assert_eq!(settle(&Ok(report(gen::ERR_NO_SPACE)), true), Settle::Fail);
        assert_eq!(
            settle(
                &Err(SendError::Refused {
                    status: gen::ERR_EXISTS,
                    message: String::new()
                }),
                true
            ),
            Settle::Fail
        );
        assert_eq!(settle(&Err(dropped()), false), Settle::Retry);
        assert_eq!(
            settle(&Err(SendError::Protocol("x".into())), true),
            Settle::Retry
        );
        assert_eq!(
            settle(&Err(SendError::Protocol("x".into())), false),
            Settle::Fail
        );
        assert_eq!(settle(&Err(SendError::Cancelled), true), Settle::Cancelled);
    }

    #[test]
    fn a_cancelled_or_failed_relay_wakes_a_file_waiting_its_turn() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let (relay, src) = rig(&[3, 3]);
        let src = Arc::new(src);
        let _first = src.open("f0").unwrap();
        let waiter = {
            let src = src.clone();
            std::thread::spawn(move || src.open("f1").err().map(|e| e.kind()))
        };
        std::thread::sleep(Duration::from_millis(200));
        relay.fail();
        assert_eq!(waiter.join().unwrap(), Some(io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn a_cancelled_job_wakes_a_file_waiting_its_turn() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let m = Arc::new(Manifest {
            entries: vec![file("f0", 3), file("f1", 3)],
        });
        let relay = Arc::new(Relay::new(&m, &Need::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let src = Arc::new(RelaySource::new(relay, m, &Need::default(), cancel.clone()));
        let _first = src.open("f0").unwrap();
        let waiter = {
            let src = src.clone();
            std::thread::spawn(move || src.open("f1").err().map(|e| e.kind()))
        };
        std::thread::sleep(Duration::from_millis(200));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(waiter.join().unwrap(), Some(io::ErrorKind::Interrupted));
    }

    // C7: fail() is unconditional and wakes a parked take.
    #[test]
    fn fail_wakes_a_take_parked_for_data() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let (relay, _src) = rig(&[3]);
        let r = relay.clone();
        let t = std::thread::spawn(move || r.take(0, 0).map(|_| ()));
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        relay.fail();
        let e = t.join().unwrap().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // C14: only bytes of the expected set are accepted or read.
    #[test]
    fn data_outside_the_expected_set_is_refused() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        let m = Manifest {
            entries: vec![file("f0", 3), file("f1", 3)],
        };
        let mut skip = Need::default();
        skip.done.insert(1);
        let relay = Relay::new(&m, &skip);
        // Out of range id, a skipped (durable) file, and an offset past the end.
        for (id, off) in [(2u32, 0u64), (1, 0), (0, 3)] {
            let e = relay.put(id, off, b"x").unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "put {id}@{off}");
        }
        for (id, off) in [(2u32, 0u64), (1, 0), (0, 3)] {
            let e = relay.take(id, off).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "take {id}@{off}");
        }
        relay.put(0, 0, b"abc").unwrap();
    }

    #[test]
    fn a_put_into_a_full_buffer_times_out_instead_of_hanging() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        set_wait_for_tests(Duration::from_millis(200));
        let m = Manifest {
            entries: vec![file("f0", RELAY_CAP as u64 + 8)],
        };
        let relay = Relay::new(&m, &Need::default());
        relay.put(0, 0, &vec![0u8; RELAY_CAP]).unwrap();
        let started = Instant::now();
        let e = relay.put(0, RELAY_CAP as u64, b"more").unwrap_err();
        reset_wait_for_tests();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // C6: what A withholds (skip_set) is exactly what B's sender does not read.
    #[test]
    fn the_relays_expected_bytes_are_exactly_the_destinations_read_set() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        use ava1::ranges::RangeSet;
        use ava1::verify::{self, Outboard, GROUP};
        let size = gen::LARGE_CUTOFF as u64 + 4 * GROUP;
        let small = 1000u64;
        let m = Manifest {
            entries: vec![
                file("done_large", size),
                file("partial_large", size),
                file("partial_small", small),
                file("fresh_large", size),
            ],
        };
        let dir = std::env::temp_dir().join(format!("relay-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The destination: file 0 fully durable, file 1 groups 0 and 1 durable
        // (CV known for 0 only), file 2 (small) partly durable (meaningless: sent whole).
        let mut durable = Need::default();
        durable.done.insert(0);
        let mut r1 = RangeSet::new();
        r1.insert(0, 2 * GROUP);
        durable.partial.insert(1, r1.clone());
        let mut r2 = RangeSet::new();
        r2.insert(0, 500);
        durable.partial.insert(2, r2);
        let groups = verify::groups(size);
        let mut ob = Outboard::open(&dir.join("1.ob"), groups).unwrap();
        ob.put(0, &[7u8; 32]).unwrap();
        ob.sync().unwrap();
        // A done file's outboard is irrelevant to the skip; a missing one is fine.
        let skip = ava1::send::skip_set(&m, &durable, Some(&dir));
        assert!(skip.done.contains(&0));
        let relay = Relay::new(&m, &skip);
        for (id, e) in m.entries.iter().enumerate() {
            let id = id as u32;
            // The destination's read set for this file: every piece (hashed or sent).
            let mut read = RangeSet::new();
            if skip.done.contains(&id) {
                // Never read.
            } else if e.size < gen::LARGE_CUTOFF as u64 {
                read.insert(0, e.size);
            } else {
                let have =
                    Outboard::open(&dir.join(format!("{id}.ob")), verify::groups(e.size)).unwrap();
                let d = durable.partial.get(&id).cloned().unwrap_or_default();
                for p in ava1::send::pieces(e.size, &d, &|g| have.get(g).is_some(), 1 << 20) {
                    read.insert(p.offset, p.offset + p.len);
                }
            }
            let expected = relay.expected.partial.get(&id).cloned().unwrap_or_default();
            assert_eq!(
                expected.iter().collect::<Vec<_>>(),
                read.iter().collect::<Vec<_>>(),
                "file {id}: relay expectation differs from the sender's read set"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A's sink writes run concurrently, so later chunks can fill the buffer while the
    // chunk the destination is parked on is still waiting for room: a deadlock that
    // only the no-progress bound ended (the restart/tree flake under load).
    #[test]
    fn the_chunk_a_reader_waits_for_is_admitted_into_a_full_buffer() {
        let _k = KNOB.lock().unwrap_or_else(|e| e.into_inner());
        set_wait_for_tests(Duration::from_secs(3));
        let m = Manifest {
            entries: vec![file("f0", 4000)],
        };
        let relay = Arc::new(Relay::new(&m, &Need::default()).with_cap(2000));
        // Later chunks fill the buffer first.
        relay.put(0, 2000, &[2u8; 1000]).unwrap();
        relay.put(0, 3000, &[3u8; 1000]).unwrap();
        let r = relay.clone();
        let reader = std::thread::spawn(move || r.take(0, 0));
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        // The chunk the reader needs: no room, but nothing else can ever free any.
        relay
            .put(0, 0, &[1u8; 1000])
            .expect("a wanted chunk must be admitted");
        assert_eq!(reader.join().unwrap().unwrap(), vec![1u8; 1000]);
        assert!(started.elapsed() < Duration::from_secs(2));
        // An unwanted chunk still waits for room (the cap holds).
        set_wait_for_tests(Duration::from_millis(200));
        let e = relay.put(0, 1000, &[9u8; 1000]).unwrap_err();
        reset_wait_for_tests();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_source_is_abandoned_only_when_both_halves_are_complete() {
        assert!(may_abandon_source(true, true));
        assert!(!may_abandon_source(true, false));
        assert!(!may_abandon_source(false, true));
        let (relay, _src) = rig(&[3]);
        assert!(!relay.finished());
        RelaySink(relay.clone()).finish().unwrap();
        assert!(relay.finished());
    }
}
