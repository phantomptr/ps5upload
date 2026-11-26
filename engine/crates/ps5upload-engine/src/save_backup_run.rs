//! What a save backup run is doing and did, kept on the engine as it goes (#895): each save (and
//! each chosen console folder, #899) with its state in every place it goes to (the network share,
//! the PS5 drive, the backup folder on this computer), why one failed, and a short history of runs.
//!
//! The record is written to the engine's state folder at every change of state (not at every
//! percent), so it outlives a closed app, an engine restart, a PS5 that crashed and a lost
//! connection. A run the engine was in the middle of when it stopped reads back as
//! "interrupted": what it had verified stays verified, what it was copying is waiting again, and
//! the copy it was writing (named in the record) is removed before it is made again.
//!
//! One record per kind of backup: the network share's (`share`) and the automatic backups to a
//! folder here (`local`). Each holds its last run whole and the last few runs in short.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::save_backup::now_ms;

/// Runs kept in short in the history, newest first.
pub const HISTORY: usize = 20;
const FILE: &str = "save-backup-runs.json";

/// Which backup a run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Which {
    /// Backups to a network share (`save_backup_server`).
    Share,
    /// The automatic backups to a folder on this computer (`save_backup`).
    Local,
}

impl Which {
    pub fn parse(s: &str) -> Option<Which> {
        match s.trim() {
            "share" | "server" => Some(Which::Share),
            "local" | "auto" => Some(Which::Local),
            _ => None,
        }
    }
}

/// A place a backup goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    /// The folder on the network share.
    Share,
    /// `ps5upload/save-backups` on a drive in the PS5.
    Drive,
    /// The backup folder on this computer.
    Local,
}

/// Where one save (or one place of it) stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    #[default]
    Waiting,
    /// Being copied off the PS5.
    Reading,
    /// Being written to the place.
    Copying,
    /// Being read back and compared.
    Checking,
    Verified,
    Failed,
    /// Not made there, and not an error (a PS5 drive that is not plugged in).
    Skipped,
}

impl Step {
    fn busy(self) -> bool {
        matches!(self, Step::Reading | Step::Copying | Step::Checking)
    }
}

/// One save's state in one place.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DestStatus {
    pub step: Step,
    /// While copying: how far, 0 to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    /// Why it failed or was skipped.
    pub error: Option<String>,
    /// The copy in that place (being written, or verified): one left unverified by a run that
    /// stopped is removed before the save is copied again.
    pub file: Option<String>,
    /// Tries so far.
    pub tries: u32,
}

/// One save (or console folder) of a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Item {
    /// `<console>|<save path>` (the key of the done records), `<console>|folder:<path>` for a
    /// folder.
    pub id: String,
    pub host: String,
    /// The console's save list `kind` (`ps5`, `ps4`, `switch`, `ps2`, `ps1`), or `folder`.
    pub kind: String,
    pub title_id: String,
    pub user_id: u32,
    pub path: String,
    /// Bytes, as the console lists the save (an estimate of the copy's size; 0 when unknown).
    pub size: u64,
    pub mtime: i64,
    pub step: Step,
    /// Why it could not be read off the PS5, or why it failed.
    pub error: Option<String>,
    /// Reading tries so far.
    pub tries: u32,
    /// The places it goes to this run, each with its state.
    pub dests: BTreeMap<Place, DestStatus>,
}

/// How a save ended up in a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Verified in every place it went to (or skipped in some).
    Verified,
    Failed,
    /// Skipped in every place.
    Skipped,
    /// Not finished: waiting, or stopped part way.
    Left,
}

impl Item {
    pub fn outcome(&self) -> Outcome {
        if self.step == Step::Failed || self.dests.values().any(|d| d.step == Step::Failed) {
            return Outcome::Failed;
        }
        if self.dests.is_empty() {
            return if self.step == Step::Verified {
                Outcome::Verified
            } else {
                Outcome::Left
            };
        }
        if self
            .dests
            .values()
            .all(|d| matches!(d.step, Step::Verified | Step::Skipped))
        {
            if self.dests.values().all(|d| d.step == Step::Skipped) {
                Outcome::Skipped
            } else {
                Outcome::Verified
            }
        } else {
            Outcome::Left
        }
    }

    /// The places not done yet: not verified there (skipped counts as done).
    pub fn left_places(&self) -> Vec<Place> {
        self.dests
            .iter()
            .filter(|(_, d)| !matches!(d.step, Step::Verified | Step::Skipped))
            .map(|(p, _)| *p)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Running,
    /// Went through every save (some may have failed).
    Done,
    /// Stopped before the end: the PS5 stopped answering. `Continue` does what is left.
    Stopped,
    Cancelled,
    /// The engine stopped while it ran (closed, restarted, crashed).
    Interrupted,
}

/// One run: what started it, every save in it, and how it ended.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Run {
    pub id: String,
    pub which: Option<Which>,
    /// `manual` (Back up now), `interval`, `closed` (a game was closed), `continue`, `retry`.
    pub trigger: String,
    pub started_at: u64,
    /// Unix ms of the last change (how far an interrupted run got).
    pub updated_at: u64,
    /// 0 while it runs.
    pub ended_at: u64,
    pub state: RunState,
    /// Why it stopped: `console_gone` (the PS5 stopped answering), `interrupted` (the engine
    /// stopped), `cancelled`; None when it went through.
    pub reason_code: Option<String>,
    /// The same in words (the console's error, for one).
    pub reason: Option<String>,
    pub items: Vec<Item>,
    /// Consoles that could not be looked at, with why.
    pub unreached: BTreeMap<String, String>,
}

/// How many saves each place verified, failed and has left.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaceCount {
    pub verified: u32,
    pub failed: u32,
    pub skipped: u32,
    pub left: u32,
    /// The first failure there, in words.
    pub first_error: Option<String>,
}

/// A run in short, for the history.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub id: String,
    pub trigger: String,
    pub started_at: u64,
    pub ended_at: u64,
    pub state: RunState,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
    pub total: u32,
    pub verified: u32,
    pub failed: u32,
    pub skipped: u32,
    pub left: u32,
    /// Saves that could not be read off the PS5 (they reached no place).
    pub read_failed: u32,
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// The first failure, in words.
    pub first_error: Option<String>,
    /// Each place on its own.
    pub places: BTreeMap<Place, PlaceCount>,
}

impl Run {
    pub fn summary(&self) -> Summary {
        let mut s = Summary {
            id: self.id.clone(),
            trigger: self.trigger.clone(),
            started_at: self.started_at,
            ended_at: self.ended_at,
            state: self.state,
            reason_code: self.reason_code.clone(),
            reason: self.reason.clone(),
            ..Summary::default()
        };
        for it in &self.items {
            s.total += 1;
            s.bytes_total += it.size;
            if it.step == Step::Failed && it.error.is_some() {
                s.read_failed += 1;
            }
            for (p, d) in &it.dests {
                let c = s.places.entry(*p).or_default();
                match d.step {
                    Step::Verified => c.verified += 1,
                    Step::Skipped => c.skipped += 1,
                    Step::Failed => {
                        c.failed += 1;
                        if c.first_error.is_none() {
                            c.first_error =
                                d.error.as_ref().map(|e| format!("{}: {e}", it.title_id));
                        }
                    }
                    _ => c.left += 1,
                }
            }
            match it.outcome() {
                Outcome::Verified => {
                    s.verified += 1;
                    s.bytes_done += it.size;
                }
                Outcome::Failed => {
                    s.failed += 1;
                    if s.first_error.is_none() {
                        s.first_error = first_error(it);
                    }
                }
                Outcome::Skipped => s.skipped += 1,
                Outcome::Left => s.left += 1,
            }
        }
        s
    }

    fn interrupt(&mut self, reason: &str) {
        self.state = RunState::Interrupted;
        self.reason_code = Some("interrupted".into());
        self.reason = Some(reason.to_string());
        self.ended_at = self.updated_at.max(self.started_at);
        for it in &mut self.items {
            if it.step.busy() {
                it.step = Step::Waiting;
            }
            for d in it.dests.values_mut() {
                if d.step.busy() {
                    d.step = Step::Waiting;
                    d.pct = None;
                }
            }
        }
    }
}

/// `<title>: <why>` of an item's first failure.
fn first_error(it: &Item) -> Option<String> {
    let why = it.error.clone().or_else(|| {
        it.dests
            .values()
            .find(|d| d.step == Step::Failed)
            .and_then(|d| d.error.clone())
    })?;
    Some(format!("{}: {why}", it.title_id))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Side {
    last: Option<Run>,
    history: Vec<Summary>,
    /// The run before the one running (kept to put back when that one is discarded).
    #[serde(skip)]
    prev: Option<Run>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Book {
    share: Side,
    local: Side,
}

impl Book {
    fn side(&mut self, w: Which) -> &mut Side {
        match w {
            Which::Share => &mut self.share,
            Which::Local => &mut self.local,
        }
    }
    fn side_ref(&self, w: Which) -> &Side {
        match w {
            Which::Share => &self.share,
            Which::Local => &self.local,
        }
    }
}

/// The record of runs, and how to stop the one running.
pub struct Journal {
    path: Option<PathBuf>,
    book: Mutex<Book>,
    cancel_share: Arc<AtomicBool>,
    cancel_local: Arc<AtomicBool>,
    /// The copy going now, per kind: the save, the place, its bytes written so far and its size.
    live: Mutex<BTreeMap<Which, Live>>,
    /// When the record was last written: changes in between wait (at most `WRITE_EVERY`), so a
    /// run of many small saves does not write the file a few times a second.
    written: Mutex<Option<std::time::Instant>>,
}

/// How often a running run's record is written at most; a run's start and end always are.
const WRITE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

type Live = (usize, Place, Arc<AtomicU64>, u64);

/// Keeps two runs begun in the same millisecond apart.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// What a run read back as when the engine stopped in the middle of it.
pub const INTERRUPTED: &str = "the engine stopped while the backup ran";

impl Journal {
    /// The record kept in the file `path` (None: in memory only). A run that was running when
    /// the file was last written is read back as interrupted.
    pub fn open(path: Option<PathBuf>) -> Journal {
        let mut book: Book = path
            .as_deref()
            .map(crate::save_backup::load_from)
            .unwrap_or_default();
        let mut changed = false;
        for w in [Which::Share, Which::Local] {
            let side = book.side(w);
            if let Some(run) = side.last.as_mut().filter(|r| r.state == RunState::Running) {
                run.interrupt(INTERRUPTED);
                let s = run.summary();
                side.history.retain(|h| h.id != s.id);
                side.history.insert(0, s);
                side.history.truncate(HISTORY);
                changed = true;
            }
        }
        let j = Journal {
            path,
            book: Mutex::new(book),
            cancel_share: Arc::default(),
            cancel_local: Arc::default(),
            live: Mutex::default(),
            written: Mutex::default(),
        };
        if changed {
            j.persist(&j.lock());
        }
        j
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self, b: &Book) {
        if let Some(p) = &self.path {
            if let Err(e) = crate::save_backup::save_to(p, b) {
                crate::log_warn!("save backup runs: {e}");
            }
        }
    }

    fn cancel_flag(&self, w: Which) -> &Arc<AtomicBool> {
        match w {
            Which::Share => &self.cancel_share,
            Which::Local => &self.cancel_local,
        }
    }

    /// Starts a run of `which` (none may be running). Its saves are added as they are found.
    pub fn begin(&self, which: Which, trigger: &str) -> Result<RunRef<'_>, String> {
        let mut b = self.lock();
        let side = b.side(which);
        if side
            .last
            .as_ref()
            .is_some_and(|r| r.state == RunState::Running)
        {
            return Err("a backup is running already".into());
        }
        let now = now_ms();
        side.prev = side.last.take();
        side.last = Some(Run {
            id: format!(
                "{}-{now}-{}",
                trigger_word(which),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ),
            which: Some(which),
            trigger: trigger.to_string(),
            started_at: now,
            updated_at: now,
            ..Run::default()
        });
        self.cancel_flag(which).store(false, Ordering::SeqCst);
        self.persist(&b);
        Ok(RunRef {
            j: self,
            which,
            id: side_id(&b, which),
            finished: false,
        })
    }

    /// Forgets the last run of `which` (one that ended with nothing in it), putting the one
    /// before it back.
    pub fn discard(&self, which: Which) {
        let mut b = self.lock();
        let side = b.side(which);
        let Some(id) = side.last.as_ref().map(|r| r.id.clone()) else {
            return;
        };
        side.history.retain(|h| h.id != id);
        side.last = side.prev.take();
        self.persist(&b);
    }

    /// The last run of `which` (running or not).
    pub fn last(&self, which: Which) -> Option<Run> {
        self.lock().side_ref(which).last.clone()
    }

    pub fn history(&self, which: Which) -> Vec<Summary> {
        self.lock().side_ref(which).history.clone()
    }

    /// Asks the run of `which` to stop after the step it is on; false when none runs.
    pub fn cancel(&self, which: Which) -> bool {
        let running = self
            .lock()
            .side_ref(which)
            .last
            .as_ref()
            .is_some_and(|r| r.state == RunState::Running);
        if running {
            self.cancel_flag(which).store(true, Ordering::SeqCst);
        }
        running
    }

    /// `{run, summary, history}` for the app.
    pub fn view(&self, which: Which) -> Value {
        let mut run = self.last(which);
        let history = self.history(which);
        // The copy going now: how far it is.
        let live = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&which)
            .cloned();
        let mut partial = 0u64;
        if let (Some(r), Some((i, place, sent, size))) = (run.as_mut(), live) {
            if let Some(it) = r.items.get_mut(i) {
                let sent = sent.load(Ordering::Relaxed).min(size);
                let places = it.dests.len().max(1) as u64;
                let item_size = it.size;
                if let Some(d) = it.dests.get_mut(&place) {
                    if d.step == Step::Copying && size > 0 && sent > 0 {
                        d.pct = Some((sent.saturating_mul(100) / size) as u8);
                        partial = item_size.saturating_mul(sent) / size / places;
                    }
                }
            }
        }
        let summary = run.as_ref().map(|r| {
            let mut s = r.summary();
            s.bytes_done += partial;
            s
        });
        json!({ "run": run, "summary": summary, "history": history, "now": now_ms() })
    }
}

fn trigger_word(w: Which) -> &'static str {
    match w {
        Which::Share => "share",
        Which::Local => "local",
    }
}

/// The record the engine keeps.
pub fn global() -> &'static Journal {
    static J: OnceLock<Journal> = OnceLock::new();
    J.get_or_init(|| Journal::open(crate::save_backup::state_file(FILE)))
}

/// The id of the last run of `which`.
fn side_id(b: &Book, which: Which) -> String {
    b.side_ref(which)
        .last
        .as_ref()
        .map(|r| r.id.clone())
        .unwrap_or_default()
}

/// What a run reads as when the pass ended without finishing it (a panic, an early return).
pub const ENDED_UNFINISHED: &str = "the backup stopped on an internal error";

/// The run in progress: every change goes through here.
pub struct RunRef<'a> {
    j: &'a Journal,
    which: Which,
    /// The run this is (a run dropped unfinished is ended, and only that one).
    id: String,
    finished: bool,
}

/// A run dropped without [`RunRef::finish`] (a pass that panicked or returned early) would read
/// as running for good, and every later run of its kind was refused ("a backup is running
/// already") until the engine restarted: it ends as interrupted instead.
impl Drop for RunRef<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.unwatch();
        let mut b = self.j.lock();
        let side = b.side(self.which);
        let Some(run) = side
            .last
            .as_mut()
            .filter(|r| r.id == self.id && r.state == RunState::Running)
        else {
            return;
        };
        run.updated_at = now_ms();
        run.interrupt(ENDED_UNFINISHED);
        let s = run.summary();
        side.history.retain(|h| h.id != s.id);
        side.history.insert(0, s);
        side.history.truncate(HISTORY);
        self.j.persist(&b);
        self.j
            .cancel_flag(self.which)
            .store(false, Ordering::SeqCst);
    }
}

impl RunRef<'_> {
    /// Set when the person asked the run to stop.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(self.j.cancel_flag(self.which))
    }

    pub fn cancelled(&self) -> bool {
        self.j.cancel_flag(self.which).load(Ordering::SeqCst)
    }

    /// Changes the run; `save`: write the record (a change of state; not a percent).
    fn with<T>(&self, save: bool, f: impl FnOnce(&mut Run) -> T) -> T {
        let mut b = self.j.lock();
        let run = b.side(self.which).last.get_or_insert_with(Run::default);
        let out = f(run);
        if save {
            run.updated_at = now_ms();
            let mut w = self.j.written.lock().unwrap_or_else(|e| e.into_inner());
            if w.is_none_or(|at| at.elapsed() >= WRITE_EVERY) {
                *w = Some(std::time::Instant::now());
                drop(w);
                self.j.persist(&b);
            }
        }
        out
    }

    /// Adds saves to the run; their indices.
    pub fn add(&self, items: Vec<Item>) -> std::ops::Range<usize> {
        self.with(true, |r| {
            let start = r.items.len();
            r.items.extend(items);
            start..r.items.len()
        })
    }

    pub fn item(&self, i: usize) -> Option<Item> {
        self.with(false, |r| r.items.get(i).cloned())
    }

    /// How many saves the run has.
    pub fn count(&self) -> usize {
        self.with(false, |r| r.items.len())
    }

    pub fn step(&self, i: usize, step: Step) {
        self.with(true, |r| {
            if let Some(it) = r.items.get_mut(i) {
                it.step = step;
                if step != Step::Failed {
                    it.error = None;
                }
            }
        });
    }

    /// The save could not be read off the PS5 (or failed before reaching any place).
    pub fn failed(&self, i: usize, why: &str) {
        self.with(true, |r| {
            if let Some(it) = r.items.get_mut(i) {
                it.step = Step::Failed;
                it.error = Some(why.to_string());
            }
        });
    }

    pub fn read_try(&self, i: usize) {
        self.with(true, |r| {
            if let Some(it) = r.items.get_mut(i) {
                it.tries += 1;
            }
        });
    }

    /// Changes the state of save `i` in `place`.
    pub fn dest(&self, i: usize, place: Place, f: impl FnOnce(&mut DestStatus)) {
        let named = self.with(true, |r| {
            let Some(d) = r.items.get_mut(i).and_then(|it| it.dests.get_mut(&place)) else {
                return false;
            };
            let before = d.file.clone();
            let before_step = d.step;
            f(d);
            if d.step != Step::Copying {
                d.pct = None;
            }
            // A copy about to be written is named in the file at once: a run cut off while it is
            // written has it removed before the save is copied again. And its being verified is
            // written at once too: left to the once-a-second write, an engine stopped just after
            // read it back as a copy cut off, and Continue removed that good copy (the older
            // ones already pruned).
            (d.step == Step::Copying && d.file.is_some() && d.file != before)
                || (d.step == Step::Verified && before_step != Step::Verified)
        });
        if named {
            self.write_now();
        }
    }

    /// Writes the record now, whenever it was last written.
    fn write_now(&self) {
        let b = self.j.lock();
        *self.j.written.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::time::Instant::now());
        self.j.persist(&b);
    }

    /// How far the copy of save `i` to `place` is (kept in memory only).
    #[cfg(test)]
    pub fn pct(&self, i: usize, place: Place, pct: u8) {
        self.with(false, |r| {
            if let Some(d) = r.items.get_mut(i).and_then(|it| it.dests.get_mut(&place)) {
                d.pct = Some(pct.min(100));
            }
        });
    }

    /// The copy of save `i` to `place` is going: `sent` counts its bytes, of `size`.
    pub fn watch(&self, i: usize, place: Place, sent: Arc<AtomicU64>, size: u64) {
        self.j
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(self.which, (i, place, sent, size));
    }

    pub fn unwatch(&self) {
        self.j
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.which);
    }

    /// A console could not be looked at.
    pub fn unreached(&self, host: &str, why: &str) {
        self.with(true, |r| {
            r.unreached.insert(host.to_string(), why.to_string());
        });
    }

    /// Ends the run: `Done` unless it was cancelled or saves were left (then `Stopped` with
    /// `reason`). Busy saves (a cancel mid-copy) are waiting again.
    pub fn finish(mut self, reason: Option<(String, String)>) -> Summary {
        self.finished = true;
        self.unwatch();
        let cancelled = self.cancelled();
        let mut b = self.j.lock();
        let side = b.side(self.which);
        let Some(run) = side.last.as_mut() else {
            return Summary::default();
        };
        let now = now_ms();
        for it in &mut run.items {
            if it.step.busy() {
                it.step = Step::Waiting;
            }
            for d in it.dests.values_mut() {
                if d.step.busy() {
                    d.step = Step::Waiting;
                    d.pct = None;
                }
            }
        }
        let left = run.items.iter().any(|it| it.outcome() == Outcome::Left);
        run.state = if cancelled {
            RunState::Cancelled
        } else if left && reason.is_some() {
            RunState::Stopped
        } else {
            RunState::Done
        };
        (run.reason_code, run.reason) = match (run.state, reason) {
            (RunState::Cancelled, _) => (Some("cancelled".into()), None),
            (RunState::Stopped, Some((code, why))) => (Some(code), Some(why)),
            _ => (None, None),
        };
        run.ended_at = now;
        run.updated_at = now;
        let s = run.summary();
        side.history.retain(|h| h.id != s.id);
        side.history.insert(0, s.clone());
        side.history.truncate(HISTORY);
        self.j.persist(&b);
        self.j
            .cancel_flag(self.which)
            .store(false, Ordering::SeqCst);
        s
    }
}

/// A new item for a run: `places` the places it goes to.
pub fn new_item(
    host: &str,
    e: &ps5upload_core::saves::SaveEntry,
    id: String,
    places: &[Place],
) -> Item {
    Item {
        id,
        host: host.to_string(),
        kind: e.kind.clone(),
        title_id: e.title_id.clone(),
        user_id: e.user_id as u32,
        path: e.path.clone(),
        size: e.size.max(0) as u64,
        mtime: e.mtime,
        dests: places.iter().map(|p| (*p, DestStatus::default())).collect(),
        ..Item::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = crate::test_tmp::temp_path(format!("ps5u-runs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join(FILE)
    }

    fn entry(title: &str, size: i64) -> ps5upload_core::saves::SaveEntry {
        ps5upload_core::saves::SaveEntry {
            title_id: title.into(),
            user_id: 1,
            path: format!("/user/home/1/savedata_prospero/{title}"),
            size,
            mtime: 100,
            kind: "ps5".into(),
        }
    }

    /// A pass that panics (or returns early) drops its run unfinished: the run ends as
    /// interrupted, and the next one may start (it was refused until the engine restarted).
    #[test]
    fn a_run_dropped_unfinished_ends_and_frees_the_next() {
        let j = Journal::open(None);
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let run = j.begin(Which::Share, "interval").unwrap();
            run.add(vec![new_item(
                "h",
                &entry("PPSA00001", 10),
                "h|a".into(),
                &[Place::Share],
            )]);
            run.step(0, Step::Copying);
            panic!("a pass that fails");
        }));
        assert!(done.is_err());
        let last = j.last(Which::Share).unwrap();
        assert_eq!(last.state, RunState::Interrupted);
        assert_eq!(last.reason.as_deref(), Some(ENDED_UNFINISHED));
        assert_eq!(last.items[0].step, Step::Waiting);
        assert_eq!(j.history(Which::Share)[0].id, last.id);
        assert!(!j.cancel(Which::Share), "nothing runs");
        let next = j.begin(Which::Share, "interval").unwrap();
        // A finished run is left as it ended; one dropped after a discard leaves the one before.
        let s = next.finish(None);
        assert_eq!(s.state, RunState::Done);
        assert_eq!(j.last(Which::Share).unwrap().state, RunState::Done);
        let run = j.begin(Which::Share, "interval").unwrap();
        j.discard(Which::Share);
        drop(run);
        assert_eq!(j.last(Which::Share).unwrap().state, RunState::Done);
    }

    /// A copy's being verified is on the disk at once, not a second later: an engine stopped in
    /// between read it back as a copy cut off, and Continue removed that good copy.
    #[test]
    fn a_verified_copy_is_written_at_once() {
        let path = temp("verified");
        {
            let j = Journal::open(Some(path.clone()));
            let run = j.begin(Which::Share, "manual").unwrap();
            run.add(vec![new_item(
                "h",
                &entry("PPSA00001", 10),
                "h|a".into(),
                &[Place::Share],
            )]);
            run.dest(0, Place::Share, |d| {
                d.step = Step::Copying;
                d.file = Some("/b/PPSA00001/x.zip".into());
            });
            run.dest(0, Place::Share, |d| d.step = Step::Verified);
            std::mem::forget(run);
        }
        let last = Journal::open(Some(path)).last(Which::Share).unwrap();
        let d = &last.items[0].dests[&Place::Share];
        assert_eq!(d.step, Step::Verified);
        assert!(last.items[0].left_places().is_empty());
    }

    #[test]
    fn a_run_survives_an_engine_restart_as_interrupted() {
        let path = temp("restart");
        {
            let j = Journal::open(Some(path.clone()));
            let run = j.begin(Which::Share, "manual").unwrap();
            let places = [Place::Share, Place::Drive];
            run.add(vec![
                new_item("h", &entry("PPSA00001", 10), "h|a".into(), &places),
                new_item("h", &entry("PPSA00002", 20), "h|b".into(), &places),
            ]);
            run.dest(0, Place::Share, |d| d.step = Step::Verified);
            run.dest(0, Place::Drive, |d| d.step = Step::Verified);
            run.step(1, Step::Copying);
            run.dest(1, Place::Share, |d| {
                d.step = Step::Copying;
                d.file = Some("/b/PPSA00002/x.zip".into());
            });
            run.pct(1, Place::Share, 40);
            // The engine stops here: no finish, and nothing of the process runs after.
            std::mem::forget(run);
        }
        let j = Journal::open(Some(path.clone()));
        let last = j.last(Which::Share).unwrap();
        assert_eq!(last.state, RunState::Interrupted);
        assert_eq!(last.reason.as_deref(), Some(INTERRUPTED));
        assert_eq!(last.items[0].outcome(), Outcome::Verified);
        let b = &last.items[1];
        assert_eq!(b.outcome(), Outcome::Left);
        assert_eq!(b.step, Step::Waiting);
        let d = &b.dests[&Place::Share];
        assert_eq!((d.step, d.pct), (Step::Waiting, None));
        // The partial copy stays named, so a Continue removes it first.
        assert_eq!(d.file.as_deref(), Some("/b/PPSA00002/x.zip"));
        assert_eq!(b.left_places(), vec![Place::Share, Place::Drive]);
        let h = j.history(Which::Share);
        assert_eq!((h[0].verified, h[0].left, h[0].total), (1, 1, 2));
        // A new run may start; the old one is in the history.
        let run = j.begin(Which::Share, "continue").unwrap();
        assert!(j.begin(Which::Share, "manual").is_err(), "one at a time");
        let s = run.finish(None);
        assert_eq!(s.state, RunState::Done);
        assert_eq!(j.history(Which::Share).len(), 2);
        // The other kind has its own record.
        assert!(j.last(Which::Local).is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_run_ends_stopped_cancelled_or_done_and_counts_per_save() {
        let j = Journal::open(None);
        let run = j.begin(Which::Local, "manual").unwrap();
        let places = [Place::Local, Place::Drive];
        run.add(vec![
            new_item("h", &entry("PPSA00001", 10), "1".into(), &places),
            new_item("h", &entry("PPSA00002", 20), "2".into(), &places),
            new_item("h", &entry("PPSA00003", 30), "3".into(), &places),
        ]);
        run.dest(0, Place::Local, |d| d.step = Step::Verified);
        run.dest(0, Place::Drive, |d| d.step = Step::Skipped);
        run.dest(1, Place::Local, |d| d.step = Step::Verified);
        run.dest(1, Place::Drive, |d| {
            d.step = Step::Failed;
            d.error = Some("the copy on the PS5 does not match".into());
        });
        let s = run.finish(Some((
            "console_gone".into(),
            "The PS5 isn't answering".into(),
        )));
        assert_eq!(s.state, RunState::Stopped);
        assert_eq!((s.verified, s.failed, s.left), (1, 1, 1));
        assert_eq!((s.bytes_total, s.bytes_done), (60, 10));
        assert_eq!(
            s.first_error.as_deref(),
            Some("PPSA00002: the copy on the PS5 does not match")
        );
        // Cancelled.
        let run = j.begin(Which::Local, "manual").unwrap();
        run.add(vec![new_item(
            "h",
            &entry("PPSA00001", 10),
            "1".into(),
            &places,
        )]);
        assert!(j.cancel(Which::Local));
        assert!(run.cancelled());
        assert_eq!(run.finish(None).state, RunState::Cancelled);
        assert!(!j.cancel(Which::Local), "nothing runs now");
        // Nothing left: done, whatever the reason given.
        let run = j.begin(Which::Local, "manual").unwrap();
        assert_eq!(
            run.finish(Some(("console_gone".into(), "x".into()))).state,
            RunState::Done
        );
        assert_eq!(j.history(Which::Local).len(), 3);
    }
}
