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
            NotReady::Unreachable => write!(f, "the PS5 does not answer on the network"),
        }
    }
}

impl std::error::Error for NotReady {}

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

/// Get `host` ready for a session without touching its Remote Play slot:
/// honours the hold-off, checks DDP, wakes a resting console when `wake`.
pub fn prepare(
    host: &str,
    creds: &SessionCreds,
    wake: bool,
    budget: Duration,
    on_wake: &mut dyn FnMut(),
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
            let deadline = Instant::now() + budget;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if let Ok(s) = ddp::probe(host, Duration::from_secs(2)) {
                    if s.code == 200 {
                        break;
                    }
                }
                if Instant::now() > deadline {
                    crate::core_debug!(
                        "rp guard: {host}: still not awake after {} s",
                        budget.as_secs()
                    );
                    bail!("the PS5 did not wake within {} s", budget.as_secs());
                }
            }
            crate::core_debug!(
                "rp guard: {host}: awake (discovery 200) {} ms after the wake; settling {} s",
                t0.elapsed().as_millis(),
                SETTLE_AFTER_WAKE.as_secs()
            );
            std::thread::sleep(SETTLE_AFTER_WAKE);
            Ok(())
        }
        c => bail!("the PS5 answered discovery with {c} {}", status.status_text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
