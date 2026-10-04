//! The speed governor (SPEC.md §16): lanes, chunk and bundle size from what the link and
//! the receiver report, once per second. Pure: the sender feeds it samples.
use crate::gen::{BN_CREDIT, BN_DISK, BN_NETWORK, BN_SOURCE, BN_WORKERS};

pub const MIN_CHUNK: u32 = 1 << 20;
/// 16 MiB frames include the MAC and the Chunk header, so the largest chunk is 15 MiB.
pub const MAX_CHUNK: u32 = 15 << 20;
pub const START_CHUNK: u32 = 4 << 20;
pub const MIN_BUNDLE: u32 = 256 << 10;
pub const MAX_BUNDLE: u32 = 15 << 20;
pub const START_BUNDLE: u32 = 1 << 20;
pub const START_LANES: u8 = 2;
pub const MAX_LANES: u8 = 8;
/// A lane probe must raise the rate by this factor to keep its lane.
const GAIN: f64 = 1.10;
/// The easier bar while the link is not yet at its best observed rate (review 003 §5): a
/// third lane that adds a few percent is worth keeping then, and reverting it holds the
/// governor back for `HOLD_TICKS`.
const GAIN_BELOW_BEST: f64 = 1.05;
/// "Below its best" means under this share of the best rate seen.
const NEAR_BEST: f64 = 0.90;
/// While lanes are below `PREFER_LANES_UNTIL` and the network is the limit, the chunk is
/// held at this size (more lanes first, bigger frames only when lanes stop helping).
const LANES_FIRST_CHUNK: u32 = 4 << 20;
const PREFER_LANES_UNTIL: u8 = 4;
const HOLD_TICKS: u32 = 30;
const STABLE_TICKS: u32 = 10;
const PROBE_TICKS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Bundle,
    Stream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mixed,
    StreamOnly,
    BundleOnly,
}

#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub secs: f64,
    /// Bytes the receiver confirmed `Received` during this tick.
    pub bytes_acked: u64,
    pub lanes: u8,
    /// Lane deaths plus requeued frames this tick.
    pub stalls: u32,
    /// The sender had frames ready but no credit at some point this tick.
    pub credit_starved: bool,
    /// Lanes had room but the readers had nothing ready.
    pub source_starved: bool,
    /// From the receiver's last Status.
    pub receiver_bottleneck: u8,
    /// Bytes made durable this tick, per class, and what is left of each class.
    pub small_durable: u64,
    pub large_durable: u64,
    pub small_left: u64,
    pub large_left: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    pub lanes: u8,
    pub chunk: u32,
    pub bundle: u32,
    pub bottleneck: u8,
    pub mode: Mode,
    pub prefer: Class,
    pub sequential: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LaneStep {
    Steady,
    Trying { before: f64, wait: u32 },
    Hold(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Probe {
    Warmup(u32),
    Run { mode: Mode, left: u32 },
    Done,
}

/// Governor switches. `Default` is the production behaviour; the pins are for benchmarking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GovernorOptions {
    /// Hold the lane count (1..=MAX_LANES) instead of probing.
    pub pin_lanes: Option<u8>,
    /// Hold the chunk size in bytes (a whole number of MiB, MIN_CHUNK..=MAX_CHUNK).
    pub pin_chunk: Option<u32>,
    /// The §5 policy: probe lanes with a lower gain bar while below the best rate, and do
    /// not grow the chunk past 4 MiB before lanes stop helping. On by default; switch it
    /// off to A/B against the old policy.
    pub lanes_first: bool,
}

impl Default for GovernorOptions {
    fn default() -> Self {
        Self {
            pin_lanes: None,
            pin_chunk: None,
            lanes_first: true,
        }
    }
}

impl GovernorOptions {
    /// `PS5UPLOAD_AVA1_LANES=n` and `PS5UPLOAD_AVA1_CHUNK=m` (MiB) pin the governor, and
    /// `PS5UPLOAD_AVA1_LANES_FIRST=0` turns the §5 policy off. Benchmarking only: a pinned
    /// governor ignores what the link says. Values out of range are ignored.
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok();
        Self::from_vars(
            get("PS5UPLOAD_AVA1_LANES").as_deref(),
            get("PS5UPLOAD_AVA1_CHUNK").as_deref(),
            get("PS5UPLOAD_AVA1_LANES_FIRST").as_deref(),
        )
    }

    pub fn from_vars(
        lanes: Option<&str>,
        chunk_mib: Option<&str>,
        lanes_first: Option<&str>,
    ) -> Self {
        let pin_lanes = lanes
            .and_then(|v| v.trim().parse::<u8>().ok())
            .filter(|n| (1..=MAX_LANES).contains(n));
        let pin_chunk = chunk_mib
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|m| (1..=(MAX_CHUNK >> 20)).contains(m))
            .map(|m| m << 20);
        Self {
            pin_lanes,
            pin_chunk,
            lanes_first: lanes_first.map(|v| v.trim() != "0").unwrap_or(true),
        }
    }
}

pub struct Governor {
    opts: GovernorOptions,
    /// The highest per-tick rate seen (bytes/s).
    best_rate: f64,
    /// A lane probe failed (or lanes are maxed): more lanes no longer help, so the chunk
    /// may grow past `LANES_FIRST_CHUNK`.
    lanes_capped: bool,
    lanes: u8,
    chunk: u32,
    bundle: u32,
    step: LaneStep,
    stable: u32,
    probe: Probe,
    rates: [(f64, f64); 3], // (small, large) durable B/s for Mixed, StreamOnly, BundleOnly
    acc: (f64, f64, f64),   // small bytes, large bytes, seconds in the current probe phase
    sequential: bool,
    mode: Mode,
}

impl Default for Governor {
    fn default() -> Self {
        Self::new()
    }
}

fn idx(m: Mode) -> usize {
    match m {
        Mode::Mixed => 0,
        Mode::StreamOnly => 1,
        Mode::BundleOnly => 2,
    }
}

/// How much a lane may have outstanding, in bytes (SPEC §12.5): two seconds of that lane's
/// throughput, never below a whole chunk. `lane_rate` is **bytes per second** — the caller
/// must pass a rate, not a per-lane byte count or a chunk size.
pub fn inflight_cap(chunk: u32, lane_rate: f64) -> u64 {
    ((lane_rate * 2.0) as u64).max(chunk as u64)
}

/// The most bytes/s a tick may report (1 TB/s): above it the sample is the peer's claim.
const MAX_PLAUSIBLE_RATE: f64 = 1e12;

/// Per-tick decay of the best rate seen.
const BEST_DECAY: f64 = 0.99;

impl Governor {
    pub fn new() -> Self {
        Self::with_options(GovernorOptions::default())
    }

    pub fn with_options(opts: GovernorOptions) -> Self {
        Self {
            opts,
            best_rate: 0.0,
            lanes_capped: false,
            lanes: opts.pin_lanes.unwrap_or(START_LANES),
            chunk: opts.pin_chunk.unwrap_or(START_CHUNK),
            bundle: START_BUNDLE,
            step: LaneStep::Steady,
            stable: 0,
            probe: Probe::Warmup(3),
            rates: [(0.0, 0.0); 3],
            acc: (0.0, 0.0, 0.0),
            sequential: false,
            mode: Mode::Mixed,
        }
    }

    fn decision(&self, bottleneck: u8, s: &Sample) -> Decision {
        let small_rate = self.rates[0].0.max(1.0);
        let large_rate = self.rates[0].1.max(1.0);
        let prefer = if s.small_left as f64 / small_rate >= s.large_left as f64 / large_rate {
            Class::Bundle
        } else {
            Class::Stream
        };
        let mode = if self.sequential {
            if s.small_left > 0 {
                Mode::BundleOnly
            } else {
                Mode::StreamOnly
            }
        } else {
            self.mode
        };
        Decision {
            lanes: self.lanes,
            chunk: self.chunk,
            bundle: self.bundle,
            bottleneck,
            mode,
            prefer,
            sequential: self.sequential,
        }
    }

    pub fn tick(&mut self, s: &Sample) -> Decision {
        if s.secs.is_nan() || s.secs <= 0.0 || s.secs.is_infinite() {
            return self.decision(BN_NETWORK, s);
        }
        // What the peer acknowledged is its claim: a rate no link carries (1 TB/s) must not
        // become the best rate seen, which would hold the lane bar down for the rest of the job.
        let rate = (s.bytes_acked as f64 / s.secs).min(MAX_PLAUSIBLE_RATE);
        let lanes_now = s.lanes.max(1);
        let lane_rate = rate / lanes_now as f64;
        let bottleneck = if s.source_starved {
            BN_SOURCE
        } else if s.credit_starved {
            match s.receiver_bottleneck {
                BN_DISK | BN_WORKERS => s.receiver_bottleneck,
                _ => BN_CREDIT,
            }
        } else {
            BN_NETWORK
        };

        // The best rate fades 1 % a tick, so after a capacity drop (a Wi-Fi roam) the stricter
        // near-the-best bar stops applying and the two bars stay symmetric.
        self.best_rate = (self.best_rate * BEST_DECAY).max(rate);
        let gain = if self.opts.lanes_first && rate < self.best_rate * NEAR_BEST {
            GAIN_BELOW_BEST
        } else {
            GAIN
        };

        // Lanes and chunk.
        if s.stalls > 0 {
            self.lanes = self.lanes.saturating_sub(1).max(1);
            self.chunk = (self.chunk / 2).max(MIN_CHUNK) & !((1 << 20) - 1);
            self.stable = 0;
            self.lanes_capped = false;
            self.step = LaneStep::Hold(HOLD_TICKS / 3);
        } else {
            self.stable += 1;
            if self.stable >= STABLE_TICKS {
                self.stable = 0;
                let mut grown = (self.chunk * 2).min(MAX_CHUNK);
                if self.opts.lanes_first
                    && bottleneck == BN_NETWORK
                    && self.lanes < PREFER_LANES_UNTIL
                    && !self.lanes_capped
                    // A lane pin means lanes are not ours to grow: treat them as capped,
                    // or the chunk would sit at 4 MiB for the whole job.
                    && self.opts.pin_lanes.is_none()
                {
                    // Lanes first: a bigger frame lengthens every decrypt stall on the
                    // console, so hold at 4 MiB (never shrinking what is already larger).
                    grown = grown.min(LANES_FIRST_CHUNK.max(self.chunk));
                }
                self.chunk = grown;
            }
            self.step = match self.step {
                LaneStep::Hold(0) => LaneStep::Steady,
                LaneStep::Hold(n) => LaneStep::Hold(n - 1),
                LaneStep::Trying { before, wait: 0 } => {
                    if rate >= before * gain {
                        LaneStep::Steady
                    } else {
                        self.lanes = self.lanes.saturating_sub(1).max(1);
                        self.lanes_capped = true; // another lane did not help
                        LaneStep::Hold(HOLD_TICKS)
                    }
                }
                LaneStep::Trying { before, wait } => LaneStep::Trying {
                    before,
                    wait: wait - 1,
                },
                LaneStep::Steady
                    if bottleneck == BN_NETWORK
                        && self.lanes < MAX_LANES
                        && self.opts.pin_lanes.is_none() =>
                {
                    self.lanes += 1;
                    LaneStep::Trying {
                        before: rate,
                        wait: 1,
                    }
                }
                st => st,
            };
        }
        if self.lanes >= MAX_LANES {
            self.lanes_capped = true;
        }
        let half_second = ((lane_rate * 0.5) as u32) & !((1 << 20) - 1);
        self.chunk = self.chunk.min(half_second.max(MIN_CHUNK));
        // Pins win over everything the link says (benchmarking).
        if let Some(n) = self.opts.pin_lanes {
            self.lanes = n;
        }
        if let Some(c) = self.opts.pin_chunk {
            self.chunk = c;
        }
        self.bundle = ((lane_rate * 0.25) as u32).clamp(MIN_BUNDLE, MAX_BUNDLE);

        // Mixing check.
        self.probe = match self.probe {
            Probe::Warmup(0) if s.small_left > 0 && s.large_left > 0 => {
                self.acc = (0.0, 0.0, 0.0);
                self.mode = Mode::Mixed;
                Probe::Run {
                    mode: Mode::Mixed,
                    left: PROBE_TICKS,
                }
            }
            Probe::Warmup(0) => Probe::Done,
            Probe::Warmup(n) => Probe::Warmup(n - 1),
            Probe::Run { mode, left } => {
                self.acc.0 += s.small_durable as f64;
                self.acc.1 += s.large_durable as f64;
                self.acc.2 += s.secs;
                if left > 1 {
                    Probe::Run {
                        mode,
                        left: left - 1,
                    }
                } else {
                    self.rates[idx(mode)] = (self.acc.0 / self.acc.2, self.acc.1 / self.acc.2);
                    self.acc = (0.0, 0.0, 0.0);
                    match mode {
                        Mode::Mixed => {
                            self.mode = Mode::StreamOnly;
                            Probe::Run {
                                mode: Mode::StreamOnly,
                                left: PROBE_TICKS,
                            }
                        }
                        Mode::StreamOnly => {
                            self.mode = Mode::BundleOnly;
                            Probe::Run {
                                mode: Mode::BundleOnly,
                                left: PROBE_TICKS,
                            }
                        }
                        Mode::BundleOnly => {
                            let (ms, ml) = self.rates[0];
                            let (sl, ll) = (s.small_left as f64, s.large_left as f64);
                            let mixed = (sl / ms.max(1.0)).max(ll / ml.max(1.0));
                            let seq = sl / self.rates[2].0.max(1.0) + ll / self.rates[1].1.max(1.0);
                            self.sequential = seq < mixed * 0.9;
                            self.mode = Mode::Mixed;
                            Probe::Done
                        }
                    }
                }
            }
            Probe::Done => Probe::Done,
        };
        self.decision(bottleneck, s)
    }
}

/// Where a job's time went, one tick (a second) at a time: the line CUTOVER §4 rows quote
/// (review 003 §2.2 item 3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobSummary {
    ticks: u32,
    credit_starved: u32,
    source_starved: u32,
    /// Credit-starved while the receiver reported disk or workers as its limit.
    receiver_bound: u32,
    lanes_sum: u64,
    chunk_sum: u64,
    receiver_bottleneck: u8,
}

fn bottleneck_name(b: u8) -> &'static str {
    match b {
        BN_NETWORK => "network",
        BN_SOURCE => "source",
        BN_DISK => "disk",
        BN_WORKERS => "workers",
        BN_CREDIT => "credit",
        _ => "none",
    }
}

impl JobSummary {
    /// One governor tick: the sample it was fed and the decision it made.
    pub fn observe(&mut self, s: &Sample, d: &Decision) {
        self.ticks += 1;
        self.credit_starved += u32::from(s.credit_starved);
        self.source_starved += u32::from(s.source_starved);
        self.receiver_bound +=
            u32::from(s.credit_starved && matches!(s.receiver_bottleneck, BN_DISK | BN_WORKERS));
        self.lanes_sum += u64::from(s.lanes);
        self.chunk_sum += u64::from(d.chunk);
        self.receiver_bottleneck = s.receiver_bottleneck;
    }

    /// `None` for a job too short to have had a tick.
    pub fn line(&self) -> Option<String> {
        if self.ticks == 0 {
            return None;
        }
        let n = f64::from(self.ticks);
        let pct = |c: u32| f64::from(c) * 100.0 / n;
        Some(format!(
            "[ava1] job bottlenecks over {} ticks: credit-starved {:.0}%, source-starved {:.0}%, receiver-bound {:.0}%; receiver reported {}; avg lanes {:.1}, avg chunk {:.1} MiB",
            self.ticks,
            pct(self.credit_starved),
            pct(self.source_starved),
            pct(self.receiver_bound),
            bottleneck_name(self.receiver_bottleneck),
            self.lanes_sum as f64 / n,
            self.chunk_sum as f64 / n / f64::from(1u32 << 20),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link where each lane carries `per_lane` B/s up to a `cap`.
    fn net(g: &mut Governor, per_lane: f64, cap: f64, ticks: usize) -> Decision {
        let mut d = g.tick(&Sample::default());
        for _ in 0..ticks {
            let rate = (d.lanes as f64 * per_lane).min(cap);
            d = g.tick(&Sample {
                secs: 1.0,
                bytes_acked: rate as u64,
                lanes: d.lanes,
                ..Default::default()
            });
        }
        d
    }

    #[test]
    fn hostile_samples_cannot_panic_or_pin_the_governor() {
        let mut g = Governor::new();
        let hostile = [
            (u64::MAX, 1e-300),
            (u64::MAX, f64::MIN_POSITIVE),
            (u64::MAX, 1.0),
            (1, f64::INFINITY),
            (1, f64::NAN),
            (0, -1.0),
        ];
        for (acked, secs) in hostile {
            for stalls in [0, 3] {
                let d = g.tick(&Sample {
                    secs,
                    bytes_acked: acked,
                    stalls,
                    lanes: 0,
                    small_left: u64::MAX,
                    large_left: u64::MAX,
                    small_durable: u64::MAX,
                    large_durable: u64::MAX,
                    receiver_bottleneck: 200,
                    ..Sample::default()
                });
                assert!((1..=MAX_LANES).contains(&d.lanes));
                assert!((MIN_CHUNK..=MAX_CHUNK).contains(&d.chunk), "{}", d.chunk);
                assert!((MIN_BUNDLE..=MAX_BUNDLE).contains(&d.bundle));
            }
        }
        assert!(g.best_rate.is_finite() && g.best_rate <= MAX_PLAUSIBLE_RATE);
        assert_eq!(inflight_cap(4 << 20, f64::INFINITY), u64::MAX);
        assert_eq!(inflight_cap(4 << 20, f64::NAN), 4 << 20);
    }

    #[test]
    fn lanes_grow_while_throughput_rises_and_stop_at_the_ceiling() {
        let mut g = Governor::new();
        let d = net(&mut g, 30e6, 110e6, 40);
        assert_eq!(d.lanes, 4, "3→4 gains 22 %, 4→5 gains nothing");
        assert_eq!(d.bottleneck, crate::gen::BN_NETWORK);
    }

    #[test]
    fn a_single_window_limited_lane_still_gets_company() {
        // Wi-Fi: 52 MB/s per lane (512 KiB rcvbuf / 10 ms), 300 MB/s link.
        let mut g = Governor::new();
        assert!(net(&mut g, 52e6, 300e6, 60).lanes >= 5);
    }

    #[test]
    fn a_stall_drops_a_lane_and_halves_the_chunk() {
        let mut g = Governor::new();
        let d0 = net(&mut g, 30e6, 110e6, 40);
        // 200 MB/s keeps the half-second cap out of the way, so only the halving shows.
        let d = g.tick(&Sample {
            secs: 1.0,
            bytes_acked: 200_000_000,
            lanes: d0.lanes,
            stalls: 1,
            ..Default::default()
        });
        assert_eq!(d.lanes, d0.lanes - 1);
        assert_eq!(d.chunk, (d0.chunk / 2).max(MIN_CHUNK) & !((1 << 20) - 1));
    }

    #[test]
    fn the_chunk_grows_back_when_stable_and_stays_whole_groups() {
        let mut g = Governor::new();
        let _ = g.tick(&Sample {
            secs: 1.0,
            bytes_acked: 1,
            lanes: 2,
            stalls: 1,
            ..Default::default()
        });
        let d = net(&mut g, 60e6, 110e6, 40);
        assert!(d.chunk > MIN_CHUNK);
        assert_eq!(d.chunk % (1 << 20), 0);
        assert!(d.chunk <= MAX_CHUNK);
    }

    #[test]
    fn the_bottleneck_follows_who_waits() {
        let mut g = Governor::new();
        let s = Sample {
            secs: 1.0,
            bytes_acked: 10,
            lanes: 2,
            source_starved: true,
            ..Default::default()
        };
        assert_eq!(g.tick(&s).bottleneck, crate::gen::BN_SOURCE);
        let s = Sample {
            secs: 1.0,
            bytes_acked: 10,
            lanes: 2,
            credit_starved: true,
            receiver_bottleneck: crate::gen::BN_DISK,
            ..Default::default()
        };
        let d = g.tick(&s);
        assert_eq!(d.bottleneck, crate::gen::BN_DISK);
        assert_eq!(
            d.lanes, 2,
            "lanes never grow while the receiver is the limit"
        );
    }

    #[test]
    fn bundles_scale_with_the_link() {
        let mut g = Governor::new();
        let fast = net(&mut g, 100e6, 110e6, 10).bundle;
        let mut g = Governor::new();
        let slow = net(&mut g, 2e6, 2e6, 10).bundle;
        assert!(fast > slow);
        assert!(
            (MIN_BUNDLE..=MAX_BUNDLE).contains(&fast) && (MIN_BUNDLE..=MAX_BUNDLE).contains(&slow)
        );
    }

    /// A disk where running both classes at once halves each one's rate (a seeking HDD).
    fn mixing(penalty: f64) -> Decision {
        let mut g = Governor::new();
        let mut d = g.tick(&Sample::default());
        let (mut small_left, mut large_left) = (400e6, 4e9);
        for _ in 0..60 {
            let (sr, lr) = match d.mode {
                Mode::Mixed => (20e6 * penalty, 80e6 * penalty),
                Mode::StreamOnly => (0.0, 80e6),
                Mode::BundleOnly => (20e6, 0.0),
            };
            small_left -= sr;
            large_left -= lr;
            d = g.tick(&Sample {
                secs: 1.0,
                bytes_acked: (sr + lr) as u64,
                lanes: d.lanes,
                small_durable: sr as u64,
                large_durable: lr as u64,
                small_left: small_left as u64,
                large_left: large_left as u64,
                ..Default::default()
            });
        }
        d
    }

    #[test]
    fn mixing_that_slows_the_disk_switches_to_sequential() {
        assert!(mixing(0.4).sequential);
        assert!(!mixing(1.0).sequential);
    }

    #[test]
    fn the_inflight_cap_is_two_seconds_of_a_lane_but_at_least_a_chunk() {
        assert_eq!(inflight_cap(4 << 20, 10e6), 20_000_000);
        assert_eq!(inflight_cap(4 << 20, 1e6), 4 << 20);
    }

    /// Ruling answer 1: both classes queued at the gate, one runs dry during the probe —
    /// the probe still completes (no panic, `Probe` ends in `Done`) and never starts twice.
    #[test]
    fn a_class_running_dry_during_the_probe_still_reaches_a_decision() {
        let mut g = Governor::new();
        let (mut small_left, mut large_left) = (100_000_000u64, 4_000_000_000u64);
        for _ in 0..25 {
            // The small class drains during the stream-only phase of the probe.
            let sr = small_left.min(10_000_000);
            let lr = large_left.min(10_000_000);
            small_left -= sr;
            large_left -= lr;
            g.tick(&Sample {
                secs: 1.0,
                bytes_acked: sr + lr,
                lanes: 2,
                small_durable: sr,
                large_durable: lr,
                small_left,
                large_left,
                ..Default::default()
            });
        }
        assert!(
            matches!(g.probe, Probe::Done),
            "the probe completes instead of hanging"
        );
        // A later tick with both classes queued again must not start a second probe.
        g.tick(&Sample {
            secs: 1.0,
            bytes_acked: 1,
            lanes: 2,
            small_left: 1,
            large_left: 1,
            ..Default::default()
        });
        assert!(matches!(g.probe, Probe::Done), "the probe never runs twice");
    }

    // ---- review 003 §5: lanes first, and the benchmark pins ----------------------------

    fn steady(rate: f64, lanes: u8) -> Sample {
        Sample {
            secs: 1.0,
            bytes_acked: rate as u64,
            lanes,
            ..Default::default()
        }
    }

    /// A governor one tick away from judging a third lane that was added at `before` B/s,
    /// having seen `best` B/s earlier.
    fn probing(opts: GovernorOptions, before: f64, best: f64) -> Governor {
        let mut g = Governor::with_options(opts);
        g.lanes = 3;
        g.best_rate = best;
        g.step = LaneStep::Trying { before, wait: 0 };
        g
    }

    #[test]
    fn a_small_lane_gain_is_kept_while_the_link_is_below_its_best() {
        // Best seen 100 MB/s; now 80 (below 90 %). The third lane lifts it 7 %: kept at the
        // 1.05 bar, reverted at the old 1.10 bar.
        let mut g = probing(GovernorOptions::default(), 80e6, 100e6);
        let d = g.tick(&steady(85.6e6, 3));
        assert_eq!(d.lanes, 3, "a 7 % gain below the best rate keeps the lane");
        let mut old = probing(
            GovernorOptions {
                lanes_first: false,
                ..Default::default()
            },
            80e6,
            100e6,
        );
        assert_eq!(
            old.tick(&steady(85.6e6, 3)).lanes,
            2,
            "the old bar reverts it"
        );
    }

    #[test]
    fn near_the_best_rate_the_lane_must_earn_ten_percent() {
        // 95 MB/s is within 90 % of the 100 MB/s best: the bar is 1.10, 7 % is not enough.
        let mut g = probing(GovernorOptions::default(), 88.8e6, 100e6);
        assert_eq!(g.tick(&steady(95e6, 3)).lanes, 2);
        // And exactly at the best rate it still needs 10 %.
        let mut g = probing(GovernorOptions::default(), 91e6, 100e6);
        assert_eq!(g.tick(&steady(100e6, 3)).lanes, 2, "9.9 % < 10 %");
        let mut g = probing(GovernorOptions::default(), 90e6, 100e6);
        assert_eq!(g.tick(&steady(100e6, 3)).lanes, 3, "11 % keeps it");
    }

    #[test]
    fn the_best_rate_fades_one_percent_a_tick_and_never_below_the_current_rate() {
        let mut g = Governor::new();
        g.best_rate = 100e6;
        g.tick(&steady(50e6, 2));
        assert!((g.best_rate - 99e6).abs() < 1.0, "{}", g.best_rate);
        for _ in 0..80 {
            g.tick(&steady(50e6, 2));
        }
        assert!(
            g.best_rate < 50e6 * 1.001,
            "decayed to the rate: {}",
            g.best_rate
        );
        assert!(g.best_rate >= 50e6, "never below the rate");
        // A faster tick takes over at once.
        g.tick(&steady(120e6, 2));
        assert!((g.best_rate - 120e6).abs() < 1.0);
    }

    /// Ticks a clean link where the network is the limit, with `lanes` held.
    fn run_stable(g: &mut Governor, lanes: u8, ticks: usize) -> Decision {
        let mut d = g.tick(&Sample::default());
        for _ in 0..ticks {
            d = g.tick(&steady(200e6, lanes));
        }
        d
    }

    #[test]
    fn the_chunk_stays_at_four_mib_while_lanes_are_few_and_the_network_limits() {
        let mut g = Governor::new();
        // Pretend the lane probe is mid-hold so lanes stay at 2 while time passes.
        g.step = LaneStep::Hold(1_000);
        let d = run_stable(&mut g, 2, 60);
        assert_eq!(d.chunk, 4 << 20, "no growth past 4 MiB with 2 lanes");
    }

    #[test]
    fn the_chunk_grows_once_lanes_stop_helping_or_reach_four() {
        // Lanes at 4: free to grow.
        let mut g = Governor::new();
        g.lanes = 4;
        g.step = LaneStep::Hold(1_000);
        assert!(run_stable(&mut g, 4, 40).chunk > 4 << 20);
        // Lanes at 2 but a probe already failed: lanes stopped helping, so grow.
        let mut g = Governor::new();
        g.step = LaneStep::Hold(1_000);
        g.lanes_capped = true;
        assert!(run_stable(&mut g, 2, 40).chunk > 4 << 20);
        // And a receiver-bound job is not the network: the rule does not apply.
        let mut g = Governor::new();
        g.step = LaneStep::Hold(1_000);
        let mut d = g.tick(&Sample::default());
        for _ in 0..40 {
            d = g.tick(&Sample {
                credit_starved: true,
                receiver_bottleneck: crate::gen::BN_DISK,
                ..steady(200e6, 2)
            });
        }
        assert!(d.chunk > 4 << 20);
    }

    #[test]
    fn with_lanes_first_off_the_chunk_grows_as_before() {
        let mut g = Governor::with_options(GovernorOptions {
            lanes_first: false,
            ..Default::default()
        });
        g.step = LaneStep::Hold(1_000);
        assert!(run_stable(&mut g, 2, 40).chunk > 4 << 20);
    }

    #[test]
    fn a_failed_probe_marks_lanes_as_not_helping() {
        let mut g = probing(GovernorOptions::default(), 100e6, 100e6);
        assert!(!g.lanes_capped);
        g.tick(&steady(100e6, 3)); // no gain
        assert!(g.lanes_capped);
    }

    #[test]
    fn pinned_lanes_and_chunk_are_honoured_through_everything() {
        let opts = GovernorOptions {
            pin_lanes: Some(4),
            pin_chunk: Some(8 << 20),
            lanes_first: true,
        };
        let mut g = Governor::with_options(opts);
        let first = g.tick(&Sample::default());
        assert_eq!((first.lanes, first.chunk), (4, 8 << 20));
        for i in 0..80 {
            // A slow, stalling, starved link must not move a pin.
            let d = g.tick(&Sample {
                secs: 1.0,
                bytes_acked: 1_000_000,
                lanes: 4,
                stalls: u32::from(i % 7 == 0),
                credit_starved: i % 3 == 0,
                ..Default::default()
            });
            assert_eq!((d.lanes, d.chunk), (4, 8 << 20), "tick {i}");
        }
        let d = net(&mut Governor::with_options(opts), 100e6, 110e6, 50);
        assert_eq!((d.lanes, d.chunk), (4, 8 << 20));
    }

    #[test]
    fn a_lane_pin_below_four_lanes_does_not_hold_the_chunk_at_four_mib() {
        let mut g = Governor::with_options(GovernorOptions {
            pin_lanes: Some(2),
            ..Default::default()
        });
        let d = run_stable(&mut g, 2, 60);
        assert_eq!(d.lanes, 2);
        assert!(d.chunk > 4 << 20, "chunk {}", d.chunk);
    }

    #[test]
    fn a_lane_pin_alone_leaves_the_chunk_to_the_governor() {
        let mut g = Governor::with_options(GovernorOptions {
            pin_lanes: Some(3),
            ..Default::default()
        });
        let d = net(&mut g, 100e6, 300e6, 40);
        assert_eq!(d.lanes, 3);
        assert!(d.chunk >= MIN_CHUNK);
    }

    #[test]
    fn the_env_knobs_parse_and_ignore_nonsense() {
        let o = GovernorOptions::from_vars(Some("6"), Some("4"), None);
        assert_eq!(
            (o.pin_lanes, o.pin_chunk, o.lanes_first),
            (Some(6), Some(4 << 20), true)
        );
        let o = GovernorOptions::from_vars(Some("0"), Some("16"), Some("0"));
        assert_eq!(
            (o.pin_lanes, o.pin_chunk, o.lanes_first),
            (None, None, false)
        );
        let o = GovernorOptions::from_vars(Some("9"), Some("0"), None);
        assert_eq!((o.pin_lanes, o.pin_chunk), (None, None));
        let o = GovernorOptions::from_vars(Some(" 2 "), Some("15"), None);
        assert_eq!((o.pin_lanes, o.pin_chunk), (Some(2), Some(15 << 20)));
        let o = GovernorOptions::from_vars(Some("x"), Some(""), None);
        assert_eq!(o, GovernorOptions::default());
    }

    #[test]
    fn the_job_summary_says_where_the_time_went() {
        let mut j = JobSummary::default();
        assert_eq!(j.line(), None, "no ticks, no line");
        let d = |chunk| Decision {
            lanes: 4,
            chunk,
            bundle: 0,
            bottleneck: crate::gen::BN_NETWORK,
            mode: Mode::Mixed,
            prefer: Class::Stream,
            sequential: false,
        };
        let ticks = [
            (false, false, 0, 2u8, 4u32 << 20),
            (true, false, crate::gen::BN_DISK, 4, 4 << 20),
            (true, false, crate::gen::BN_NONE, 4, 8 << 20),
            (false, true, crate::gen::BN_DISK, 6, 8 << 20),
        ];
        for (credit, source, rbn, lanes, chunk) in ticks {
            j.observe(
                &Sample {
                    secs: 1.0,
                    lanes,
                    credit_starved: credit,
                    source_starved: source,
                    receiver_bottleneck: rbn,
                    ..Default::default()
                },
                &d(chunk),
            );
        }
        let line = j.line().unwrap();
        assert!(line.contains("over 4 ticks"), "{line}");
        assert!(line.contains("credit-starved 50%"), "{line}");
        assert!(line.contains("source-starved 25%"), "{line}");
        assert!(
            line.contains("receiver-bound 25%"),
            "only the disk tick that was starved: {line}"
        );
        assert!(line.contains("receiver reported disk"), "{line}");
        assert!(line.contains("avg lanes 4.0"), "{line}");
        assert!(line.contains("avg chunk 6.0 MiB"), "{line}");
    }
}
