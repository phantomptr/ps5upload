//! When it is safe to open a Remote Play session, and how to get there.
//!
//! The safe rules (measured on FW 13.60, see the numbers in each rule):
//!
//! 1. **Ask before you knock.** A session-init is not a free question: it
//!    reserves the console's one Remote Play slot. So readiness is checked
//!    with DDP discovery (UDP 9302), which takes nothing.
//! 2. **A console in rest mode looks half alive.** It answers DDP with 620
//!    ("Server Standby") and still hands out a session nonce, then resets or
//!    refuses the control connection because it has not booted. This is what
//!    looked like a stuck session on the Fat (2026-10-27): it had gone to rest.
//!    So: 620 → wake (if allowed), wait for 200, then let it settle.
//! 3. **"In use" (0x80108b10) means wait, not retry.** Retrying re-reserves
//!    the slot and keeps it held. One attempt, then back off for at least
//!    [`IN_USE_BACKOFF`].
//! 4. **One session per console at a time, even across stops.** After a
//!    session ends the console keeps the slot for a while; a new session is
//!    not opened before [`RELEASE_AFTER_CLEAN_STOP`] (clean stop: DISCONNECT
//!    then closing ctrl, as chiaki does) or [`RELEASE_AFTER_DROP`] (the client
//!    vanished). Measured on the Fat, FW 13.60, one attempt per trial with a
//!    quiet minute between trials: after a clean stop "in use" at 1 s and
//!    5 s, free at 10 s and 15 s; after a frozen client (sockets open, nothing
//!    answered) "in use" at 15 s, free at 30 s.
//! 5. **A console that has gone back to rest mode is not a stuck session.**
//!    Consoles rest on their own idle timer; check DDP first (rule 2). A
//!    console woken for Remote Play was seen staying awake for 3+ minutes
//!    after a clean stop.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::ddp;
use crate::rp_regist::credential_from_regist_key;
use crate::rp_session::SessionCreds;

/// How long the console keeps the slot after a clean stop.
pub const RELEASE_AFTER_CLEAN_STOP: Duration = Duration::from_secs(12);
/// How long it keeps the slot after the client disappeared without a word.
pub const RELEASE_AFTER_DROP: Duration = Duration::from_secs(35);
/// After an "in use" answer.
pub const IN_USE_BACKOFF: Duration = Duration::from_secs(30);
/// From DDP 200 after a wake until the Remote Play service takes sessions.
const SETTLE_AFTER_WAKE: Duration = Duration::from_secs(5);

/// The console's "Remote Play is already in use" reason code.
pub const REASON_IN_USE: &str = "80108b10";

/// Why a session cannot be opened right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReady {
    /// The console is asleep and waking was not allowed.
    Standby,
    /// The slot may still be held; try again after this long.
    Busy(Duration),
    /// The console says Remote Play is in use: mostly another device (the PS Remote Play app,
    /// chiaki), which is not going away by itself; try again after this long.
    InUse(Duration),
    /// No answer to discovery at all.
    Unreachable,
}

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotReady::Standby => write!(f, "the PS5 is in rest mode"),
            NotReady::Busy(d) => write!(
                f,
                "the PS5 is still finishing the last Remote Play session; try again in {} s",
                d.as_secs().max(1)
            ),
            NotReady::InUse(d) => write!(
                f,
                "Remote Play on the PS5 is in use, by another device or a session still finishing; try again in {} s",
                d.as_secs().max(1)
            ),
            NotReady::Unreachable => write!(f, "the PS5 does not answer on the network"),
        }
    }
}

impl std::error::Error for NotReady {}

/// The user stopped while the console was getting ready: no session, and nothing to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "stopped before the session started")
    }
}

impl std::error::Error for Cancelled {}

/// Per console: when it may next be asked for a session.
fn next_allowed() -> &'static Mutex<HashMap<String, Instant>> {
    static M: std::sync::OnceLock<Mutex<HashMap<String, Instant>>> = std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember that `host` must not be asked again for `wait`.
pub fn hold_off(host: &str, wait: Duration) {
    crate::core_debug!(
        "rp guard: {host}: holding off new sessions for {} s",
        wait.as_secs()
    );
    if let Ok(mut m) = next_allowed().lock() {
        let until = Instant::now() + wait;
        let e = m.entry(host.to_string()).or_insert(until);
        if *e < until {
            *e = until;
        }
    }
}

fn held_for(host: &str) -> Option<Duration> {
    let m = next_allowed().lock().ok()?;
    let until = *m.get(host)?;
    until.checked_duration_since(Instant::now())
}

fn regist_key_hex(creds: &SessionCreds) -> String {
    creds
        .regist_key
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Sleeps `d`, looking at `stop` every so often. True when stopped.
fn sleep_or_stop(d: Duration, stop: &mut dyn FnMut() -> bool) -> bool {
    let end = Instant::now() + d;
    loop {
        if stop() {
            return true;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// Waits for `awake` (asked about once a second) within `budget`, then lets the console settle.
fn wait_awake(
    budget: Duration,
    settle: Duration,
    awake: &mut dyn FnMut() -> bool,
    stop: &mut dyn FnMut() -> bool,
) -> Result<()> {
    let deadline = Instant::now() + budget;
    loop {
        if sleep_or_stop(Duration::from_secs(1).min(budget), stop) {
            return Err(Cancelled.into());
        }
        if awake() {
            break;
        }
        if Instant::now() > deadline {
            bail!("the PS5 did not wake within {} s", budget.as_secs());
        }
    }
    if sleep_or_stop(settle, stop) {
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Get `host` ready for a session without touching its Remote Play slot:
/// honours the hold-off, checks DDP, wakes a resting console when `wake`. `stop` ends the wait
/// for a console waking ([`Cancelled`]): nobody is left to watch the session it would lead to.
pub fn prepare(
    host: &str,
    creds: &SessionCreds,
    wake: bool,
    budget: Duration,
    on_wake: &mut dyn FnMut(),
    stop: &mut dyn FnMut() -> bool,
) -> Result<()> {
    if let Some(left) = held_for(host) {
        crate::core_debug!(
            "rp guard: {host}: held off for {} s more (the last session's slot)",
            left.as_secs()
        );
        return Err(NotReady::Busy(left).into());
    }
    let t0 = Instant::now();
    let status = match ddp::probe(host, Duration::from_secs(2)) {
        Ok(s) => s,
        Err(e) => {
            crate::core_debug!(
                "rp guard: {host}: no discovery answer in {} ms: {e:#}",
                t0.elapsed().as_millis()
            );
            return Err(NotReady::Unreachable.into());
        }
    };
    crate::core_debug!(
        "rp guard: {host}: discovery {} {} in {} ms (wake allowed: {wake})",
        status.code,
        status.status_text,
        t0.elapsed().as_millis()
    );
    match status.code {
        200 => Ok(()),
        620 => {
            if !wake {
                return Err(NotReady::Standby.into());
            }
            on_wake();
            let credential = credential_from_regist_key(&regist_key_hex(creds))?;
            ddp::wake(host, &credential.to_string())?;
            let r = wait_awake(
                budget,
                SETTLE_AFTER_WAKE,
                &mut || ddp::probe(host, Duration::from_secs(2)).is_ok_and(|s| s.code == 200),
                stop,
            );
            crate::core_debug!(
                "rp guard: {host}: {} {} ms after the wake",
                match &r {
                    Ok(()) => "awake and settled".to_string(),
                    Err(e) => e.to_string(),
                },
                t0.elapsed().as_millis()
            );
            r
        }
        c => bail!("the PS5 answered discovery with {c} {}", status.status_text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_ends_the_wait_for_a_console_waking() {
        let t0 = Instant::now();
        // Still asleep, then stopped a moment in: over at once, not at the end of the budget.
        let mut polls = 0;
        let r = wait_awake(
            Duration::from_secs(60),
            Duration::from_secs(5),
            &mut || false,
            &mut || {
                polls += 1;
                polls > 3
            },
        );
        assert!(r.unwrap_err().is::<Cancelled>());
        assert!(t0.elapsed() < Duration::from_secs(5));
        // And during the settling.
        let t0 = Instant::now();
        let mut polls = 0;
        let r = wait_awake(
            Duration::from_millis(10),
            Duration::from_secs(5),
            &mut || true,
            &mut || {
                polls += 1;
                polls > 4
            },
        );
        assert!(r.unwrap_err().is::<Cancelled>());
        assert!(t0.elapsed() < Duration::from_secs(4));
    }

    #[test]
    fn a_console_that_wakes_goes_on_and_one_that_never_does_is_said() {
        let r = wait_awake(
            Duration::from_millis(50),
            Duration::from_millis(50),
            &mut || true,
            &mut || false,
        );
        assert!(r.is_ok());
        let r = wait_awake(Duration::ZERO, Duration::ZERO, &mut || false, &mut || false);
        // budget 0: one look, then the answer (a real budget gives it a minute).
        let e = format!("{:#}", r.unwrap_err());
        assert!(e.contains("did not wake"), "{e}");
    }

    #[test]
    fn hold_off_blocks_until_it_passes() {
        hold_off("10.255.0.1", Duration::from_secs(60));
        assert!(held_for("10.255.0.1").is_some());
        assert!(held_for("10.255.0.2").is_none());
        // A shorter hold never shortens a longer one.
        hold_off("10.255.0.1", Duration::from_secs(1));
        assert!(held_for("10.255.0.1").unwrap() > Duration::from_secs(30));
    }
}
