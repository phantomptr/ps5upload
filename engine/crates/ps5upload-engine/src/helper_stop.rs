//! `POST /api/ps5/helper/stop { addr }`: turn the console's helper off (#471).
//!
//! The stop is the same `node.shutdown` over the paired AVA1 session that replacing a helper
//! sends (`payload_lifecycle::shutdown_running_payload`). It is refused while this engine has a
//! transfer, download, console file operation or install running against that console: pulling
//! the helper out from under it would fail the job halfway. After an acknowledged shutdown the
//! helper's port is watched until it stops answering, so `stopped` means gone, not "asked".
//!
//! The busy count lives here: each console job holds a [`BusyGuard`] for as long as it runs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Jobs running per console and the consoles whose helper is being stopped or removed, under
/// one lock: a job is let in ([`try_enter`]) and a stop marked ([`begin_stopping`]) atomically
/// with respect to each other, so no job starts after a stop has read the console as idle.
#[derive(Default)]
struct Consoles {
    /// Jobs running per console (case-folded host, no port).
    busy: HashMap<String, usize>,
    /// Consoles whose helper is being stopped or removed right now (a call that can take minutes).
    stopping: std::collections::HashSet<String>,
}

fn consoles() -> std::sync::MutexGuard<'static, Consoles> {
    static M: std::sync::OnceLock<Mutex<Consoles>> = std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(Consoles::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn key(addr: &str) -> String {
    crate::install::console_id(addr).to_ascii_lowercase()
}

/// One console's key for per-console tables (case-folded host, no port).
pub(crate) fn console_key(addr: &str) -> String {
    key(addr)
}

/// What a job started while the console's helper is being stopped or removed is told.
pub(crate) const STOPPING_MESSAGE: &str = "PS5Upload is being stopped or removed on this console";

/// What a second stop or removal of a console is told while one runs.
pub(crate) const ALREADY_STOPPING_MESSAGE: &str = "a stop is already running on this console";

/// Marks a console's helper as being stopped or removed until dropped: [`is_busy`] says so, so
/// the queue starts nothing there meanwhile, and [`try_enter`] lets no job in.
pub(crate) struct StoppingGuard(String);

/// Mark `addr` as being stopped, or None when a stop or removal of it already runs.
pub(crate) fn begin_stopping(addr: &str) -> Option<StoppingGuard> {
    let k = key(addr);
    // The lock is released before any guard exists: a guard built and dropped while it is held
    // (`then_some` builds its value eagerly) would lock it again in Drop and deadlock, and would
    // clear the marker of the stop already running.
    let inserted = consoles().stopping.insert(k.clone());
    if inserted {
        Some(StoppingGuard(k))
    } else {
        None
    }
}

impl Drop for StoppingGuard {
    fn drop(&mut self) {
        consoles().stopping.remove(&self.0);
    }
}

/// Whether a job holding a [`BusyGuard`] runs against `addr` (a stop in progress not counted).
pub(crate) fn jobs_running(addr: &str) -> bool {
    consoles().busy.get(&key(addr)).is_some_and(|n| *n > 0)
}

/// Marks a console busy until dropped (a panic unwinding the job drops it too).
pub(crate) struct BusyGuard(String);

fn add_job(c: &mut Consoles, k: &str) {
    *c.busy.entry(k.to_string()).or_insert(0) += 1;
}

/// Mark `addr` busy for the life of the returned guard, whether or not a stop runs (tests stand
/// in for a running job with it). A request that starts a job takes [`try_enter`].
#[cfg(test)]
pub(crate) fn enter(addr: &str) -> BusyGuard {
    let k = key(addr);
    add_job(&mut consoles(), &k);
    BusyGuard(k)
}

/// Mark `addr` busy for the life of the returned guard, or None while its helper is being
/// stopped or removed (checked and counted under one lock). Taken before a handler's first await,
/// so nothing slips in between a stop's check and the job's start.
pub(crate) fn try_enter(addr: &str) -> Option<BusyGuard> {
    let k = key(addr);
    {
        let mut c = consoles();
        if c.stopping.contains(&k) {
            return None;
        }
        add_job(&mut c, &k);
    }
    Some(BusyGuard(k))
}

/// The answer to a request refused by [`try_enter`]: 409 `{ error, reason: "stopping" }`.
pub(crate) fn stopping_refusal() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::CONFLICT,
        axum::Json(serde_json::json!({ "error": STOPPING_MESSAGE, "reason": "stopping" })),
    )
        .into_response()
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let mut c = consoles();
        if let Some(n) = c.busy.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.busy.remove(&self.0);
            }
        }
    }
}

/// Whether a job holding a [`BusyGuard`] is running against `addr`, or its helper is being
/// stopped or removed.
pub(crate) fn is_busy(addr: &str) -> bool {
    let k = key(addr);
    let c = consoles();
    c.busy.get(&k).is_some_and(|n| *n > 0) || c.stopping.contains(&k)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stopped {
    /// The helper acknowledged and its port went quiet.
    Stopped,
    /// Nothing answered: no helper was running.
    NotRunning,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StopError {
    /// A job of this engine is using the console.
    Busy,
    /// Another stop or removal of this console's helper is running.
    AlreadyStopping,
    /// The helper refused the stop because this app is not paired with it.
    NotPaired,
    /// Something answers on the helper's port but the stop failed (a timeout, a lost session).
    Failed(String),
    /// The helper acknowledged but still answers after the wait.
    StillRunning,
}

impl StopError {
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            StopError::Busy => "busy",
            StopError::AlreadyStopping => "stopping",
            StopError::NotPaired => "refused",
            StopError::Failed(_) => "failed",
            StopError::StillRunning => "still_running",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            StopError::Busy => {
                "the helper is in use: wait for the transfers and installs on this console to finish, then stop it".into()
            }
            StopError::AlreadyStopping => ALREADY_STOPPING_MESSAGE.into(),
            StopError::NotPaired => {
                "the helper did not accept the stop request; pair with this console and try again".into()
            }
            StopError::Failed(e) => format!("the helper did not take the stop request: {e}"),
            StopError::StillRunning => {
                "the helper acknowledged the stop but is still answering; try again in a moment".into()
            }
        }
    }
}

/// Whether `e` is the helper refusing this app for not being paired with it.
pub(crate) fn is_pairing_refusal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<ps5upload_core::mgmt::MgmtError>()
        .is_some_and(|m| {
            m.status == ps5upload_ava1::gen::ERR_NOT_PAIRED
                || m.status == ps5upload_ava1::gen::ERR_PAIRING_CLOSED
                || m.cause == "not_paired"
        })
}

/// The decision, with the console calls passed in so it can be tested without one.
///
/// `shutdown` sends `node.shutdown` (Ok once acknowledged); `port_open` probes the helper's port
/// once. After an acknowledgement the port is polled every `step` until `wait` has
/// passed.
pub(crate) fn stop_with(
    busy: bool,
    shutdown: impl FnOnce() -> anyhow::Result<()>,
    mut port_open: impl FnMut() -> bool,
    wait: Duration,
    step: Duration,
) -> Result<Stopped, StopError> {
    if busy {
        return Err(StopError::Busy);
    }
    if let Err(e) = shutdown() {
        return if !port_open() {
            Ok(Stopped::NotRunning)
        } else if is_pairing_refusal(&e) {
            Err(StopError::NotPaired)
        } else {
            Err(StopError::Failed(format!("{e:#}")))
        };
    }
    let deadline = Instant::now() + wait;
    loop {
        if !port_open() {
            return Ok(Stopped::Stopped);
        }
        if Instant::now() >= deadline {
            return Err(StopError::StillRunning);
        }
        std::thread::sleep(step);
    }
}

/// Stop the helper on `addr` for real (blocking).
pub(crate) fn stop(addr: &str, busy: bool) -> Result<Stopped, StopError> {
    let host = crate::install::console_id(addr);
    // `console_id` drops IPv6 brackets; `join_host_port` puts them back.
    let port =
        ps5upload_core::payload_lifecycle::join_host_port(&host, ps5upload_ava1::gen::DEFAULT_PORT);
    let r = stop_with(
        busy,
        || {
            // `node.shutdown`, as `payload_lifecycle::shutdown_running_payload` sends it, but
            // keeping the error: a timeout is not "pair with this console".
            ps5upload_core::mgmt::call_with(
                &host,
                ps5upload_core::mgmt::m::NODE_SHUTDOWN,
                "SHUTDOWN",
                &[],
                Some(Duration::from_secs(2)),
            )
            .map(|_| ())
        },
        || ps5upload_core::payload_lifecycle::port_is_open(&port, Duration::from_millis(800)),
        Duration::from_secs(8),
        Duration::from_millis(400),
    );
    crate::log_info!("helper_stop: addr={host} -> {r:?}");
    r
}

// ── "Remove PS5Upload from this PS5" (`node.uninstall`, method 200) ─────────────────────────
//
// The helper turns Minion off and removes its tile, shows the icons it hid, answers, exits and
// deletes its own folders as its last step (payload/src/helper_uninstall.c). Stored copies of the
// helper in loaders are the app's to remove first: it knows them (lib/helperStoredCopies).

#[derive(Debug, PartialEq)]
pub(crate) enum UninstallError {
    /// A job of this engine is using the console.
    Busy,
    /// Another stop or removal of this console's helper is running.
    AlreadyStopping,
    /// Something is mounted in the helper's folders (an image, a share, a save): unmount first.
    InUse(Vec<String>),
    /// The console itself runs something the removal would cut off: `installing` (a package),
    /// `converting` (Convert on the PS5) or `extracting` (an archive).
    ConsoleBusy(String),
    /// The running helper predates the request.
    OldHelper,
    /// Nothing answered: no helper is running to remove anything.
    NotRunning,
    /// The helper refused or the call failed.
    Failed(String),
}

impl UninstallError {
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            UninstallError::Busy => "busy",
            UninstallError::AlreadyStopping => "stopping",
            UninstallError::InUse(_) => "in_use",
            UninstallError::ConsoleBusy(cause) => match cause.as_str() {
                "installing" => "console_installing",
                "converting" => "console_converting",
                "extracting" => "console_extracting",
                _ => "console_busy",
            },
            UninstallError::OldHelper => "old_helper",
            UninstallError::NotRunning => "not_running",
            UninstallError::Failed(_) => "failed",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            UninstallError::Busy => "the helper is in use: wait for the transfers and installs on this console to finish, then try again".into(),
            UninstallError::AlreadyStopping => ALREADY_STOPPING_MESSAGE.into(),
            UninstallError::ConsoleBusy(cause) => format!("the PS5 is busy ({cause}): wait for it to finish, then try again"),
            UninstallError::InUse(p) => format!("something is still mounted in PS5Upload's folders on the console; unmount it first: {}", p.join(", ")),
            UninstallError::OldHelper => "this helper is too old to remove itself; send this app's helper, then try again".into(),
            UninstallError::NotRunning => "the helper is not running on this console; send it, then try again".into(),
            UninstallError::Failed(e) => format!("the helper could not remove PS5Upload: {e}"),
        }
    }
}

/// What a refusal body says, when it is one of ours (`{ok:false, err, paths?, cause?}`).
fn refusal(cause: &str) -> Option<(String, Vec<String>, String)> {
    let v: serde_json::Value = serde_json::from_str(cause.trim()).ok()?;
    let err = v.get("err")?.as_str()?.to_string();
    let paths = v
        .get("paths")
        .and_then(|p| p.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let why = v
        .get("cause")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    Some((err, paths, why))
}

/// Plain words for a refusal token the helper answers `node.uninstall` with (any other token is
/// passed on as it came).
fn refusal_words(err: &str) -> String {
    match err {
        "uninstall_mounts_unreadable" => "it could not read what is mounted on the console, so nothing was removed; try again in a moment".into(),
        "bad_request" => "it did not understand the request; update this app and the helper, then try again".into(),
        other => other.to_string(),
    }
}

/// The decision, with the console calls passed in. `call` sends `node.uninstall`; after an
/// answer the helper's port is polled until it goes quiet (it exits right after answering) or
/// `wait` passes. Ok carries the helper's answer plus `"exited"`. `check` only asks whether the
/// helper would go ahead (it answers and keeps running): no wait.
pub(crate) fn uninstall_with(
    busy: bool,
    check: bool,
    call: impl FnOnce() -> anyhow::Result<Vec<u8>>,
    mut port_open: impl FnMut() -> bool,
    wait: Duration,
    step: Duration,
) -> Result<serde_json::Value, UninstallError> {
    if busy {
        return Err(UninstallError::Busy);
    }
    let body = match call() {
        Ok(b) => b,
        Err(e) => {
            if ps5upload_core::console_files::is_unsupported(&e) {
                return Err(UninstallError::OldHelper);
            }
            if let Some(m) = e.downcast_ref::<ps5upload_core::mgmt::MgmtError>() {
                if let Some((err, paths, why)) = refusal(&m.cause) {
                    return Err(match err.as_str() {
                        "uninstall_in_use" => UninstallError::InUse(paths),
                        "uninstall_busy" => UninstallError::ConsoleBusy(why),
                        _ => UninstallError::Failed(refusal_words(&err)),
                    });
                }
            }
            return Err(if port_open() {
                UninstallError::Failed(e.to_string())
            } else {
                UninstallError::NotRunning
            });
        }
    };
    let mut v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| UninstallError::Failed("the helper's answer is not JSON".into()))?;
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        let err = v.get("err").and_then(|e| e.as_str()).unwrap_or("refused");
        return Err(UninstallError::Failed(refusal_words(err)));
    }
    if check {
        v["exited"] = false.into();
        return Ok(v);
    }
    let deadline = Instant::now() + wait;
    let exited = loop {
        if !port_open() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(step);
    };
    v["exited"] = exited.into();
    Ok(v)
}

/// Remove PS5Upload from `addr` for real (blocking).
pub(crate) fn uninstall(
    addr: &str,
    busy: bool,
    keep_backups: bool,
    check: bool,
) -> Result<serde_json::Value, UninstallError> {
    let host = crate::install::console_id(addr);
    let port =
        ps5upload_core::payload_lifecycle::join_host_port(&host, ps5upload_ava1::gen::DEFAULT_PORT);
    let body = serde_json::json!({ "keep_backups": keep_backups, "check": check }).to_string();
    let r = uninstall_with(
        busy,
        check,
        || {
            // The tile's removal waits on Sony calls that can take a while.
            ps5upload_core::mgmt::call_with(
                &host,
                ps5upload_core::mgmt::m::NODE_UNINSTALL,
                "NODE_UNINSTALL",
                body.as_bytes(),
                Some(Duration::from_secs(240)),
            )
        },
        || ps5upload_core::payload_lifecycle::port_is_open(&port, Duration::from_millis(800)),
        Duration::from_secs(15),
        Duration::from_millis(500),
    );
    crate::log_info!(
        "helper_uninstall: addr={host} keep_backups={keep_backups} check={check} -> {r:?}"
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn refused(cause: &str) -> anyhow::Error {
        anyhow::Error::new(ps5upload_core::mgmt::MgmtError {
            label: "NODE_UNINSTALL".into(),
            status: 1,
            cause: cause.into(),
        })
    }

    #[test]
    fn uninstall_is_refused_while_busy_without_touching_the_console() {
        let called = Cell::new(false);
        let r = uninstall_with(
            true,
            false,
            || {
                called.set(true);
                Ok(Vec::new())
            },
            || true,
            NO_WAIT,
            NO_WAIT,
        );
        assert_eq!(r, Err(UninstallError::Busy));
        assert!(!called.get());
    }

    #[test]
    fn uninstall_names_what_is_still_mounted() {
        let r = uninstall_with(
            false,
            false,
            || {
                Err(refused(
                    r#"{"ok":false,"err":"uninstall_in_use","paths":["/data/ps5upload/mounts/a"]}"#,
                ))
            },
            || true,
            NO_WAIT,
            NO_WAIT,
        );
        let e = r.unwrap_err();
        assert_eq!(
            e,
            UninstallError::InUse(vec!["/data/ps5upload/mounts/a".into()])
        );
        assert_eq!(e.reason(), "in_use");
        assert!(e.message().contains("/data/ps5upload/mounts/a"));
    }

    #[test]
    fn unreadable_mounts_are_said_in_words() {
        let r = uninstall_with(
            false,
            false,
            || {
                Err(refused(
                    r#"{"ok":false,"err":"uninstall_mounts_unreadable"}"#,
                ))
            },
            || true,
            NO_WAIT,
            NO_WAIT,
        );
        let m = r.unwrap_err().message();
        assert!(!m.contains("uninstall_mounts_unreadable"), "{m}");
        assert!(m.contains("could not read what is mounted"), "{m}");
        let r = uninstall_with(
            false,
            false,
            || Ok(br#"{"ok":false,"err":"uninstall_mounts_unreadable"}"#.to_vec()),
            || true,
            NO_WAIT,
            NO_WAIT,
        );
        assert!(!r.unwrap_err().message().contains("uninstall_mounts"));
    }

    #[test]
    fn uninstall_tells_an_older_helper_apart() {
        let old = anyhow::Error::new(ps5upload_core::mgmt::MgmtError {
            label: "NODE_UNINSTALL".into(),
            status: ps5upload_core::console_files::ERR_UNKNOWN_METHOD,
            cause: "unknown method".into(),
        });
        let r = uninstall_with(false, false, || Err(old), || true, NO_WAIT, NO_WAIT);
        assert_eq!(r, Err(UninstallError::OldHelper));
    }

    #[test]
    fn uninstall_with_nothing_answering_is_not_running() {
        let r = uninstall_with(
            false,
            false,
            || Err(anyhow::anyhow!("connection refused")),
            || false,
            NO_WAIT,
            NO_WAIT,
        );
        assert_eq!(r, Err(UninstallError::NotRunning));
    }

    #[test]
    fn uninstall_answers_with_the_helpers_reply_once_it_exits() {
        let probes = Cell::new(0);
        let r = uninstall_with(
            false,
            false,
            || Ok(br#"{"ok":true,"tile":"removed","tile_failed":false,"icons_shown":2,"keep_backups":true}"#.to_vec()),
            || {
                probes.set(probes.get() + 1);
                probes.get() < 2
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(r["tile"], "removed");
        assert_eq!(r["icons_shown"], 2);
        assert_eq!(r["exited"], true);
    }

    #[test]
    fn a_check_answers_without_waiting_for_an_exit() {
        let probes = Cell::new(0);
        let r = uninstall_with(
            false,
            true,
            || Ok(br#"{"ok":true,"check":true}"#.to_vec()),
            || {
                probes.set(probes.get() + 1);
                true
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(r["check"], true);
        assert_eq!(r["exited"], false);
        assert_eq!(probes.get(), 0, "a check never waits for the helper to go");
    }

    const NO_WAIT: Duration = Duration::from_millis(0);

    #[test]
    fn refused_while_busy_without_touching_the_console() {
        let called = Cell::new(false);
        let r = stop_with(
            true,
            || {
                called.set(true);
                Ok(())
            },
            || {
                called.set(true);
                false
            },
            NO_WAIT,
            NO_WAIT,
        );
        assert_eq!(r, Err(StopError::Busy));
        assert!(!called.get(), "a busy console is never sent the shutdown");
        assert_eq!(StopError::Busy.reason(), "busy");
    }

    #[test]
    fn nothing_answering_is_not_running() {
        let r = stop_with(
            false,
            || Err(anyhow::anyhow!("connection refused")),
            || false,
            NO_WAIT,
            NO_WAIT,
        );
        assert_eq!(r, Ok(Stopped::NotRunning));
    }

    #[test]
    fn only_a_pairing_refusal_asks_to_pair() {
        let unpaired = || {
            Err(anyhow::Error::new(ps5upload_core::mgmt::MgmtError {
                label: "SHUTDOWN".into(),
                status: ps5upload_ava1::gen::ERR_NOT_PAIRED,
                cause: "not_paired".into(),
            }))
        };
        let e = stop_with(false, unpaired, || true, NO_WAIT, NO_WAIT).unwrap_err();
        assert_eq!(e, StopError::NotPaired);
        assert_eq!(e.reason(), "refused");
        assert!(e.message().contains("pair with this console"));

        // A timeout (or any other failure) says what happened, not "pair".
        let e = stop_with(
            false,
            || {
                Err(anyhow::anyhow!(
                    "management call SHUTDOWN failed: timed out after 2s"
                ))
            },
            || true,
            NO_WAIT,
            NO_WAIT,
        )
        .unwrap_err();
        assert_eq!(e.reason(), "failed");
        assert!(e.message().contains("timed out"), "{}", e.message());
        assert!(!e.message().contains("pair"), "{}", e.message());
    }

    #[test]
    fn a_stop_in_progress_marks_the_console_busy_and_one_at_a_time() {
        let addr = "192.0.2.61";
        let g = begin_stopping(addr).expect("first stop");
        assert!(is_busy(addr), "the queue starts nothing during a stop");
        assert!(!jobs_running(addr), "a stop is not a job");
        // A second stop is refused without blocking (run on a thread with a deadline, so a lock
        // taken twice fails this test instead of hanging the run) and leaves the first one's mark.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(begin_stopping("192.0.2.61:9120").is_none());
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "one stop per console, answered at once"
        );
        assert!(is_busy(addr), "the running stop keeps its mark");
        drop(g);
        assert!(!is_busy(addr));
        assert!(begin_stopping(addr).is_some());
    }

    #[test]
    fn no_job_is_let_in_while_a_stop_runs() {
        let addr = "192.0.2.62";
        let job = try_enter(addr).expect("free console");
        assert!(jobs_running(addr));
        drop(job);
        let stop = begin_stopping(addr).expect("first stop");
        assert!(try_enter(addr).is_none(), "a stop running refuses new jobs");
        assert!(try_enter("192.0.2.62:9021").is_none(), "whatever the port");
        assert!(!jobs_running(addr), "a refused job is not counted");
        assert!(
            try_enter("192.0.2.63").is_some(),
            "other consoles stay free"
        );
        assert_eq!(
            stopping_refusal().status(),
            axum::http::StatusCode::CONFLICT
        );
        drop(stop);
        assert!(try_enter(addr).is_some());
    }

    #[test]
    fn a_second_stop_says_one_is_running() {
        assert_eq!(StopError::AlreadyStopping.reason(), "stopping");
        assert!(StopError::AlreadyStopping
            .message()
            .contains("already running"));
        assert!(!StopError::AlreadyStopping.message().contains("transfers"));
        assert_eq!(UninstallError::AlreadyStopping.reason(), "stopping");
        assert!(UninstallError::AlreadyStopping
            .message()
            .contains("already running"));
    }

    #[tokio::test]
    async fn the_route_tells_a_second_stop_one_is_running() {
        let installs = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let first = begin_stopping("192.0.2.64").expect("first stop");
        let (status, body) = call(installs, Some("192.0.2.64")).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert_eq!(body["reason"], "stopping");
        assert!(body["error"].as_str().unwrap().contains("already running"));
        drop(first);
    }

    #[test]
    fn uninstall_says_what_the_console_is_busy_with() {
        let r = uninstall_with(
            false,
            true,
            || {
                Err(refused(
                    r#"{"ok":false,"err":"uninstall_busy","cause":"converting"}"#,
                ))
            },
            || true,
            NO_WAIT,
            NO_WAIT,
        );
        let e = r.unwrap_err();
        assert_eq!(e, UninstallError::ConsoleBusy("converting".into()));
        assert_eq!(e.reason(), "console_converting");
        assert_eq!(
            UninstallError::ConsoleBusy("installing".into()).reason(),
            "console_installing"
        );
        assert_eq!(
            UninstallError::ConsoleBusy("extracting".into()).reason(),
            "console_extracting"
        );
        assert_eq!(
            UninstallError::ConsoleBusy("?".into()).reason(),
            "console_busy"
        );
    }

    #[test]
    fn stopped_only_once_the_port_goes_quiet() {
        let probes = Cell::new(0);
        let r = stop_with(
            false,
            || Ok(()),
            || {
                probes.set(probes.get() + 1);
                probes.get() < 3
            },
            Duration::from_secs(5),
            Duration::from_millis(1),
        );
        assert_eq!(r, Ok(Stopped::Stopped));
        assert_eq!(probes.get(), 3);
    }

    #[test]
    fn a_helper_that_keeps_answering_is_reported() {
        let r = stop_with(
            false,
            || Ok(()),
            || true,
            Duration::from_millis(5),
            Duration::from_millis(1),
        );
        assert_eq!(r, Err(StopError::StillRunning));
    }

    async fn call(
        installs: crate::pkg_install::PkgInstallStateHandle,
        addr: Option<&str>,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let resp = crate::ps5_helper_stop(
            installs,
            crate::HelperStopReq {
                addr: addr.map(str::to_string),
            },
        )
        .await;
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    /// The route refuses (409, `reason: busy`) while a transfer or an install of this engine
    /// runs against the console, before it ever contacts the console (TEST-NET addresses: a
    /// shutdown sent there would only time out).
    #[tokio::test]
    async fn the_route_refuses_while_a_job_runs_against_that_console() {
        let installs = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());

        let job = enter("192.0.2.51");
        let (status, body) = call(installs.clone(), Some("192.0.2.51")).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert_eq!(body["reason"], "busy");
        assert!(body["error"].as_str().unwrap().contains("in use"));
        drop(job);

        let install = installs.jobs.begin("192.0.2.52").unwrap();
        let (status, body) = call(installs.clone(), Some("192.0.2.52")).await;
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert_eq!(body["reason"], "busy");
        installs.jobs.finish(&install);
    }

    #[tokio::test]
    async fn the_route_names_its_console() {
        let installs = std::sync::Arc::new(crate::pkg_install::PkgInstallState::default());
        let (status, _) = call(installs, None).await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn a_running_job_marks_its_console_busy_until_it_ends() {
        let addr = "192.0.2.41";
        assert!(!is_busy(addr));
        let a = enter("192.0.2.41:1234");
        let b = enter(addr);
        assert!(is_busy(addr), "the port does not split a console");
        assert!(!is_busy("192.0.2.42"), "other consoles stay free");
        drop(a);
        assert!(is_busy(addr));
        drop(b);
        assert!(!is_busy(addr));
    }
}
