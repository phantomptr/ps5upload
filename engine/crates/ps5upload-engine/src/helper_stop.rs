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

/// Jobs running per console (case-folded host, no port).
fn busy_counts() -> &'static Mutex<HashMap<String, usize>> {
    static M: std::sync::OnceLock<Mutex<HashMap<String, usize>>> = std::sync::OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn key(addr: &str) -> String {
    crate::install::console_id(addr).to_ascii_lowercase()
}

/// Marks a console busy until dropped (a panic unwinding the job drops it too).
pub(crate) struct BusyGuard(String);

/// Mark `addr` busy for the life of the returned guard.
pub(crate) fn enter(addr: &str) -> BusyGuard {
    let k = key(addr);
    *busy_counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(k.clone())
        .or_insert(0) += 1;
    BusyGuard(k)
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let mut g = busy_counts().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = g.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.remove(&self.0);
            }
        }
    }
}

/// Whether a job holding a [`BusyGuard`] is running against `addr`.
pub(crate) fn is_busy(addr: &str) -> bool {
    busy_counts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key(addr))
        .is_some_and(|n| *n > 0)
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
    /// Something answers on the helper's port but did not take the stop (not paired, say).
    Refused,
    /// The helper acknowledged but still answers after the wait.
    StillRunning,
}

impl StopError {
    pub(crate) fn reason(&self) -> &'static str {
        match self {
            StopError::Busy => "busy",
            StopError::Refused => "refused",
            StopError::StillRunning => "still_running",
        }
    }

    pub(crate) fn message(&self) -> &'static str {
        match self {
            StopError::Busy => {
                "the helper is in use: wait for the transfers and installs on this console to finish, then stop it"
            }
            StopError::Refused => {
                "the helper did not accept the stop request; pair with this console and try again"
            }
            StopError::StillRunning => {
                "the helper acknowledged the stop but is still answering; try again in a moment"
            }
        }
    }
}

/// The decision, with the console calls passed in so it can be tested without one.
///
/// `shutdown` sends `node.shutdown` and says whether it was acknowledged; `port_open` probes the
/// helper's port once. After an acknowledgement the port is polled every `step` until `wait` has
/// passed.
pub(crate) fn stop_with(
    busy: bool,
    shutdown: impl FnOnce() -> bool,
    mut port_open: impl FnMut() -> bool,
    wait: Duration,
    step: Duration,
) -> Result<Stopped, StopError> {
    if busy {
        return Err(StopError::Busy);
    }
    if !shutdown() {
        return if port_open() {
            Err(StopError::Refused)
        } else {
            Ok(Stopped::NotRunning)
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
        || ps5upload_core::payload_lifecycle::shutdown_running_payload(&host).unwrap_or(false),
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
    /// Something is mounted in the helper's folders (an image, a share, a save): unmount first.
    InUse(Vec<String>),
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
            UninstallError::InUse(_) => "in_use",
            UninstallError::OldHelper => "old_helper",
            UninstallError::NotRunning => "not_running",
            UninstallError::Failed(_) => "failed",
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            UninstallError::Busy => "the helper is in use: wait for the transfers and installs on this console to finish, then try again".into(),
            UninstallError::InUse(p) => format!("something is still mounted in PS5Upload's folders on the console; unmount it first: {}", p.join(", ")),
            UninstallError::OldHelper => "this helper is too old to remove itself; send this app's helper, then try again".into(),
            UninstallError::NotRunning => "the helper is not running on this console; send it, then try again".into(),
            UninstallError::Failed(e) => format!("the helper could not remove PS5Upload: {e}"),
        }
    }
}

/// What a refusal body says, when it is one of ours (`{ok:false, err, paths?}`).
fn refusal(cause: &str) -> Option<(String, Vec<String>)> {
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
    Some((err, paths))
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
                if let Some((err, paths)) = refusal(&m.cause) {
                    return Err(if err == "uninstall_in_use" {
                        UninstallError::InUse(paths)
                    } else {
                        UninstallError::Failed(err)
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
        return Err(UninstallError::Failed(err.to_string()));
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
                true
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
        let r = stop_with(false, || false, || false, NO_WAIT, NO_WAIT);
        assert_eq!(r, Ok(Stopped::NotRunning));
    }

    #[test]
    fn an_unacknowledged_stop_with_the_port_open_is_refused() {
        let r = stop_with(false, || false, || true, NO_WAIT, NO_WAIT);
        assert_eq!(r, Err(StopError::Refused));
    }

    #[test]
    fn stopped_only_once_the_port_goes_quiet() {
        let probes = Cell::new(0);
        let r = stop_with(
            false,
            || true,
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
            || true,
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
