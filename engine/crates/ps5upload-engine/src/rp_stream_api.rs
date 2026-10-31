//! Remote Play over a WebSocket.
//!
//! `GET /api/ps5/rp/ws` upgrades to a WebSocket that views the stream with a
//! paired console (see `ps5upload_core::rp_stream`, ported from chiaki-ng,
//! AGPL-3.0):
//!
//! * client → engine, text JSON: first `{"host", "regist_key", "rp_key",
//!   "width"?, "height"?, "fps"?, "bitrate_kbps"?, "codec"? ("h264", "h265",
//!   or "h265_hdr": HEVC Main10 in HDR), "keep_alive_s"?,
//!   "attach_only"?, "low_latency"?, "mic"?, "dualsense"?}`, then `{"pad": {"buttons", "l2", "r2",
//!   "lx", "ly", "rx", "ry"}}` whenever the pad changes, `{"keyframe": true}`,
//!   `{"ping": t}` (answered at once with `{"pong": t}`, for the page to time
//!   the round trip), or `{"stop": true}`.
//!   Microphone (experimental), only for a session started with `"mic": true`:
//!   `{"mic": true|false}` turns the console's mic
//!   on or off, and binary `[5][one Opus packet]` is one mic frame (48 kHz
//!   stereo, 10 ms, 40 bytes; see `rp_stream::mic`). Only the attached page's
//!   mic counts, and the mic goes off whenever the page changes.
//! * engine → client, binary `[u8 kind][payload]`: 0 = video codec header
//!   (Annex-B parameter sets), 1 = video access unit, 3 = audio header,
//!   4 = audio frame (Opus), 6 = DualSense haptics frame (10 ms of 3 kHz stereo s16le PCM).
//!   Text JSON = status (`{"stage": …}`), rumble (`{"rumble": [strong, weak]}`), light bar
//!   (`{"light": [r, g, b]}`) and, for a session started with `"dualsense": true`, adaptive
//!   triggers (`{"triggers": {"left": [mode, 10 bytes…], "right": […]}}`) and the console's
//!   effect strengths (`{"ds_intensity": [haptics, triggers]}`, 0 off 1 strong 2 medium 3 weak).
//!
//! The session belongs to the engine, not to the socket. The console serves
//! one session at a time and holds its slot for 12-30 s after one ends, so a
//! page that leaves (another screen, a reload, a dropped socket) only
//! detaches: the session waits `keep_alive_s` (60 by default) for a page to
//! attach again, with the pad let go. A start for the console already
//! streaming attaches to it (headers replayed, a keyframe asked for), and the
//! page it was showing on is told `{"stage": "moved"}`. Only `stop`, the wait
//! running out, or the console ends it; the page then gets `{"stage":
//! "ended"}` and a Close frame. `GET /api/ps5/rp/session` says what is
//! running. `GET /rp-player` is a test page.
//!
//! Latency: video goes through a queue a few frames deep (two for a page that
//! asked for `low_latency`); a page that falls behind loses frames (and the
//! picture waits for a keyframe) rather than watching a growing backlog. Audio
//! and status have their own queue, sent first.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use ps5upload_core::rp_stream::{
    run_stream, Codec, ControllerState, MicInput, Progress, StreamConfig, StreamSink,
};
use tokio::sync::mpsc;

/// Video frames queued for a page. At 60 fps this is ~100 ms: enough to ride
/// out a hiccup, never seconds of backlog.
const VIDEO_QUEUE: usize = 6;
/// For a page in low-latency mode: ~33 ms at 60 fps. A busy network costs it
/// more skips to the next keyframe.
const VIDEO_QUEUE_LOW_LATENCY: usize = 2;
/// Audio frames (100/s), status, rumble and light bar.
const CTL_QUEUE: usize = 512;
const DEFAULT_KEEP_ALIVE: Duration = Duration::from_secs(60);
const MAX_KEEP_ALIVE: Duration = Duration::from_secs(600);
/// Engine → page binary kind: one DualSense haptics frame (rp_stream `StreamSink::haptics`).
const KIND_HAPTICS: u8 = 6;
/// Page → engine binary kind: one microphone Opus packet.
const KIND_MIC: u8 = 5;
/// Mic frames waiting for the stream thread (10 ms each): a stalled thread
/// drops voice rather than playing it late.
const MIC_QUEUE: usize = 50;

#[derive(Debug, serde::Deserialize)]
struct StartReq {
    host: String,
    regist_key: String,
    rp_key: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    fps: Option<u32>,
    #[serde(default)]
    bitrate_kbps: Option<u32>,
    #[serde(default)]
    codec: Option<String>,
    /// How long the session waits for a page after the last one left.
    #[serde(default)]
    keep_alive_s: Option<u64>,
    /// Join the running session only; never start one (a page reconnecting).
    #[serde(default)]
    attach_only: bool,
    /// Set the session up for the microphone (experimental). Only then are
    /// `{"mic": …}` and mic frames used; without it the session is as before.
    #[serde(default)]
    mic: bool,
    /// A DualSense is attached to the page's computer: the console is told the pad is one and
    /// sends adaptive triggers and haptics. Without it the session is as before.
    #[serde(default)]
    dualsense: bool,
    /// A shorter video queue for this page (see VIDEO_QUEUE_LOW_LATENCY).
    #[serde(default)]
    low_latency: bool,
    /// Instant replay: keep the last this-many seconds (15, 30 or 60; 0 / none = off).
    #[serde(default)]
    replay_s: Option<u64>,
}

impl StartReq {
    fn video_queue(&self) -> usize {
        if self.low_latency {
            VIDEO_QUEUE_LOW_LATENCY
        } else {
            VIDEO_QUEUE
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct ClientMsg {
    #[serde(default)]
    pad: Option<ControllerState>,
    #[serde(default)]
    stop: bool,
    #[serde(default)]
    keyframe: bool,
    #[serde(default)]
    mic: Option<bool>,
    /// Instant replay switched on (seconds) or off (0) during the session.
    #[serde(default)]
    replay_s: Option<u64>,
    /// The page's clock, echoed back as `{"pong": …}`.
    #[serde(default)]
    ping: Option<serde_json::Value>,
}

/// The answer to a ping: the page's own value back, untouched.
fn pong(m: &ClientMsg) -> Option<Message> {
    m.ping
        .as_ref()
        .map(|t| text(serde_json::json!({ "pong": t })))
}

/// The page showing the stream: its two queues.
struct Viewer {
    id: u64,
    video: mpsc::Sender<Message>,
    ctl: mpsc::Sender<Message>,
}

/// The one running session.
struct Live {
    host: String,
    hevc: bool,
    keep_alive: Duration,
    /// Every pad state the page sent, in order: a quick tap is a press and a
    /// release, and keeping only the latest would lose the press.
    pad: Mutex<std::collections::VecDeque<ControllerState>>,
    /// Every controller's latest state: the page (source 0) and phones (pad_link.rs).
    sources: crate::pad_link::PadSources,
    stop: AtomicBool,
    viewer: Mutex<Option<Viewer>>,
    /// Since when no page has been attached.
    alone_since: Mutex<Option<Instant>>,
    video_header: Mutex<Option<Vec<u8>>>,
    audio_header: Mutex<Option<Vec<u8>>>,
    playing: AtomicBool,
    /// The page cannot use a frame until the next keyframe.
    need_key: AtomicBool,
    want_key: AtomicBool,
    dropped: AtomicU64,
    next_viewer: AtomicU64,
    /// The session was set up for the mic; if not, the mic is ignored.
    mic_enabled: AtomicBool,
    /// The microphone: on/off and Opus frames for the stream, in order.
    mic: Mutex<std::collections::VecDeque<MicInput>>,
    mic_on: AtomicBool,
    /// Instant replay's frames, while it is on (rp_replay.rs).
    replay: Mutex<Option<crate::rp_replay::Replay>>,
    /// Watchers (pad_link.rs, watch party): each gets its own copy of the picture and sound.
    spectators: Mutex<Vec<Spectator>>,
}

/// A watcher's queue. `need_key`: it cannot use a frame until the next keyframe (it just
/// joined, or fell behind and was skipped ahead).
struct Spectator {
    id: u64,
    tx: mpsc::Sender<Message>,
    need_key: bool,
}

/// Frames queued for a watcher before it is skipped to the next keyframe (about 2 s at 60 fps).
const SPECTATOR_QUEUE: usize = 120;

/// The console serves one Remote Play session at a time; so do we.
static LIVE: Mutex<Option<Arc<Live>>> = Mutex::new(None);

/// The page's controller among the session's controllers (phones are 1 and up).
const PAGE_SOURCE: u64 = 0;

fn live() -> Option<Arc<Live>> {
    LIVE.lock().ok().and_then(|g| g.clone())
}

/// A phone's controller changed (pad_link.rs); dropped when no session runs.
pub(crate) fn push_pad(source: u64, state: Option<ControllerState>) {
    if let Some(l) = live() {
        l.push_pad(source, state);
    }
}

/// A watcher joins the session: its queue, with the codec headers already in it, and a
/// keyframe asked for. None when no session runs.
pub(crate) fn add_spectator(id: u64) -> Option<mpsc::Receiver<Message>> {
    let l = live()?;
    let (tx, rx) = mpsc::channel(SPECTATOR_QUEUE);
    let vh = l.video_header.lock().ok().and_then(|g| g.clone());
    let ah = l.audio_header.lock().ok().and_then(|g| g.clone());
    let _ = tx.try_send(text(serde_json::json!({ "watch": { "hevc": l.hevc } })));
    if let Some(h) = vh {
        let _ = tx.try_send(bin(0, &h));
    }
    if let Some(h) = ah {
        let _ = tx.try_send(bin(3, &h));
    }
    l.spectators.lock().ok()?.push(Spectator {
        id,
        tx,
        need_key: true,
    });
    l.want_key.store(true, Ordering::Relaxed);
    Some(rx)
}

/// A watcher left.
pub(crate) fn remove_spectator(id: u64) {
    if let Some(l) = live() {
        if let Ok(mut g) = l.spectators.lock() {
            g.retain(|s| s.id != id);
        }
    }
}

/// The console being played, if any.
pub(crate) fn live_host() -> Option<String> {
    live().map(|l| l.host.clone())
}

/// What a phone is told about the session: null when none runs.
pub(crate) fn live_state() -> serde_json::Value {
    live()
        .map(|l| l.snapshot())
        .unwrap_or(serde_json::Value::Null)
}

fn text(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

fn bin(kind: u8, payload: &[u8]) -> Message {
    let mut v = Vec::with_capacity(1 + payload.len());
    v.push(kind);
    v.extend_from_slice(payload);
    Message::Binary(Bytes::from(v))
}

/// True when an Annex-B access unit starts a picture a decoder can begin at.
fn is_keyframe(au: &[u8], hevc: bool) -> bool {
    let mut i = 0;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            if hevc {
                let t = (au[i + 3] >> 1) & 0x3f;
                if (16..=21).contains(&t) {
                    return true; // IRAP: BLA, IDR, CRA
                }
                if t < 16 {
                    return false;
                }
            } else {
                let t = au[i + 3] & 0x1f;
                if t == 5 {
                    return true;
                }
                if t == 1 {
                    return false;
                }
            }
        }
        i += 1;
    }
    false
}

impl Live {
    /// Every watcher gets `kind`'s message; a video access unit only from a keyframe on, and a
    /// watcher that fell behind skips to the next keyframe (it never slows the session).
    fn to_spectators(&self, kind: u8, data: &[u8], key: bool) {
        let Ok(mut g) = self.spectators.lock() else {
            return;
        };
        if g.is_empty() {
            return;
        }
        let mut lagging = false;
        for sp in g.iter_mut() {
            if kind == 1 {
                if sp.need_key && !key {
                    continue;
                }
                sp.need_key = false;
            }
            if let Err(mpsc::error::TrySendError::Full(_)) = sp.tx.try_send(bin(kind, data)) {
                if kind == 1 {
                    sp.need_key = true;
                    lagging = true;
                }
            }
        }
        if lagging {
            self.want_key.store(true, Ordering::Relaxed);
        }
    }

    /// Instant replay on (with that window) or off; a new window starts over.
    fn set_replay(&self, seconds: Option<u64>) {
        let want = crate::rp_replay::window_s(seconds);
        if let Ok(mut g) = self.replay.lock() {
            if g.as_ref().map(|r| r.window_s()) != want {
                *g = want.map(crate::rp_replay::Replay::new);
            }
        }
    }

    fn record(&self, kind: u8, t_us: u64, key: bool, data: &[u8]) {
        if let Ok(mut g) = self.replay.lock() {
            if let Some(r) = g.as_mut() {
                r.push(kind, t_us, key, data);
            }
        }
    }

    /// The replay as the page reads it, or None when it is off or holds nothing yet.
    fn replay_snapshot(&self) -> Option<Vec<u8>> {
        let vh = self.video_header.lock().ok()?.clone()?;
        let ah = self.audio_header.lock().ok().and_then(|g| g.clone());
        let g = self.replay.lock().ok()?;
        let r = g.as_ref()?;
        r.frames().next()?;
        Some(crate::rp_replay::encode(&vh, ah.as_deref(), r))
    }

    fn new(host: String, hevc: bool, keep_alive: Duration) -> Self {
        Live {
            host,
            hevc,
            keep_alive,
            pad: Default::default(),
            sources: Default::default(),
            stop: AtomicBool::new(false),
            viewer: Mutex::new(None),
            alone_since: Mutex::new(None),
            video_header: Mutex::new(None),
            audio_header: Mutex::new(None),
            playing: AtomicBool::new(false),
            need_key: AtomicBool::new(false),
            want_key: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            next_viewer: AtomicU64::new(1),
            mic_enabled: AtomicBool::new(false),
            mic: Default::default(),
            mic_on: AtomicBool::new(false),
            replay: Mutex::new(None),
            spectators: Mutex::new(Vec::new()),
        }
    }

    fn is_viewer(&self, id: u64) -> bool {
        self.viewer
            .lock()
            .map(|g| g.as_ref().is_some_and(|v| v.id == id))
            .unwrap_or(false)
    }

    /// The page `id` turns its mic on or off. Other pages are ignored.
    fn set_mic(&self, id: u64, on: bool) {
        if !self.mic_enabled.load(Ordering::Relaxed) || !self.is_viewer(id) {
            return;
        }
        self.mic_switch(on);
    }

    fn mic_switch(&self, on: bool) {
        if self.mic_on.swap(on, Ordering::Relaxed) == on {
            return;
        }
        if let Ok(mut q) = self.mic.lock() {
            if !on {
                q.retain(|m| !matches!(m, MicInput::Frame(_)));
            }
            q.push_back(if on { MicInput::On } else { MicInput::Off });
        }
    }

    /// One Opus packet from page `id`'s mic; dropped unless that page is
    /// the one attached and its mic is on.
    fn mic_frame(&self, id: u64, opus: &[u8]) {
        if !self.mic_on.load(Ordering::Relaxed) || !self.is_viewer(id) {
            return;
        }
        if let Ok(mut q) = self.mic.lock() {
            let frames = q.iter().filter(|m| matches!(m, MicInput::Frame(_))).count();
            if frames < MIC_QUEUE {
                q.push_back(MicInput::Frame(opus.to_vec()));
            }
        }
    }

    fn ctl(&self) -> Option<mpsc::Sender<Message>> {
        self.viewer.lock().ok()?.as_ref().map(|v| v.ctl.clone())
    }

    fn video_tx(&self) -> Option<mpsc::Sender<Message>> {
        self.viewer.lock().ok()?.as_ref().map(|v| v.video.clone())
    }

    fn send_ctl(&self, m: Message) {
        if let Some(tx) = self.ctl() {
            let _ = tx.try_send(m);
        }
    }

    /// Show the stream on a new page; the page it was on is told it moved.
    fn attach(&self, video: mpsc::Sender<Message>, ctl: mpsc::Sender<Message>) -> u64 {
        let id = self.next_viewer.fetch_add(1, Ordering::Relaxed);
        // The new page starts from the codec headers and the next keyframe.
        if let Some(h) = self.video_header.lock().ok().and_then(|g| g.clone()) {
            let _ = ctl.try_send(bin(0, &h));
        }
        if let Some(h) = self.audio_header.lock().ok().and_then(|g| g.clone()) {
            let _ = ctl.try_send(bin(3, &h));
        }
        if self.playing.load(Ordering::Relaxed) {
            let _ = ctl.try_send(text(
                serde_json::json!({"stage": "first_frame", "attached": true}),
            ));
        }
        self.need_key.store(true, Ordering::Relaxed);
        self.want_key.store(true, Ordering::Relaxed);
        let old = self
            .viewer
            .lock()
            .ok()
            .and_then(|mut g| g.replace(Viewer { id, video, ctl }));
        if let Some(old) = old {
            let _ = old
                .ctl
                .try_send(text(serde_json::json!({"stage": "moved"})));
        }
        if let Ok(mut a) = self.alone_since.lock() {
            *a = None;
        }
        // A new page starts with the mic off: it has not asked for it.
        self.mic_switch(false);
        id
    }

    /// The page `id` left: the session waits for another, with the pad let go.
    fn detach(&self, id: u64) {
        let Ok(mut g) = self.viewer.lock() else {
            return;
        };
        if g.as_ref().is_some_and(|v| v.id == id) {
            *g = None;
            if let Ok(mut a) = self.alone_since.lock() {
                *a = Some(Instant::now());
            }
            self.push_pad(PAGE_SOURCE, None);
        }
    }

    /// One controller changed (None: it left): the console gets every controller merged.
    fn push_pad(&self, source: u64, state: Option<ControllerState>) {
        let merged = self.sources.set(source, state);
        if let Ok(mut g) = self.pad.lock() {
            if g.len() < 256 {
                g.push_back(merged);
            }
            drop(g);
            self.mic_switch(false);
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        let attached = self.viewer.lock().map(|g| g.is_some()).unwrap_or(false);
        serde_json::json!({
            "host": self.host,
            "playing": self.playing.load(Ordering::Relaxed),
            "attached": attached,
        })
    }
}

struct Sink {
    live: Arc<Live>,
    t0: Instant,
}

impl StreamSink for Sink {
    fn progress(&mut self, p: Progress) {
        let ms = self.t0.elapsed().as_millis() as u64;
        let v = match p {
            Progress::Waking => serde_json::json!({"stage": "waking", "ms": ms}),
            Progress::Stats(s) => serde_json::json!({"stage": "stats", "stats": s}),
            Progress::Nonce => serde_json::json!({"stage": "nonce", "ms": ms}),
            Progress::SessionId => serde_json::json!({"stage": "session", "ms": ms}),
            Progress::TakionConnected => serde_json::json!({"stage": "takion", "ms": ms}),
            Progress::Bang => serde_json::json!({"stage": "bang", "ms": ms}),
            Progress::StreamInfo(r) => {
                serde_json::json!({"stage": "streaminfo", "ms": ms, "resolutions": r})
            }
            Progress::FirstFrame(n) => {
                self.live.playing.store(true, Ordering::Relaxed);
                serde_json::json!({"stage": "first_frame", "ms": ms, "bytes": n})
            }
        };
        self.live.send_ctl(text(v));
    }
    fn video(&mut self, au: &[u8], header: bool) -> bool {
        let live = &self.live;
        if header {
            // Kept for pages that attach later; sent ahead of the frames.
            if let Ok(mut h) = live.video_header.lock() {
                *h = Some(au.to_vec());
            }
            live.send_ctl(bin(0, au));
            live.to_spectators(0, au, false);
            return true;
        }
        let key = is_keyframe(au, live.hevc);
        live.record(
            crate::rp_replay::KIND_VIDEO,
            self.t0.elapsed().as_micros() as u64,
            key,
            au,
        );
        live.to_spectators(1, au, key);
        let Some(tx) = live.video_tx() else {
            return true; // nobody watching: the session waits for a page
        };
        if live.need_key.load(Ordering::Relaxed) {
            if !is_keyframe(au, live.hevc) {
                live.want_key.store(true, Ordering::Relaxed);
                return true;
            }
            live.need_key.store(false, Ordering::Relaxed);
        }
        if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(bin(1, au)) {
            // The page is behind: skip to the next keyframe rather than queue.
            live.dropped.fetch_add(1, Ordering::Relaxed);
            live.need_key.store(true, Ordering::Relaxed);
            live.want_key.store(true, Ordering::Relaxed);
        }
        true
    }
    fn audio(&mut self, frame: &[u8], header: bool) -> bool {
        if header {
            if let Ok(mut h) = self.live.audio_header.lock() {
                *h = Some(frame.to_vec());
            }
        } else {
            self.live.record(
                crate::rp_replay::KIND_AUDIO,
                self.t0.elapsed().as_micros() as u64,
                false,
                frame,
            );
        }
        self.live.send_ctl(bin(if header { 3 } else { 4 }, frame));
        self.live
            .to_spectators(if header { 3 } else { 4 }, frame, false);
        true
    }
    fn rumble(&mut self, left: u8, right: u8) {
        let v = serde_json::json!({"rumble": [left, right]});
        crate::pad_link::broadcast(&v.to_string());
        self.live.send_ctl(text(v));
    }
    fn light_bar(&mut self, rgb: [u8; 3]) {
        let v = serde_json::json!({"light": rgb});
        crate::pad_link::broadcast(&v.to_string());
        self.live.send_ctl(text(v));
    }
    fn trigger_effects(&mut self, left: (u8, [u8; 10]), right: (u8, [u8; 10])) {
        let side = |(mode, p): (u8, [u8; 10])| {
            let mut v = vec![mode];
            v.extend_from_slice(&p);
            v
        };
        self.live.send_ctl(text(
            serde_json::json!({"triggers": {"left": side(left), "right": side(right)}}),
        ));
    }
    fn dualsense_intensity(&mut self, haptics: u8, triggers: u8) {
        self.live.send_ctl(text(
            serde_json::json!({"ds_intensity": [haptics, triggers]}),
        ));
    }
    fn haptics(&mut self, pcm: &[u8]) {
        self.live.send_ctl(bin(KIND_HAPTICS, pcm));
    }
    fn input(&mut self) -> Option<ControllerState> {
        self.live.pad.lock().ok()?.pop_front()
    }
    fn stopped(&mut self) -> bool {
        if self.live.stop.load(Ordering::Relaxed) {
            return true;
        }
        self.live
            .alone_since
            .lock()
            .ok()
            .and_then(|a| *a)
            .is_some_and(|t| t.elapsed() > self.live.keep_alive)
    }
    fn wants_keyframe(&mut self) -> bool {
        let page = self.live.want_key.swap(false, Ordering::Relaxed);
        // Instant replay needs a keyframe every few seconds (a clip can only start at one).
        let now = self.t0.elapsed().as_micros() as u64;
        let replay = self
            .live
            .replay
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| r.wants_keyframe(now)))
            .unwrap_or(false);
        page || replay
    }
    fn mic(&mut self) -> Option<MicInput> {
        self.live.mic.lock().ok()?.pop_front()
    }
}

/// An error as the player shows it: a stable `code` to pick the plain-English
/// message from, plus the raw text for the details line.
fn error_status(e: &anyhow::Error) -> serde_json::Value {
    use ps5upload_core::rp_stream::guard::NotReady;
    let text = format!("{e:#}");
    let (code, retry_s) = match e.downcast_ref::<NotReady>() {
        Some(NotReady::Busy(d)) => ("busy", Some(d.as_secs().max(1))),
        Some(NotReady::Standby) => ("asleep", None),
        Some(NotReady::Unreachable) => ("unreachable", None),
        None if text.contains("refused the control session") => ("keys", None),
        // Session init answers 80108b12 while the console's own Remote Play switch is off.
        None if text.contains("80108b12") => ("rp_off", None),
        // Any other refusal at session init: a code nobody has explained yet (80108b26, …).
        None if text.contains("session init refused") => ("refused", None),
        None if text.contains("did not wake") => ("wake_failed", None),
        None if text.contains("login passcode") => ("passcode", None),
        None if text.contains("Nagare") || text.contains("AvCap") => ("video_refused", None),
        // Mid-stream: the console went quiet or closed the control connection.
        None if text.contains("console stopped sending")
            || text.contains("closed the Remote Play control connection") =>
        {
            ("lost", None)
        }
        None => ("other", None),
    };
    serde_json::json!({"stage": "error", "code": code, "retry_s": retry_s, "error": text})
}

pub async fn ws(upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(session)
}

/// What is streaming now, for a page that wants to show or rejoin it.
pub async fn current() -> Json<serde_json::Value> {
    let live = LIVE.lock().ok().and_then(|g| g.clone());
    Json(match live {
        Some(l) => l.snapshot(),
        None => serde_json::json!({"host": null}),
    })
}

/// `GET /api/ps5/rp/replay`: the instant replay's frames (rp_replay.rs), for the page to save as
/// a clip. 404 when no session runs, replay is off, or nothing is held yet.
pub async fn replay() -> Response {
    let snap = live().and_then(|l| l.replay_snapshot());
    match snap {
        Some(b) => (
            [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
            Bytes::from(b),
        )
            .into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no replay: start one, or wait for the first keyframe"})),
        )
            .into_response(),
    }
}

async fn close(socket: &mut WebSocket, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: reason.into(),
        })))
        .await;
}

/// Start the session `start` asks for on a new worker thread.
fn begin(start: &StartReq, creds: ps5upload_core::rp_session::SessionCreds) -> Arc<Live> {
    let mut cfg = StreamConfig::default();
    // PS5 Remote Play streams at most 1080p: asked for 2160p, the Pro (FW
    // 13.60) ends the stream with "Nagare did not init! AvCap failed to
    // initialize video: [InitResult:-6]", and chiaki-ng has no preset above
    // 1080p. So anything taller is asked for as 1080p.
    if let (Some(w), Some(h)) = (start.width, start.height) {
        let (w, h) = if h > 1080 { (1920, 1080) } else { (w, h) };
        cfg.width = w;
        cfg.height = h;
        cfg.bitrate_kbps = if h >= 1080 { 15000 } else { 10000 };
    }
    if let Some(f) = start.fps {
        cfg.fps = f;
    }
    if let Some(b) = start.bitrate_kbps {
        cfg.bitrate_kbps = b;
    }
    if let Some(c) = start.codec.as_deref().and_then(Codec::from_name) {
        cfg.codec = c;
    }
    cfg.mic = start.mic;
    cfg.dualsense = start.dualsense;
    let live = Arc::new(Live::new(
        start.host.clone(),
        cfg.codec.is_hevc(),
        start
            .keep_alive_s
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_KEEP_ALIVE)
            .min(MAX_KEEP_ALIVE),
    ));
    live.mic_enabled.store(cfg.mic, Ordering::Relaxed);
    live.set_replay(start.replay_s);
    let l = live.clone();
    tokio::task::spawn_blocking(move || {
        let mut sink = Sink {
            live: l.clone(),
            t0: Instant::now(),
        };
        // A panic must still end the session: left in LIVE, it would turn every later start
        // for this console into a join of a dead stream.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_stream(
                &l.host,
                &creds,
                cfg,
                Duration::from_secs(4 * 3600),
                &mut sink,
            )
        }))
        .unwrap_or_else(|p| {
            Err(anyhow::anyhow!(
                "the stream stopped on an internal error: {}",
                panic_text(&p)
            ))
        });
        let end = match r {
            Ok(rep) => serde_json::json!({
                "stage": "ended",
                "frames": rep.video.frames,
                "dropped": rep.video.frames_dropped,
                "rebuilt": rep.video.frames_recovered,
                "audio_frames": rep.audio_frames,
                "units_seen": rep.units_seen,
                "loss_seen": rep.loss_seen,
                "loss_reported": rep.loss_reported,
                "video_mb": rep.video.bytes as f64 / 1e6,
                "bad_mac": rep.av_bad_mac,
                "data_resent": rep.data_resent,
                "data_given_up": rep.data_given_up,
                "io_soft_errors": rep.io_soft_errors,
                "ws_dropped": l.dropped.load(Ordering::Relaxed),
                "reason": rep.disconnect_reason,
            }),
            Err(e) => error_status(&e),
        };
        crate::log_info!("rp_stream: {end}");
        // A fan boost for this console ends with its session.
        crate::fan_boost::session_ended(&l.host);
        if let Ok(mut g) = LIVE.lock() {
            if g.as_ref().is_some_and(|x| Arc::ptr_eq(x, &l)) {
                *g = None;
            }
        }
        // The last word, then the page's queues close and its socket with them.
        let viewer = l.viewer.lock().ok().and_then(|mut g| g.take());
        if let Some(v) = viewer {
            let _ = v.ctl.blocking_send(text(end));
        }
    });
    live
}

/// A panic's message, when it has one.
fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

async fn session(mut socket: WebSocket) {
    // The first message says where and how.
    let start: StartReq = match socket.recv().await {
        Some(Ok(Message::Text(t))) => {
            match serde_json::from_str(t.as_str()) {
                Ok(s) => s,
                Err(e) => {
                    let _ = socket
                    .send(text(serde_json::json!({"stage": "error", "error": format!("bad start: {e}")})))
                    .await;
                    close(&mut socket, "bad start").await;
                    return;
                }
            }
        }
        _ => return,
    };
    let (video_tx, mut video_rx) = mpsc::channel::<Message>(start.video_queue());
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<Message>(CTL_QUEUE);

    let running = LIVE.lock().ok().and_then(|g| g.clone());
    let live = match running {
        Some(l) if l.host == start.host => l,
        Some(l) => {
            let _ = socket
                .send(text(serde_json::json!({
                    "stage": "error",
                    "code": "busy_other",
                    "host": l.host,
                    "error": format!("Remote Play is already streaming {}", l.host),
                })))
                .await;
            close(&mut socket, "busy").await;
            return;
        }
        None if start.attach_only => {
            let _ = socket
                .send(text(
                    serde_json::json!({"stage": "ended", "reason": "no session"}),
                ))
                .await;
            close(&mut socket, "no session").await;
            return;
        }
        None => {
            let creds = match ps5upload_core::rp_session::SessionCreds::from_hex(
                &start.regist_key,
                &start.rp_key,
            ) {
                Ok(c) => c,
                Err(e) => {
                    let _ = socket
                        .send(text(
                            serde_json::json!({"stage": "error", "error": format!("{e:#}")}),
                        ))
                        .await;
                    close(&mut socket, "bad keys").await;
                    return;
                }
            };
            // Two pages starting at once: the second joins the first.
            let joined = {
                let Ok(mut g) = LIVE.lock() else {
                    return;
                };
                match g.as_ref() {
                    Some(l) if l.host == start.host => Some(l.clone()),
                    Some(_) => None,
                    None => {
                        let l = begin(&start, creds);
                        *g = Some(l.clone());
                        Some(l)
                    }
                }
            };
            match joined {
                Some(l) => l,
                None => {
                    let _ = socket
                        .send(text(serde_json::json!({"stage": "error", "code": "busy_other", "error": "Remote Play is already streaming another console"})))
                        .await;
                    close(&mut socket, "busy").await;
                    return;
                }
            }
        }
    };
    // Pongs go on this page's own status queue, behind what is already there,
    // so the time measured is the time the page's messages really take.
    let pong_tx = ctl_tx.clone();
    let id = live.attach(video_tx, ctl_tx);

    let mut reason = "ended";
    loop {
        tokio::select! {
            // Audio and status first: a big keyframe must not hold up sound.
            biased;
            out = ctl_rx.recv() => match out {
                Some(m) => {
                    let moved = matches!(&m, Message::Text(t) if t.as_str().contains("\"moved\""));
                    if socket.send(m).await.is_err() {
                        break;
                    }
                    if moved {
                        reason = "moved";
                        break;
                    }
                }
                None => break, // the session ended
            },
            out = video_rx.recv() => match out {
                Some(m) => {
                    if socket.send(m).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inc = socket.recv() => match inc {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(m) = serde_json::from_str::<ClientMsg>(t.as_str()) {
                        if let Some(p) = pong(&m) {
                            let _ = pong_tx.try_send(p);
                        }
                        if let Some(p) = m.pad {
                            live.push_pad(PAGE_SOURCE, Some(p));
                        }
                        if m.keyframe {
                            live.need_key.store(true, Ordering::Relaxed);
                            live.want_key.store(true, Ordering::Relaxed);
                        }
                        if m.stop {
                            live.stop.store(true, Ordering::Relaxed);
                        }
                        if let Some(on) = m.mic {
                            live.set_mic(id, on);
                        }
                        if m.replay_s.is_some() {
                            live.set_replay(m.replay_s);
                        }
                    }
                }
                Some(Ok(Message::Binary(b))) if b.first() == Some(&KIND_MIC) => {
                    live.mic_frame(id, &b[1..]);
                }
                // The page left: the session waits for it (or another).
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    live.detach(id);
                    return;
                }
                _ => {}
            },
        }
    }
    live.detach(id);
    close(&mut socket, reason).await;
}

pub async fn player() -> impl IntoResponse {
    Html(include_str!("rp_player.html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchers_start_at_a_keyframe_and_a_slow_one_skips_ahead() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let (fast_tx, mut fast) = mpsc::channel(8);
        let (slow_tx, mut slow) = mpsc::channel(1);
        live.spectators.lock().unwrap().extend([
            Spectator {
                id: 1,
                tx: fast_tx,
                need_key: true,
            },
            Spectator {
                id: 2,
                tx: slow_tx,
                need_key: true,
            },
        ]);
        // A delta frame before any keyframe is useless to a new watcher: not sent.
        live.to_spectators(1, b"delta", false);
        assert!(fast.try_recv().is_err());
        // The keyframe goes to both; the slow one's queue is now full.
        live.to_spectators(1, b"key", true);
        assert!(fast.try_recv().is_ok());
        // Sound goes along; the full one just misses it.
        live.to_spectators(4, b"opus", false);
        assert!(fast.try_recv().is_ok());
        // The next frame overflows the slow one: it waits for a keyframe, one is asked for, and
        // the fast one is not held back.
        live.want_key.store(false, Ordering::Relaxed);
        live.to_spectators(1, b"delta2", false);
        assert!(fast.try_recv().is_ok());
        assert!(live.spectators.lock().unwrap()[1].need_key);
        assert!(live.want_key.load(Ordering::Relaxed));
        let _ = slow.try_recv(); // the keyframe it had
        live.to_spectators(1, b"delta3", false);
        assert!(slow.try_recv().is_err());
        live.to_spectators(1, b"key2", true);
        assert!(slow.try_recv().is_ok());
    }

    const KEY: &[u8] = &[0, 0, 0, 1, 0x65, 1, 2]; // H.264 IDR slice
    const DELTA: &[u8] = &[0, 0, 0, 1, 0x41, 1, 2]; // H.264 non-IDR slice

    fn page() -> (
        mpsc::Sender<Message>,
        mpsc::Receiver<Message>,
        mpsc::Sender<Message>,
        mpsc::Receiver<Message>,
    ) {
        let (vt, vr) = mpsc::channel(VIDEO_QUEUE);
        let (ct, cr) = mpsc::channel(CTL_QUEUE);
        (vt, vr, ct, cr)
    }

    fn kinds(rx: &mut mpsc::Receiver<Message>) -> Vec<String> {
        let mut out = vec![];
        while let Ok(m) = rx.try_recv() {
            out.push(match m {
                Message::Binary(b) => format!("bin{}", b[0]),
                Message::Text(t) => t.to_string(),
                _ => "other".into(),
            });
        }
        out
    }

    #[test]
    fn a_ping_is_answered_with_its_own_value() {
        let m: ClientMsg = serde_json::from_str(r#"{"ping": 1234.5}"#).unwrap();
        match pong(&m) {
            Some(Message::Text(t)) => assert_eq!(t.as_str(), r#"{"pong":1234.5}"#),
            other => panic!("no pong: {other:?}"),
        }
        let pad: ClientMsg = serde_json::from_str(r#"{"keyframe": true}"#).unwrap();
        assert!(pong(&pad).is_none());
    }

    #[test]
    fn low_latency_asks_for_a_shorter_video_queue() {
        let normal: StartReq =
            serde_json::from_str(r##"{"host": "h", "regist_key": "", "rp_key": ""}"##).unwrap();
        assert_eq!(normal.video_queue(), VIDEO_QUEUE);
        let low: StartReq = serde_json::from_str(
            r##"{"host": "h", "regist_key": "", "rp_key": "", "low_latency": true}"##,
        )
        .unwrap();
        assert_eq!(low.video_queue(), VIDEO_QUEUE_LOW_LATENCY);
    }

    #[test]
    fn keyframes_by_nal_type() {
        assert!(is_keyframe(KEY, false));
        assert!(!is_keyframe(DELTA, false));
        assert!(is_keyframe(&[0, 0, 1, 19 << 1, 1], true)); // HEVC IDR_W_RADL
        assert!(!is_keyframe(&[0, 0, 1, 1 << 1, 1], true)); // HEVC TRAIL_R
    }

    #[test]
    fn a_stream_lost_midway_has_its_own_code() {
        for msg in [
            "the console stopped sending: nothing for 15 s (the network dropped, or the console ended the stream)",
            "the console closed the Remote Play control connection",
        ] {
            assert_eq!(error_status(&anyhow::anyhow!(msg))["code"], "lost", "{msg}");
        }
        assert_eq!(
            error_status(&anyhow::anyhow!("something else"))["code"],
            "other"
        );
    }

    #[test]
    fn remote_play_switched_off_has_its_own_code() {
        let e = anyhow::anyhow!("session init refused (HTTP 403): 80108b12");
        assert_eq!(error_status(&e)["code"], "rp_off");
        let e = anyhow::anyhow!("session init refused (HTTP 403): 80108b26");
        assert_eq!(error_status(&e)["code"], "refused");
    }

    #[test]
    fn a_panic_has_its_message() {
        let p = std::panic::catch_unwind(|| panic!("index out of bounds")).unwrap_err();
        assert_eq!(panic_text(&p), "index out of bounds");
    }

    #[test]
    fn a_page_that_joins_gets_headers_and_waits_for_a_keyframe() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        // Nobody watching yet: frames are let go, the headers kept.
        assert!(sink.video(&[0, 0, 1, 0x67], true));
        assert!(sink.audio(&[2; 14], true));
        assert!(sink.video(KEY, false));
        sink.progress(Progress::FirstFrame(7));

        let (vt, mut vr, ct, mut cr) = page();
        live.attach(vt, ct);
        let ctl = kinds(&mut cr);
        assert_eq!(&ctl[..2], ["bin0", "bin3"]);
        assert!(ctl[2].contains("first_frame"));
        assert!(sink.wants_keyframe());
        // A delta before the keyframe is useless to the new page.
        sink.video(DELTA, false);
        sink.video(KEY, false);
        sink.video(DELTA, false);
        assert_eq!(kinds(&mut vr), ["bin1", "bin1"]);
    }

    #[test]
    fn a_second_page_takes_the_stream_and_the_first_is_told() {
        let live = Live::new("h".into(), false, DEFAULT_KEEP_ALIVE);
        let (vt, _vr, ct, mut cr) = page();
        let first = live.attach(vt, ct);
        let (vt2, _vr2, ct2, _cr2) = page();
        live.attach(vt2, ct2);
        assert!(kinds(&mut cr).iter().any(|m| m.contains("moved")));
        // The first page leaving afterwards does not detach the second.
        live.detach(first);
        assert!(live.viewer.lock().unwrap().is_some());
    }

    #[test]
    fn dualsense_effects_reach_the_page() {
        let live = Arc::new(Live::new("h".into(), false, Duration::from_secs(60)));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, mut cr) = page();
        live.attach(vt, ct);
        let _ = kinds(&mut cr);
        sink.trigger_effects((0x21, [1; 10]), (0x05, [0; 10]));
        sink.dualsense_intensity(1, 2);
        sink.haptics(&[0u8; 120]);
        assert_eq!(
            kinds(&mut cr),
            vec![
                r#"{"triggers":{"left":[33,1,1,1,1,1,1,1,1,1,1],"right":[5,0,0,0,0,0,0,0,0,0,0]}}"#
                    .to_string(),
                r#"{"ds_intensity":[1,2]}"#.to_string(),
                "bin6".to_string(),
            ]
        );
        let s: StartReq = serde_json::from_str(
            r##"{"host": "h", "regist_key": "", "rp_key": "", "dualsense": true}"##,
        )
        .unwrap();
        assert!(s.dualsense);
    }

    #[test]
    fn a_page_leaving_lets_go_of_the_pad_and_starts_the_wait() {
        let live = Arc::new(Live::new("h".into(), false, Duration::from_millis(0)));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, _cr) = page();
        let id = live.attach(vt, ct);
        assert!(!sink.stopped());
        live.detach(id);
        assert_eq!(sink.input(), Some(ControllerState::default()));
        std::thread::sleep(Duration::from_millis(2));
        assert!(sink.stopped(), "no page came back within the wait");
    }

    #[test]
    fn only_the_attached_page_drives_the_mic() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        live.mic_enabled.store(true, Ordering::Relaxed);
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, _cr) = page();
        let first = live.attach(vt, ct);
        // Frames before the mic is on are dropped.
        live.mic_frame(first, &[1; 40]);
        assert_eq!(sink.mic(), None);
        live.set_mic(first, true);
        live.set_mic(first, true); // no second On
        live.mic_frame(first, &[2; 40]);
        assert_eq!(sink.mic(), Some(MicInput::On));
        assert_eq!(sink.mic(), Some(MicInput::Frame(vec![2; 40])));
        assert_eq!(sink.mic(), None);
        // Another page takes over: the mic goes off, the old page is ignored.
        let (vt2, _vr2, ct2, _cr2) = page();
        let second = live.attach(vt2, ct2);
        assert_eq!(sink.mic(), Some(MicInput::Off));
        live.set_mic(first, true);
        live.mic_frame(first, &[3; 40]);
        assert_eq!(sink.mic(), None);
        live.set_mic(second, true);
        for _ in 0..MIC_QUEUE + 10 {
            live.mic_frame(second, &[4; 40]);
        }
        assert_eq!(sink.mic(), Some(MicInput::On));
        let mut n = 0;
        while let Some(MicInput::Frame(_)) = sink.mic() {
            n += 1;
        }
        assert_eq!(n, MIC_QUEUE, "a stalled stream drops voice, not memory");
        // Leaving turns it off.
        live.detach(second);
        assert_eq!(sink.mic(), Some(MicInput::Off));
    }

    #[test]
    fn a_session_without_the_mic_ignores_it() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, _cr) = page();
        let id = live.attach(vt, ct);
        live.set_mic(id, true);
        live.mic_frame(id, &[1; 40]);
        assert_eq!(sink.mic(), None);
        let s: StartReq =
            serde_json::from_str(r#"{"host":"h","regist_key":"","rp_key":""}"#).unwrap();
        assert!(!s.mic);
    }

    #[test]
    fn a_slow_page_skips_to_the_next_keyframe() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, mut vr, ct, _cr) = page();
        live.attach(vt, ct);
        sink.wants_keyframe();
        sink.video(KEY, false);
        for _ in 0..VIDEO_QUEUE + 3 {
            sink.video(DELTA, false);
        }
        assert_eq!(kinds(&mut vr).len(), VIDEO_QUEUE);
        assert!(sink.wants_keyframe());
        sink.video(DELTA, false);
        assert!(kinds(&mut vr).is_empty(), "deltas wait for the keyframe");
        sink.video(KEY, false);
        assert_eq!(kinds(&mut vr).len(), 1);
    }
}
