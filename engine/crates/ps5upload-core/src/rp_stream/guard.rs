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

/// How long to leave a console alone after its `n`th session start in a row closed with no
/// answer (1-based). Each start is a knock on the console's one slot, so they spread out.
pub fn closed_wait(n: u32) -> Duration {
    const WAITS: [u64; 5] = [15, 30, 60, 90, 120];
    Duration::from_secs(WAITS[(n.max(1) as usize - 1).min(WAITS.len() - 1)])
}

/// Session starts in a row each console closed with no answer, and when the last one was.
fn closed_answers() -> &'static Mutex<HashMap<String, (u32, Instant)>> {
    static M: std::sync::OnceLock<Mutex<HashMap<String, (u32, Instant)>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A run of closed answers older than this is over: the next one counts from 1 again.
const CLOSED_RUN_GAP: Duration = Duration::from_secs(10 * 60);

/// Per console: what discovery said just before the last session start, for the line a closed
/// start writes (#720: a console on FW 13.00 closed every start for 40 minutes, or until it was
/// restarted, after sessions that had ended cleanly; the log had nothing about its state).
fn last_discovery() -> &'static Mutex<HashMap<String, ddp::DdpStatus>> {
    static M: std::sync::OnceLock<Mutex<HashMap<String, ddp::DdpStatus>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn note_discovery(host: &str, status: &ddp::DdpStatus) {
    if let Ok(mut m) = last_discovery().lock() {
        m.insert(host.to_string(), status.clone());
    }
}

/// How our last session with a console ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEnd {
    /// We closed it (a stop, or an error on our side): DISCONNECT over Takion, then ctrl closed.
    Stopped,
    /// The PS5 ended it, with the reason it gave: "Server shutting down" when a controller at the
    /// console or another device took over (#769). We still said goodbye and closed ctrl.
    ByConsole(String),
    /// It broke off (an error, the page gone) with no clean goodbye.
    Dropped,
}

impl SessionEnd {
    fn describe(&self) -> String {
        match self {
            SessionEnd::Stopped => "closed by us".to_string(),
            SessionEnd::ByConsole(r) => format!("ended by the PS5 ({r:?})"),
            SessionEnd::Dropped => "broke off".to_string(),
        }
    }
}

/// Per console: how and when our last session there ended.
fn last_end() -> &'static Mutex<HashMap<String, (SessionEnd, Instant)>> {
    static M: std::sync::OnceLock<Mutex<HashMap<String, (SessionEnd, Instant)>>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Our session with `host` ended (it had a nonce). Nothing of it is kept that a new session
/// would reuse: the nonce, keys and sockets were that session's own. What is kept: a short
/// hold-off while the console lets go (rule 4; the same after the PS5 ended it itself, it was
/// not measured apart), a fresh count of closed starts, and how it ended, for the next closed
/// start's line.
pub fn session_ended(host: &str, end: SessionEnd) {
    if let Ok(mut m) = closed_answers().lock() {
        m.remove(host);
    }
    let wait = match end {
        SessionEnd::Dropped => RELEASE_AFTER_DROP,
        SessionEnd::Stopped | SessionEnd::ByConsole(_) => RELEASE_AFTER_CLEAN_STOP,
    };
    crate::clog!(
        Info,
        "rp",
        "rp guard: {host}: our session {}; next start in {} s at the earliest",
        end.describe(),
        wait.as_secs()
    );
    hold_off(host, wait);
    if let Ok(mut m) = last_end().lock() {
        m.insert(host.to_string(), (end, Instant::now()));
    }
}

/// How our last session with `host` ended and how long ago, for a log line.
fn last_end_text(host: &str) -> String {
    match last_end().lock().ok().and_then(|m| m.get(host).cloned()) {
        Some((end, at)) => format!(
            "our last session here {} {} s before",
            end.describe(),
            at.elapsed().as_secs()
        ),
        None => "no session of ours here since the engine started".to_string(),
    }
}

/// The closed start's line: which in a row, what discovery said just before (its status, the app
/// it said runs, its firmware), how our last session there ended, and the wait. No names or ids
/// of the console.
pub(crate) fn closed_line(host: &str, closed: &InitClosed, ddp: Option<&ddp::DdpStatus>) -> String {
    let seen = match ddp {
        Some(d) => format!(
            "discovery said {} {}, running {}, firmware {}",
            d.code,
            d.status_text,
            if d.running_app_titleid.is_empty() {
                "nothing"
            } else {
                d.running_app_titleid.as_str()
            },
            if d.system_version.is_empty() {
                "?"
            } else {
                d.system_version.as_str()
            },
        ),
        None => "no discovery answer on record".to_string(),
    };
    format!(
        "rp guard: {host}: the PS5 closed session start {} in a row with no answer ({seen}; {}); asking again in {} s",
        closed.in_a_row,
        last_end_text(host),
        closed.wait.as_secs()
    )
}

/// `host` closed a session start with no answer: counts it and holds off new starts for
/// [`closed_wait`]. Returns which in a row it is and the wait.
pub fn closed_answer(host: &str) -> InitClosed {
    let now = Instant::now();
    let n = closed_answers()
        .lock()
        .map(|mut m| {
            let e = m.entry(host.to_string()).or_insert((0, now));
            if now.duration_since(e.1) > CLOSED_RUN_GAP {
                e.0 = 0;
            }
            e.0 += 1;
            e.1 = now;
            e.0
        })
        .unwrap_or(1);
    let wait = closed_wait(n);
    hold_off(host, wait);
    let closed = InitClosed { in_a_row: n, wait };
    let ddp = last_discovery()
        .lock()
        .ok()
        .and_then(|m| m.get(host).cloned());
    crate::clog!(Info, "rp", "{}", closed_line(host, &closed, ddp.as_ref()));
    closed
}

/// The console gave a nonce: whatever held its slot has let go.
pub fn slot_answered(host: &str) {
    if let Ok(mut m) = closed_answers().lock() {
        m.remove(host);
    }
}

/// The console closed a session start without a word (no nonce, no "in use" code): its one
/// Remote Play slot is still taken, by another device or by a session it has not let go of.
/// Seen on FW 13.00 for minutes after our own stop (#720). Nothing is asked again before `wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitClosed {
    /// Which closed answer in a row this is.
    pub in_a_row: u32,
    pub wait: Duration,
}

impl std::fmt::Display for InitClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({} in a row; asking again in {} s)",
            crate::rp_session::SESSION_INIT_CLOSED,
            self.in_a_row,
            self.wait.as_secs()
        )
    }
}

impl std::error::Error for InitClosed {}

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
    note_discovery(host, &status);
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
    fn closed_answers_in_a_row_wait_longer_each_time_and_a_nonce_starts_over() {
        assert_eq!(closed_wait(1), Duration::from_secs(15));
        assert_eq!(closed_wait(3), Duration::from_secs(60));
        assert_eq!(closed_wait(50), Duration::from_secs(120));
        let host = "10.255.0.9";
        let a = closed_answer(host);
        let b = closed_answer(host);
        assert_eq!((a.in_a_row, b.in_a_row), (1, 2));
        assert_eq!(b.wait, Duration::from_secs(30));
        // Nothing knocks again before the wait is over.
        assert!(held_for(host).unwrap() > Duration::from_secs(25));
        // The text still says what the console did.
        assert!(b
            .to_string()
            .contains(crate::rp_session::SESSION_INIT_CLOSED));
        slot_answered(host);
        assert_eq!(closed_answer(host).in_a_row, 1);
    }

    #[test]
    fn a_closed_start_says_what_discovery_saw_just_before() {
        let closed = InitClosed {
            in_a_row: 3,
            wait: Duration::from_secs(60),
        };
        let d = ddp::DdpStatus {
            code: 200,
            status_text: "Ok".into(),
            host_name: "Living room".into(),
            host_id: "AABBCCDDEEFF".into(),
            system_version: "13000000".into(),
            running_app_titleid: "PPSA01234".into(),
            ..Default::default()
        };
        let line = closed_line("10.255.0.7", &closed, Some(&d));
        assert_eq!(
            line,
            "rp guard: 10.255.0.7: the PS5 closed session start 3 in a row with no answer \
             (discovery said 200 Ok, running PPSA01234, firmware 13000000; no session of ours \
             here since the engine started); asking again in 60 s"
        );
        // Nothing that names the console.
        assert!(!line.contains("Living room") && !line.contains("AABBCCDDEEFF"));
        let home = ddp::DdpStatus {
            code: 200,
            status_text: "Ok".into(),
            ..Default::default()
        };
        assert!(closed_line("h", &closed, Some(&home)).contains("running nothing, firmware ?"));
        assert!(closed_line("h", &closed, None).contains("no discovery answer on record"));
        // What prepare saw is what the next closed start reports.
        note_discovery("10.255.0.8", &d);
        assert_eq!(last_discovery().lock().unwrap().get("10.255.0.8"), Some(&d));
    }

    #[test]
    fn a_session_the_ps5_ended_leaves_nothing_behind_but_a_short_wait() {
        // #769: a controller at the console took the session over ("Server shutting down").
        // A run of closed starts from before counts from 1 again, the wait is the clean stop's
        // (not the dropped client's 35 s), and the next closed start's line says how it ended.
        let kicked = || SessionEnd::ByConsole("Server shutting down".into());
        let fresh = "10.255.0.10";
        session_ended(fresh, kicked());
        let left = held_for(fresh).unwrap();
        assert!(
            left <= RELEASE_AFTER_CLEAN_STOP && left > RELEASE_AFTER_CLEAN_STOP / 2,
            "{left:?}"
        );
        let host = "10.255.0.11";
        closed_answer(host);
        closed_answer(host);
        session_ended(host, kicked());
        let next = closed_answer(host);
        assert_eq!(next.in_a_row, 1);
        let line = closed_line(host, &next, None);
        assert!(
            line.contains(
                "our last session here ended by the PS5 (\"Server shutting down\") 0 s before"
            ),
            "{line}"
        );
    }

    #[test]
    fn how_our_last_session_ended_sets_the_wait() {
        let host = "10.255.0.12";
        session_ended(host, SessionEnd::Dropped);
        assert!(held_for(host).unwrap() > RELEASE_AFTER_CLEAN_STOP);
        assert!(last_end_text(host).contains("broke off"));
        let host = "10.255.0.13";
        session_ended(host, SessionEnd::Stopped);
        assert!(held_for(host).unwrap() <= RELEASE_AFTER_CLEAN_STOP);
        assert!(last_end_text(host).contains("closed by us"));
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
