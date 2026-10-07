//! Sequential (forward-only) sources: 7z folders, solid RAR (SPEC.md §17).
//!
//! A `SeqSource` can only produce its bytes in decode order. The sender runs **one**
//! decode thread over it instead of the random-access readers. The thread maps each
//! decoded entry to its manifest id by path (the manifest is sorted; an archive is
//! not), cuts it into ordinary `Read::Record` / `Read::Chunk` / `Read::Root` messages,
//! and takes the same read-ahead permits the random readers take, so a slow lane stops
//! the decoder (bounded RAM). Resume restarts at the earliest restart point any
//! unfinished file needs and discards what the receiver already has; a `FileRetry` is
//! served by a further pass over only the retried files.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::sync::Semaphore;

use crate::gen;
use crate::manifest::Manifest;
use crate::ranges::{Need, RangeSet};
use crate::send::{pieces, Piece, Read};
use crate::verify::{self, FileHasher, Outboard, GROUP};

/// A decode restart point the source understands (7z: a folder, RAR: an entry). Opaque
/// to the sender except for its order: a smaller restart decodes everything a larger
/// one does. `START` is the beginning of the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct Restart(pub u64);

impl Restart {
    pub const START: Restart = Restart(0);
}

/// What the sender wants of one decoded entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Keep {
    /// Every byte.
    All,
    /// Nothing: decode and discard, or seek past when the format allows.
    Skip,
    /// A large file the receiver partly holds: the byte ranges it lacks. The source
    /// still delivers the whole entry (a decoder cannot seek inside one); the sender
    /// sends only these and hashes the rest when it has no CV for it.
    Ranges(RangeSet),
}

/// Receives one pass's entries. `begin` is only called for entries `want` did not
/// `Skip`; `data` is called with consecutive pieces of the entry and `end` closes it.
/// Any error stops the pass and is returned from it.
pub trait EntrySink {
    fn begin(&mut self, path: &str) -> io::Result<()>;
    fn data(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn end(&mut self) -> io::Result<()>;
}

/// A source that can only be read forward. Its manifest is built from its headers up
/// front; paths are the manifest's ('/'-separated, relative).
pub trait SeqSource: Send + Sync {
    /// One forward pass from `restart`. For every entry in decode order the source
    /// calls `want(path, size)`; `Skip` means do not deliver it, anything else is
    /// delivered through `sink`. Entries before `restart` are not visited. The pass
    /// returns early with an error when `sink` fails or `cancel` is set. `cancel` is
    /// raised for every way the job can end (cancel, lane or receiver failure, teardown),
    /// not only a user cancel. A source MUST poll it at least every 1 MiB of input it
    /// consumes, including while discarding a `Keep::Skip` stretch (which never reaches
    /// the sink), or the job's teardown waits for the stretch to end.
    fn pass(
        &self,
        restart: Restart,
        want: &mut dyn FnMut(&str, u64) -> Keep,
        sink: &mut dyn EntrySink,
        cancel: &AtomicBool,
    ) -> io::Result<()>;
    /// The latest restart point that still decodes the manifest entry `file_id`
    /// (7z: its folder's first entry; RAR non-solid: the entry itself).
    fn restart_for(&self, file_id: u32) -> Restart;
    /// Called once when the upload ends, before the decode thread is joined; wakes a
    /// decoder parked on something other than the sink. Must not block.
    fn close(&self) {}
}

/// A job gets at most this many passes (the first plus retries); a further `FileRetry`
/// fails the job instead of decoding a fourth time.
pub const MAX_PASSES: u32 = 3;

/// Files the receiver reported as needing another pass (`FileRetry`), handed to the
/// decode thread.
#[derive(Default)]
pub(crate) struct Retries {
    q: Mutex<Vec<u32>>,
    cv: Condvar,
}

impl Retries {
    pub(crate) fn push(&self, id: u32) {
        self.q.lock().unwrap().push(id);
        self.cv.notify_all();
    }

    /// The queued ids, waiting briefly when there are none.
    fn take(&self, wait: Duration) -> Vec<u32> {
        let mut g = self.q.lock().unwrap();
        if g.is_empty() {
            g = self.cv.wait_timeout(g, wait).unwrap().0;
        }
        std::mem::take(&mut *g)
    }
}

/// Whether the decode thread has been parked on the read-ahead budget. While it is,
/// lanes that find the queue empty are waiting on the network or the receiver (the
/// permits are held by frames in flight), not on the source (SPEC.md 17.4).
#[derive(Default)]
pub(crate) struct BudgetWait {
    waits: std::sync::atomic::AtomicU64,
    parked: AtomicBool,
}

impl BudgetWait {
    /// True when the decode thread parked since `*seen` (or is parked now); advances `seen`.
    pub(crate) fn waited_since(&self, seen: &mut u64) -> bool {
        let n = self.waits.load(Ordering::Relaxed);
        let waited = n != *seen || self.parked.load(Ordering::Relaxed);
        *seen = n;
        waited
    }
}

pub(crate) struct DecodeCtx {
    pub budget_wait: Arc<BudgetWait>,
    pub seq: Arc<dyn SeqSource>,
    pub manifest: Arc<Manifest>,
    pub need: Need,
    pub cutoff: u64,
    pub persist: Option<PathBuf>,
    pub budget: Arc<Semaphore>,
    /// The current largest chunk of data (already capped by the credit grant).
    pub chunk: Box<dyn Fn() -> u64 + Send>,
    pub tx: mpsc::UnboundedSender<Read>,
    pub stop: Arc<AtomicBool>,
    pub cancel: Arc<AtomicBool>,
    pub retries: Arc<Retries>,
    pub rt: tokio::runtime::Handle,
}

/// Per-file plan for a pass.
struct Want {
    keep: Keep,
    /// Durable ranges (empty unless `keep` is `Ranges`).
    durable: RangeSet,
}

fn gone(c: &DecodeCtx) -> bool {
    c.stop.load(Ordering::Relaxed) || c.cancel.load(Ordering::Relaxed)
}

/// The decode thread's body. Returns when the job ends (`stop`/`cancel`) or fails.
pub(crate) fn run(c: DecodeCtx) {
    // The flag handed to the source: set by `stop` OR `cancel`.
    let abort = Arc::new(AtomicBool::new(false));
    let fin = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (abort, fin, stop, cancel) =
            (abort.clone(), fin.clone(), c.stop.clone(), c.cancel.clone());
        std::thread::spawn(move || {
            while !fin.load(Ordering::Relaxed) {
                if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                    abort.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    run_passes(&c, &abort);
    fin.store(true, Ordering::Relaxed);
    let _ = watcher.join();
}

fn run_passes(c: &DecodeCtx, abort: &AtomicBool) {
    let mut by_path: HashMap<String, u32> = HashMap::new();
    for (i, e) in c.manifest.entries.iter().enumerate() {
        if e.kind == gen::ENTRY_FILE {
            by_path.insert(e.path.clone(), i as u32);
        }
    }
    let by_path = Arc::new(by_path);

    // Pass 1: every file the receiver does not have.
    let mut wants: BTreeMap<u32, Want> = BTreeMap::new();
    for (i, e) in c.manifest.entries.iter().enumerate() {
        let id = i as u32;
        if e.kind != gen::ENTRY_FILE || c.need.done.contains(&id) {
            continue;
        }
        let w = match c.need.partial.get(&id) {
            Some(d) if e.size >= c.cutoff && d.covered() > 0 => {
                let mut lacks = RangeSet::new();
                for (s, t) in d.missing(e.size) {
                    lacks.insert(s, t);
                }
                Want {
                    keep: Keep::Ranges(lacks),
                    durable: d.clone(),
                }
            }
            _ => Want {
                keep: Keep::All,
                durable: RangeSet::new(),
            },
        };
        wants.insert(id, w);
    }
    let mut passes = 0u32;
    loop {
        if !wants.is_empty() {
            match one_pass(c, abort, &by_path, &mut wants) {
                // A pass that had nothing to decode does not count against the cap.
                Ok(ran) => passes += ran as u32,
                Err(e) => {
                    if !gone(c) {
                        let _ = c.tx.send(Read::Failed(e));
                    }
                    return;
                }
            }
        }
        // Wait for a retry (the receiver may report a file that failed verification
        // at any time before the job ends).
        let ids = loop {
            if gone(c) {
                return;
            }
            let ids = c.retries.take(Duration::from_millis(50));
            if !ids.is_empty() {
                break ids;
            }
        };
        if passes >= MAX_PASSES {
            let _ = c.tx.send(Read::Failed(io::Error::other(format!(
                "the source was decoded {MAX_PASSES} times and files still failed verification"
            ))));
            return;
        }
        wants = ids
            .into_iter()
            .map(|id| {
                (
                    id,
                    Want {
                        keep: Keep::All,
                        durable: RangeSet::new(),
                    },
                )
            })
            .collect();
    }
}

fn one_pass(
    c: &DecodeCtx,
    abort: &AtomicBool,
    by_path: &Arc<HashMap<String, u32>>,
    wants: &mut BTreeMap<u32, Want>,
) -> io::Result<bool> {
    // A large file whose every missing group is absent and whose CVs are all known
    // needs no decoding: its root comes from the persisted outboard alone.
    let mut finished: HashSet<u32> = HashSet::new();
    let ids: Vec<u32> = wants.keys().copied().collect();
    for id in ids {
        let e = &c.manifest.entries[id as usize];
        if e.size < c.cutoff || !matches!(wants[&id].keep, Keep::Ranges(_)) {
            continue;
        }
        let hasher = load_hasher(c, id, e.size);
        if pieces(
            e.size,
            &wants[&id].durable,
            &|g| hasher.cv(g).is_some(),
            GROUP,
        )
        .is_empty()
        {
            if let Some(root) = hasher.root() {
                let _ = c.tx.send(Read::Root { file_id: id, root });
                finished.insert(id);
            }
        }
    }
    let todo: Vec<u32> = wants
        .keys()
        .copied()
        .filter(|i| !finished.contains(i))
        .collect();
    if todo.is_empty() {
        return Ok(false);
    }
    let restart = todo
        .iter()
        .map(|i| c.seq.restart_for(*i))
        .min()
        .unwrap_or(Restart::START);
    let mut sink = Feeder {
        c,
        by_path: by_path.clone(),
        wants: &*wants,
        finished: &finished,
        cur: None,
        done: HashSet::new(),
    };
    let wants_ref: &BTreeMap<u32, Want> = &*wants;
    let by = by_path.clone();
    let fin = finished.clone();
    let mut want = move |path: &str, _size: u64| -> Keep {
        match by.get(path) {
            Some(id) if !fin.contains(id) => {
                wants_ref.get(id).map_or(Keep::Skip, |w| w.keep.clone())
            }
            _ => Keep::Skip,
        }
    };
    c.seq.pass(restart, &mut want, &mut sink, abort)?;
    if gone(c) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
    }
    let done = std::mem::take(&mut sink.done);
    for id in &todo {
        if !done.contains(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} was not found in the archive",
                    c.manifest.entries[*id as usize].path
                ),
            ));
        }
    }
    Ok(true)
}

fn load_hasher(c: &DecodeCtx, id: u32, size: u64) -> FileHasher {
    let mut h = FileHasher::new(size);
    if let Some(d) = &c.persist {
        if let Ok(ob) = Outboard::open(&d.join(format!("{id}.ob")), verify::groups(size)) {
            for g in 0..verify::groups(size) {
                if let Some(cv) = ob.get(g) {
                    h.set_cv(g, cv);
                }
            }
        }
    }
    h
}

/// The entry being cut into messages.
struct Cur {
    id: u32,
    size: u64,
    small: Option<(Vec<u8>, tokio::sync::OwnedSemaphorePermit)>,
    plan: Vec<Piece>,
    pi: usize,
    /// Stream offset consumed so far.
    pos: u64,
    buf: Vec<u8>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    hasher: FileHasher,
    ob: Option<Outboard>,
    ob_synced: Instant,
}

struct Feeder<'a> {
    c: &'a DecodeCtx,
    by_path: Arc<HashMap<String, u32>>,
    wants: &'a BTreeMap<u32, Want>,
    finished: &'a HashSet<u32>,
    cur: Option<Cur>,
    done: HashSet<u32>,
}

fn stopped() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "stopped")
}

impl Feeder<'_> {
    fn acquire(&self, kib: u64) -> io::Result<tokio::sync::OwnedSemaphorePermit> {
        let kib = kib.clamp(1, u32::MAX as u64) as u32;
        match self.c.budget.clone().try_acquire_many_owned(kib) {
            Ok(p) => return Ok(p),
            Err(tokio::sync::TryAcquireError::Closed) => return Err(stopped()),
            Err(tokio::sync::TryAcquireError::NoPermits) => {}
        }
        // Parked on the budget: the lanes, not the source, are the limit.
        let w = &self.c.budget_wait;
        w.waits.fetch_add(1, Ordering::Relaxed);
        w.parked.store(true, Ordering::Relaxed);
        let r = self
            .c
            .rt
            .block_on(self.c.budget.clone().acquire_many_owned(kib))
            .map_err(|_| stopped());
        w.parked.store(false, Ordering::Relaxed);
        r
    }

    fn changed(&self, id: u32) -> io::Error {
        io::Error::other(format!(
            "{} changed while it was being sent",
            self.c.manifest.entries[id as usize].path
        ))
    }

    fn finish_piece(&mut self) -> io::Result<()> {
        let id = self.cur.as_ref().expect("an entry is open").id;
        let cur = self.cur.as_mut().expect("an entry is open");
        let p = cur.plan[cur.pi];
        let data = std::mem::take(&mut cur.buf);
        for (k, g) in data.chunks(GROUP as usize).enumerate() {
            let gi = p.offset / GROUP + k as u64;
            cur.hasher.add_group(gi, g);
            if let (Some(ob), Some(cv)) = (cur.ob.as_mut(), cur.hasher.cv(gi)) {
                let _ = ob.put(gi, &cv);
            }
        }
        // Like the random reader: persist about once a second and at the last piece.
        if cur.pi + 1 == cur.plan.len() || cur.ob_synced.elapsed() >= Duration::from_secs(1) {
            if let Some(ob) = cur.ob.as_mut() {
                let _ = ob.sync();
            }
            cur.ob_synced = Instant::now();
        }
        if p.send {
            let budget = cur.permit.take().expect("send pieces hold a permit");
            let _ = self.c.tx.send(Read::Chunk {
                file_id: id,
                offset: p.offset,
                data,
                budget,
            });
        }
        cur.pi += 1;
        Ok(())
    }
}

impl EntrySink for Feeder<'_> {
    fn begin(&mut self, path: &str) -> io::Result<()> {
        if gone(self.c) {
            return Err(stopped());
        }
        let Some(&id) = self.by_path.get(path) else {
            return Err(io::Error::other(format!("unexpected entry {path}")));
        };
        let Some(w) = self.wants.get(&id).filter(|_| !self.finished.contains(&id)) else {
            return Err(io::Error::other(format!(
                "{path} was delivered but not wanted"
            )));
        };
        if self.cur.is_some() || self.done.contains(&id) {
            return Err(io::Error::other(format!(
                "{path} appears twice in the archive"
            )));
        }
        let size = self.c.manifest.entries[id as usize].size;
        if size < self.c.cutoff {
            let permit = self.acquire(size / 1024 + 1)?;
            self.cur = Some(Cur {
                id,
                size,
                small: Some((Vec::with_capacity(size as usize), permit)),
                plan: Vec::new(),
                pi: 0,
                pos: 0,
                buf: Vec::new(),
                permit: None,
                hasher: FileHasher::new(0),
                ob: None,
                ob_synced: Instant::now(),
            });
            return Ok(());
        }
        let hasher = load_hasher(self.c, id, size);
        let ob = self.c.persist.as_ref().and_then(|d| {
            std::fs::create_dir_all(d).ok()?;
            Outboard::open(&d.join(format!("{id}.ob")), verify::groups(size)).ok()
        });
        let chunk = ((self.c.chunk)() / GROUP * GROUP).max(GROUP);
        let plan = pieces(size, &w.durable, &|g| hasher.cv(g).is_some(), chunk);
        self.cur = Some(Cur {
            id,
            size,
            small: None,
            plan,
            pi: 0,
            pos: 0,
            buf: Vec::new(),
            permit: None,
            hasher,
            ob,
            ob_synced: Instant::now(),
        });
        Ok(())
    }

    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        if gone(self.c) {
            return Err(stopped());
        }
        let Some(cur) = self.cur.as_mut() else {
            return Err(io::Error::other("data outside an entry"));
        };
        if cur.pos + bytes.len() as u64 > cur.size {
            let id = cur.id;
            return Err(self.changed(id));
        }
        if let Some((buf, _)) = cur.small.as_mut() {
            buf.extend_from_slice(bytes);
            cur.pos += bytes.len() as u64;
            return Ok(());
        }
        let mut b = bytes;
        while !b.is_empty() {
            let cur = self.cur.as_mut().expect("an entry is open");
            let Some(p) = cur.plan.get(cur.pi).copied() else {
                cur.pos += b.len() as u64;
                break;
            };
            if cur.pos < p.offset {
                let n = ((p.offset - cur.pos) as usize).min(b.len());
                cur.pos += n as u64;
                b = &b[n..];
                continue;
            }
            if cur.buf.is_empty() && p.send && cur.permit.is_none() {
                let permit = self.acquire(p.len / 1024 + 1)?;
                self.cur.as_mut().expect("an entry is open").permit = Some(permit);
            }
            let cur = self.cur.as_mut().expect("an entry is open");
            if cur.buf.is_empty() {
                cur.buf.reserve(p.len as usize);
            }
            let n = ((p.len as usize) - cur.buf.len()).min(b.len());
            cur.buf.extend_from_slice(&b[..n]);
            cur.pos += n as u64;
            b = &b[n..];
            if cur.buf.len() as u64 == p.len {
                self.finish_piece()?;
            }
        }
        Ok(())
    }

    fn end(&mut self) -> io::Result<()> {
        let Some(cur) = self.cur.take() else {
            return Err(io::Error::other("end outside an entry"));
        };
        if cur.pos != cur.size {
            return Err(self.changed(cur.id));
        }
        if let Some((data, budget)) = cur.small {
            let root = *blake3::hash(&data).as_bytes();
            let _ = self.c.tx.send(Read::Record {
                file_id: cur.id,
                root,
                data,
                budget,
            });
        } else {
            if cur.pi != cur.plan.len() {
                return Err(self.changed(cur.id));
            }
            let Some(root) = cur.hasher.root() else {
                return Err(self.changed(cur.id));
            };
            let _ = self.c.tx.send(Read::Root {
                file_id: cur.id,
                root,
            });
        }
        self.done.insert(cur.id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Entry;
    use std::sync::atomic::AtomicUsize;

    const CUT: u64 = 4096;

    /// An in-memory archive: entries in decode order, folders of `folder` entries.
    struct MemSeq {
        entries: Vec<(String, Vec<u8>)>,
        folder: usize,
        /// manifest id -> decode index
        index: HashMap<u32, usize>,
        restarts: Mutex<Vec<Restart>>,
        wants: Mutex<Vec<(String, Keep)>>,
        delivered: AtomicUsize,
        bytes_out: AtomicUsize,
        slice: usize,
    }

    impl SeqSource for MemSeq {
        fn pass(
            &self,
            restart: Restart,
            want: &mut dyn FnMut(&str, u64) -> Keep,
            sink: &mut dyn EntrySink,
            cancel: &AtomicBool,
        ) -> io::Result<()> {
            self.restarts.lock().unwrap().push(restart);
            for (path, data) in self.entries.iter().skip(restart.0 as usize) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(stopped());
                }
                let k = want(path, data.len() as u64);
                self.wants.lock().unwrap().push((path.clone(), k.clone()));
                if k == Keep::Skip {
                    continue;
                }
                sink.begin(path)?;
                for c in data.chunks(self.slice) {
                    sink.data(c)?;
                    self.bytes_out.fetch_add(c.len(), Ordering::SeqCst);
                }
                sink.end()?;
                self.delivered.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        }
        fn restart_for(&self, id: u32) -> Restart {
            let i = self.index[&id];
            Restart((i - i % self.folder) as u64)
        }
    }

    fn bytes(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect()
    }

    /// A sorted manifest over `decode` (an unsorted list) and the source for it.
    fn fixture(decode: Vec<(&str, Vec<u8>)>, folder: usize) -> (Arc<Manifest>, Arc<MemSeq>) {
        let mut sorted: Vec<(String, usize)> = decode
            .iter()
            .map(|(p, d)| (p.to_string(), d.len()))
            .collect();
        sorted.sort();
        let m = Manifest {
            entries: sorted
                .iter()
                .map(|(p, n)| Entry {
                    kind: gen::ENTRY_FILE,
                    mode: 0o644,
                    size: *n as u64,
                    mtime: 0,
                    path: p.clone(),
                    root: None,
                })
                .collect(),
        };
        let mut index = HashMap::new();
        for (i, e) in m.entries.iter().enumerate() {
            index.insert(
                i as u32,
                decode.iter().position(|(p, _)| *p == e.path).unwrap(),
            );
        }
        let s = MemSeq {
            entries: decode
                .into_iter()
                .map(|(p, d)| (p.to_string(), d))
                .collect(),
            folder,
            index,
            restarts: Mutex::default(),
            wants: Mutex::default(),
            delivered: AtomicUsize::new(0),
            bytes_out: AtomicUsize::new(0),
            slice: 1000,
        };
        (Arc::new(m), Arc::new(s))
    }

    struct Run {
        budget: Arc<Semaphore>,
        bw: Arc<BudgetWait>,
        rx: mpsc::UnboundedReceiver<Read>,
        retries: Arc<Retries>,
        stop: Arc<AtomicBool>,
        cancel: Arc<AtomicBool>,
        h: std::thread::JoinHandle<()>,
    }

    fn start(
        m: &Arc<Manifest>,
        s: &Arc<MemSeq>,
        need: Need,
        persist: Option<PathBuf>,
        budget_kib: usize,
        chunk: u64,
    ) -> Run {
        let (tx, rx) = mpsc::unbounded_channel();
        let retries = Arc::new(Retries::default());
        let (stop, cancel) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let bw = Arc::new(BudgetWait::default());
        let budget = Arc::new(Semaphore::new(budget_kib));
        let ctx = DecodeCtx {
            seq: s.clone(),
            manifest: m.clone(),
            need,
            cutoff: CUT,
            persist,
            budget: budget.clone(),
            budget_wait: bw.clone(),
            chunk: Box::new(move || chunk),
            tx,
            stop: stop.clone(),
            cancel: cancel.clone(),
            retries: retries.clone(),
            rt: tokio::runtime::Handle::current(),
        };
        let h = std::thread::spawn(move || run(ctx));
        Run {
            budget,
            bw,
            rx,
            retries,
            stop,
            cancel,
            h,
        }
    }

    impl Run {
        async fn next(&mut self) -> Read {
            tokio::time::timeout(Duration::from_secs(10), self.rx.recv())
                .await
                .expect("no message within 10 s")
                .expect("channel closed")
        }
        fn end(self) {
            self.stop.store(true, Ordering::Relaxed);
            self.budget.close(); // the sender's teardown does the same, waking a parked decoder
            self.h.join().unwrap();
        }
    }

    fn tag(r: &Read) -> String {
        match r {
            Read::Record { file_id, .. } => format!("rec{file_id}"),
            Read::Chunk {
                file_id, offset, ..
            } => format!("chunk{file_id}@{offset}"),
            Read::Root { file_id, .. } => format!("root{file_id}"),
            Read::Failed(e) => format!("failed {e}"),
        }
    }

    fn big() -> Vec<u8> {
        bytes(2 * GROUP as usize + 5000, 7)
    }

    fn root_of(d: &[u8]) -> [u8; 32] {
        let mut h = FileHasher::new(d.len() as u64);
        for (i, g) in d.chunks(GROUP as usize).enumerate() {
            h.add_group(i as u64, g);
        }
        h.root().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_pass_feeds_records_and_chunks_in_decode_order() {
        let big = big();
        // Decode order b, big, a; the manifest is sorted a, b, big.
        let (m, s) = fixture(
            vec![
                ("b", bytes(10, 1)),
                ("big", big.clone()),
                ("a", bytes(20, 2)),
            ],
            10,
        );
        let mut r = start(&m, &s, Need::default(), None, 96 * 1024, 4 * GROUP);
        let mut got = Vec::new();
        let mut data = Vec::new();
        let mut roots = HashMap::new();
        loop {
            let x = r.next().await;
            got.push(tag(&x));
            match x {
                Read::Chunk { data: d, .. } => data.extend(d),
                Read::Root { file_id, root } => {
                    roots.insert(file_id, root);
                }
                Read::Record {
                    file_id,
                    root,
                    data,
                    ..
                } => {
                    assert_eq!(root, *blake3::hash(&data).as_bytes(), "id {file_id}");
                }
                Read::Failed(e) => panic!("{e}"),
            }
            if got.len() == 4 {
                break;
            }
        }
        // ids: a=0, b=1, big=2. One chunk (the file is under 4 groups) after b's record.
        assert_eq!(got, ["rec1", "chunk2@0", "root2", "rec0"]);
        assert_eq!(data, big);
        assert_eq!(roots[&2], root_of(&big));
        r.end();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_manifest_order_differs_from_decode_order() {
        let (m, s) = fixture(
            vec![("z", bytes(5, 1)), ("m", bytes(6, 2)), ("a", bytes(7, 3))],
            10,
        );
        let mut r = start(&m, &s, Need::default(), None, 96 * 1024, GROUP);
        let mut ids = Vec::new();
        for _ in 0..3 {
            match r.next().await {
                Read::Record { file_id, data, .. } => {
                    assert_eq!(data.len() as u64, m.entries[file_id as usize].size);
                    ids.push(file_id);
                }
                x => panic!("{}", tag(&x)),
            }
        }
        assert_eq!(ids, [2, 1, 0], "z=2, m=1, a=0 in decode order");
        r.end();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_resume_starts_at_the_restart_point_and_skips_done_files() {
        // Decode order: p q | r s | t u (folders of two). Done: everything but s and u.
        let names = ["p", "q", "r", "s", "t", "u"];
        let (m, s) = fixture(
            names
                .iter()
                .map(|n| (*n, bytes(8, n.as_bytes()[0])))
                .collect(),
            2,
        );
        let mut need = Need::default();
        for n in ["p", "q", "r", "t"] {
            need.done
                .insert(m.entries.iter().position(|e| e.path == n).unwrap() as u32);
        }
        let mut r = start(&m, &s, need, None, 96 * 1024, GROUP);
        let mut paths = Vec::new();
        for _ in 0..2 {
            match r.next().await {
                Read::Record { file_id, .. } => {
                    paths.push(m.entries[file_id as usize].path.clone())
                }
                x => panic!("{}", tag(&x)),
            }
        }
        assert_eq!(paths, ["s", "u"]);
        // The earliest unfinished file is s, in the folder starting at decode index 2.
        assert_eq!(s.restarts.lock().unwrap()[0], Restart(2));
        let kept: Vec<String> = s
            .wants
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, k)| *k != Keep::Skip)
            .map(|(p, _)| p.clone())
            .collect();
        assert_eq!(kept, ["s", "u"]);
        // r (done, in s's folder) was offered and skipped; p and q were never visited.
        assert!(!s
            .wants
            .lock()
            .unwrap()
            .iter()
            .any(|(p, _)| p == "p" || p == "q"));
        // Receiving the last record can race the source's post-send accounting.
        r.end();
        assert_eq!(s.delivered.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_partial_large_file_sends_only_missing_groups() {
        let big = big(); // 3 groups: 0 and 1 full, 2 short
        let (m, s) = fixture(vec![("big", big.clone())], 1);
        let dir = std::env::temp_dir().join(format!("seq-partial-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The receiver holds groups 0 and 1; the outboard knows group 0's CV only.
        let mut ob = Outboard::open(&dir.join("0.ob"), 3).unwrap();
        ob.put(0, &verify::group_cv(&big[..GROUP as usize], 0))
            .unwrap();
        ob.sync().unwrap();
        let mut need = Need::default();
        let mut d = RangeSet::new();
        d.insert(0, 2 * GROUP);
        need.partial.insert(0, d);
        let mut r = start(&m, &s, need, Some(dir.clone()), 96 * 1024, GROUP);
        let mut sent = Vec::new();
        let root;
        loop {
            match r.next().await {
                Read::Chunk { offset, data, .. } => sent.push((offset, data.len())),
                Read::Root { root: x, .. } => {
                    root = x;
                    break;
                }
                x => panic!("{}", tag(&x)),
            }
        }
        // Only group 2 is sent; group 1 was decoded and hashed (no CV), group 0 was not hashed.
        assert_eq!(sent, [(2 * GROUP, 5000)]);
        assert_eq!(root, root_of(&big));
        let wants = s.wants.lock().unwrap();
        assert!(
            matches!(&wants[0].1, Keep::Ranges(r) if r.covers(2 * GROUP, 2 * GROUP + 5000) && !r.covers(0, 1))
        );
        drop(wants);
        r.end();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_file_with_every_group_known_needs_no_decode() {
        let big = big();
        let (m, s) = fixture(vec![("big", big.clone())], 1);
        let dir = std::env::temp_dir().join(format!("seq-known-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ob = Outboard::open(&dir.join("0.ob"), 3).unwrap();
        for (i, g) in big.chunks(GROUP as usize).enumerate() {
            ob.put(i as u64, &verify::group_cv(g, i as u64)).unwrap();
        }
        ob.sync().unwrap();
        let mut need = Need::default();
        let mut d = RangeSet::new();
        d.insert(0, big.len() as u64);
        need.partial.insert(0, d);
        let mut r = start(&m, &s, need, Some(dir.clone()), 96 * 1024, GROUP);
        match r.next().await {
            Read::Root { root, .. } => assert_eq!(root, root_of(&big)),
            x => panic!("{}", tag(&x)),
        }
        assert!(s.restarts.lock().unwrap().is_empty(), "no pass was needed");
        r.end();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_retry_runs_a_second_pass_for_the_retried_file_only() {
        let (m, s) = fixture(
            vec![
                ("a", bytes(9, 1)),
                ("b", bytes(9, 2)),
                ("c", bytes(9, 3)),
                ("d", bytes(9, 4)),
            ],
            2,
        );
        let mut r = start(&m, &s, Need::default(), None, 96 * 1024, GROUP);
        for _ in 0..4 {
            assert!(matches!(r.next().await, Read::Record { .. }));
        }
        r.retries.push(3); // d, in the folder starting at decode index 2
        match r.next().await {
            Read::Record { file_id, .. } => assert_eq!(file_id, 3),
            x => panic!("{}", tag(&x)),
        }
        assert_eq!(*s.restarts.lock().unwrap(), [Restart(0), Restart(2)]);
        let second: Vec<_> = s.wants.lock().unwrap().iter().skip(4).cloned().collect();
        assert_eq!(
            second,
            [("c".to_string(), Keep::Skip), ("d".to_string(), Keep::All)]
        );
        // The job's third pass is its last: a further retry fails it.
        r.retries.push(3);
        assert!(matches!(r.next().await, Read::Record { .. }));
        r.retries.push(3);
        match r.next().await {
            Read::Failed(e) => assert!(e.to_string().contains("3 times"), "{e}"),
            x => panic!("{}", tag(&x)),
        }
        r.end();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_decoder_blocks_on_the_read_ahead_budget() {
        // 6 groups, chunk = 1 group; a budget that holds two of them.
        let big = bytes(6 * GROUP as usize, 9);
        let (m, s) = fixture(vec![("big", big)], 1);
        let kib = 2 * (GROUP as usize / 1024 + 1);
        let mut r = start(&m, &s, Need::default(), None, kib, GROUP);
        let mut held = Vec::new();
        for _ in 0..2 {
            match r.next().await {
                Read::Chunk { budget, .. } => held.push(budget),
                x => panic!("{}", tag(&x)),
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        // The decoder is parked: it produced at most the held chunks plus the slice
        // that tried to start the next one, and nothing more arrived.
        assert!(r.rx.try_recv().is_err());
        let out = s.bytes_out.load(Ordering::SeqCst);
        assert!(
            out <= 3 * GROUP as usize,
            "decoder ran ahead of the budget: {out}"
        );
        drop(held);
        let mut chunks = 2;
        loop {
            match r.next().await {
                Read::Chunk { budget, .. } => {
                    chunks += 1;
                    drop(budget);
                }
                Read::Root { .. } => break,
                x => panic!("{}", tag(&x)),
            }
        }
        assert_eq!(chunks, 6);
        r.end();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_cancel_stops_the_pass() {
        let entries: Vec<(String, Vec<u8>)> = (0..200)
            .map(|i| (format!("f{i:03}"), bytes(2000, i as u8)))
            .collect();
        let (m, s) = fixture(
            entries
                .iter()
                .map(|(p, d)| (p.as_str(), d.clone()))
                .collect(),
            10,
        );
        let mut r = start(&m, &s, Need::default(), None, 96 * 1024, GROUP);
        assert!(matches!(r.next().await, Read::Record { .. }));
        r.cancel.store(true, Ordering::Relaxed);
        let h = std::mem::replace(&mut r.h, std::thread::spawn(|| {}));
        tokio::task::spawn_blocking(move || h.join().unwrap())
            .await
            .unwrap();
        assert!(
            s.delivered.load(Ordering::SeqCst) < 200,
            "the pass ran to the end"
        );
        // A cancelled job reports no failure.
        while let Ok(x) = r.rx.try_recv() {
            assert!(!matches!(x, Read::Failed(_)));
        }
    }

    /// Skips a huge stretch without ever calling the sink, polling the flag.
    struct SkipForever;
    impl SeqSource for SkipForever {
        fn pass(
            &self,
            _r: Restart,
            _w: &mut dyn FnMut(&str, u64) -> Keep,
            _s: &mut dyn EntrySink,
            cancel: &AtomicBool,
        ) -> io::Result<()> {
            for _ in 0..2000 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(stopped());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        }
        fn restart_for(&self, _id: u32) -> Restart {
            Restart::START
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_decoder_parked_on_the_budget_reports_it() {
        let (m, s) = fixture(vec![("big", big())], 1);
        // One chunk of budget: holding the first message leaves the decoder parked.
        let r = start(
            &m,
            &s,
            Need::default(),
            None,
            (GROUP / 1024) as usize + 1,
            GROUP,
        );
        let mut seen = 0;
        let t = Instant::now();
        while !r.bw.waited_since(&mut seen) {
            assert!(t.elapsed() < Duration::from_secs(10), "never parked");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        r.end();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_stop_reaches_a_source_that_is_skipping() {
        let (m, _s) = fixture(vec![("a", bytes(9, 1))], 1);
        let (tx, _rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = DecodeCtx {
            seq: Arc::new(SkipForever),
            manifest: m.clone(),
            need: Need::default(),
            cutoff: CUT,
            persist: None,
            budget: Arc::new(Semaphore::new(1024)),
            budget_wait: Arc::default(),
            chunk: Box::new(|| GROUP),
            tx,
            stop: stop.clone(),
            cancel: Arc::default(),
            retries: Arc::default(),
            rt: tokio::runtime::Handle::current(),
        };
        let h = std::thread::spawn(move || run(ctx));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let t = Instant::now();
        stop.store(true, Ordering::Relaxed); // a lane failure ends the job: stop, not cancel
        tokio::task::spawn_blocking(move || h.join().unwrap())
            .await
            .unwrap();
        assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_a_pass_with_nothing_to_do_does_not_count() {
        // Every group of the only file is known: the first pass decodes nothing, so
        // three retry passes are still allowed and the fourth request fails.
        let big = big();
        let (m, s) = fixture(vec![("big", big.clone())], 1);
        let dir = std::env::temp_dir().join(format!("seq-nocount-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut ob = Outboard::open(&dir.join("0.ob"), 3).unwrap();
        for (i, g) in big.chunks(GROUP as usize).enumerate() {
            ob.put(i as u64, &verify::group_cv(g, i as u64)).unwrap();
        }
        ob.sync().unwrap();
        let mut need = Need::default();
        let mut d = RangeSet::new();
        d.insert(0, big.len() as u64);
        need.partial.insert(0, d);
        let mut r = start(&m, &s, need, Some(dir.clone()), 96 * 1024, 8 * GROUP);
        assert!(matches!(r.next().await, Read::Root { .. }));
        for _ in 0..3 {
            r.retries.push(0);
            let _ = std::fs::remove_file(dir.join("0.ob"));
            assert!(matches!(r.next().await, Read::Chunk { .. }));
            assert!(matches!(r.next().await, Read::Root { .. }));
        }
        r.retries.push(0);
        assert!(matches!(r.next().await, Read::Failed(_)));
        r.end();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn seq_a_missing_entry_fails_the_job() {
        let (m, s) = fixture(vec![("a", bytes(9, 1)), ("b", bytes(9, 2))], 2);
        // The archive lost b between listing and sending.
        let s2 = Arc::new(MemSeq {
            entries: s.entries[..1].to_vec(),
            folder: 2,
            index: s.index.clone(),
            restarts: Mutex::default(),
            wants: Mutex::default(),
            delivered: AtomicUsize::new(0),
            bytes_out: AtomicUsize::new(0),
            slice: 1000,
        });
        let mut r = start(&m, &s2, Need::default(), None, 96 * 1024, GROUP);
        assert!(matches!(r.next().await, Read::Record { .. }));
        match r.next().await {
            Read::Failed(e) => assert!(e.to_string().contains("not found"), "{e}"),
            x => panic!("{}", tag(&x)),
        }
        r.end();
    }
}

#[cfg(test)]
mod budget_wait_tests {
    use super::*;

    #[test]
    fn a_parked_decode_thread_is_seen_by_the_governor_tick() {
        let w = BudgetWait::default();
        let mut seen = 0;
        assert!(
            !w.waited_since(&mut seen),
            "no wait yet: the source is the limit"
        );
        w.waits.fetch_add(1, Ordering::Relaxed);
        assert!(
            w.waited_since(&mut seen),
            "a wait began since the last tick"
        );
        assert!(!w.waited_since(&mut seen), "and is consumed");
        w.parked.store(true, Ordering::Relaxed);
        assert!(w.waited_since(&mut seen), "still parked across ticks");
    }
}
