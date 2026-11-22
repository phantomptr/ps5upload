//! Copying AVA1's progress into the counters the engine's ticker reads.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use ava1::send::{Progress, PHASE_SKIP};
use ps5upload_core::transfer::{TransferConfig, LIVE_PHASE_SKIPPING};

/// Copies AVA1 progress into `cfg`'s counters every 200 ms until dropped, and once
/// more on drop (the final values, so a caller asserting progress right after the
/// transfer returns sees what the ticker will report).
///
/// Mapping (C12/A3): `progress_bytes` ← `bytes_sent` (Received bytes),
/// `progress_files` and `progress_files_finalized` ← `files_durable`,
/// `progress_bytes_finalized` ← `bytes_durable`. `progress_bytes` also counts what the
/// console already held when the job opened (whole files it skipped, or the durable
/// ranges a resumed file kept): the bar starts where the last run left off and reaches 100%
/// when the console has acknowledged the last byte it needed, not before (see [`Opened`]).
/// AVA1 has no per-file "sent" counter;
/// durability is its analogue, so the UI's file counter moves in commit-sized steps —
/// slower but honest, and strictly better than showing 0 for the whole transfer.
/// The engine's ticker reads these counters absolutely, so the bridge `store`s, never
/// `fetch_add`s.
pub struct Bridge {
    handle: tokio::task::AbortHandle,
    counters: Arc<Counters>,
}

/// What the console held of a job at each open, as its map said: written by the sender's space
/// check (`upload::record_opened`), read by the bridge.
///
/// `bytes_sent` counts every byte the console acknowledged receiving, over every attempt of
/// the job, and the console's `Durable` reports add up every range it synced. Neither is what
/// the console has: bytes it acknowledged are lost when the helper stops or the job is
/// reopened from its journal, and a range written twice (a frame of a stopped run that lands
/// after the resume, then the same range from the resumed run) is reported durable twice. The
/// count used to add the first open's figure to `bytes_sent` and never read below the durable
/// sum, so it ran ahead and the job read "everything sent" while bytes were still on their
/// way (#873). Each open says what the console really holds, and from there it needs exactly
/// the rest once: the count is the console's figure at the latest open plus what it has
/// acknowledged since.
#[derive(Debug)]
pub struct Opened {
    /// Bytes the console held before this job's own sending: the first open's figure, raised
    /// by a later open only when bytes an earlier run sent became durable after it.
    resumed: AtomicU64,
    /// The console's figure at the latest open less `bytes_sent` at that moment, so
    /// `bytes_sent + base` is what the console has of the job. [`NO_BASE`] before any open.
    base: AtomicI64,
    /// The job ended with every file in place.
    complete: AtomicBool,
}

const NO_BASE: i64 = i64::MIN;

impl Default for Opened {
    fn default() -> Self {
        Opened {
            resumed: AtomicU64::new(0),
            base: AtomicI64::new(NO_BASE),
            complete: AtomicBool::new(false),
        }
    }
}

impl Opened {
    /// An open: the console's map says it holds `durable` bytes of the job, and the job has
    /// had `sent` bytes acknowledged so far.
    pub fn opened(&self, durable: u64, sent: u64) {
        self.resumed
            .fetch_max(durable.saturating_sub(sent), Ordering::AcqRel);
        let base =
            i64::try_from(durable).unwrap_or(i64::MAX) - i64::try_from(sent).unwrap_or(i64::MAX);
        self.base.store(base.max(NO_BASE + 1), Ordering::Release);
    }

    /// The job ended with every file in place: the count reads the whole job.
    pub fn complete(&self) {
        self.complete.store(true, Ordering::Release);
    }

    /// Bytes the console held when the job first opened.
    pub fn resumed(&self) -> u64 {
        self.resumed.load(Ordering::Acquire)
    }

    /// What the console has of the job once it has acknowledged `sent` bytes, or `None` before
    /// the first open (and on a path without the space check).
    pub fn done(&self, sent: u64) -> Option<u64> {
        let base = self.base.load(Ordering::Acquire);
        (base != NO_BASE).then(|| {
            u64::try_from((i128::from(base) + i128::from(sent)).max(0)).unwrap_or(u64::MAX)
        })
    }
}

/// Bytes of the job the console has, for the bar: what it held at the latest open plus what it
/// has acknowledged since (see [`Opened`]), and the whole job once it ended with every file in
/// place. Before any open, and on a path without the space check: the bytes acknowledged plus
/// the files the console skipped, never below what it reports durable.
pub fn bytes_done(p: &Progress, opened: &Opened) -> u64 {
    let sent = p.bytes_sent.load(Ordering::Relaxed);
    match opened.done(sent) {
        Some(done) if opened.complete.load(Ordering::Acquire) => {
            done.max(p.bytes_total.load(Ordering::Relaxed))
        }
        Some(done) => done,
        None => sent
            .saturating_add(
                p.skipped_bytes
                    .load(Ordering::Relaxed)
                    .max(opened.resumed()),
            )
            .max(p.bytes_durable.load(Ordering::Relaxed)),
    }
}

/// How long the console may confirm nothing new, once it has acknowledged every byte, before
/// the engine says so in its log. The job's snapshot carries the time itself from the start.
pub const QUIET_AFTER: Duration = Duration::from_secs(60);

/// What the console has confirmed of a job: durable bytes, finished files, files still to
/// save in place, whether it is saving them, and the bytes it has still to read back (its
/// `Status.readback_left`, AVA1 1.0.12; 0 from an older helper). Any change is the console
/// getting on with it: a read-back that advances is not a quiet console.
pub type Confirmed = (u64, u64, u32, bool, u64);

/// The stretch after the last byte: how long ago the console acknowledged it, and how long
/// since it last confirmed anything new. Pure (the caller brings the clock).
#[derive(Debug, Default)]
pub struct QuietWatch {
    all_sent_at: Option<Instant>,
    moved_at: Option<Instant>,
    seen: Confirmed,
    told: bool,
    longest: Duration,
}

/// What a [`QuietWatch::observe`] asks its caller to say.
#[derive(Debug, PartialEq, Eq)]
pub enum QuietNote {
    /// Nothing confirmed for this long (said once per quiet stretch).
    Quiet(Duration),
    /// The console confirmed something again, after a stretch this long that was reported.
    MovedAgain(Duration),
}

impl QuietWatch {
    /// One look at the job. `all_sent`: the console has acknowledged every byte it needed.
    pub fn observe(
        &mut self,
        now: Instant,
        all_sent: bool,
        confirmed: Confirmed,
        after: Duration,
    ) -> Option<QuietNote> {
        if !all_sent {
            // Bytes are moving (a file the console asked for again, a reconnect): not this
            // stretch. What was measured stays for the end-of-job line.
            self.moved_at = None;
            self.told = false;
            return None;
        }
        self.all_sent_at.get_or_insert(now);
        let Some(moved_at) = self.moved_at else {
            self.moved_at = Some(now);
            self.seen = confirmed;
            return None;
        };
        if confirmed != self.seen {
            let was = now.duration_since(moved_at);
            self.seen = confirmed;
            self.moved_at = Some(now);
            return std::mem::take(&mut self.told).then_some(QuietNote::MovedAgain(was));
        }
        let quiet = now.duration_since(moved_at);
        self.longest = self.longest.max(quiet);
        if quiet >= after && !self.told {
            self.told = true;
            return Some(QuietNote::Quiet(quiet));
        }
        None
    }

    /// Since the console first acknowledged the last byte (zero before that).
    pub fn since_all_sent(&self, now: Instant) -> Duration {
        self.all_sent_at
            .map_or(Duration::ZERO, |t| now.duration_since(t))
    }

    /// Since the console last confirmed anything new while it had every byte (zero otherwise).
    pub fn quiet_for(&self, now: Instant) -> Duration {
        self.moved_at
            .map_or(Duration::ZERO, |t| now.duration_since(t))
    }

    /// The longest stretch with nothing confirmed.
    pub fn longest(&self) -> Duration {
        self.longest
    }
}

struct Counters {
    p: Arc<Progress>,
    bytes: Option<Arc<AtomicU64>>,
    files: Option<Arc<std::sync::atomic::AtomicU64>>,
    files_finalized: Option<Arc<std::sync::atomic::AtomicU64>>,
    bytes_finalized: Option<Arc<std::sync::atomic::AtomicU64>>,
    live: Option<Arc<ps5upload_core::transfer::LiveNotes>>,
    /// What the console held at each open (see [`Bridge::start_resumed`]).
    opened: Arc<Opened>,
    resumed_out: Option<Arc<AtomicU64>>,
    rates: Mutex<RateHistory>,
    /// The job asked the console to read its large files back (JF_READBACK, #783).
    readback: AtomicBool,
    /// While the console reads back after the last byte: the most it said it had left to
    /// read (the "of" in "X of Y checked"); 0 outside that stretch.
    verify_peak: AtomicU64,
    /// The job's name for the log lines about the stretch after its last byte ("": none).
    label: String,
    quiet: Mutex<QuietWatch>,
    /// The session the current attempt runs on: its state goes in those log lines.
    session: Mutex<Weak<ava1::session::Session>>,
}

/// The longest `RateHistory` kept: past it every other point goes and the step doubles.
pub const RATE_POINTS_MAX: usize = 120;
/// The first step of a `RateHistory`, in seconds.
const RATE_STEP_SECS: f64 = 10.0;

/// The job's rate over time, for the per-job record (`rate_history`): one point per step
/// as `[seconds since start, MB/s acknowledged over the step, lanes open, bottleneck code,
/// MiB resent so far]`. It tells a link that slowed (lanes held, rate down: the network)
/// from a sender that gave up its lanes (lanes down to one with the rate), and shows the
/// dip-then-stall shape of a long upload. The step doubles whenever the history is full, so
/// a job of any length stays within `RATE_POINTS_MAX` points.
pub struct RateHistory {
    start: Instant,
    step: f64,
    last_at: f64,
    last_bytes: u64,
    points: Vec<[f64; 5]>,
}

impl RateHistory {
    fn new(start: Instant) -> Self {
        Self {
            start,
            step: RATE_STEP_SECS,
            last_at: 0.0,
            last_bytes: 0,
            points: Vec::new(),
        }
    }

    /// Adds a point when a step has passed since the last one (or `force`, at the end).
    pub fn sample(&mut self, now: Instant, p: &Progress, force: bool) {
        let at = now.duration_since(self.start).as_secs_f64();
        let secs = at - self.last_at;
        if secs < self.step && !(force && secs >= 1.0) {
            return;
        }
        let bytes = p.bytes_sent.load(Ordering::Relaxed);
        let rate = bytes.saturating_sub(self.last_bytes) as f64 / secs / 1e6;
        let round1 = |x: f64| (x * 10.0).round() / 10.0;
        self.points.push([
            at.round(),
            round1(rate),
            f64::from(p.lanes.load(Ordering::Relaxed)),
            f64::from(p.bottleneck.load(Ordering::Relaxed)),
            round1(p.resent_bytes.load(Ordering::Relaxed) as f64 / f64::from(1u32 << 20)),
        ]);
        self.last_at = at;
        self.last_bytes = bytes;
        if self.points.len() > RATE_POINTS_MAX {
            let mut i = 0;
            self.points.retain(|_| {
                i += 1;
                i % 2 == 0
            });
            self.step *= 2.0;
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!(self.points)
    }
}

impl Bridge {
    pub fn start(p: Arc<Progress>, cfg: &TransferConfig) -> Bridge {
        Bridge::start_resumed(p, cfg, Arc::default())
    }

    /// [`Bridge::start`] with `opened`: what the console already held at each open of the job
    /// (whole files in place plus durable ranges), written by the sender's space check.
    /// Counted as done; the first open's figure is copied to `cfg.progress_bytes_resumed`.
    pub fn start_resumed(p: Arc<Progress>, cfg: &TransferConfig, opened: Arc<Opened>) -> Bridge {
        Bridge::start_job(p, cfg, opened, String::new())
    }

    /// [`Bridge::start_resumed`] for a job with a name: `label` starts the engine's log lines
    /// about the stretch after the job's last byte (see [`QUIET_AFTER`]).
    pub fn start_job(
        p: Arc<Progress>,
        cfg: &TransferConfig,
        opened: Arc<Opened>,
        label: String,
    ) -> Bridge {
        let counters = Arc::new(Counters {
            p,
            bytes: cfg.progress_bytes.clone(),
            files: cfg.progress_files.clone(),
            files_finalized: cfg.progress_files_finalized.clone(),
            bytes_finalized: cfg.progress_bytes_finalized.clone(),
            live: cfg.progress_live.clone(),
            opened,
            resumed_out: cfg.progress_bytes_resumed.clone(),
            rates: Mutex::new(RateHistory::new(Instant::now())),
            readback: AtomicBool::new(false),
            verify_peak: AtomicU64::new(0),
            label,
            quiet: Mutex::default(),
            session: Mutex::new(Weak::new()),
        });
        let ticker = counters.clone();
        let handle = tokio::spawn(async move {
            loop {
                ticker.store_at(false);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .abort_handle();
        Bridge { handle, counters }
    }

    /// Whether this attempt asked the console to read its large files back: then the stretch
    /// where every byte is on the console but files are still being committed is reported as
    /// `verifying` (the read-back is most of it).
    pub fn set_readback(&self, on: bool) {
        self.counters.readback.store(on, Ordering::Relaxed);
    }

    /// The session this attempt runs on: whether it is still open goes in the log lines.
    pub fn set_session(&self, s: &Arc<ava1::session::Session>) {
        *self
            .counters
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(s);
    }
}

/// The read-back after the last byte as (bytes left, bytes in all) for the row's "X of Y": what
/// the console last said it has left (`Status.readback_left`) and the most it said since the
/// stretch began, kept in `peak`. (0, 0) outside the stretch and from a helper that does not say
/// (AVA1 before 1.0.12), so the row shows no figures.
pub fn verify_counts(verifying: bool, left: u64, peak: &AtomicU64) -> (u64, u64) {
    if !verifying || left == 0 && peak.load(Ordering::Relaxed) == 0 {
        peak.store(0, Ordering::Relaxed);
        return (0, 0);
    }
    let total = peak.fetch_max(left, Ordering::Relaxed).max(left);
    (left, total)
}

/// Every byte of the job is durable on the console but not every file is done: with the
/// read-back on, the console is reading the last large files back from its drive.
pub fn verifying(p: &Progress, readback: bool) -> bool {
    let total = p.bytes_total.load(Ordering::Relaxed);
    let files = p.files_total.load(Ordering::Relaxed);
    readback
        && total > 0
        && p.bytes_durable.load(Ordering::Relaxed) >= total
        && p.files_durable.load(Ordering::Relaxed) < files
        && !p.settling.load(Ordering::Relaxed)
}

impl Counters {
    /// The session's state in a few words.
    fn session_words(&self) -> String {
        let s = self
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade();
        match s {
            None => "no session".into(),
            Some(s) if s.is_closed() => "the session has closed".into(),
            Some(s) => match s.rtt() {
                Some(rtt) => format!("the session is open (round trip {} ms)", rtt.as_millis()),
                None => "the session is open".into(),
            },
        }
    }

    /// What the console has and has not confirmed, for a log line.
    fn figures(&self, done: u64) -> String {
        let p = &self.p;
        let (durable, total) = (
            p.bytes_durable.load(Ordering::Relaxed),
            p.bytes_total.load(Ordering::Relaxed),
        );
        let doing = if p.settling.load(Ordering::Relaxed) {
            format!(
                "it reports {} files still to save in place",
                p.unswept.load(Ordering::Relaxed)
            )
        } else if verifying(p, self.readback.load(Ordering::Relaxed)) {
            match p.readback_left.load(Ordering::Relaxed) {
                0 => "it has made every byte durable and is reading the large files back from its \
                      drive"
                    .into(),
                left => format!(
                    "it has made every byte durable and is reading the large files back from its \
                     drive, {} MiB still to read",
                    left >> 20
                ),
            }
        } else if durable >= total {
            "it has made every byte durable and is putting the files in place".into()
        } else {
            "it is still writing what it received".into()
        };
        format!(
            "{durable} of {total} bytes durable, {} of {} files done; {doing}; this run had {} \
             bytes acknowledged ({} of them sent twice), the console held {} before it, {done} \
             counted in all; attempt {}, {} lanes, limited by {}; {}",
            p.files_durable.load(Ordering::Relaxed),
            p.files_total.load(Ordering::Relaxed),
            p.bytes_sent.load(Ordering::Relaxed),
            p.resent_bytes.load(Ordering::Relaxed),
            self.opened.resumed(),
            p.attempts.load(Ordering::Relaxed),
            p.lanes.load(Ordering::Relaxed),
            bottleneck_name(p.bottleneck.load(Ordering::Relaxed)),
            self.session_words(),
        )
    }

    /// The stretch after the last byte: the snapshot's two times, and the engine's log lines.
    fn watch(&self, done: u64, last: bool) {
        let p = &self.p;
        let total = p.bytes_total.load(Ordering::Relaxed);
        let all_sent = total > 0
            && done >= total
            && p.attempts.load(Ordering::Relaxed) > 0
            && p.phase.load(Ordering::Relaxed) != PHASE_SKIP;
        let confirmed = (
            p.bytes_durable.load(Ordering::Relaxed),
            p.files_durable.load(Ordering::Relaxed),
            p.unswept.load(Ordering::Relaxed),
            p.settling.load(Ordering::Relaxed),
            p.readback_left.load(Ordering::Relaxed),
        );
        let now = Instant::now();
        let mut w = self.quiet.lock().unwrap_or_else(|e| e.into_inner());
        let note = if last {
            None
        } else {
            w.observe(now, all_sent, confirmed, QUIET_AFTER)
        };
        if let Some(l) = &self.live {
            let (since, quiet) = if all_sent && !last {
                (w.since_all_sent(now), w.quiet_for(now))
            } else {
                (Duration::ZERO, Duration::ZERO)
            };
            l.all_sent_ms
                .store(since.as_millis() as u64, Ordering::Relaxed);
            l.console_quiet_ms
                .store(quiet.as_millis() as u64, Ordering::Relaxed);
        }
        if self.label.is_empty() {
            return;
        }
        match note {
            Some(QuietNote::Quiet(d)) => ps5upload_core::clog!(
                Warn,
                "transfer",
                "{}: every byte was acknowledged {} s ago and the console has confirmed nothing \
                 new for {} s: {}",
                self.label,
                w.since_all_sent(now).as_secs(),
                d.as_secs(),
                self.figures(done)
            ),
            Some(QuietNote::MovedAgain(d)) => ps5upload_core::clog!(
                Info,
                "transfer",
                "{}: the console confirmed progress again after {} s: {}",
                self.label,
                d.as_secs(),
                self.figures(done)
            ),
            None => {}
        }
        // A job that went on long after its last byte says where the time went when it ends,
        // in the console's own words when it gave any (its line counts the read-back).
        let after = w.since_all_sent(now);
        if last && after >= QUIET_AFTER {
            let line = p
                .telemetry
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .console_line
                .clone();
            ps5upload_core::clog!(
                Info,
                "transfer",
                "{}: ended {} s after its last byte was acknowledged (longest stretch with \
                 nothing confirmed: {} s): {}; the console's line: {}",
                self.label,
                after.as_secs(),
                w.longest().as_secs(),
                self.figures(done),
                line.as_deref().unwrap_or("none")
            );
        }
    }

    fn store_at(&self, last: bool) {
        let resumed = self.opened.resumed();
        let done = bytes_done(&self.p, &self.opened);
        self.watch(done, last);
        if let Some(r) = &self.resumed_out {
            // Before the bytes below, which include it: a snapshot never sees bytes that
            // count the resumed part without the figure that names it.
            r.store(resumed, Ordering::Release);
        }
        if let Some(l) = &self.live {
            let p = &self.p;
            l.bottleneck
                .store(p.bottleneck.load(Ordering::Relaxed), Ordering::Relaxed);
            let phase = match p.phase.load(Ordering::Relaxed) {
                PHASE_SKIP => LIVE_PHASE_SKIPPING,
                _ => 0,
            };
            l.phase.store(phase, Ordering::Relaxed);
            l.skip_done_bytes
                .store(p.skip_done_bytes.load(Ordering::Relaxed), Ordering::Relaxed);
            l.skip_total_bytes.store(
                p.skip_total_bytes.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            l.settling
                .store(p.settling.load(Ordering::Relaxed), Ordering::Relaxed);
            let verifying_now = !last && verifying(p, self.readback.load(Ordering::Relaxed));
            l.verifying.store(verifying_now, Ordering::Relaxed);
            let (left, total) = verify_counts(
                verifying_now,
                p.readback_left.load(Ordering::Relaxed),
                &self.verify_peak,
            );
            l.verify_left_bytes.store(left, Ordering::Relaxed);
            l.verify_total_bytes.store(total, Ordering::Relaxed);
            l.unswept
                .store(p.unswept.load(Ordering::Relaxed), Ordering::Relaxed);
            l.unswept_peak
                .store(p.unswept_peak.load(Ordering::Relaxed), Ordering::Relaxed);
            if let Some(mut t) = crate::telemetry::snapshot(p) {
                let mut r = self.rates.lock().unwrap_or_else(|e| e.into_inner());
                r.sample(Instant::now(), p, last);
                t["rate_history"] = r.json();
                *l.telemetry.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
            }
        }
        if let Some(x) = &self.bytes {
            // What the console already had counts as done, so a resume's bar starts where the
            // last run stopped and reaches 100% (see `bytes_done`).
            x.store(done, Ordering::Release);
        }
        if let Some(x) = &self.files {
            x.store(
                self.p.files_durable.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
        if let Some(x) = &self.files_finalized {
            x.store(
                self.p.files_durable.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
        if let Some(x) = &self.bytes_finalized {
            x.store(
                self.p.bytes_durable.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.handle.abort();
        self.counters.store_at(true);
    }
}

/// The same words the benchmark reports (Tasks 26–28), so a report and a job's
/// `commit_ack` say the same thing.
pub fn bottleneck_name(b: u8) -> &'static str {
    match b {
        crate::gen::BN_NETWORK => "network",
        crate::gen::BN_SOURCE => "source",
        crate::gen::BN_DISK => "console drive",
        crate::gen::BN_WORKERS => "console workers",
        crate::gen::BN_CREDIT => "console memory",
        _ => "none",
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn verifying_is_the_stretch_after_the_last_byte_with_the_read_back_on() {
        let p = Progress::default();
        p.bytes_total.store(100, Ordering::Relaxed);
        p.files_total.store(3, Ordering::Relaxed);
        p.bytes_durable.store(60, Ordering::Relaxed);
        p.files_durable.store(1, Ordering::Relaxed);
        assert!(!verifying(&p, true), "bytes still on their way");
        p.bytes_durable.store(100, Ordering::Relaxed);
        assert!(verifying(&p, true));
        assert!(!verifying(&p, false), "not asked for: just finishing");
        p.settling.store(true, Ordering::Relaxed);
        assert!(!verifying(&p, true), "settling has its own line");
        p.settling.store(false, Ordering::Relaxed);
        p.files_durable.store(3, Ordering::Relaxed);
        assert!(!verifying(&p, true), "every file done");
    }

    /// #873: the read-back after the last byte reads as "X of Y": Y is the most the console said
    /// it had left since the stretch began, X what it says now. Nothing from an older helper.
    #[test]
    fn the_read_back_after_the_last_byte_counts_down_from_its_peak() {
        let peak = AtomicU64::new(0);
        assert_eq!(
            verify_counts(false, 500, &peak),
            (0, 0),
            "bytes still arriving"
        );
        assert_eq!(
            verify_counts(true, 0, &peak),
            (0, 0),
            "an older helper says nothing"
        );
        assert_eq!(verify_counts(true, 400, &peak), (400, 400));
        assert_eq!(verify_counts(true, 250, &peak), (250, 400));
        assert_eq!(
            verify_counts(true, 0, &peak),
            (0, 400),
            "all read, the commit is next"
        );
        // A file read back wrong is sent again (not verifying), then read back anew.
        assert_eq!(verify_counts(false, 300, &peak), (0, 0));
        assert_eq!(verify_counts(true, 120, &peak), (120, 120));
    }

    /// #873: a read-back that advances keeps the console from counting as quiet.
    #[test]
    fn a_read_back_that_advances_is_not_a_quiet_console() {
        let mut w = QuietWatch::default();
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let after = QUIET_AFTER;
        assert_eq!(w.observe(t0, true, (100, 0, 0, false, 900), after), None);
        for k in 1..20u64 {
            assert_eq!(
                w.observe(
                    t0 + s(30 * k),
                    true,
                    (100, 0, 0, false, 900 - 40 * k),
                    after
                ),
                None
            );
        }
        assert!(w.quiet_for(t0 + s(30 * 19)) < after);
        assert_eq!(
            w.observe(t0 + s(30 * 19 + 61), true, (100, 0, 0, false, 140), after),
            Some(QuietNote::Quiet(s(61))),
            "stopped advancing: quiet again"
        );
    }

    #[test]
    fn the_rate_history_keeps_a_point_per_step_and_thins_a_long_job() {
        let p = Progress::default();
        let t0 = Instant::now();
        let mut h = RateHistory::new(t0);
        p.lanes.store(4, Ordering::Relaxed);
        p.bottleneck
            .store(crate::gen::BN_NETWORK, Ordering::Relaxed);
        p.bytes_sent.store(100_000_000, Ordering::Relaxed);
        h.sample(t0 + Duration::from_secs(5), &p, false); // within the first step
        assert!(h.points.is_empty());
        h.sample(t0 + Duration::from_secs(10), &p, false);
        assert_eq!(h.points, vec![[10.0, 10.0, 4.0, 1.0, 0.0]]);
        p.lanes.store(1, Ordering::Relaxed);
        p.resent_bytes.store(3 << 20, Ordering::Relaxed);
        p.bytes_sent.store(120_000_000, Ordering::Relaxed);
        h.sample(t0 + Duration::from_secs(14), &p, true); // the end: a short last step
        assert_eq!(h.points[1], [14.0, 5.0, 1.0, 1.0, 3.0]);
        for k in 0..1000u64 {
            p.bytes_sent.store(120_000_000 + k, Ordering::Relaxed);
            h.sample(t0 + Duration::from_secs(20 + 10 * k), &p, false);
        }
        assert!(h.points.len() <= RATE_POINTS_MAX);
        assert!(h.step > RATE_STEP_SECS);
        assert!(h.points.windows(2).all(|w| w[0][0] < w[1][0]));
    }

    #[tokio::test]
    async fn a_resumed_job_counts_what_the_console_already_had_as_done() {
        let p = Arc::new(Progress::default());
        let live = Arc::new(ps5upload_core::transfer::LiveNotes::default());
        let bytes = Arc::new(AtomicU64::new(0));
        let mut cfg = TransferConfig::new("h");
        cfg.progress_live = Some(live.clone());
        cfg.progress_bytes = Some(bytes.clone());
        cfg.progress_bytes_resumed = Some(live.resumed_bytes.clone());
        let opened = Arc::new(Opened::default());
        let bridge = Bridge::start_resumed(p.clone(), &cfg, opened.clone());
        // A single 100-byte file whose first 60 bytes were already durable: no skipped
        // files, 60 resumed, 5 sent this run.
        opened.opened(60, 0);
        p.bytes_sent.store(5, Ordering::Relaxed);
        drop(bridge);
        assert_eq!(bytes.load(Ordering::Relaxed), 65);
        assert_eq!(live.resumed_bytes.load(Ordering::Relaxed), 60);

        // A folder resume reports its skipped whole files; what the console held includes
        // them, so they are not counted twice.
        let p = Arc::new(Progress::default());
        let opened = Arc::new(Opened::default());
        opened.opened(40, 0);
        p.skipped_bytes.store(30, Ordering::Relaxed);
        p.bytes_sent.store(10, Ordering::Relaxed);
        drop(Bridge::start_resumed(p, &cfg, opened));
        assert_eq!(bytes.load(Ordering::Relaxed), 50);

        // A fresh job (or a path without the space check): skipped bytes as before.
        let p = Arc::new(Progress::default());
        p.skipped_bytes.store(30, Ordering::Relaxed);
        p.bytes_sent.store(10, Ordering::Relaxed);
        drop(Bridge::start(p, &cfg));
        assert_eq!(bytes.load(Ordering::Relaxed), 40);
        assert_eq!(live.resumed_bytes.load(Ordering::Relaxed), 0);
    }

    /// #873: the count is what the console has, so "every byte sent" is never read early.
    #[test]
    fn the_count_starts_again_from_what_the_console_holds_at_each_open() {
        let p = Progress::default();
        let opened = Opened::default();
        p.bytes_total.store(100, Ordering::Relaxed);
        // A resume: the console held 60, so it needs 40.
        opened.opened(60, 0);
        assert_eq!(bytes_done(&p, &opened), 60);
        // Frames of the stopped run land after the resume and the resumed run sends the same
        // ranges again: the console reports them durable twice. They are not more of the job.
        p.bytes_sent.store(30, Ordering::Relaxed);
        p.bytes_durable.store(110, Ordering::Relaxed);
        assert_eq!(bytes_done(&p, &opened), 90, "10 bytes are still to come");
        // The helper stops with those 30 acknowledged and 20 of them durable: the next open
        // says 80, and the 10 that were lost are sent again without being counted again.
        opened.opened(80, 30);
        assert_eq!(bytes_done(&p, &opened), 80);
        assert_eq!(
            opened.resumed(),
            60,
            "what the console held when the job first opened"
        );
        p.bytes_sent.store(50, Ordering::Relaxed);
        assert_eq!(bytes_done(&p, &opened), 100);
        // A file that failed a check is sent again: past the job's size (the engine's ticker
        // caps what it shows), and `upload::rechecked_bytes` names it.
        p.bytes_sent.store(57, Ordering::Relaxed);
        assert_eq!(bytes_done(&p, &opened), 107);

        // The console finished the job from bytes this run did not have to send (the stopped
        // run's frames completed a file): done is done.
        let (p, opened) = (Progress::default(), Opened::default());
        p.bytes_total.store(100, Ordering::Relaxed);
        opened.opened(60, 0);
        p.bytes_sent.store(25, Ordering::Relaxed);
        assert_eq!(bytes_done(&p, &opened), 85);
        opened.complete();
        assert_eq!(bytes_done(&p, &opened), 100);

        // A later open that finds more than the job sent (bytes of an earlier run became
        // durable after the first open) raises what the resume is said to have found.
        let opened = Opened::default();
        opened.opened(60, 0);
        opened.opened(75, 5);
        assert_eq!(opened.resumed(), 70);
    }

    #[test]
    fn the_quiet_watch_times_the_stretch_after_the_last_byte() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let after = s(60);
        let mut w = QuietWatch::default();
        // Bytes still on their way: nothing to time.
        assert_eq!(w.observe(t0, false, (10, 0, 0, false, 0), after), None);
        assert_eq!(w.since_all_sent(t0 + s(5)), Duration::ZERO);
        assert_eq!(w.quiet_for(t0 + s(5)), Duration::ZERO);
        // The last byte is acknowledged; the console keeps confirming files for two minutes.
        let t1 = t0 + s(10);
        assert_eq!(w.observe(t1, true, (100, 0, 0, false, 0), after), None);
        for k in 1..=120u64 {
            let note = w.observe(t1 + s(k), true, (100, k, 0, false, 0), after);
            assert_eq!(
                note, None,
                "a console that is getting on with it is not quiet"
            );
        }
        assert_eq!(w.since_all_sent(t1 + s(120)), s(120));
        assert_eq!(w.quiet_for(t1 + s(120)), Duration::ZERO);
        // Then it confirms nothing: said once, at the limit.
        let t2 = t1 + s(120);
        assert_eq!(
            w.observe(t2 + s(59), true, (100, 120, 0, false, 0), after),
            None
        );
        assert_eq!(
            w.observe(t2 + s(60), true, (100, 120, 0, false, 0), after),
            Some(QuietNote::Quiet(s(60)))
        );
        assert_eq!(
            w.observe(t2 + s(300), true, (100, 120, 0, false, 0), after),
            None
        );
        assert_eq!(w.quiet_for(t2 + s(300)), s(300));
        // It moves again (a file done, or the files left to save going down): said, and the
        // next quiet stretch is a new one.
        assert_eq!(
            w.observe(t2 + s(310), true, (100, 121, 0, false, 0), after),
            Some(QuietNote::MovedAgain(s(310)))
        );
        assert_eq!(w.quiet_for(t2 + s(311)), s(1));
        assert_eq!(
            w.observe(t2 + s(369), true, (100, 121, 0, false, 0), after),
            None
        );
        assert_eq!(
            w.observe(t2 + s(370), true, (100, 121, 0, false, 0), after),
            Some(QuietNote::Quiet(s(60)))
        );
        assert_eq!(w.longest(), s(300));
        // A file is asked for again: bytes move, so this is not that stretch any more.
        assert_eq!(
            w.observe(t2 + s(400), false, (40, 120, 0, false, 0), after),
            None
        );
        assert_eq!(w.quiet_for(t2 + s(401)), Duration::ZERO);
        assert_eq!(
            w.since_all_sent(t2 + s(400)),
            s(520),
            "from the first time, for the end"
        );
    }

    #[tokio::test]
    async fn the_snapshot_times_the_stretch_after_the_last_byte() {
        let p = Arc::new(Progress::default());
        let live = Arc::new(ps5upload_core::transfer::LiveNotes::default());
        let mut cfg = TransferConfig::new("h");
        cfg.progress_live = Some(live.clone());
        let opened = Arc::new(Opened::default());
        let bridge = Bridge::start_job(p.clone(), &cfg, opened.clone(), "job".into());
        p.attempts.store(1, Ordering::Relaxed);
        p.bytes_total.store(100, Ordering::Relaxed);
        opened.opened(0, 0);
        p.bytes_sent.store(99, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(live.all_sent_ms.load(Ordering::Relaxed), 0, "a byte to go");
        p.bytes_sent.store(100, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(900)).await;
        let (since, quiet) = (
            live.all_sent_ms.load(Ordering::Relaxed),
            live.console_quiet_ms.load(Ordering::Relaxed),
        );
        assert!((300..5_000).contains(&since), "{since}");
        assert!((300..5_000).contains(&quiet), "{quiet}");
        // The console confirms a file: no longer quiet, still after the last byte.
        p.files_durable.store(1, Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert!(live.console_quiet_ms.load(Ordering::Relaxed) < quiet);
        assert!(live.all_sent_ms.load(Ordering::Relaxed) > since);
        drop(bridge);
        assert_eq!(live.all_sent_ms.load(Ordering::Relaxed), 0, "the job ended");
    }

    #[tokio::test]
    async fn the_bridge_copies_the_live_notes_while_running_and_once_more_on_drop() {
        let p = Arc::new(Progress::default());
        let live = Arc::new(ps5upload_core::transfer::LiveNotes::default());
        let mut cfg = TransferConfig::new("h");
        cfg.progress_live = Some(live.clone());
        let bridge = Bridge::start(p.clone(), &cfg);
        p.bottleneck.store(crate::gen::BN_DISK, Ordering::Relaxed);
        p.phase.store(PHASE_SKIP, Ordering::Relaxed);
        p.skip_done_bytes.store(5, Ordering::Relaxed);
        p.skip_total_bytes.store(20, Ordering::Relaxed);
        p.settling.store(true, Ordering::Relaxed);
        p.unswept.store(7, Ordering::Relaxed);
        p.unswept_peak.store(40, Ordering::Relaxed);
        drop(bridge); // the final store
        assert_eq!(live.bottleneck.load(Ordering::Relaxed), crate::gen::BN_DISK);
        assert_eq!(live.phase.load(Ordering::Relaxed), LIVE_PHASE_SKIPPING);
        assert_eq!(live.skip_done_bytes.load(Ordering::Relaxed), 5);
        assert_eq!(live.skip_total_bytes.load(Ordering::Relaxed), 20);
        assert!(live.settling.load(Ordering::Relaxed));
        assert_eq!(live.unswept.load(Ordering::Relaxed), 7);
        assert_eq!(live.unswept_peak.load(Ordering::Relaxed), 40);
    }
}
