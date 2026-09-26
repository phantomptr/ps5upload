//! Collapsing repeated request failures in the log.
//!
//! A console that is off answers every status poll with 502, once a
//! second, forever. Logged plainly that buries everything else: a real
//! failure elsewhere scrolls past in a wall of identical lines, and the
//! user reading their log learns nothing they did not know after the
//! first one.
//!
//! So: warn on the first failure, stay quiet while it keeps failing,
//! warn again occasionally so a long outage is still visible, and warn
//! once on recovery with the count of what was suppressed. The full
//! per-request trace still goes to the debug log, which is what the
//! crash-trace relies on -- this only governs what reaches the user at
//! the default level.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a continuing failure stays quiet before it says so again.
/// Long enough that a console left off is not noisy; short enough that
/// somebody watching the log sees the problem is ongoing.
pub const REPEAT_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
struct FailState {
    /// Failures seen since the last time we logged this key.
    suppressed: u64,
    last_logged: Instant,
}

/// What the caller should do about one request outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAction {
    /// Log it as a warning. `suppressed` is how many identical failures
    /// went unlogged since the last warning (0 for a first failure).
    Warn { suppressed: u64 },
    /// A previously-failing endpoint answered again.
    Recovered { suppressed: u64 },
    /// Say nothing; this is more of what we already reported.
    Quiet,
}

#[derive(Default)]
pub struct FailureLog {
    seen: HashMap<String, FailState>,
}

impl FailureLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one request outcome and say whether it is worth logging.
    ///
    /// `key` identifies the endpoint (method + path). `now` is passed in
    /// so the behaviour is testable without sleeping.
    pub fn observe(&mut self, key: &str, failed: bool, now: Instant) -> LogAction {
        if failed {
            match self.seen.get_mut(key) {
                None => {
                    // First failure for this endpoint — always say so.
                    self.seen.insert(
                        key.to_string(),
                        FailState {
                            suppressed: 0,
                            last_logged: now,
                        },
                    );
                    LogAction::Warn { suppressed: 0 }
                }
                Some(st) => {
                    if now.duration_since(st.last_logged) >= REPEAT_AFTER {
                        let suppressed = st.suppressed;
                        st.suppressed = 0;
                        st.last_logged = now;
                        LogAction::Warn { suppressed }
                    } else {
                        st.suppressed += 1;
                        LogAction::Quiet
                    }
                }
            }
        } else {
            // Success. Only interesting if it follows a failure.
            match self.seen.remove(key) {
                Some(st) => LogAction::Recovered {
                    suppressed: st.suppressed,
                },
                None => LogAction::Quiet,
            }
        }
    }
}

/// Process-wide state for the request logger.
pub fn failure_log() -> &'static Mutex<FailureLog> {
    static LOG: std::sync::OnceLock<Mutex<FailureLog>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| Mutex::new(FailureLog::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn first_failure_is_always_reported() {
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        assert_eq!(
            f.observe("GET /api/ps5/status", true, t0),
            LogAction::Warn { suppressed: 0 }
        );
    }

    #[test]
    fn repeats_stay_quiet_until_the_interval_passes() {
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        f.observe("k", true, t0);
        // A console polled once a second for the next minute.
        for s in 1..60 {
            assert_eq!(f.observe("k", true, at(t0, s)), LogAction::Quiet, "at {s}s");
        }
        // Past the interval it speaks again, and says how many it ate.
        assert_eq!(
            f.observe("k", true, at(t0, 60)),
            LogAction::Warn { suppressed: 59 }
        );
    }

    #[test]
    fn counter_resets_after_each_report() {
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        f.observe("k", true, t0);
        for s in 1..=60 {
            f.observe("k", true, at(t0, s));
        }
        // Second window: the count covers only that window, not all time.
        for s in 61..120 {
            f.observe("k", true, at(t0, s));
        }
        assert_eq!(
            f.observe("k", true, at(t0, 120)),
            LogAction::Warn { suppressed: 59 }
        );
    }

    #[test]
    fn recovery_is_reported_with_what_was_suppressed() {
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        f.observe("k", true, t0);
        for s in 1..10 {
            f.observe("k", true, at(t0, s));
        }
        assert_eq!(
            f.observe("k", false, at(t0, 10)),
            LogAction::Recovered { suppressed: 9 }
        );
    }

    #[test]
    fn success_without_a_prior_failure_says_nothing() {
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        assert_eq!(f.observe("k", false, t0), LogAction::Quiet);
        // And repeated successes stay quiet.
        assert_eq!(f.observe("k", false, at(t0, 1)), LogAction::Quiet);
    }

    #[test]
    fn a_failure_after_recovery_reports_again() {
        // Flapping must not be silent: each new outage gets a line.
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        f.observe("k", true, t0);
        f.observe("k", false, at(t0, 1));
        assert_eq!(
            f.observe("k", true, at(t0, 2)),
            LogAction::Warn { suppressed: 0 }
        );
    }

    #[test]
    fn endpoints_are_tracked_independently() {
        // One console being off must not silence a different endpoint's
        // first failure.
        let mut f = FailureLog::new();
        let t0 = Instant::now();
        f.observe("GET /a", true, t0);
        assert_eq!(
            f.observe("GET /b", true, at(t0, 1)),
            LogAction::Warn { suppressed: 0 }
        );
        assert_eq!(f.observe("GET /a", true, at(t0, 2)), LogAction::Quiet);
    }
}
