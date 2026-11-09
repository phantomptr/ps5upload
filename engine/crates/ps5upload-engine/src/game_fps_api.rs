//! The running game's sampled frame rate, live and captured (#774).
//!
//! The helper answers with the display's frame count (`ps5upload_core::game_fps`); this module
//! keeps one sampler per console that reads it once a second while someone is looking (a live
//! read in the last [`WATCH_LEASE`]) or a capture is running, and stops on its own otherwise.
//! A capture keeps every sample in memory (up to [`MAX_CAPTURE_SAMPLES`], six hours), with the
//! console's basic temperatures every [`TEMPS_EVERY`]; it is lost when the engine restarts.
//!
//! * `GET  /api/ps5/hw/fps?addr=` -> `{ok, state, fps, frames, interval_ms, at_ms, error?,
//!   capture}`. Starts the sampler: the first answer may be `state: "starting"` with no `fps`.
//!   `state` is the helper's (`ok`, `no_game`, `no_device`, `failed`), or `unsupported` (an
//!   older helper) or `offline` (no answer; `error` says why). `capture` is
//!   `{active, started_ms, stopped_ms, stop_reason, samples, stats}`.
//! * `POST /api/ps5/hw/fps/capture/start {addr}` -> the same, with a new, empty capture.
//! * `POST /api/ps5/hw/fps/capture/stop {addr}` -> the same, the capture kept for export.
//! * `GET  /api/ps5/hw/fps/capture?addr=&since=N` -> `{ok, active, started_ms, stopped_ms,
//!   stop_reason, stats, total, samples}`: the samples from index `since` on (all by default).
//! * `GET  /api/ps5/hw/fps/capture/csv?addr=` -> `text/csv`, one row per sample (404 when there
//!   is no capture).
//!
//! FPS here is sampled: frames over each interval of about one second, not frame times.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::game_fps::{
    capture_csv, capture_stats, read_flips, CaptureSample, FlipRead, FpsMeter, FpsSample,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{console_addr_or_default, missing_console_addr, required_console_addr, AppState};

/// A live read keeps the sampler going this long.
pub const WATCH_LEASE: Duration = Duration::from_secs(15);
/// How often the frame count is read.
const STEP: Duration = Duration::from_secs(1);
/// How often while the helper cannot be reached or is too old.
const SLOW_STEP: Duration = Duration::from_secs(3);
/// How often a capture reads the console's basic temperatures (the Dashboard's own pace).
pub const TEMPS_EVERY: Duration = Duration::from_secs(5);
/// A temperature older than this is not put on a sample.
const TEMPS_FRESH: Duration = Duration::from_secs(12);
/// A capture stops by itself after this many samples: six hours at one a second.
pub const MAX_CAPTURE_SAMPLES: usize = 6 * 3600;

#[derive(Debug, Default)]
struct Capture {
    started_ms: i64,
    stopped_ms: Option<i64>,
    stop_reason: Option<&'static str>,
    samples: Vec<CaptureSample>,
}

impl Capture {
    fn active(&self) -> bool {
        self.stopped_ms.is_none()
    }
}

#[derive(Debug)]
struct Session {
    meter: FpsMeter,
    state: String,
    error: Option<String>,
    last: Option<(FpsSample, i64)>,
    watch_until: Instant,
    running: bool,
    capture: Option<Capture>,
    temps: Option<(i32, i32, Instant)>,
    temps_read_at: Option<Instant>,
}

impl Session {
    fn new(now: Instant) -> Self {
        Session {
            meter: FpsMeter::default(),
            state: "starting".into(),
            error: None,
            last: None,
            watch_until: now,
            running: false,
            capture: None,
            temps: None,
            temps_read_at: None,
        }
    }

    fn capturing(&self) -> bool {
        self.capture.as_ref().is_some_and(Capture::active)
    }

    fn wanted(&self, now: Instant) -> bool {
        self.watch_until > now || self.capturing()
    }

    /// One read's outcome: the state, the rate, and a capture row when there is one.
    fn apply(&mut self, read: Result<FlipRead, String>, now: Instant, now_ms: i64) {
        let r = match read {
            Ok(r) => r,
            Err(e) => {
                self.meter.reset();
                self.state = "offline".into();
                self.error = Some(e);
                self.last = None;
                return;
            }
        };
        self.error = None;
        self.state = r.state.clone();
        let Some(s) = self.meter.push(&r) else {
            if !r.is_ok() {
                self.last = None;
            }
            return;
        };
        self.last = Some((s, now_ms));
        let temps = self
            .temps
            .filter(|(_, _, at)| now.duration_since(*at) <= TEMPS_FRESH);
        if let Some(c) = self.capture.as_mut().filter(|c| c.active()) {
            c.samples.push(CaptureSample {
                t_ms: now_ms,
                elapsed_ms: now_ms - c.started_ms,
                fps: s.fps,
                frames: s.frames,
                interval_ms: s.interval_us as f64 / 1000.0,
                cpu_temp_c: temps.map(|t| t.0).filter(|v| *v > 0),
                soc_temp_c: temps.map(|t| t.1).filter(|v| *v > 0),
            });
            if c.samples.len() >= MAX_CAPTURE_SAMPLES {
                c.stopped_ms = Some(now_ms);
                c.stop_reason = Some("limit");
            }
        }
    }

    fn temps_due(&mut self, now: Instant) -> bool {
        if !self.capturing() || self.state != "ok" {
            return false;
        }
        if self
            .temps_read_at
            .is_some_and(|at| now.duration_since(at) < TEMPS_EVERY)
        {
            return false;
        }
        self.temps_read_at = Some(now);
        true
    }

    fn capture_json(&self) -> Value {
        match &self.capture {
            None => json!({ "active": false, "samples": 0 }),
            Some(c) => json!({
                "active": c.active(),
                "started_ms": c.started_ms,
                "stopped_ms": c.stopped_ms,
                "stop_reason": c.stop_reason,
                "samples": c.samples.len(),
                "stats": capture_stats(&c.samples),
            }),
        }
    }

    fn snapshot(&self) -> Value {
        let mut v = json!({
            "ok": true,
            "sampled": true,
            "state": self.state,
            "fps": Value::Null,
            "capture": self.capture_json(),
        });
        if let Some((s, at_ms)) = &self.last {
            v["fps"] = json!(s.fps);
            v["frames"] = json!(s.frames);
            v["interval_ms"] = json!(s.interval_us as f64 / 1000.0);
            v["at_ms"] = json!(at_ms);
        }
        if let Some(e) = &self.error {
            v["error"] = json!(e);
        }
        v
    }
}

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static S: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_session<T>(addr: &str, f: impl FnOnce(&mut Session) -> T) -> T {
    let mut map = sessions().lock().unwrap_or_else(|e| e.into_inner());
    let s = map
        .entry(addr.to_string())
        .or_insert_with(|| Session::new(Instant::now()));
    f(s)
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Starts the console's sampler unless it runs already.
fn ensure_running(addr: &str) {
    let start = with_session(addr, |s| !std::mem::replace(&mut s.running, true));
    if start {
        let addr = addr.to_string();
        tokio::spawn(run(addr));
    }
}

async fn run(addr: String) {
    loop {
        let begun = Instant::now();
        let keep = with_session(&addr, |s| {
            if !s.wanted(begun) {
                // nobody looking and no capture running: stop, and forget the live rate so the
                // next look does not show an old one (a finished capture stays for export)
                s.running = false;
                s.meter.reset();
                s.last = None;
                s.error = None;
                s.state = "starting".into();
                return false;
            }
            true
        });
        if !keep {
            return;
        }
        let a = addr.clone();
        let read = match tokio::task::spawn_blocking(move || read_flips(&a)).await {
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(format!("{e:#}")),
            Err(e) => Err(format!("the read failed: {e}")),
        };
        let slow = !matches!(&read, Ok(r) if r.state != "unsupported");
        let temps_due = with_session(&addr, |s| {
            s.apply(read, Instant::now(), unix_ms());
            s.temps_due(Instant::now())
        });
        if temps_due {
            // beside the frame reads, so a slow sensor read never delays one
            let addr = addr.clone();
            tokio::spawn(async move {
                let a = addr.clone();
                let t = tokio::task::spawn_blocking(move || {
                    ps5upload_core::hw::hw_temps_basic_within(&a, Duration::from_secs(3))
                        .map(|t| (t.cpu_temp, t.soc_temp))
                })
                .await;
                if let Ok(Ok((cpu, soc))) = t {
                    with_session(&addr, |s| s.temps = Some((cpu, soc, Instant::now())));
                }
            });
        }
        let step = if slow { SLOW_STEP } else { STEP };
        tokio::time::sleep(step.saturating_sub(begun.elapsed())).await;
    }
}

// ─── Routes ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct AddrQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    since: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct AddrBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

/// `GET /api/ps5/hw/fps`: the latest sample; keeps the sampler going for [`WATCH_LEASE`].
pub(crate) async fn fps_live(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let v = with_session(&addr, |s| {
        s.watch_until = s.watch_until.max(Instant::now() + WATCH_LEASE);
        s.snapshot()
    });
    ensure_running(&addr);
    Json(v).into_response()
}

/// `POST /api/ps5/hw/fps/capture/start`: a new capture (the previous one is dropped).
pub(crate) async fn fps_capture_start(Json(req): Json<AddrBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let v = with_session(&addr, |s| {
        s.capture = Some(Capture {
            started_ms: unix_ms(),
            ..Capture::default()
        });
        s.temps_read_at = None;
        s.snapshot()
    });
    ensure_running(&addr);
    Json(v).into_response()
}

/// `POST /api/ps5/hw/fps/capture/stop`: ends the capture and keeps it for export.
pub(crate) async fn fps_capture_stop(Json(req): Json<AddrBody>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let v = with_session(&addr, |s| {
        if let Some(c) = s.capture.as_mut().filter(|c| c.active()) {
            c.stopped_ms = Some(unix_ms());
            c.stop_reason = Some("stopped");
        }
        s.snapshot()
    });
    Json(v).into_response()
}

fn no_capture() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "no capture for this console", "reason": "no_capture" })),
    )
        .into_response()
}

/// `GET /api/ps5/hw/fps/capture`: the samples from `since` on.
pub(crate) async fn fps_capture_get(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let since = q.since.unwrap_or(0);
    let v = with_session(&addr, |s| {
        s.capture.as_ref().map(|c| {
            let from = since.min(c.samples.len());
            json!({
                "ok": true,
                "sampled": true,
                "active": c.active(),
                "started_ms": c.started_ms,
                "stopped_ms": c.stopped_ms,
                "stop_reason": c.stop_reason,
                "stats": capture_stats(&c.samples),
                "total": c.samples.len(),
                "since": from,
                "samples": &c.samples[from..],
            })
        })
    });
    match v {
        Some(v) => Json(v).into_response(),
        None => no_capture(),
    }
}

/// `GET /api/ps5/hw/fps/capture/csv`: the capture as a CSV file.
pub(crate) async fn fps_capture_csv(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> Response {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let v = with_session(&addr, |s| {
        s.capture
            .as_ref()
            .map(|c| (capture_csv(&c.samples), c.started_ms))
    });
    let Some((csv, started_ms)) = v else {
        return no_capture();
    };
    let name = format!(
        "ps5-fps-{}.csv",
        ps5upload_core::game_fps::iso_utc_ms(started_ms)
            .get(..19)
            .unwrap_or("capture")
            .replace([':', 'T'], "-")
    );
    Response::builder()
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        )
        .body(Body::from(csv))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(flips: u64, mono_us: u64) -> Result<FlipRead, String> {
        Ok(FlipRead {
            state: "ok".into(),
            flips,
            mono_us,
            ..FlipRead::default()
        })
    }

    fn capturing(now: Instant) -> Session {
        let mut s = Session::new(now);
        s.capture = Some(Capture {
            started_ms: 1_000,
            ..Capture::default()
        });
        s
    }

    #[test]
    fn reads_become_a_live_rate_and_capture_rows() {
        let now = Instant::now();
        let mut s = capturing(now);
        s.apply(ok(100, 1_000_000), now, 2_000);
        assert_eq!(s.state, "ok");
        assert!(s.last.is_none(), "one read is no rate yet");
        s.temps = Some((61, 55, now));
        s.apply(ok(160, 2_000_000), now, 3_000);
        let v = s.snapshot();
        assert_eq!(v["fps"], json!(60.0));
        assert_eq!(v["at_ms"], json!(3_000));
        assert_eq!(v["sampled"], json!(true));
        let c = s.capture.as_ref().unwrap();
        assert_eq!(c.samples.len(), 1);
        let row = &c.samples[0];
        assert_eq!((row.elapsed_ms, row.frames), (2_000, 60));
        assert_eq!((row.cpu_temp_c, row.soc_temp_c), (Some(61), Some(55)));
        assert_eq!(v["capture"]["samples"], json!(1));
        assert_eq!(v["capture"]["active"], json!(true));
    }

    #[test]
    fn no_game_and_offline_clear_the_rate_and_add_no_rows() {
        let now = Instant::now();
        let mut s = capturing(now);
        s.apply(ok(100, 1_000_000), now, 2_000);
        s.apply(ok(160, 2_000_000), now, 3_000);
        s.apply(
            Ok(FlipRead {
                state: "no_game".into(),
                ..FlipRead::default()
            }),
            now,
            4_000,
        );
        assert_eq!(s.state, "no_game");
        assert_eq!(s.snapshot()["fps"], Value::Null);
        s.apply(Err("helper_not_ava1".into()), now, 5_000);
        let v = s.snapshot();
        assert_eq!(
            (v["state"].as_str(), v["error"].as_str()),
            (Some("offline"), Some("helper_not_ava1"))
        );
        assert_eq!(s.capture.as_ref().unwrap().samples.len(), 1);
    }

    #[test]
    fn stale_temperatures_stay_off_the_rows() {
        let now = Instant::now();
        let mut s = capturing(now);
        s.temps = Some((61, 55, now));
        let later = now + TEMPS_FRESH + Duration::from_secs(1);
        s.apply(ok(0, 1_000_000), later, 2_000);
        s.apply(ok(60, 2_000_000), later, 3_000);
        let row = &s.capture.as_ref().unwrap().samples[0];
        assert_eq!((row.cpu_temp_c, row.soc_temp_c), (None, None));
    }

    #[test]
    fn a_capture_stops_at_its_limit() {
        let now = Instant::now();
        let mut s = capturing(now);
        s.capture.as_mut().unwrap().samples = vec![
            CaptureSample {
                t_ms: 0,
                elapsed_ms: 0,
                fps: 60.0,
                frames: 60,
                interval_ms: 1000.0,
                cpu_temp_c: None,
                soc_temp_c: None,
            };
            MAX_CAPTURE_SAMPLES - 1
        ];
        s.apply(ok(0, 1_000_000), now, 2_000);
        s.apply(ok(60, 2_000_000), now, 3_000);
        let c = s.capture.as_ref().unwrap();
        assert_eq!(c.samples.len(), MAX_CAPTURE_SAMPLES);
        assert_eq!((c.stopped_ms, c.stop_reason), (Some(3_000), Some("limit")));
        assert!(!s.capturing());
        // nothing more is added
        s.apply(ok(120, 3_000_000), now, 4_000);
        assert_eq!(
            s.capture.as_ref().unwrap().samples.len(),
            MAX_CAPTURE_SAMPLES
        );
    }

    #[test]
    fn temperatures_are_read_only_while_capturing_a_game_and_not_too_often() {
        let now = Instant::now();
        let mut s = Session::new(now);
        s.state = "ok".into();
        assert!(!s.temps_due(now), "no capture");
        let mut s = capturing(now);
        s.state = "no_game".into();
        assert!(!s.temps_due(now), "no game");
        s.state = "ok".into();
        assert!(s.temps_due(now));
        assert!(!s.temps_due(now + Duration::from_secs(1)));
        assert!(s.temps_due(now + TEMPS_EVERY));
    }

    #[test]
    fn the_sampler_runs_only_while_watched_or_capturing() {
        let now = Instant::now();
        let mut s = Session::new(now);
        assert!(!s.wanted(now));
        s.watch_until = now + WATCH_LEASE;
        assert!(s.wanted(now));
        assert!(!s.wanted(now + WATCH_LEASE));
        let s = capturing(now);
        assert!(s.wanted(now + Duration::from_secs(3600)));
    }

    #[tokio::test]
    async fn start_and_stop_need_a_console() {
        let r = fps_capture_start(Json(AddrBody { addr: None })).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = fps_capture_stop(Json(AddrBody { addr: None })).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn no_capture_is_a_404_and_a_stopped_capture_exports() {
        let state = AppState {
            jobs: Default::default(),
            default_ps5_addr: "192.0.2.77".into(),
            events_tx: tokio::sync::broadcast::channel(4).0,
        };
        let q = || {
            Query(AddrQuery {
                addr: Some("192.0.2.77".into()),
                since: None,
            })
        };
        let r = fps_capture_csv(State(state.clone()), q()).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        let r = fps_capture_get(State(state.clone()), q()).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        // a capture with one row, stopped (no sampler is started here)
        with_session("192.0.2.77", |s| {
            let mut c = Capture {
                started_ms: 1_793_000_000_000,
                stopped_ms: Some(1_793_000_002_000),
                stop_reason: Some("stopped"),
                ..Capture::default()
            };
            c.samples.push(CaptureSample {
                t_ms: 1_793_000_001_000,
                elapsed_ms: 1_000,
                fps: 60.0,
                frames: 60,
                interval_ms: 1000.0,
                cpu_temp_c: None,
                soc_temp_c: None,
            });
            s.capture = Some(c);
        });
        let r = fps_capture_csv(State(state.clone()), q()).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            r.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"ps5-fps-2026-10-26-07-33-20.csv\""
        );
        let body = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.starts_with("timestamp_utc,elapsed_s,fps_sampled,"));
        assert_eq!(text.lines().count(), 2);
        let r = fps_capture_get(
            State(state),
            Query(AddrQuery {
                addr: Some("192.0.2.77".into()),
                since: Some(5),
            }),
        )
        .await;
        let body = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            (v["total"].as_u64(), v["since"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(v["samples"], json!([]));
        assert_eq!(v["active"], json!(false));
    }
}
