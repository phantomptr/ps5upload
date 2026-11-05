//! Go live (#635): the running Remote Play stream sent straight to YouTube, Twitch, Kick,
//! Facebook or any RTMP(S)/SRT server, from the engine, without OBS.
//!
//! The picture is the console's own (nothing re-encoded): the live feed (rp_live.rs) gives
//! MPEG-TS with a keyframe every 2 s while anyone reads it, held frames after a loss, and
//! parameter sets before every keyframe. One ffmpeg process (ffmpeg_tool.rs: downloaded on
//! first use, hash-checked) reads that on stdin, copies the video, turns Opus into AAC and
//! publishes (golive_ffmpeg.rs). Several sites at once go through ffmpeg's tee muxer with
//! `onfail=ignore`: one site failing leaves the rest streaming; it is let back in a minute
//! later (a short restart for all), and a site that refused the key is left out.
//!
//! If ffmpeg stops (a dropped connection, the site restarting), it is started again with a
//! backoff of 2, 4, 8, 15, 30 s, while the Remote Play session keeps running. A refused key
//! (twice) or a codec the site does not take ends it with the reason. If the Remote Play
//! session ends, Go live waits up to two minutes for a new one (restarting Play in H.264 for
//! Twitch, say) and carries on with it.
//!
//! API (behind the engine's usual LAN guard; the web UI's Go live runs on the engine's
//! computer; AUTOMATION.md has the requests, answers and error codes):
//! * `GET /api/stream/status`: ffmpeg (installed, download size, progress), the session and
//!   each preset's and saved destination's checks against it, and the live run (phase,
//!   elapsed, bitrate, fps, per destination state, reconnects, errors).
//! * `POST /api/stream/start {"destinations": [{"key_id"}], "audio_kbps"?}`,
//!   `POST /api/stream/stop`.
//! * `POST /api/stream/destinations` (add: `{preset, key, server?, url?, label?,
//!   bandwidth_test?}`), `POST /api/stream/destinations/{id}` (change: only what changes; no
//!   `key` keeps it), `DELETE /api/stream/destinations/{id}`. Keys are never returned
//!   (golive_dest.rs).
//! * `POST /api/stream/tools/ffmpeg/install`: download ffmpeg (progress in the status).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path};
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::ffmpeg_tool::{self, FfmpegCmd};
use crate::golive_dest::{self, Preset};
use crate::golive_ffmpeg::{self as gf, Backoff, Failure, Next, Output, Progress};
use crate::rp_stream_api::live_feed::{self, SessionInfo};

/// How long Go live waits for Remote Play to start again after it ended.
const WAIT_FOR_PLAY: Duration = Duration::from_secs(120);
/// A site that dropped out of several is let back in after this (all restart briefly).
const REJOIN_AFTER: Duration = Duration::from_secs(60);
/// …at most this many times, then it stays out.
const MAX_REJOINS: u32 = 5;
/// ffmpeg gets this long to finish cleanly when stopped, then it is killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// ffmpeg has no network timeout of its own: a connection that dies silently (Wi-Fi gone, a NAT
/// entry expired) blocks its writes for as long as the OS keeps the socket (minutes). No
/// progress for this long while live means that: it is stopped and the run reconnects.
const STALL_AFTER: Duration = Duration::from_secs(20);
/// The same before the first frame went out: a server that took the connection and never
/// answered the handshake.
const CONNECT_STALL_AFTER: Duration = Duration::from_secs(30);
/// How long a new start waits for a stop still finishing, before it says "already live": a site
/// refuses a second stream on a key the old ffmpeg still holds. STOP_GRACE, then the kill and the
/// run's last words.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// A site of several that fails this many times in a row right after a start stays out: that is
/// what a refused key looks like when the site only closes the connection, and each try would
/// cut everyone else's stream for a moment.
const MAX_EARLY_FAILS: u32 = 2;
/// ffmpeg's last lines (scrubbed of keys) kept for the status.
const LOG_LINES: usize = 12;

/// How long a run waits for things (tests shorten them).
#[derive(Clone, Copy)]
pub(crate) struct Timing {
    wait_for_play: Duration,
    rejoin_after: Duration,
    stop_grace: Duration,
    stall_after: Duration,
    connect_stall_after: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            wait_for_play: WAIT_FOR_PLAY,
            rejoin_after: REJOIN_AFTER,
            stop_grace: STOP_GRACE,
            stall_after: STALL_AFTER,
            connect_stall_after: CONNECT_STALL_AFTER,
        }
    }
}

/// One destination of a run, with its secret parts resolved.
#[derive(Clone)]
pub(crate) struct Dest {
    pub id: String,
    pub label: String,
    pub preset: &'static Preset,
    pub out: Output,
    /// Strings to scrub from anything shown or logged.
    pub secrets: Vec<String>,
    pub bandwidth_test: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct Problem {
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
struct DestStatus {
    id: String,
    label: String,
    preset: &'static str,
    name: &'static str,
    /// "connecting", "live", "retrying" (back with the next restart), "failed" (left out).
    state: &'static str,
    error: Option<Problem>,
    bandwidth_test: bool,
    #[serde(skip)]
    rejoins: u32,
    /// Failures in a row right after a start.
    #[serde(skip)]
    early_fails: u32,
}

#[derive(Default)]
struct RunState {
    /// "starting", "live", "reconnecting", "waiting_for_play", "ended", "failed". While a stop is
    /// finishing the status says "stopping" (`Run::phase`).
    phase: &'static str,
    error: Option<Problem>,
    live_since: Option<Instant>,
    reconnects: u32,
    retry_at: Option<Instant>,
    progress: Option<Progress>,
    kbps: f64,
    dests: Vec<DestStatus>,
    session: Option<SessionInfo>,
    /// Times the upload fell behind and the feed skipped to the next keyframe.
    upload_skips: u64,
    /// ffmpeg ran slower than real time (speed < 0.95) for a few seconds in a row.
    slow_upload: bool,
    slow_ticks: u32,
    log: VecDeque<String>,
    ended_at: Option<Instant>,
}

pub(crate) struct Run {
    started: Instant,
    stop: AtomicBool,
    wake: tokio::sync::Notify,
    audio_kbps: u32,
    dests: Vec<Dest>,
    ffmpeg: FfmpegCmd,
    timing: Timing,
    st: Mutex<RunState>,
    sent: AtomicU64,
}

static RUN: Mutex<Option<Arc<Run>>> = Mutex::new(None);

fn current() -> Option<Arc<Run>> {
    RUN.lock().ok().and_then(|g| g.clone())
}

/// A broadcast is under way (or waiting for Play to come back): it needs the Remote Play session.
pub(crate) fn run_needs_session() -> bool {
    current().is_some_and(|r| !r.stopped() && !r.finished())
}

impl Run {
    fn set(&self, f: impl FnOnce(&mut RunState)) {
        if let Ok(mut g) = self.st.lock() {
            f(&mut g);
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify_waiters();
    }

    /// Nothing of the run is left: no ffmpeg, no connection to a site.
    fn finished(&self) -> bool {
        self.st
            .lock()
            .map(|g| matches!(g.phase, "ended" | "failed"))
            .unwrap_or(true)
    }

    /// The phase as anyone outside sees it: from a stop until ffmpeg has gone, "stopping".
    fn phase(&self, phase: &'static str) -> &'static str {
        if self.stopped() && !matches!(phase, "ended" | "failed") {
            "stopping"
        } else {
            phase
        }
    }

    /// Until the run has finished, or `limit` passed.
    async fn wait_finished(&self, limit: Duration) {
        let end = Instant::now() + limit;
        while !self.finished() && Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Sleeps `d`, or less if stopped.
    async fn sleep(&self, d: Duration) {
        // Registered before the check: `notify_waiters` keeps no permit, so a stop between the
        // check and the wait would otherwise sleep the whole backoff (up to 30 s).
        let woken = self.wake.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        if self.stopped() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(d) => {}
            _ = woken => {}
        }
    }

    fn secrets(&self) -> Vec<String> {
        self.dests
            .iter()
            .flat_map(|d| d.secrets.iter().cloned())
            .collect()
    }

    fn fail(&self, p: Problem) {
        crate::log_warn!("golive: stopped: {} ({})", p.message, p.code);
        self.set(|s| {
            s.phase = "failed";
            s.error = Some(p);
            s.retry_at = None;
            s.ended_at = Some(Instant::now());
        });
    }

    pub(crate) fn status(&self) -> serde_json::Value {
        let Ok(s) = self.st.lock() else {
            return serde_json::Value::Null;
        };
        let now = Instant::now();
        let p = s.progress.clone().unwrap_or_default();
        serde_json::json!({
            "phase": self.phase(s.phase),
            "error": s.error,
            "started_s": now.saturating_duration_since(self.started).as_secs(),
            "live_s": s.live_since.map(|t| now.saturating_duration_since(t).as_secs()),
            "reconnects": s.reconnects,
            "retry_in_s": s.retry_at.map(|t| t.saturating_duration_since(now).as_secs_f64().ceil() as u64),
            "kbps": (s.kbps).round(),
            "fps": (p.fps * 10.0).round() / 10.0,
            "speed": p.speed,
            "frames": p.frame,
            "dropped_frames": p.drop_frames,
            "upload_skips": s.upload_skips,
            "slow_upload": s.slow_upload,
            "audio_kbps": self.audio_kbps,
            "destinations": s.dests,
            "session": s.session,
            "log": s.log,
        })
    }
}

/// Where a destination streams to, for a bug report: its service and, for a custom server, only
/// the host (never the path or query, where a stream key may sit, nor the key itself).
pub(crate) fn server_host(server: &str) -> Option<String> {
    let rest = server.split_once("://").map(|(_, r)| r).unwrap_or(server);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // user:pass@host -> host
    let host = authority.rsplit('@').next().unwrap_or("");
    (!host.is_empty()).then(|| host.to_string())
}

/// Go live for a bug report: the destinations as service + host, the run's phase and health.
/// The run's ffmpeg lines and the labels the user typed are left out.
pub(crate) fn diag_summary() -> serde_json::Value {
    let (destinations, store_error) = match golive_dest::store() {
        Ok(s) => (
            s.list()
                .into_iter()
                .map(|d| {
                    serde_json::json!({
                        "service": d.dest.preset,
                        "server_host": server_host(&d.dest.server),
                        "enabled": d.dest.enabled,
                        "bandwidth_test": d.dest.bandwidth_test,
                        "key_lost": d.key_lost,
                    })
                })
                .collect::<Vec<_>>(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e)),
    };
    let run = current().and_then(|r| {
        let s = r.st.lock().ok()?;
        let now = Instant::now();
        Some(serde_json::json!({
            "phase": r.phase(s.phase),
            "error_code": s.error.as_ref().map(|e| e.code),
            "started_s": now.saturating_duration_since(r.started).as_secs(),
            "live_s": s.live_since.map(|t| now.saturating_duration_since(t).as_secs()),
            "ended_s_ago": s.ended_at.map(|t| now.saturating_duration_since(t).as_secs()),
            "reconnects": s.reconnects,
            "kbps": s.kbps.round(),
            "upload_skips": s.upload_skips,
            "slow_upload": s.slow_upload,
            "audio_kbps": r.audio_kbps,
            "destinations": s.dests.iter().map(|d| serde_json::json!({
                "service": d.preset,
                "state": d.state,
                "error_code": d.error.as_ref().map(|e| e.code),
            })).collect::<Vec<_>>(),
        }))
    });
    serde_json::json!({
        "ffmpeg": ffmpeg_tool::status_json().get("installed").cloned(),
        "destinations": destinations,
        "store_error": store_error,
        "run": run,
    })
}

/// How one ffmpeg run ended.
#[derive(Debug, Default)]
struct Exit {
    ran: Duration,
    out_time_s: f64,
    failure: Option<Failure>,
    session_ended: bool,
    rejoin: bool,
    spawn_error: Option<String>,
}

/// Starts a run, unless one is still going (None). The check and the start are one step under
/// the lock: two starts at once would otherwise both pass, and the first run would go on with
/// nothing able to stop it.
pub(crate) fn start_run(dests: Vec<Dest>, audio_kbps: u32, ffmpeg: FfmpegCmd) -> Option<Arc<Run>> {
    start_run_with(dests, audio_kbps, ffmpeg, Timing::default())
}

fn start_run_with(
    dests: Vec<Dest>,
    audio_kbps: u32,
    ffmpeg: FfmpegCmd,
    timing: Timing,
) -> Option<Arc<Run>> {
    let mut slot = RUN.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|old| !old.finished()) {
        return None;
    }
    let r = Arc::new(Run {
        started: Instant::now(),
        stop: AtomicBool::new(false),
        wake: tokio::sync::Notify::new(),
        audio_kbps,
        timing,
        st: Mutex::new(RunState {
            phase: "starting",
            dests: dests
                .iter()
                .map(|d| DestStatus {
                    id: d.id.clone(),
                    label: d.label.clone(),
                    preset: d.preset.id,
                    name: d.preset.name,
                    state: "connecting",
                    error: None,
                    bandwidth_test: d.bandwidth_test,
                    rejoins: 0,
                    early_fails: 0,
                })
                .collect(),
            ..Default::default()
        }),
        dests,
        ffmpeg,
        sent: AtomicU64::new(0),
    });
    *slot = Some(r.clone());
    drop(slot);
    crate::log_info!(
        "golive: going live to {}",
        r.dests
            .iter()
            .map(|d| format!("{} ({})", d.preset.name, d.out.format))
            .collect::<Vec<_>>()
            .join(", ")
    );
    tokio::spawn(run(r.clone()));
    Some(r)
}

async fn run(r: Arc<Run>) {
    let mut backoff = Backoff::default();
    let mut waiting_since: Option<Instant> = None;
    loop {
        if r.stopped() {
            break;
        }
        let Some(reader) = live_feed::open_reader("go-live") else {
            let since = *waiting_since.get_or_insert_with(Instant::now);
            r.set(|s| {
                s.phase = "waiting_for_play";
                s.retry_at = None;
                for d in s.dests.iter_mut().filter(|d| d.state != "failed") {
                    d.state = "connecting";
                }
            });
            if since.elapsed() >= r.timing.wait_for_play {
                r.fail(Problem {
                    code: "play_ended",
                    message: "Remote Play stopped, so the stream ended.".into(),
                });
                break;
            }
            r.sleep(Duration::from_millis(500)).await;
            continue;
        };
        waiting_since = None;
        // The session (it may be a new one, with another codec) against each site.
        let session = reader.session.clone();
        let mut first_block: Option<Problem> = None;
        let active: Vec<usize> = {
            let Ok(mut s) = r.st.lock() else { break };
            s.session = Some(session.clone());
            let mut v = Vec::new();
            for (i, d) in r.dests.iter().enumerate() {
                if s.dests[i].state == "failed" {
                    continue;
                }
                let blocking = golive_dest::check(d.preset, &session)
                    .into_iter()
                    .find(|c| c.blocking);
                match blocking {
                    Some(c) => {
                        let p = Problem {
                            code: "codec_not_allowed",
                            message: c.message,
                        };
                        first_block.get_or_insert(p.clone());
                        s.dests[i].state = "failed";
                        s.dests[i].error = Some(p);
                    }
                    None => {
                        s.dests[i].state = "connecting";
                        v.push(i);
                    }
                }
            }
            v
        };
        if active.is_empty() {
            let p = first_block.unwrap_or(Problem {
                code: "all_failed",
                message: "None of the destinations took the stream.".into(),
            });
            r.fail(p);
            break;
        }
        let exit = run_ffmpeg(&r, reader, &active).await;
        if r.stopped() {
            break;
        }
        if let Some(e) = exit.spawn_error {
            r.fail(Problem {
                code: "ffmpeg_failed",
                message: format!("Could not start ffmpeg: {e}"),
            });
            break;
        }
        if exit.session_ended {
            crate::log_info!("golive: the Remote Play session ended; waiting for a new one");
            continue;
        }
        if exit.rejoin {
            continue;
        }
        let f = gf::settle(
            exit.failure.unwrap_or(Failure::Other),
            exit.out_time_s,
            exit.ran,
        );
        match backoff.after(f, exit.ran, Instant::now()) {
            Next::Retry(d) => {
                crate::log_info!(
                    "golive: ffmpeg stopped ({f:?} after {:.0} s); again in {} s",
                    exit.ran.as_secs_f64(),
                    d.as_secs()
                );
                let msg = failure_message(&r, &active, f);
                r.set(|s| {
                    s.phase = "reconnecting";
                    s.reconnects += 1;
                    s.retry_at = Some(Instant::now() + d);
                    s.live_since = None;
                    s.error = Some(Problem {
                        code: failure_code(f),
                        message: msg.clone(),
                    });
                    // Not one that this run already left out for good (its own reason stands).
                    for &i in &active {
                        if s.dests[i].state != "failed" {
                            s.dests[i].state = "retrying";
                        }
                    }
                });
                r.sleep(d).await;
            }
            Next::GiveUp(f) => {
                let msg = failure_message(&r, &active, f);
                r.set(|s| {
                    for &i in &active {
                        if s.dests[i].state != "failed" {
                            s.dests[i].state = "failed";
                            s.dests[i].error = Some(Problem {
                                code: failure_code(f),
                                message: msg.clone(),
                            });
                        }
                    }
                });
                r.fail(Problem {
                    code: failure_code(f),
                    message: msg,
                });
                break;
            }
        }
    }
    r.set(|s| {
        if s.phase != "failed" {
            s.phase = "ended";
            s.retry_at = None;
            s.ended_at = Some(Instant::now());
        }
        s.live_since = None;
    });
    crate::log_info!(
        "golive: ended after {:.0} s, {} MB sent",
        r.started.elapsed().as_secs_f64(),
        r.sent.load(Ordering::Relaxed) / 1_000_000
    );
}

/// `f` in words, naming the site when all of `idx` are the same one.
fn failure_message(r: &Run, idx: &[usize], f: Failure) -> String {
    let mut v: Vec<&str> = idx.iter().map(|&i| r.dests[i].preset.name).collect();
    let mut seen = std::collections::HashSet::new();
    v.retain(|n| seen.insert(*n));
    if v.len() == 1 {
        f.message(v[0])
    } else {
        f.message_many()
    }
}

pub(crate) fn failure_code(f: Failure) -> &'static str {
    match f {
        Failure::Unreachable => "unreachable",
        Failure::Tls => "tls",
        Failure::Rejected => "rejected",
        Failure::Codec => "codec_not_allowed",
        Failure::Dropped => "dropped",
        Failure::Other => "stopped",
    }
}

fn rank(f: Failure) -> u8 {
    match f {
        Failure::Codec => 5,
        Failure::Rejected => 4,
        // Above "unreachable": a failed handshake is followed by "Cannot open connection".
        Failure::Tls => 3,
        Failure::Unreachable => 2,
        Failure::Dropped => 1,
        Failure::Other => 0,
    }
}

/// One ffmpeg process for the outputs `idx`, fed from `reader`, until it stops.
async fn run_ffmpeg(r: &Arc<Run>, mut reader: live_feed::FeedReader, idx: &[usize]) -> Exit {
    let outs: Vec<Output> = idx.iter().map(|&i| r.dests[i].out.clone()).collect();
    let hevc = reader.session.codec != "h264";
    let mut cmd = r.ffmpeg.command();
    cmd.args(gf::args(&outs, r.audio_kbps, hevc))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Exit {
                spawn_error: Some(e.to_string()),
                ..Default::default()
            }
        }
    };
    let started = Instant::now();
    let failure: Arc<Mutex<Option<Failure>>> = Arc::new(Mutex::new(None));
    let out_time = Arc::new(Mutex::new(0f64));
    // A failed output of several: its index among `idx`, when it went.
    let rejoin_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    // Upload skips of the earlier ffmpegs of this run (each reader counts from zero).
    let skips_before = r.st.lock().map(|g| g.upload_skips).unwrap_or(0);

    // Progress, once a second.
    let progress_task = child.stdout.take().map(|so| {
        let r = r.clone();
        let out_time = out_time.clone();
        let idx = idx.to_vec();
        tokio::spawn(async move {
            let mut lines = BufReader::new(so).lines();
            let mut parser = gf::ProgressParser::default();
            while let Ok(Some(line)) = lines.next_line().await {
                let Some(p) = parser.push(&line) else {
                    continue;
                };
                if let Ok(mut t) = out_time.lock() {
                    *t = p.out_time_s;
                }
                let flowing = p.flowing();
                r.set(|s| {
                    if flowing {
                        if s.phase != "live" {
                            s.phase = "live";
                            s.error = None;
                            s.retry_at = None;
                        }
                        s.live_since.get_or_insert_with(Instant::now);
                        for &i in &idx {
                            if s.dests[i].state == "connecting" {
                                s.dests[i].state = "live";
                                s.dests[i].error = None;
                            }
                        }
                    }
                    let slow = p.speed.is_some_and(|x| x < 0.95) && p.out_time_s > 5.0;
                    s.slow_ticks = if slow { s.slow_ticks + 1 } else { 0 };
                    s.slow_upload = s.slow_ticks >= 4;
                    s.progress = Some(p);
                });
            }
        })
    });

    // The log: scrubbed, classified, and the tee muxer's word on each output.
    let log_task = child.stderr.take().map(|se| {
        let r = r.clone();
        let failure = failure.clone();
        let rejoin_at = rejoin_at.clone();
        let out_time = out_time.clone();
        let idx = idx.to_vec();
        let secrets = r.secrets();
        tokio::spawn(async move {
            let mut lines = BufReader::new(se).lines();
            let mut logged = 0u32;
            while let Ok(Some(line)) = lines.next_line().await {
                let clean = gf::redact(&line, &secrets);
                if clean.trim().is_empty() {
                    continue;
                }
                if logged < 40 {
                    logged += 1;
                    crate::log_info!("golive: ffmpeg: {clean}");
                }
                let kind = gf::classify(&line);
                // Which output a tee line is about (by its number, or its URL).
                let which = gf::failed_output(&line).or_else(|| {
                    let spec = gf::failed_open(&line)?;
                    idx.iter()
                        .position(|&i| spec.ends_with(r.dests[i].out.url.as_str()))
                });
                r.set(|s| {
                    s.log.push_back(clean.clone());
                    while s.log.len() > LOG_LINES {
                        s.log.pop_front();
                    }
                    if let (Some(n), true) = (which, idx.len() > 1) {
                        let Some(&i) = idx.get(n) else { return };
                        let t_out = out_time.lock().map(|t| *t).unwrap_or(0.0);
                        let d = &mut s.dests[i];
                        let (f, stay_out) = slave_failed(
                            kind,
                            t_out,
                            started.elapsed(),
                            &mut d.early_fails,
                            d.rejoins,
                        );
                        let site = r.dests[i].preset.name;
                        d.error = Some(Problem {
                            code: failure_code(f),
                            message: f.message(site),
                        });
                        if stay_out {
                            d.state = "failed";
                        } else {
                            d.state = "retrying";
                            d.rejoins += 1;
                            if let Ok(mut g) = rejoin_at.lock() {
                                g.get_or_insert(Instant::now() + r.timing.rejoin_after);
                            }
                        }
                    }
                });
                if let Some(k) = kind {
                    if let Ok(mut g) = failure.lock() {
                        if g.is_none_or(|old| rank(k) > rank(old)) {
                            *g = Some(k);
                        }
                    }
                }
            }
        })
    });

    // The feed into ffmpeg.
    let session_ended = Arc::new(AtomicBool::new(false));
    let stats = reader.stats.clone();
    let mut stdin = child.stdin.take();
    let writer = {
        let r = r.clone();
        let session_ended = session_ended.clone();
        tokio::spawn(async move {
            let Some(w) = stdin.as_mut() else { return };
            loop {
                match reader.next().await {
                    Some(b) => {
                        if w.write_all(&b).await.is_err() {
                            break;
                        }
                        r.sent.fetch_add(b.len() as u64, Ordering::Relaxed);
                    }
                    None => {
                        session_ended.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            }
            // Dropping stdin ends ffmpeg's input: it finishes and exits.
        })
    };

    let mut last_sent = r.sent.load(Ordering::Relaxed);
    let mut last_tick = Instant::now();
    let mut rejoin = false;
    let mut moved = (0f64, Instant::now());
    // Since when ffmpeg has had the end of the session and is finishing.
    let mut session_over: Option<Instant> = None;
    let grace = r.timing.stop_grace;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let status = loop {
        tokio::select! {
            st = child.wait() => break st.ok(),
            _ = r.wake.notified() => {
                if r.stopped() {
                    break finish(&mut child, &writer, grace).await;
                }
            }
            _ = tick.tick() => {
                if r.stopped() {
                    break finish(&mut child, &writer, grace).await;
                }
                let now = Instant::now();
                let sent = r.sent.load(Ordering::Relaxed);
                let secs = now.saturating_duration_since(last_tick).as_secs_f64().max(0.001);
                let kbps = (sent - last_sent) as f64 * 8.0 / 1000.0 / secs;
                last_sent = sent;
                last_tick = now;
                let skips = stats.dropped_slow.load(Ordering::Relaxed);
                r.set(|s| {
                    s.kbps = if s.kbps == 0.0 { kbps } else { s.kbps * 0.6 + kbps * 0.4 };
                    s.upload_skips = skips_before + skips;
                });
                let t_out = out_time.lock().map(|t| *t).unwrap_or(0.0);
                if t_out > moved.0 {
                    moved = (t_out, now);
                }
                // The session is over and ffmpeg has had the end of its input. If its upload is
                // stuck it never gets to finish, and the stall check below is off by now: the
                // run would stay "live" for as long as the system keeps that connection.
                if session_ended.load(Ordering::Relaxed) {
                    let since = *session_over.get_or_insert(now);
                    if now.saturating_duration_since(since) >= grace {
                        crate::log_info!("golive: ffmpeg did not finish after the session ended; stopping it");
                        let _ = child.kill().await;
                        break child.wait().await.ok();
                    }
                }
                let limit = if moved.0 > 0.0 { r.timing.stall_after } else { r.timing.connect_stall_after };
                if now.saturating_duration_since(moved.1) >= limit
                    && !session_ended.load(Ordering::Relaxed)
                {
                    crate::log_info!(
                        "golive: no progress for {} s: the connection is gone, starting again",
                        limit.as_secs()
                    );
                    if let Ok(mut g) = failure.lock() {
                        g.get_or_insert(if moved.0 > 0.0 {
                            Failure::Dropped
                        } else {
                            Failure::Unreachable
                        });
                    }
                    break finish(&mut child, &writer, grace).await;
                }
                let due = rejoin_at.lock().ok().and_then(|g| *g).is_some_and(|t| now >= t);
                if due {
                    crate::log_info!("golive: letting a dropped destination back in (short restart)");
                    rejoin = true;
                    break finish(&mut child, &writer, grace).await;
                }
            }
        }
    };
    writer.abort();
    let skips = stats.dropped_slow.load(Ordering::Relaxed);
    r.set(|s| s.upload_skips = skips_before + skips);
    if let Some(t) = progress_task {
        let _ = tokio::time::timeout(Duration::from_secs(2), t).await;
    }
    if let Some(t) = log_task {
        let _ = tokio::time::timeout(Duration::from_secs(2), t).await;
    }
    let ran = started.elapsed();
    let ok = status.is_some_and(|s| s.success());
    let failure = failure.lock().ok().and_then(|g| *g);
    crate::log_info!(
        "golive: ffmpeg exited ({}) after {:.0} s",
        status.map_or("killed".to_string(), |s| s.to_string()),
        ran.as_secs_f64()
    );
    Exit {
        ran,
        out_time_s: out_time.lock().map(|t| *t).unwrap_or(0.0),
        failure: if ok && failure.is_none() {
            None
        } else {
            failure.or(Some(Failure::Other))
        },
        session_ended: session_ended.load(Ordering::Relaxed),
        rejoin,
        spawn_error: None,
    }
}

/// What a failed site of several means: the failure to show, and whether it stays out rather
/// than being let back in. A refused key or a codec named in the log stays out at once. Most
/// sites refuse a key by just closing the connection, which reads as a drop: one that fails
/// right after a start is taken for refused (`gf::settle`), as with a single site, and like a
/// single site gets one more try. Any other failure right after a start (a server that can't be
/// reached) also stays out once it has failed that way twice in a row, and every restart costs
/// the other sites a moment of their stream.
fn slave_failed(
    named: Option<Failure>,
    out_time_s: f64,
    ran: Duration,
    early_fails: &mut u32,
    rejoins: u32,
) -> (Failure, bool) {
    let f = gf::settle(named.unwrap_or(Failure::Dropped), out_time_s, ran);
    if matches!(named, Some(Failure::Rejected | Failure::Codec)) {
        return (f, true);
    }
    let early = out_time_s < gf::EARLY_OUT_TIME_S && ran < gf::EARLY_RUN;
    *early_fails = if early { *early_fails + 1 } else { 0 };
    (f, *early_fails >= MAX_EARLY_FAILS || rejoins >= MAX_REJOINS)
}

/// Ends ffmpeg's input (it then finishes its outputs cleanly), and kills it if it takes long.
async fn finish(
    child: &mut tokio::process::Child,
    writer: &tokio::task::JoinHandle<()>,
    grace: Duration,
) -> Option<std::process::ExitStatus> {
    writer.abort();
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(st) => st.ok(),
        Err(_) => {
            let _ = child.kill().await;
            child.wait().await.ok()
        }
    }
}

// ---- API ----

fn err(code: StatusCode, error: &str, message: impl Into<String>) -> Response {
    (
        code,
        Json(serde_json::json!({ "error": error, "message": message.into() })),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct StartReq {
    #[serde(default)]
    destinations: Vec<StartDest>,
    #[serde(default)]
    audio_kbps: Option<u32>,
}

#[derive(Deserialize)]
struct StartDest {
    key_id: String,
}

/// Resolves saved destinations into a run's (with keys).
fn resolve(ids: &[String]) -> Result<Vec<Dest>, (StatusCode, &'static str, String)> {
    let store =
        golive_dest::store().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, "store", e))?;
    let mut v = Vec::new();
    for id in ids {
        let Some((d, s)) = store.get(id) else {
            return Err((
                StatusCode::NOT_FOUND,
                "destination_missing",
                "That destination is gone: add it again.".into(),
            ));
        };
        let p = golive_dest::preset(&d.preset).ok_or((
            StatusCode::BAD_REQUEST,
            "preset_unknown",
            "Unknown streaming site.".to_string(),
        ))?;
        let Some((url, format)) = golive_dest::publish_url(&d, &s) else {
            return Err((
                StatusCode::BAD_REQUEST,
                "key_lost",
                format!("{} has no stream key saved: add it again.", d.label),
            ));
        };
        v.push(Dest {
            id: d.id.clone(),
            label: d.label.clone(),
            preset: p,
            out: Output { url, format },
            secrets: s.sensitive(),
            bandwidth_test: d.bandwidth_test,
        });
    }
    Ok(v)
}

/// Go live answers this computer (a loopback peer, which must also address it by a loopback
/// name: a page whose name was re-pointed at 127.0.0.1, DNS rebinding, carries its own and could
/// otherwise send this computer's screen to its own server) and the peers the LAN guard admits
/// (`PS5UPLOAD_ALLOW_IP`: the web UI of an engine on a NAS or in Docker). The same rule as
/// Play Together's host API (join.rs).
fn guard(peer: std::net::IpAddr, headers: &HeaderMap) -> Option<Response> {
    (peer.is_loopback()
        && !crate::vinput_api::host_is_local(headers)
        && !crate::host_guard::host_named_by_operator(headers))
    .then(crate::vinput_api::host_not_local)
}

/// `POST /api/stream/start`.
pub async fn start(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<StartReq>,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    if req.destinations.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "no_destinations",
            "Pick at least one place to stream to.",
        );
    }
    // Each destination once, in the order given (dedup alone only drops neighbours: A, B, A
    // would publish twice with A's key and the site would refuse the second).
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<String> = req
        .destinations
        .iter()
        .map(|d| d.key_id.clone())
        .filter(|id| seen.insert(id.clone()))
        .collect();
    let dests = match resolve(&ids) {
        Ok(d) => d,
        Err((code, e, msg)) => return err(code, e, msg),
    };
    let Some(ff) = ffmpeg_tool::find() else {
        return (
            StatusCode::PRECONDITION_FAILED,
            Json(serde_json::json!({
                "error": "ffmpeg_missing",
                "message": "Streaming needs ffmpeg: download it first.",
                "ffmpeg": ffmpeg_tool::status_json(),
            })),
        )
            .into_response();
    };
    let Some(session) = live_feed::session_info() else {
        return err(
            StatusCode::CONFLICT,
            "no_session",
            "Start Remote Play first: Go live sends what it shows.",
        );
    };
    let blocked: Vec<serde_json::Value> = dests
        .iter()
        .filter_map(|d| {
            let c: Vec<_> = golive_dest::check(d.preset, &session)
                .into_iter()
                .filter(|c| c.blocking)
                .collect();
            (!c.is_empty())
                .then(|| serde_json::json!({"id": d.id, "name": d.preset.name, "checks": c}))
        })
        .collect();
    if !blocked.is_empty() {
        let first = blocked[0]["checks"][0]["message"]
            .as_str()
            .unwrap_or("")
            .to_string();
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "codec_not_allowed",
                "message": first,
                "codec": session.codec,
                "destinations": blocked,
            })),
        )
            .into_response();
    }
    // A stop still finishing (a script's stop-then-start): its ffmpeg is about to go, and a site
    // would refuse a second stream on the key it still holds.
    if let Some(old) = current().filter(|r| r.stopped()) {
        old.wait_finished(STOP_WAIT).await;
    }
    let audio = req.audio_kbps.unwrap_or(160).clamp(64, 320);
    let Some(r) = start_run(dests, audio, ff) else {
        return err(
            StatusCode::CONFLICT,
            "already_live",
            "Already live: stop first.",
        );
    };
    Json(serde_json::json!({ "ok": true, "live": r.status() })).into_response()
}

/// `POST /api/stream/stop`.
pub async fn stop(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    match current() {
        Some(r) => {
            r.request_stop();
            crate::log_info!("golive: stop asked");
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        None => Json(serde_json::json!({ "ok": true, "live": null })).into_response(),
    }
}

/// `GET /api/stream/status`.
pub async fn status(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    let session = live_feed::session_info();
    let check = |p: &Preset| {
        session
            .as_ref()
            .map(|s| golive_dest::check(p, s))
            .unwrap_or_default()
    };
    let presets: Vec<serde_json::Value> = golive_dest::PRESETS
        .iter()
        .map(|p| {
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["checks"] = serde_json::json!(check(p));
            v
        })
        .collect();
    let (destinations, store_error) = match golive_dest::store() {
        Ok(s) => (
            s.list()
                .into_iter()
                .map(|d| {
                    let mut v = serde_json::to_value(&d).unwrap_or_default();
                    if let Some(p) = golive_dest::preset(&d.dest.preset) {
                        v["checks"] = serde_json::json!(check(p));
                    }
                    v
                })
                .collect::<Vec<_>>(),
            None,
        ),
        Err(e) => (Vec::new(), Some(e)),
    };
    // A finished run stays in the status for a while (its error, how long it went).
    let live = current().and_then(|r| {
        let ended = r.st.lock().ok().and_then(|g| g.ended_at);
        match ended {
            Some(t) if t.elapsed() > Duration::from_secs(600) => None,
            _ => Some(r.status()),
        }
    });
    Json(serde_json::json!({
        "ffmpeg": ffmpeg_tool::status_json(),
        "session": session,
        "presets": presets,
        "destinations": destinations,
        "store_error": store_error,
        "live": live,
    }))
    .into_response()
}

fn invalid(e: golive_dest::Invalid) -> Response {
    err(error_status(e.0), e.0, e.1)
}

/// The HTTP status of an error code: the request was fine when this computer's disk failed it.
fn error_status(code: &str) -> StatusCode {
    match code {
        "not_found" => StatusCode::NOT_FOUND,
        "disk" | "store" => StatusCode::INTERNAL_SERVER_ERROR,
        "unsupported" => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::BAD_REQUEST,
    }
}

/// `POST /api/stream/destinations`.
pub async fn add_destination(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(inp): Json<golive_dest::DestinationInput>,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    let store = match golive_dest::store() {
        Ok(s) => s,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store", e),
    };
    match store.add(&inp) {
        Ok(v) => {
            crate::log_info!("golive: destination {} ({}) added", v.dest.id, v.name);
            Json(v).into_response()
        }
        Err(e) => invalid(e),
    }
}

/// `POST /api/stream/destinations/{id}`.
pub async fn update_destination(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(inp): Json<golive_dest::DestinationInput>,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    let store = match golive_dest::store() {
        Ok(s) => s,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store", e),
    };
    match store.update(&id, &inp) {
        Ok(v) => Json(v).into_response(),
        Err(e) => invalid(e),
    }
}

/// `DELETE /api/stream/destinations/{id}`.
pub async fn remove_destination(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    let store = match golive_dest::store() {
        Ok(s) => s,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, "store", e),
    };
    if store.remove(&id) {
        crate::log_info!("golive: destination {id} removed");
        Json(serde_json::json!({"ok": true})).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "not_found", "No such destination.")
    }
}

/// `POST /api/stream/tools/ffmpeg/install`.
pub async fn install_ffmpeg(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if let Some(r) = guard(peer.ip(), &headers) {
        return r;
    }
    match ffmpeg_tool::start_install() {
        Ok(_) => Json(serde_json::json!({"ok": true, "ffmpeg": ffmpeg_tool::status_json()}))
            .into_response(),
        Err((code, msg)) => err(error_status(code), code, msg),
    }
}

#[cfg(test)]
#[path = "rp_golive_tests.rs"]
mod tests;
