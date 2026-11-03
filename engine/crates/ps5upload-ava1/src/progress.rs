//! Copying AVA1's progress into the counters the engine's ticker reads.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
/// console already held when the job first opened (whole files it skipped, or the durable
/// ranges a resumed file kept): the bar starts where the last run left off and reaches 100%. AVA1 has no per-file "sent" counter;
/// durability is its analogue, so the UI's file counter moves in commit-sized steps —
/// slower but honest, and strictly better than showing 0 for the whole transfer.
/// The engine's ticker reads these counters absolutely, so the bridge `store`s, never
/// `fetch_add`s.
pub struct Bridge {
    handle: tokio::task::AbortHandle,
    counters: Arc<Counters>,
}

struct Counters {
    p: Arc<Progress>,
    bytes: Option<Arc<AtomicU64>>,
    files: Option<Arc<std::sync::atomic::AtomicU64>>,
    files_finalized: Option<Arc<std::sync::atomic::AtomicU64>>,
    bytes_finalized: Option<Arc<std::sync::atomic::AtomicU64>>,
    live: Option<Arc<ps5upload_core::transfer::LiveNotes>>,
    /// Bytes the console held when the job first opened (see [`Bridge::start_resumed`]).
    resumed: Arc<AtomicU64>,
    resumed_out: Option<Arc<AtomicU64>>,
    rates: Mutex<RateHistory>,
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

    /// [`Bridge::start`] with `resumed`: the bytes the console already held when the job
    /// first opened (whole files in place plus durable ranges), set once by the sender's
    /// first open. Counted as done and copied to `cfg.progress_bytes_resumed`.
    pub fn start_resumed(
        p: Arc<Progress>,
        cfg: &TransferConfig,
        resumed: Arc<AtomicU64>,
    ) -> Bridge {
        let counters = Arc::new(Counters {
            p,
            bytes: cfg.progress_bytes.clone(),
            files: cfg.progress_files.clone(),
            files_finalized: cfg.progress_files_finalized.clone(),
            bytes_finalized: cfg.progress_bytes_finalized.clone(),
            live: cfg.progress_live.clone(),
            resumed,
            resumed_out: cfg.progress_bytes_resumed.clone(),
            rates: Mutex::new(RateHistory::new(Instant::now())),
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
}

impl Counters {
    fn store_at(&self, last: bool) {
        let resumed = self.resumed.load(Ordering::Acquire);
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
            // last run stopped and reaches 100%. `resumed` covers the skipped whole files too.
            // Never below what the console reports durable either: a resume right after a
            // cancel finds bytes the earlier run sent landing without this run sending them.
            let had = self.p.skipped_bytes.load(Ordering::Relaxed).max(resumed);
            let done = self
                .p
                .bytes_sent
                .load(Ordering::Relaxed)
                .saturating_add(had)
                .max(self.p.bytes_durable.load(Ordering::Relaxed));
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
        let resumed = Arc::new(AtomicU64::new(0));
        let bridge = Bridge::start_resumed(p.clone(), &cfg, resumed.clone());
        // A single 100-byte file whose first 60 bytes were already durable: no skipped
        // files, 60 resumed, 5 sent this run.
        resumed.store(60, Ordering::Relaxed);
        p.bytes_sent.store(5, Ordering::Relaxed);
        drop(bridge);
        assert_eq!(bytes.load(Ordering::Relaxed), 65);
        assert_eq!(live.resumed_bytes.load(Ordering::Relaxed), 60);

        // A folder resume reports its skipped whole files; `resumed` includes them, so they
        // are not counted twice.
        let p = Arc::new(Progress::default());
        let resumed = Arc::new(AtomicU64::new(40));
        p.skipped_bytes.store(30, Ordering::Relaxed);
        p.bytes_sent.store(10, Ordering::Relaxed);
        drop(Bridge::start_resumed(p, &cfg, resumed));
        assert_eq!(bytes.load(Ordering::Relaxed), 50);

        // A fresh job (or a path without the space check): skipped bytes as before.
        let p = Arc::new(Progress::default());
        p.skipped_bytes.store(30, Ordering::Relaxed);
        p.bytes_sent.store(10, Ordering::Relaxed);
        drop(Bridge::start(p, &cfg));
        assert_eq!(bytes.load(Ordering::Relaxed), 40);
        assert_eq!(live.resumed_bytes.load(Ordering::Relaxed), 0);
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
