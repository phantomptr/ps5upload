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
//!   `{"ping": t}` (answered at once with `{"pong": t, "engine_us": e}`, for the page to time
//!   the round trip and, with `e` the engine's clock when the ping came, to line its clock up
//!   with the engine's), or `{"stop": true}`.
//!   Microphone (experimental), only for a session started with `"mic": true`:
//!   `{"mic": true|false}` turns the console's mic
//!   on or off, and binary `[5][one Opus packet]` is one mic frame (48 kHz
//!   stereo, 10 ms, 40 bytes; see `rp_stream::mic`). Only the attached page's
//!   mic counts, and the mic goes off whenever the page changes.
//!   Native keyboard & mouse (vinput_api.rs): `{"kbm": {"on", "mouse", "keyboard"}}` (repeated as
//!   its heartbeat) and binary `[8][16-byte report]`, from the attached page only; answered with
//!   `{"kbm": {"on", "mouse", "keyboard", "err", "reason"}}`.
//!   The console's on-screen keyboard, only for a session started with `"keyboard": true`:
//!   `{"keyboard_text": "…"}`, `{"keyboard_accept": true}`, `{"keyboard_reject": true}`
//!   (see rp_keyboard.rs).
//!   The user's sign-in passcode, when the console asks for it before the stream starts (the
//!   page is told `{"stage": "passcode", "retry": bool, "wait_s": n}`): `{"passcode": "1234"}`
//!   (four digits) or `{"passcode_cancel": true}`. Only the attached page's answer counts; the
//!   passcode is passed to the console and never kept or logged.
//! * engine → client, binary `[u8 kind][payload]`: 0 = video codec header
//!   (Annex-B parameter sets), 1 = video access unit, 3 = audio header,
//!   4 = audio frame (Opus), 6 = DualSense haptics frame (10 ms of 3 kHz stereo s16le PCM),
//!   7 = video access unit with the time the engine had it whole: `[7][u64 BE engine µs][AU]`,
//!   sent instead of 1 only to a page that started with `"timing": true` (older pages, the
//!   watch page and rp-player never ask, and keep getting 1).
//!   Text JSON = status (`{"stage": …}`), rumble (`{"rumble": [strong, weak]}`), light bar
//!   (`{"light": [r, g, b]}`) and, for a session started with `"dualsense": true`, adaptive
//!   triggers (`{"triggers": {"left": [mode, 10 bytes…], "right": […]}}`) and the console's
//!   effect strengths (`{"ds_intensity": [haptics, triggers]}`, 0 off 1 strong 2 medium 3 weak).
//!   For a session started with `"keyboard": true`, the console's keyboard
//!   (`{"keyboard": {"open": …, "text": …}}`, rp_keyboard.rs).
//!   A phone's touchpad & keyboard (pad_link.rs) reaches the page as text, for the page to merge
//!   into its own keyboard & mouse (it stays the only owner of the console's devices):
//!   `{"pad_kbm": {"id", "on", "mouse", "keyboard"}}` (the phone's heartbeat, every 500 ms while
//!   on), `{"pad_kbm": {"id", "r": "<32 hex chars: one 16-byte report>"}}`, and
//!   `{"pad_kbm": {"id", "left": true}}` when the phone leaves or loses the controller (let go
//!   of whatever it held).
//!
//! The session belongs to the engine, not to the socket. The console serves
//! one session at a time and holds its slot for 12-30 s after one ends, so a
//! page that leaves (another screen, a reload, a dropped socket) only
//! detaches: the session waits `keep_alive_s` (60 by default) for a page to
//! attach again, with the pad let go. So does a page that vanished without closing (it answers no
//! WebSocket ping, or a write to it stalls). A start for the console already
//! streaming attaches to it (headers replayed, a keyframe asked for), and the
//! page it was showing on is told `{"stage": "moved"}`. Only `stop`, the wait
//! running out, or the console ends it; the page then gets `{"stage":
//! "ended"}` and a Close frame. `GET /api/ps5/rp/session` says what is
//! running (with its `session` id). `GET /rp-player` is a test page.
//!
//! Picture on this device (#670, #649): `"picture": false` in the start message, `{"picture":
//! false}` later, or `POST /api/ps5/rp/picture` (rp_automation.rs) stop the page's picture and
//! sound and its controller, for the session (the page is told `{"picture": bool}`, on attach too
//! when it is off). Nothing else changes: watchers, players, the live feed (Go live, OBS) and
//! instant replay fan out before the page's queue, and a page left off keeps answering pings.
//! `{"picture": true}` brings it back without a restart: the headers again and a keyframe. While
//! it is off and Go live or Play Together is running, no page is needed for the session to go on
//! (`keep_alive_s` does not run out).
//!
//! Automation (rp_automation.rs, #615): a local script or agent can ask for the picture the page
//! shows (`GET /api/ps5/rp/frame`) and hold controller input as one more controller
//! (`POST /api/ps5/rp/input`), without taking the stream over. The engine decodes no video itself:
//! the attached page is sent `{"snapshot": {"id", "format", "max_width", "after_seq"?}}` and
//! answers with binary `[9][u32 BE meta length][meta JSON][image]` (see
//! `rp_automation::KIND_SNAPSHOT`). Watchers and phones are never asked.
//!
//! A session can also start with no page at all (rp_control.rs, `POST /api/ps5/rp/start`), and the
//! frame request then gets its picture from the live feed through ffmpeg.
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
    run_stream, valid_passcode, Codec, ControllerState, KeyboardEvent, KeyboardInput, MicInput,
    Passcode, Progress, StreamConfig, StreamSink, PASSCODE_CANCELLED, PASSCODE_CONSOLE_GAVE_UP,
    PASSCODE_TIMEOUT, PASSCODE_TOO_MANY, PASSCODE_WAIT,
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
/// A start while the console's last session is finishing: asked to look again this soon.
const STOPPING_RETRY_S: u64 = 2;
/// Pad states waiting for the stream thread.
const PAD_QUEUE: usize = 256;
/// Engine → page binary kind: one DualSense haptics frame (rp_stream `StreamSink::haptics`).
const KIND_HAPTICS: u8 = 6;
/// Page → engine binary kind: one microphone Opus packet.
const KIND_MIC: u8 = 5;
/// Engine → page binary kind: a video access unit after the engine's time it was whole, u64 BE
/// µs on `engine_us`'s clock (for a page that asked with `"timing": true`).
const KIND_VIDEO_TIMED: u8 = 7;
/// Mic frames waiting for the stream thread (10 ms each): a stalled thread
/// drops voice rather than playing it late.
const MIC_QUEUE: usize = 50;

#[path = "rp_automation.rs"]
pub(crate) mod automation;
#[path = "rp_control.rs"]
pub(crate) mod control;
#[path = "rp_live.rs"]
pub(crate) mod live_feed;

/// Session ids: each session the engine starts gets the next one, so a client naming an old
/// session is refused rather than steered into its replacement.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, serde::Deserialize)]
struct StartReq {
    #[serde(alias = "console", alias = "addr")]
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
    /// Video as kind 7 (each access unit with the engine's time), for the page's latency stages.
    #[serde(default)]
    timing: bool,
    /// Typing into the console's on-screen keyboard (rp_keyboard.rs). Without it the session
    /// is as before.
    #[serde(default)]
    keyboard: bool,
    /// The picture, sound and controller on this page: off for this session, or back on. Absent
    /// (a page that does not know, a reload): what the session has now.
    #[serde(default)]
    picture: Option<bool>,
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
    /// Native keyboard & mouse on or off, and its heartbeat (vinput_api.rs).
    #[serde(default)]
    kbm: Option<crate::vinput_api::KbmMsg>,
    /// Typing into the console's keyboard (rp_keyboard.rs).
    #[serde(flatten)]
    keyboard: crate::rp_keyboard::PageKeyboard,
    /// The user's passcode, when the console asked for it.
    #[serde(default)]
    passcode: Option<Secret>,
    /// The user would rather not enter it: the start ends.
    #[serde(default)]
    passcode_cancel: bool,
    /// The picture on this device on or off (see the module's top).
    #[serde(default)]
    picture: Option<bool>,
}

/// A passcode as the page sent it: never printed, not even in a debug dump.
#[derive(serde::Deserialize)]
#[serde(transparent)]
struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// The answer to a ping: the page's own value back, untouched, and the engine's clock now.
fn pong(m: &ClientMsg) -> Option<Message> {
    m.ping
        .as_ref()
        .map(|t| text(serde_json::json!({ "pong": t, "engine_us": engine_us() })))
}

/// The engine's clock for latency stamps: µs since the first time it was read, monotonic.
fn engine_us() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// `[7][t_us BE][au]`: a video access unit with the engine's time.
fn bin_timed(t_us: u64, au: &[u8]) -> Message {
    let mut v = Vec::with_capacity(9 + au.len());
    v.push(KIND_VIDEO_TIMED);
    v.extend_from_slice(&t_us.to_be_bytes());
    v.extend_from_slice(au);
    Message::Binary(Bytes::from(v))
}

/// A replay's video header, audio header and frames, ready to encode.
type ReplayParts = (Vec<u8>, Option<Vec<u8>>, Vec<crate::rp_replay::Frame>);

/// The page showing the stream: its two queues, and whether it takes timed video (kind 7).
struct Viewer {
    id: u64,
    video: mpsc::Sender<Message>,
    ctl: mpsc::Sender<Message>,
    timing: bool,
}

/// The one running session.
struct Live {
    host: String,
    /// This session's id (NEXT_SESSION), shown by `GET /api/ps5/rp/session`.
    session: u64,
    hevc: bool,
    keep_alive: Duration,
    /// Every pad state the page sent, in order: a quick tap is a press and a
    /// release, and keeping only the latest would lose the press.
    pad: Mutex<std::collections::VecDeque<ControllerState>>,
    /// Every controller's latest state: the page (source 0) and phones (pad_link.rs).
    sources: crate::pad_link::PadSources,
    /// A Play Together guest has the stream's controller: see [`guest_seat`].
    guest_seat: AtomicBool,
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
    /// No picture, sound or controller for the attached page: it only keeps the session's
    /// controls (see the module's top).
    page_off: AtomicBool,
    /// When a keyframe was last asked for on behalf of a watcher or player.
    guest_key_asked: Mutex<Option<Instant>>,
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
    /// The console's on-screen keyboard (rp_keyboard.rs).
    keyboard: crate::rp_keyboard::KeyboardLink,
    /// While the console waits for the user's passcode: whether the last one was wrong.
    passcode_ask: Mutex<Option<bool>>,
    /// The page's answer, until the stream thread takes it.
    passcode_answer: Mutex<Option<Passcode>>,
    /// A local automation client's frame requests and held input (rp_automation.rs).
    auto: automation::AutoState,
    /// The live feed's readers (rp_live.rs, OBS).
    feed: Arc<live_feed::LiveFeed>,
}

/// A watcher's queue. `need_key`: it cannot use a frame until the next keyframe (it just
/// joined, or fell behind and was skipped ahead).
struct Spectator {
    id: u64,
    tx: mpsc::Sender<Message>,
    need_key: bool,
    /// A player's (join.rs, Play Together) own shallow video queue: the codec header and the
    /// pictures go there, sound and the rest on `tx`. None for a watcher (everything on `tx`).
    video: Option<mpsc::Sender<Message>>,
    /// The codec header could not be queued: it goes again ahead of the next keyframe.
    need_header: bool,
    /// Pictures skipped to the next keyframe because the queue was full (players' health).
    skipped: Option<Arc<AtomicU64>>,
    /// When a keyframe was last asked for on its behalf.
    key_asked: Option<Instant>,
    /// Skips in a row (each soon after the last), and when the last was.
    lag_skips: u32,
    last_skip: Option<Instant>,
}

impl Spectator {
    fn watcher(id: u64, tx: mpsc::Sender<Message>) -> Self {
        Spectator {
            id,
            tx,
            need_key: true,
            video: None,
            need_header: false,
            skipped: None,
            key_asked: None,
            lag_skips: 0,
            last_skip: None,
        }
    }

    /// A picture did not fit its queue.
    fn skipped_at(&mut self, now: Instant) {
        if self
            .last_skip
            .is_none_or(|t| now.saturating_duration_since(t) > LAG_FORGIVEN)
        {
            self.lag_skips = 0;
        }
        self.lag_skips = self.lag_skips.saturating_add(1);
        self.last_skip = Some(now);
    }
}

/// A player's picture and sound (join.rs): the shallow video queue, everything else.
pub(crate) struct PlayerFeed {
    pub video: mpsc::Receiver<Message>,
    pub other: mpsc::Receiver<Message>,
    pub hevc: bool,
}

/// Frames queued for a watcher before it is skipped to the next keyframe (about 2 s at 60 fps).
const SPECTATOR_QUEUE: usize = 120;
/// The least time between keyframes asked for on behalf of watchers and players (the console has
/// one stream: each keyframe costs everyone's picture some quality, so one guest who can't keep
/// up must not set the rate).
const GUEST_KEY_GAP: Duration = Duration::from_secs(2);
/// …and for one who keeps falling behind: this many skips, each within [`LAG_FORGIVEN`] of the
/// last, and it gets a keyframe this rarely.
const LAG_SKIPS: u32 = 3;
const LAGGING_KEY_GAP: Duration = Duration::from_secs(5);
const LAG_FORGIVEN: Duration = Duration::from_secs(10);

/// How often a page's or phone's socket hears a WebSocket ping. The browser answers it itself,
/// without the page's timers, which a hidden tab has hardly any of.
pub(crate) const PEER_PING_EVERY: Duration =
    Duration::from_millis(if cfg!(test) { 100 } else { 1000 });
/// Pings in a row with nothing back at all: a device that vanished without closing (out of Wi-Fi
/// range, a laptop lid, a phone asleep). The kernel would take minutes to say so.
pub(crate) const PEER_QUIET_PINGS: u32 = 5;
/// A write this slow: nothing has drained the socket for that long, the same thing seen from the
/// other side. It would otherwise hold up everything else this socket's loop does.
const PEER_SEND_STALL: Duration = Duration::from_secs(if cfg!(test) { 1 } else { 5 });
/// What a page may send at once: a snapshot's picture, its metadata and its kind.
const PAGE_MSG_MAX: usize = automation::MAX_SNAPSHOT_BYTES + 64 * 1024;

/// Sends `m`; false when the socket is gone or stuck.
pub(crate) async fn send_limited(socket: &mut WebSocket, m: Message) -> bool {
    matches!(
        tokio::time::timeout(PEER_SEND_STALL, socket.send(m)).await,
        Ok(Ok(()))
    )
}

/// One console however it is spelled (`10.0.0.2`, `10.0.0.2:9120`).
fn same_console(a: &str, b: &str) -> bool {
    crate::vinput_api::host_key(a) == crate::vinput_api::host_key(b)
}

/// The console serves one Remote Play session at a time; so do we.
static LIVE: Mutex<Option<Arc<Live>>> = Mutex::new(None);

/// Tests that put a session in [`LIVE`] (phones, Go live) take turns.
#[cfg(test)]
pub(crate) static LIVE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The last session's start: what was asked, each handshake stage with its time, and how it
/// ended. For a bug report (`/api/diag/features`): a refusal like 80108b26 is gone from the page
/// once it is closed. No key, no address (the console is named by the report's own fields).
static LAST_SESSION: Mutex<Option<serde_json::Value>> = Mutex::new(None);

/// Most handshake stages kept in [`LAST_SESSION`] (a session has about eight).
const LAST_STAGES: usize = 16;

fn note_last(f: impl FnOnce(&mut serde_json::Value)) {
    if let Ok(mut g) = LAST_SESSION.lock() {
        if let Some(v) = g.as_mut() {
            f(v);
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Remote Play for a bug report: the session running now (counts and switches) and the last
/// session's start and end. Never keys, the passcode, or the console's address.
pub(crate) fn diag_summary() -> serde_json::Value {
    let live = live().map(|l| {
        let spectators = l.spectators.lock().map(|g| g.len()).unwrap_or(0);
        let players = l
            .spectators
            .lock()
            .map(|g| g.iter().filter(|s| s.video.is_some()).count())
            .unwrap_or(0);
        serde_json::json!({
            "session": l.session,
            "hevc": l.hevc,
            "playing": l.playing.load(Ordering::Relaxed),
            "page_attached": l.viewer.lock().map(|g| g.is_some()).unwrap_or(false),
            "picture": !l.picture_off(),
            "alone_s": l.alone_since.lock().ok().and_then(|g| g.map(|t| t.elapsed().as_secs())),
            "keep_alive_s": l.keep_alive.as_secs(),
            "watchers": spectators - players,
            "players": players,
            "ws_dropped": l.dropped.load(Ordering::Relaxed),
            "mic_enabled": l.mic_enabled.load(Ordering::Relaxed),
            "mic_on": l.mic_on.load(Ordering::Relaxed),
            "replay_on": l.replay.lock().map(|g| g.is_some()).unwrap_or(false),
            "live_feed_readers": l.feed.readers(),
            "passcode_waiting": l.passcode_ask.lock().map(|g| g.is_some()).unwrap_or(false),
        })
    });
    serde_json::json!({
        "live": live,
        "last_session": LAST_SESSION.lock().ok().and_then(|g| g.clone()),
    })
}

/// The page's controller among the session's controllers (phones are 1 and up).
const PAGE_SOURCE: u64 = 0;
/// A Play Together guest on the stream's own controller (join.rs `STREAM_SEAT`).
const GUEST_SEAT_SOURCE: u64 = u64::MAX - 1;

fn live() -> Option<Arc<Live>> {
    LIVE.lock().ok().and_then(|g| g.clone())
}

/// A phone's controller changed (pad_link.rs); dropped when no session runs.
pub(crate) fn push_pad(source: u64, state: Option<ControllerState>) {
    if let Some(l) = live() {
        l.push_pad(source, state);
    }
}

/// The session on `host`, if it is the one running.
fn live_on(host: &str) -> Option<Arc<Live>> {
    live().filter(|l| l.host == host)
}

/// Nobody on the host's side uses the stream's controller: the picture is off on their device and
/// no phone is linked as a controller, and no guest has it yet.
pub(crate) fn stream_seat_free(host: &str) -> bool {
    live_on(host).is_some_and(|l| l.seat_free())
}

/// A guest has the stream's controller, or no longer. The page stays off it meanwhile, also with
/// its picture back on, so two people never share one player.
pub(crate) fn guest_seat(host: &str, on: bool) {
    if let Some(l) = live_on(host) {
        l.set_guest_seat(on);
    }
}

/// The guest's state on the stream's controller (None: let go).
pub(crate) fn push_guest_pad(host: &str, state: Option<ControllerState>) {
    if let Some(l) = live_on(host) {
        l.push_pad(GUEST_SEAT_SOURCE, state);
    }
}

/// A clock for motion samples that come with none of their own: µs, wrapping.
pub(crate) fn motion_clock_us() -> u32 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_micros() as u32
}

/// A phone's touchpad & keyboard input for the page (pad_link.rs): `{"pad_kbm": v}` on the
/// viewer's socket, for the page to merge into its own keyboard & mouse. Dropped when no page
/// shows the session.
pub(crate) fn push_pad_kbm(v: serde_json::Value) {
    if let Some(l) = live() {
        l.send_ctl(text(serde_json::json!({ "pad_kbm": v })));
    }
}

/// A watcher joins the session: its queue, with the codec headers already in it, and a
/// keyframe asked for. None when no session runs.
pub(crate) fn add_spectator(id: u64) -> Option<mpsc::Receiver<Message>> {
    let l = live()?;
    let (tx, rx) = mpsc::channel(SPECTATOR_QUEUE);
    l.add_spectator(Spectator::watcher(id, tx), None);
    Some(rx)
}

/// A player joins (join.rs, Play Together) the session on `host`: like a watcher, but pictures
/// go through a queue of `video_queue` frames (a watcher's holds about 2 s), so a player is
/// never far behind: a full queue skips to the next keyframe (counted in `skipped`) instead of
/// adding delay. None when no session runs for `host`.
pub(crate) fn add_player(
    id: u64,
    host: &str,
    video_queue: usize,
    skipped: Arc<AtomicU64>,
) -> Option<PlayerFeed> {
    let l = live()
        .filter(|l| crate::vinput_api::host_key(&l.host) == crate::vinput_api::host_key(host))?;
    let (tx, other) = mpsc::channel(SPECTATOR_QUEUE);
    let (vtx, video) = mpsc::channel(video_queue.max(2));
    let sp = Spectator {
        video: Some(vtx.clone()),
        skipped: Some(skipped),
        ..Spectator::watcher(id, tx)
    };
    l.add_spectator(sp, Some(&vtx));
    Some(PlayerFeed {
        video,
        other,
        hevc: l.hevc,
    })
}

/// The session running for `host`: its id (a new id is a new session), or None.
pub(crate) fn live_session(host: &str) -> Option<u64> {
    let key = crate::vinput_api::host_key(host);
    live()
        .filter(|l| crate::vinput_api::host_key(&l.host) == key && !l.stop.load(Ordering::Relaxed))
        .map(|l| l.session)
}

/// A Play Together player's picture is waiting for a keyframe (it skipped frames it could not
/// keep up with, or a decode failed; join.rs): ask the console for one, within the limits for
/// guests ([`GUEST_KEY_GAP`]).
pub(crate) fn request_keyframe(host: &str, spectator: u64) {
    let key = crate::vinput_api::host_key(host);
    if let Some(l) = live()
        .filter(|l| crate::vinput_api::host_key(&l.host) == key && !l.stop.load(Ordering::Relaxed))
    {
        l.guest_key_ask(spectator, Instant::now());
    }
}

/// A watcher's picture needs a keyframe.
pub(crate) fn request_spectator_keyframe(id: u64) {
    if let Some(l) = live().filter(|l| !l.stop.load(Ordering::Relaxed)) {
        l.guest_key_ask(id, Instant::now());
    }
}

/// A watcher left.
pub(crate) fn remove_spectator(id: u64) {
    if let Some(l) = live() {
        if let Ok(mut g) = l.spectators.lock() {
            g.retain(|s| s.id != id);
        }
    }
}

/// A page shows the Remote Play session of `host` (that page owns the console's keyboard and mouse
/// input, vinput_api.rs).
pub(crate) fn page_attached(host: &str) -> bool {
    let key = crate::vinput_api::host_key(host);
    live().is_some_and(|l| {
        crate::vinput_api::host_key(&l.host) == key
            && l.viewer.lock().map(|g| g.is_some()).unwrap_or(false)
    })
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

/// `[kind][payload]`, the form every socket takes. One copy per frame: each sink clones the handle,
/// and the replay keeps `slice(1..)`.
fn tagged(kind: u8, payload: &[u8]) -> Bytes {
    let mut v = Vec::with_capacity(1 + payload.len());
    v.push(kind);
    v.extend_from_slice(payload);
    Bytes::from(v)
}

fn bin(kind: u8, payload: &[u8]) -> Message {
    Message::Binary(tagged(kind, payload))
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
    /// A watcher or player joins: the codec headers first (a player's video header on its video
    /// queue), then a keyframe is asked for.
    fn add_spectator(&self, sp: Spectator, video: Option<&mpsc::Sender<Message>>) {
        let vh = self.video_header.lock().ok().and_then(|g| g.clone());
        let ah = self.audio_header.lock().ok().and_then(|g| g.clone());
        let _ = sp
            .tx
            .try_send(text(serde_json::json!({ "watch": { "hevc": self.hevc } })));
        if let Some(h) = vh {
            let _ = video.unwrap_or(&sp.tx).try_send(bin(0, &h));
        }
        if let Some(h) = ah {
            let _ = sp.tx.try_send(bin(3, &h));
        }
        if let Ok(mut g) = self.spectators.lock() {
            g.push(sp);
        }
        self.want_key.store(true, Ordering::Relaxed);
    }

    /// Every watcher gets `kind`'s message; a video access unit only from a keyframe on, and a
    /// watcher that fell behind skips to the next keyframe (it never slows the session).
    #[cfg(test)]
    fn to_spectators(&self, kind: u8, data: &[u8], key: bool) {
        self.to_spectators_at(kind, data, key, Instant::now());
    }

    #[cfg(test)]
    fn to_spectators_at(&self, kind: u8, data: &[u8], key: bool, now: Instant) {
        self.to_spectators_tagged_at(kind, &tagged(kind, data), key, now);
    }

    /// `to_spectators` with the frame in its `[kind][payload]` form.
    fn to_spectators_tagged(&self, kind: u8, msg: &Bytes, key: bool) {
        self.to_spectators_tagged_at(kind, msg, key, Instant::now());
    }

    fn to_spectators_tagged_at(&self, kind: u8, msg: &Bytes, key: bool, now: Instant) {
        let Ok(mut g) = self.spectators.lock() else {
            return;
        };
        if g.is_empty() {
            return;
        }
        for sp in g.iter_mut() {
            if kind > 1 {
                // Sound and the rest: a full queue just misses it.
                let _ = sp.tx.try_send(Message::Binary(msg.clone()));
                continue;
            }
            // A player has its shallow queue for the header and pictures; a watcher one for all.
            let q = sp.video.clone().unwrap_or_else(|| sp.tx.clone());
            if kind == 0 {
                // Lost, the next keyframes would decode with the previous parameter sets.
                if q.try_send(Message::Binary(msg.clone())).is_err() {
                    sp.need_header = true;
                    sp.need_key = true;
                }
                continue;
            }
            if sp.need_key && !key {
                // Waiting for a keyframe nobody else asked for.
                self.lag_ask(sp, now);
                continue;
            }
            if sp.need_header {
                let h = self.video_header.lock().ok().and_then(|g| g.clone());
                match h {
                    Some(h) if q.try_send(bin(0, &h)).is_err() => {
                        sp.skipped_at(now);
                        self.lag_ask(sp, now);
                        continue;
                    }
                    _ => sp.need_header = false,
                }
            }
            sp.need_key = false;
            if let Err(mpsc::error::TrySendError::Full(_)) =
                q.try_send(Message::Binary(msg.clone()))
            {
                sp.need_key = true;
                sp.skipped_at(now);
                if let Some(c) = &sp.skipped {
                    c.fetch_add(1, Ordering::Relaxed);
                }
                self.lag_ask(sp, now);
            }
        }
    }

    /// `sp` waits for a keyframe: ask the console, unless that was done too recently. Every
    /// watcher and player together get at most one per [`GUEST_KEY_GAP`], and one who keeps
    /// falling behind only one per [`LAGGING_KEY_GAP`]; it still gets any keyframe that comes.
    fn lag_ask(&self, sp: &mut Spectator, now: Instant) {
        let own_gap = if sp.lag_skips >= LAG_SKIPS {
            LAGGING_KEY_GAP
        } else {
            GUEST_KEY_GAP
        };
        let ago = |t: Option<Instant>, gap: Duration| {
            t.is_none_or(|t| now.saturating_duration_since(t) >= gap)
        };
        if !ago(sp.key_asked, own_gap) {
            return;
        }
        let Ok(mut last) = self.guest_key_asked.lock() else {
            return;
        };
        if ago(*last, GUEST_KEY_GAP) {
            *last = Some(now);
            sp.key_asked = Some(now);
            self.want_key.store(true, Ordering::Relaxed);
        }
    }

    /// Watcher or player `id` itself asks for a keyframe.
    fn guest_key_ask(&self, id: u64, now: Instant) {
        if let Ok(mut g) = self.spectators.lock() {
            if let Some(sp) = g.iter_mut().find(|s| s.id == id) {
                self.lag_ask(sp, now);
            }
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

    fn record(&self, kind: u8, t_us: u64, key: bool, data: Bytes) {
        if let Ok(mut g) = self.replay.lock() {
            if let Some(r) = g.as_mut() {
                r.push(kind, t_us, key, data);
            }
        }
    }

    /// What the replay needs to be saved: both headers and the frames (handles, so this lock, which
    /// the stream thread needs for every frame, is held only for the list). None when it is off or
    /// holds nothing yet.
    fn replay_parts(&self) -> Option<ReplayParts> {
        let vh = self.video_header.lock().ok()?.clone()?;
        let ah = self.audio_header.lock().ok().and_then(|g| g.clone());
        let g = self.replay.lock().ok()?;
        let frames: Vec<crate::rp_replay::Frame> = g.as_ref()?.frames().cloned().collect();
        (!frames.is_empty()).then_some((vh, ah, frames))
    }

    fn new(host: String, hevc: bool, keep_alive: Duration) -> Self {
        Live {
            host,
            session: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            hevc,
            keep_alive,
            pad: Default::default(),
            sources: Default::default(),
            guest_seat: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            viewer: Mutex::new(None),
            alone_since: Mutex::new(None),
            video_header: Mutex::new(None),
            audio_header: Mutex::new(None),
            playing: AtomicBool::new(false),
            need_key: AtomicBool::new(false),
            want_key: AtomicBool::new(false),
            page_off: AtomicBool::new(false),
            guest_key_asked: Mutex::new(None),
            dropped: AtomicU64::new(0),
            next_viewer: AtomicU64::new(1),
            mic_enabled: AtomicBool::new(false),
            mic: Default::default(),
            mic_on: AtomicBool::new(false),
            replay: Mutex::new(None),
            spectators: Mutex::new(Vec::new()),
            keyboard: Default::default(),
            passcode_ask: Mutex::new(None),
            passcode_answer: Mutex::new(None),
            auto: Default::default(),
            feed: Arc::new(live_feed::LiveFeed::new(hevc)),
        }
    }

    /// What the page is told while the console waits for the passcode.
    fn passcode_status(retry: bool) -> Message {
        text(serde_json::json!({
            "stage": "passcode",
            "retry": retry,
            "wait_s": PASSCODE_WAIT.as_secs(),
        }))
    }

    /// The console asks for the passcode: ask the page (and any page that attaches meanwhile).
    fn ask_passcode(&self, retry: bool) {
        if let Ok(mut a) = self.passcode_answer.lock() {
            *a = None;
        }
        if let Ok(mut q) = self.passcode_ask.lock() {
            *q = Some(retry);
        }
        self.send_ctl(Self::passcode_status(retry));
    }

    /// Page `id` answered. Kept only from the attached page, while a passcode is asked for,
    /// and only four digits (or a cancel).
    fn answer_passcode(&self, id: u64, answer: Passcode) -> bool {
        if !self.is_viewer(id) {
            return false;
        }
        if let Passcode::Digits(d) = &answer {
            if !valid_passcode(d) {
                return false;
            }
        }
        let Ok(q) = self.passcode_ask.lock() else {
            return false;
        };
        if q.is_none() {
            return false;
        }
        match self.passcode_answer.lock() {
            Ok(mut a) => {
                *a = Some(answer);
                true
            }
            Err(_) => false,
        }
    }

    /// The answer for the stream thread, once.
    fn take_passcode(&self) -> Option<Passcode> {
        let a = self.passcode_answer.lock().ok()?.take()?;
        if let Ok(mut q) = self.passcode_ask.lock() {
            *q = None;
        }
        Some(a)
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

    /// The picture on this device off (or back on) for this session. Off: the page's queues get
    /// no more media, and what its controller and keyboard held is let go. Back on: the codec
    /// headers again and the next keyframe, so the picture returns without a restart. Says so to
    /// the page either way: the change may have come from the API.
    pub(crate) fn set_picture(&self, on: bool) {
        let was_on = !self.page_off.swap(!on, Ordering::Relaxed);
        if was_on != on {
            crate::log_info!(
                "rp_stream: {}: the picture on this device is {}",
                self.host,
                if on { "on" } else { "off" }
            );
        }
        self.send_ctl(text(serde_json::json!({ "picture": on })));
        if on {
            if was_on {
                return;
            }
            if let Some(h) = self.video_header.lock().ok().and_then(|g| g.clone()) {
                self.send_ctl(bin(0, &h));
            }
            if let Some(h) = self.audio_header.lock().ok().and_then(|g| g.clone()) {
                self.send_ctl(bin(3, &h));
            }
            self.need_key.store(true, Ordering::Relaxed);
            self.want_key.store(true, Ordering::Relaxed);
        } else if was_on {
            self.push_pad(PAGE_SOURCE, None);
            let viewer = self
                .viewer
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|v| v.id));
            if let Some(id) = viewer {
                crate::vinput_api::off(&self.host, crate::vinput_api::Owner::Page(id), "left");
            }
        }
    }

    fn picture_off(&self) -> bool {
        self.page_off.load(Ordering::Relaxed)
    }

    fn seat_free(&self) -> bool {
        self.picture_off() && !self.guest_seat.load(Ordering::Relaxed) && self.sources.is_empty()
    }

    fn set_guest_seat(&self, on: bool) {
        if self.guest_seat.swap(on, Ordering::Relaxed) == on {
            return;
        }
        crate::log_info!(
            "rp_stream: {}: a guest {} the stream's controller",
            self.host,
            if on { "has" } else { "left" }
        );
        self.push_pad(if on { PAGE_SOURCE } else { GUEST_SEAT_SOURCE }, None);
    }

    /// The page's controller counts: its picture is on and no guest has the stream's controller.
    fn page_pad_open(&self) -> bool {
        !self.picture_off() && !self.guest_seat.load(Ordering::Relaxed)
    }

    /// With no page attached, the session still has a reason to run: the picture is off here on
    /// purpose, and Go live or Play Together needs the stream.
    fn held_without_page(&self) -> bool {
        self.picture_off() && self.stream_needed()
    }

    fn stream_needed(&self) -> bool {
        crate::rp_golive::run_needs_session() || crate::join::needs_session(&self.host)
    }

    /// `keep_alive_s` ran out with no page, and nothing holds the session.
    fn alone_too_long(&self) -> bool {
        self.wait_ran_out() && !self.held_without_page()
    }

    fn wait_ran_out(&self) -> bool {
        self.alone_since
            .lock()
            .ok()
            .and_then(|a| *a)
            .is_some_and(|t| t.elapsed() > self.keep_alive)
    }

    fn ctl(&self) -> Option<mpsc::Sender<Message>> {
        self.viewer.lock().ok()?.as_ref().map(|v| v.ctl.clone())
    }

    /// The page's video queue, and whether it takes timed video.
    fn video_tx(&self) -> Option<(mpsc::Sender<Message>, bool)> {
        self.viewer
            .lock()
            .ok()?
            .as_ref()
            .map(|v| (v.video.clone(), v.timing))
    }

    fn send_ctl(&self, m: Message) {
        if let Some(tx) = self.ctl() {
            let _ = tx.try_send(m);
        }
    }

    /// Show the stream on a new page; the page it was on is told it moved.
    fn attach(
        &self,
        video: mpsc::Sender<Message>,
        ctl: mpsc::Sender<Message>,
        timing: bool,
    ) -> u64 {
        let id = self.next_viewer.fetch_add(1, Ordering::Relaxed);
        // The new page starts from the codec headers and the next keyframe, unless the picture is
        // off here: then it is told, and the headers come when it is turned back on.
        let off = self.picture_off();
        if off {
            let _ = ctl.try_send(text(serde_json::json!({ "picture": false })));
        } else {
            if let Some(h) = self.video_header.lock().ok().and_then(|g| g.clone()) {
                let _ = ctl.try_send(bin(0, &h));
            }
            if let Some(h) = self.audio_header.lock().ok().and_then(|g| g.clone()) {
                let _ = ctl.try_send(bin(3, &h));
            }
        }
        if self.playing.load(Ordering::Relaxed) {
            let _ = ctl.try_send(text(
                serde_json::json!({"stage": "first_frame", "attached": true}),
            ));
        }
        if let Some(k) = self.keyboard.on_attach() {
            let _ = ctl.try_send(text(k));
        }
        // The console still waits for the passcode: this page is the one to ask now.
        if let Some(retry) = self.passcode_ask.lock().ok().and_then(|g| *g) {
            let _ = ctl.try_send(Self::passcode_status(retry));
        }
        if !off {
            self.need_key.store(true, Ordering::Relaxed);
            self.want_key.store(true, Ordering::Relaxed);
        }
        let old = self.viewer.lock().ok().and_then(|mut g| {
            g.replace(Viewer {
                id,
                video,
                ctl,
                timing,
            })
        });
        if let Some(old) = old {
            // A picture asked of the page that just lost the stream will not come.
            self.auto.viewer_gone(old.id);
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
            drop(g);
            if let Ok(mut a) = self.alone_since.lock() {
                *a = Some(Instant::now());
            }
            self.auto.viewer_gone(id);
            self.push_pad(PAGE_SOURCE, None);
            self.mic_switch(false);
        }
    }

    /// One controller changed (None: it left): the console gets every controller merged.
    fn push_pad(&self, source: u64, state: Option<ControllerState>) {
        let merged = self.sources.set(source, state);
        if let Ok(mut g) = self.pad.lock() {
            if !self.playing.load(Ordering::Relaxed) {
                // Nothing reads these during a wake and the handshake. Replayed at the start they
                // would be stale presses; only where the pad is now counts.
                g.clear();
            } else if g.len() >= PAD_QUEUE {
                // The stream thread is held up: an old state goes, never the newest (a release
                // lost here would leave its press held).
                g.pop_front();
            }
            g.push_back(merged);
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        let attached = self.viewer.lock().map(|g| g.is_some()).unwrap_or(false);
        // The console waits for the user's passcode: phones and watchers are told only that (the
        // prompt itself goes to the page that runs the session, see ask_passcode).
        let passcode = self
            .passcode_ask
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false);
        serde_json::json!({
            "host": self.host,
            "session": self.session,
            "playing": self.playing.load(Ordering::Relaxed),
            "attached": attached,
            "passcode": passcode,
            // Off: the picture, sound and controller on this device are off for this session.
            "picture": !self.picture_off(),
            // Told to stop, and finishing (a wake or handshake takes a moment to cut short).
            "stopping": self.stop.load(Ordering::Relaxed),
        })
    }
}

struct Sink {
    live: Arc<Live>,
    t0: Instant,
}

impl Sink {
    /// A video frame to every sink. `key_known`: what the caller already found out about it (the
    /// scan is not free, and there is one per frame).
    fn video_known(&mut self, au: &[u8], header: bool, key_known: Option<bool>) -> bool {
        let live = &self.live;
        if header {
            // Kept for pages that attach later; sent ahead of the frames.
            if let Ok(mut h) = live.video_header.lock() {
                *h = Some(au.to_vec());
            }
            live.feed.video_header(au);
            if !live.picture_off() {
                live.send_ctl(bin(0, au));
            }
            live.to_spectators_tagged(0, &tagged(0, au), false);
            return true;
        }
        // When the engine had the frame whole, before any queue: the page's "net" stage.
        let t_us = engine_us();
        let key = key_known.unwrap_or_else(|| is_keyframe(au, live.hevc));
        let msg = tagged(1, au);
        live.record(
            crate::rp_replay::KIND_VIDEO,
            self.t0.elapsed().as_micros() as u64,
            key,
            msg.slice(1..),
        );
        live.to_spectators_tagged(1, &msg, key);
        if live.picture_off() {
            return true; // guests and the feed have it; this page asked not to
        }
        let Some((tx, timing)) = live.video_tx() else {
            return true; // nobody watching: the session waits for a page
        };
        if live.need_key.load(Ordering::Relaxed) {
            if !key {
                live.want_key.store(true, Ordering::Relaxed);
                return true;
            }
            live.need_key.store(false, Ordering::Relaxed);
        }
        let m = if timing {
            bin_timed(t_us, au)
        } else {
            Message::Binary(msg)
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(m) {
            // The page is behind: skip to the next keyframe rather than queue.
            live.dropped.fetch_add(1, Ordering::Relaxed);
            live.need_key.store(true, Ordering::Relaxed);
            live.want_key.store(true, Ordering::Relaxed);
        }
        true
    }
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
        if v["stage"] != "stats" {
            crate::log_detail!("rp_stream: {}: stage {v}", self.live.host);
            let stage = v.clone();
            note_last(|l| {
                if let Some(a) = l["stages"].as_array_mut() {
                    if a.len() < LAST_STAGES {
                        a.push(stage);
                    }
                }
            });
        }
        self.live.send_ctl(text(v));
    }
    fn video(&mut self, au: &[u8], header: bool) -> bool {
        self.video_known(au, header, None)
    }
    fn video_frame(&mut self, au: &[u8], index: u16) -> bool {
        // The live feed (OBS) first: it needs the frame index, the rest of the fan-out does not.
        let key = is_keyframe(au, self.live.hevc);
        self.live.feed.video(au, index, key, Instant::now());
        self.video_known(au, false, Some(key))
    }
    fn video_lost(&mut self, _from: u16, _to: u16) {
        self.live.feed.lost();
    }
    fn audio(&mut self, frame: &[u8], header: bool) -> bool {
        if header {
            if let Ok(mut h) = self.live.audio_header.lock() {
                *h = Some(frame.to_vec());
            }
            self.live.feed.audio_header(frame);
        } else {
            self.live.feed.audio(frame, Instant::now());
        }
        let kind = if header { 3 } else { 4 };
        let msg = tagged(kind, frame);
        if !header {
            self.live.record(
                crate::rp_replay::KIND_AUDIO,
                self.t0.elapsed().as_micros() as u64,
                false,
                msg.slice(1..),
            );
        }
        if !self.live.picture_off() {
            self.live.send_ctl(Message::Binary(msg.clone()));
        }
        self.live.to_spectators_tagged(kind, &msg, false);
        true
    }
    fn rumble(&mut self, left: u8, right: u8) {
        let v = serde_json::json!({"rumble": [left, right]});
        crate::pad_link::broadcast(&v.to_string());
        crate::join::stream_rumble(&self.live.host, left, right);
        if !self.live.picture_off() {
            self.live.send_ctl(text(v));
        }
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
        if !self.live.picture_off() {
            self.live.send_ctl(text(
                serde_json::json!({"triggers": {"left": side(left), "right": side(right)}}),
            ));
        }
    }
    fn dualsense_intensity(&mut self, haptics: u8, triggers: u8) {
        self.live.send_ctl(text(
            serde_json::json!({"ds_intensity": [haptics, triggers]}),
        ));
    }
    fn haptics(&mut self, pcm: &[u8]) {
        if !self.live.picture_off() {
            self.live.send_ctl(bin(KIND_HAPTICS, pcm));
        }
    }
    fn input(&mut self) -> Option<ControllerState> {
        self.live.pad.lock().ok()?.pop_front()
    }
    fn stopped(&mut self) -> bool {
        if self.live.stop.load(Ordering::Relaxed) {
            return true;
        }
        self.live.alone_too_long()
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
        // The live feed (OBS): a reader joining, catching up or after a loss, and every 2 s.
        let feed = self.live.feed.wants_keyframe(Instant::now());
        page || replay || feed
    }
    fn mic(&mut self) -> Option<MicInput> {
        self.live.mic.lock().ok()?.pop_front()
    }
    fn keyboard(&mut self, ev: KeyboardEvent) {
        let v = self.live.keyboard.event(ev);
        self.live.send_ctl(text(v));
    }
    fn keyboard_input(&mut self) -> Option<KeyboardInput> {
        self.live.keyboard.next()
    }
    fn passcode_needed(&mut self, retry: bool) {
        crate::log_info!("rp_stream: the console asks for the user's passcode (retry {retry})");
        self.live.ask_passcode(retry);
    }
    fn passcode(&mut self) -> Option<Passcode> {
        self.live.take_passcode()
    }
}

/// An error as the player shows it: a stable `code` to pick the plain-English
/// message from, plus the raw text for the details line.
fn error_status(e: &anyhow::Error) -> serde_json::Value {
    use ps5upload_core::rp_stream::guard::NotReady;
    let text = format!("{e:#}");
    let (code, retry_s) = match e.downcast_ref::<NotReady>() {
        Some(NotReady::Busy(d)) => ("busy", Some(d.as_secs().max(1))),
        Some(NotReady::InUse(d)) => ("in_use", Some(d.as_secs().max(1))),
        Some(NotReady::Standby) => ("asleep", None),
        Some(NotReady::Unreachable) => ("unreachable", None),
        None if text.contains("refused the control session") => ("keys", None),
        // Session init answers 80108b12 while the console's own Remote Play switch is off.
        None if text.contains("80108b12") => ("rp_off", None),
        // Any other refusal at session init: a code nobody has explained yet (80108b26, …).
        None if text.contains("session init refused") => ("refused", None),
        None if text.contains("did not wake") => ("wake_failed", None),
        None if text.contains(PASSCODE_CANCELLED) => ("passcode_cancelled", None),
        None if text.contains(PASSCODE_TOO_MANY) => ("passcode_too_many", None),
        None if text.contains(PASSCODE_TIMEOUT) || text.contains(PASSCODE_CONSOLE_GAVE_UP) => {
            ("passcode_timeout", None)
        }
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
    // A snapshot (rp_automation.rs) comes as one message: left at tungstenite's 16 MiB per frame,
    // a large one would end this socket, and the player's own picture with it.
    upgrade
        .max_message_size(PAGE_MSG_MAX)
        .max_frame_size(PAGE_MSG_MAX)
        .on_upgrade(session)
}

#[derive(serde::Deserialize)]
pub struct CurrentQuery {
    #[serde(default)]
    passive: Option<String>,
}

/// What is streaming now, for a page that wants to show or rejoin it.
///
/// Asking counts as use for a session started through the API (`keep_alive_s`), unless the asker
/// says `passive=1`: the app looks every few seconds whatever it is showing, and that must not keep
/// a script's forgotten session going.
pub async fn current(
    headers: axum::http::HeaderMap,
    axum::extract::Query(q): axum::extract::Query<CurrentQuery>,
) -> Json<serde_json::Value> {
    let live = LIVE.lock().ok().and_then(|g| g.clone());
    let local = crate::vinput_api::host_is_local(&headers);
    if local
        && !q
            .passive
            .as_deref()
            .is_some_and(|p| p == "1" || p == "true")
    {
        if let Some(l) = &live {
            control::touch(l);
        }
    }
    let mut v = current_for(live.as_ref().map(|l| l.snapshot()), local);
    if local {
        control::add_details(&mut v, live.as_deref());
    }
    Json(v)
}

/// The session's state for `current`. Its id is only for a local script driving it (#615,
/// AUTOMATION.md; the app's pages read only `host`): left out for a request that does not
/// address this computer as localhost.
fn current_for(snapshot: Option<serde_json::Value>, local: bool) -> serde_json::Value {
    let mut v = snapshot.unwrap_or_else(|| serde_json::json!({"host": null, "session": null}));
    if !local {
        if let Some(o) = v.as_object_mut() {
            o.remove("session");
        }
    }
    v
}

/// `GET /api/ps5/rp/replay`: the instant replay's frames (rp_replay.rs), for the page to save as
/// a clip. 404 when no session runs, replay is off, or nothing is held yet.
pub async fn replay() -> Response {
    let snap = match live().and_then(|l| l.replay_parts()) {
        // Up to 256 MiB of copying: not on a reactor thread.
        Some((vh, ah, frames)) => tokio::task::spawn_blocking(move || {
            crate::rp_replay::encode(&vh, ah.as_deref(), &frames)
        })
        .await
        .ok(),
        None => None,
    };
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
    send_limited(
        socket,
        Message::Close(Some(CloseFrame {
            code: 1000,
            reason: reason.into(),
        })),
    )
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
    cfg.keyboard = start.keyboard;
    let asked = serde_json::json!({
        "width": cfg.width,
        "height": cfg.height,
        "fps": cfg.fps,
        "bitrate_kbps": cfg.bitrate_kbps,
        "codec": format!("{:?}", cfg.codec),
        "mic": cfg.mic,
        "dualsense": cfg.dualsense,
        "keyboard": cfg.keyboard,
        "wake": cfg.wake,
        "keep_alive_s": start.keep_alive_s,
        "replay_s": start.replay_s,
        "low_latency": start.low_latency,
        "picture": start.picture,
    });
    crate::log_detail!("rp_stream: {}: starting a session: {asked}", start.host);
    if let Ok(mut g) = LAST_SESSION.lock() {
        *g = Some(serde_json::json!({
            "started_unix_ms": unix_ms(),
            "asked": asked,
            "stages": [],
            "end": null,
        }));
    }
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
    live.keyboard.enabled.store(cfg.keyboard, Ordering::Relaxed);
    live.set_replay(start.replay_s);
    live.feed.set_config(cfg);
    let l = live.clone();
    tokio::task::spawn_blocking(move || {
        let mut sink = Sink {
            live: l.clone(),
            t0: Instant::now(),
        };
        // A panic must still end the session: left in LIVE, it would turn every later start
        // for this console into a join of a dead stream.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // No time limit: a console that went quiet or closed ctrl ends the stream by itself
            // (SILENCE_LIMIT), and a long session or broadcast must not be cut for its length.
            run_stream(&l.host, &creds, cfg, Duration::MAX, &mut sink)
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
        let end_note = end.clone();
        note_last(|v| {
            v["end"] = end_note;
            v["ended_unix_ms"] = unix_ms().into();
        });
        // A fan boost for this console ends with its session.
        crate::fan_boost::session_ended(&l.host);
        // The live feed's readers reach the end of their stream.
        l.feed.close();
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
    crate::log_detail!(
        "rp_stream: {}: a page connects (attach_only {}, a session running: {})",
        start.host,
        start.attach_only,
        running
            .as_ref()
            .map(|l| if same_console(&l.host, &start.host) {
                "this console's"
            } else {
                "another console's"
            })
            .unwrap_or("none")
    );
    // On its way out: joining it would end with it, a moment later and with no word why.
    if running
        .as_ref()
        .is_some_and(|l| same_console(&l.host, &start.host) && l.stop.load(Ordering::Relaxed))
    {
        let m = if start.attach_only {
            serde_json::json!({"stage": "ended", "reason": "no session"})
        } else {
            serde_json::json!({
                "stage": "error",
                "code": "busy",
                "retry_s": STOPPING_RETRY_S,
                "error": "the last session is still finishing",
            })
        };
        let _ = socket.send(text(m)).await;
        close(&mut socket, "stopping").await;
        return;
    }
    let live = match running {
        Some(l) if same_console(&l.host, &start.host) => l,
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
                    Some(l) if same_console(&l.host, &start.host) => Some(l.clone()),
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
    let kbm_tx = ctl_tx.clone();
    if let Some(on) = start.picture {
        live.set_picture(on);
    }
    let id = live.attach(video_tx, ctl_tx, start.timing);
    crate::log_detail!("rp_stream: {}: page {id} shows the session", live.host);

    let mut reason = "ended";
    let mut ping = tokio::time::interval(PEER_PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Pings since the page was last heard from. Counted, not timed: a computer waking from sleep
    // has its clock moved on, not a silent page.
    let mut quiet = 0u32;
    loop {
        tokio::select! {
            // Audio and status first: a big keyframe must not hold up sound.
            biased;
            out = ctl_rx.recv() => match out {
                Some(m) => {
                    let moved = matches!(&m, Message::Text(t) if t.as_str().contains("\"moved\""));
                    if !send_limited(&mut socket, m).await {
                        reason = "gone";
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
                    if !send_limited(&mut socket, m).await {
                        reason = "gone";
                        break;
                    }
                }
                None => break,
            },
            inc = socket.recv() => { quiet = 0; match inc {
                Some(Ok(Message::Text(t))) => {
                    if let Ok(m) = serde_json::from_str::<ClientMsg>(t.as_str()) {
                        if let Some(p) = pong(&m) {
                            let _ = pong_tx.try_send(p);
                        }
                        if let Some(p) = m
                            .pad
                            .filter(|_| live.page_pad_open())
                        {
                            live.push_pad(PAGE_SOURCE, Some(p));
                        }
                        if let Some(on) = m.picture.filter(|_| live.is_viewer(id)) {
                            live.set_picture(on);
                        }
                        if m.keyframe {
                            crate::log_detail!("rp_stream: {}: page {id} asks for a keyframe", live.host);
                            if !live.picture_off() {
                                live.need_key.store(true, Ordering::Relaxed);
                                live.want_key.store(true, Ordering::Relaxed);
                            }
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
                        if let Some(k) = &m.kbm {
                            if live.is_viewer(id) && !live.picture_off() {
                                let tx = kbm_tx.clone();
                                let status: crate::vinput_api::StatusFn = Arc::new(move |v| {
                                    let _ = tx.try_send(text(v));
                                });
                                crate::vinput_api::control(&live.host, crate::vinput_api::Owner::Page(id), k, status);
                            }
                        }
                        if live.is_viewer(id) {
                            live.keyboard.page_typed(&m.keyboard);
                        }
                        if let Some(Secret(p)) = m.passcode {
                            live.answer_passcode(id, Passcode::Digits(p));
                        }
                        if m.passcode_cancel {
                            live.answer_passcode(id, Passcode::Cancel);
                        }
                    }
                }
                Some(Ok(Message::Binary(b))) if b.first() == Some(&KIND_MIC) => {
                    live.mic_frame(id, &b[1..]);
                }
                // The picture a local automation client asked for (rp_automation.rs).
                Some(Ok(Message::Binary(b))) if b.first() == Some(&automation::KIND_SNAPSHOT) => {
                    live.auto.page_answered(id, b.slice(1..));
                }
                // A native keyboard & mouse report: only from the page the session is shown on.
                Some(Ok(Message::Binary(b))) if b.first() == Some(&crate::vinput_api::KIND_KBM) => {
                    if live.is_viewer(id) && !live.picture_off() {
                        crate::vinput_api::report(&live.host, crate::vinput_api::Owner::Page(id), &b[1..]);
                    }
                }
                // The page left: the session waits for it (or another).
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    crate::log_detail!("rp_stream: {}: page {id} left; the session waits", live.host);
                    live.detach(id);
                    crate::vinput_api::off(&live.host, crate::vinput_api::Owner::Page(id), "left");
                    return;
                }
                _ => {}
            }},
            // After the socket, so a page whose messages wait behind a busy loop is not called
            // silent.
            _ = ping.tick() => {
                quiet += 1;
                if quiet > PEER_QUIET_PINGS || !send_limited(&mut socket, Message::Ping(Bytes::new())).await {
                    reason = "gone";
                    break;
                }
            }
        }
    }
    crate::log_detail!(
        "rp_stream: {}: page {id} off the session ({reason})",
        live.host
    );
    live.detach(id);
    // Whatever ended it (stop, the session over, another page): this page's keys and buttons go.
    crate::vinput_api::off(&live.host, crate::vinput_api::Owner::Page(id), "left");
    close(&mut socket, reason).await;
}

pub async fn player() -> impl IntoResponse {
    Html(include_str!("rp_player.html"))
}

/// A session in [`LIVE`] for other modules' tests (hold [`LIVE_TEST_LOCK`]).
#[cfg(test)]
pub(crate) mod test_live {
    use super::*;

    pub(crate) struct Session(Arc<Live>);

    impl Session {
        pub(crate) fn start(host: &str) -> Self {
            let live = Arc::new(Live::new(host.into(), false, DEFAULT_KEEP_ALIVE));
            *LIVE.lock().unwrap() = Some(live.clone());
            Session(live)
        }

        /// The pad the console would get now.
        pub(crate) fn merged(&self) -> ControllerState {
            self.0.sources.set(u64::MAX - 1, None)
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            *LIVE.lock().unwrap() = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session id (for a local script) is left out for a request not addressed to
    /// localhost; the rest, which the app reads, stays.
    #[test]
    fn the_session_id_goes_only_to_localhost() {
        let snap = || Some(serde_json::json!({"host": "10.0.0.9", "session": 3, "playing": true}));
        assert_eq!(current_for(snap(), true)["session"], 3);
        let v = current_for(snap(), false);
        assert!(v.get("session").is_none());
        assert_eq!(v["host"], "10.0.0.9");
        assert_eq!(v["playing"], true);
        assert_eq!(current_for(None, false), serde_json::json!({"host": null}));
    }

    #[test]
    fn the_console_asking_for_the_passcode_reaches_the_page_and_back() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let (vtx, _vrx) = mpsc::channel(8);
        let (ctx, mut crx) = mpsc::channel(8);
        let id = live.attach(vtx, ctx, false);
        let next_text = |rx: &mut mpsc::Receiver<Message>| loop {
            match rx.try_recv() {
                Ok(Message::Text(t)) => {
                    break serde_json::from_str::<serde_json::Value>(t.as_str()).unwrap()
                }
                Ok(_) => continue,
                Err(e) => panic!("no status: {e}"),
            }
        };
        // Nothing is taken before the console asks.
        assert!(!live.answer_passcode(id, Passcode::Digits("1234".into())));
        // A watcher (watch party): it never gets the prompt.
        let (stx, mut srx) = mpsc::channel(8);
        live.spectators.lock().unwrap().push(Spectator {
            need_key: true,
            ..Spectator::watcher(77, stx)
        });
        let mut sink = Sink {
            live: Arc::new(live),
            t0: Instant::now(),
        };
        assert_eq!(sink.live.snapshot()["passcode"], false);
        sink.passcode_needed(false);
        // Phones and watchers see only that the console waits (pad_link sends the snapshot).
        assert_eq!(sink.live.snapshot()["passcode"], true);
        let v = next_text(&mut crx);
        assert_eq!(v["stage"], "passcode");
        assert_eq!(v["retry"], false);
        assert_eq!(v["wait_s"], PASSCODE_WAIT.as_secs());
        assert!(srx.try_recv().is_err(), "a watcher was asked");
        // Nor can it answer.
        assert!(!sink
            .live
            .answer_passcode(77, Passcode::Digits("1234".into())));
        assert!(sink.passcode().is_none());
        // Not four digits, or from another page: ignored.
        assert!(!sink.live.answer_passcode(id, Passcode::Digits("12".into())));
        assert!(!sink
            .live
            .answer_passcode(id + 1, Passcode::Digits("1234".into())));
        // A ClientMsg carries it; the answer goes to the stream thread once.
        let m: ClientMsg = serde_json::from_str(r#"{"passcode":"1234"}"#).unwrap();
        assert!(!format!("{:?}", m.passcode).contains("1234"));
        assert!(sink
            .live
            .answer_passcode(id, Passcode::Digits(m.passcode.unwrap().0)));
        assert!(matches!(sink.passcode(), Some(Passcode::Digits(d)) if d == "1234"));
        assert!(sink.passcode().is_none());
        // Wrong: asked again, and a page attaching meanwhile is asked too.
        sink.passcode_needed(true);
        assert_eq!(next_text(&mut crx)["retry"], true);
        let (vtx2, _vrx2) = mpsc::channel(8);
        let (ctx2, mut crx2) = mpsc::channel(8);
        let id2 = sink.live.attach(vtx2, ctx2, false);
        assert_eq!(next_text(&mut crx2)["stage"], "passcode");
        let m: ClientMsg = serde_json::from_str(r#"{"passcode_cancel":true}"#).unwrap();
        assert!(m.passcode_cancel);
        assert!(sink.live.answer_passcode(id2, Passcode::Cancel));
        assert!(matches!(sink.passcode(), Some(Passcode::Cancel)));
    }

    /// One page attached to a session, as the stream thread feeds it.
    fn pic_page(live: &Arc<Live>) -> (u64, mpsc::Receiver<Message>, mpsc::Receiver<Message>) {
        let (vtx, vrx) = mpsc::channel(8);
        let (ctx, crx) = mpsc::channel(64);
        (live.attach(vtx, ctx, false), vrx, crx)
    }

    fn take_all(rx: &mut mpsc::Receiver<Message>) -> Vec<Message> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn bin_kinds(m: &[Message]) -> Vec<u8> {
        m.iter()
            .filter_map(|m| match m {
                Message::Binary(b) => b.first().copied(),
                _ => None,
            })
            .collect()
    }

    fn says_picture(m: &[Message], on: bool) -> bool {
        m.iter().any(|m| {
            matches!(m, Message::Text(t)
                if serde_json::from_str::<serde_json::Value>(t.as_str()).is_ok_and(|v| v["picture"] == on))
        })
    }

    // H.264: an IDR slice and a plain slice.
    const PIC_KEY: &[u8] = &[0, 0, 1, 0x65, 1];
    const PIC_DELTA: &[u8] = &[0, 0, 1, 0x41, 1];

    #[test]
    fn a_guest_on_the_stream_controller_keeps_the_page_off_it() {
        let l = Live::new("10.0.0.7".into(), false, Duration::from_secs(60));
        l.playing.store(true, Ordering::Relaxed);
        assert!(!l.seat_free(), "the page uses it");
        assert!(l.page_pad_open());
        l.set_picture(false);
        assert!(l.seat_free(), "the picture is off: it sits idle");

        l.set_guest_seat(true);
        assert!(!l.seat_free(), "one guest has it");
        let cross = ControllerState {
            buttons: 1,
            ..Default::default()
        };
        l.push_pad(GUEST_SEAT_SOURCE, Some(cross));
        assert_eq!(l.pad.lock().unwrap().back().map(|p| p.buttons), Some(1));

        l.set_picture(true);
        assert!(
            !l.page_pad_open(),
            "picture back, the guest keeps the controller"
        );

        l.set_guest_seat(false);
        assert_eq!(
            l.pad.lock().unwrap().back().map(|p| p.buttons),
            Some(0),
            "what they held is let go"
        );
        assert!(l.page_pad_open());
    }

    #[test]
    fn a_linked_phone_is_someone_using_the_stream_controller() {
        let l = Live::new("10.0.0.7".into(), false, Duration::from_secs(60));
        l.set_picture(false);
        l.push_pad(1, Some(ControllerState::default()));
        assert!(!l.seat_free());
        l.push_pad(1, None);
        assert!(l.seat_free());
    }

    #[test]
    fn picture_off_stops_the_page_and_nobody_else() {
        let live = Arc::new(Live::new("10.0.0.9".into(), false, Duration::from_secs(60)));
        let (id, mut video, mut ctl) = pic_page(&live);
        // A guest (watcher), and a reader of the live feed's source: the replay window.
        let (stx, mut guest) = mpsc::channel(32);
        live.spectators
            .lock()
            .unwrap()
            .push(Spectator::watcher(1, stx));
        live.set_replay(Some(15));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        sink.video(b"hdr", true);
        sink.audio(b"ahdr", true);
        sink.video(PIC_KEY, false);
        assert_eq!(take_all(&mut video).len(), 1);
        assert_eq!(bin_kinds(&take_all(&mut ctl)), vec![0, 3]);
        take_all(&mut guest);

        // A pad held down lets go; off, the page's pad goes nowhere.
        let held = ControllerState {
            buttons: 1,
            ..Default::default()
        };
        live.playing.store(true, Ordering::Relaxed);
        live.push_pad(PAGE_SOURCE, Some(held));
        live.want_key.store(false, Ordering::Relaxed);
        live.set_picture(false);
        assert_eq!(live.sources.set(u64::MAX - 1, None).buttons, 0);
        assert!(says_picture(&take_all(&mut ctl), false));
        assert_eq!(live.snapshot()["picture"], false);
        assert!(!live.want_key.swap(false, Ordering::Relaxed));

        sink.video(b"hdr2", true);
        sink.video(PIC_KEY, false);
        sink.video(PIC_DELTA, false);
        sink.audio(b"opus", false);
        sink.haptics(b"pcm");
        sink.rumble(1, 2);
        assert!(take_all(&mut video).is_empty(), "video reached a page off");
        assert!(take_all(&mut ctl).is_empty(), "media reached a page off");
        // Everyone else got all of it.
        assert_eq!(bin_kinds(&take_all(&mut guest)), vec![0, 1, 1, 4]);
        assert!(live.replay_parts().is_some());
        // The session is still the page's: not detached, nothing asked of the console.
        assert!(live.is_viewer(id));
        assert!(!live.alone_too_long());
    }

    #[test]
    fn turning_the_picture_back_on_needs_no_restart() {
        let live = Arc::new(Live::new("10.0.0.9".into(), false, Duration::from_secs(60)));
        let (_id, mut video, mut ctl) = pic_page(&live);
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        sink.video(b"hdr", true);
        sink.audio(b"ahdr", true);
        live.set_picture(false);
        // New headers while it is off are the ones it gets.
        sink.video(b"hdr2", true);
        take_all(&mut ctl);
        take_all(&mut video);
        live.want_key.store(false, Ordering::Relaxed);

        live.set_picture(true);
        let got = take_all(&mut ctl);
        assert!(says_picture(&got, true));
        assert_eq!(bin_kinds(&got), vec![0, 3]);
        let hdr = got
            .iter()
            .find_map(|m| match m {
                Message::Binary(b) if b[0] == 0 => Some(b[1..].to_vec()),
                _ => None,
            })
            .unwrap();
        assert_eq!(hdr, b"hdr2");
        assert!(live.want_key.load(Ordering::Relaxed), "no keyframe asked");
        // Only from the next keyframe on.
        sink.video(PIC_DELTA, false);
        assert!(take_all(&mut video).is_empty());
        sink.video(PIC_KEY, false);
        sink.video(PIC_DELTA, false);
        assert_eq!(take_all(&mut video).len(), 2);
        // Asked again while on: nothing changes.
        live.want_key.store(false, Ordering::Relaxed);
        live.set_picture(true);
        assert!(!live.want_key.load(Ordering::Relaxed));
    }

    #[test]
    fn a_page_joining_a_session_with_the_picture_off_is_told_and_gets_nothing() {
        let live = Arc::new(Live::new("10.0.0.9".into(), false, Duration::from_secs(60)));
        *live.video_header.lock().unwrap() = Some(b"hdr".to_vec());
        live.playing.store(true, Ordering::Relaxed);
        live.set_picture(false);
        let (_id, _video, mut ctl) = pic_page(&live);
        let got = take_all(&mut ctl);
        assert!(says_picture(&got, false));
        assert!(bin_kinds(&got).is_empty());
        assert!(got
            .iter()
            .any(|m| matches!(m, Message::Text(t) if t.as_str().contains("first_frame"))));
        assert!(!live.want_key.load(Ordering::Relaxed));
    }

    #[test]
    fn with_no_page_the_wait_runs_out_unless_the_picture_is_off_for_a_reason() {
        // Another test's Go live run may be under way (it is process-wide), so what Go live says
        // is left out of the first part; Play Together is per console.
        let live = Arc::new(Live::new(
            "10.0.77.1".into(),
            false,
            Duration::from_millis(0),
        ));
        assert!(!live.wait_ran_out());
        *live.alone_since.lock().unwrap() = Some(Instant::now() - Duration::from_secs(5));
        assert!(live.wait_ran_out());
        // Picture on: nothing holds it, whoever needs the stream.
        assert!(!live.held_without_page());
        assert!(live.alone_too_long());
        // Off, with friends invited: it waits. On again: the window applies as ever.
        crate::join::test_open_party("10.0.77.1");
        assert!(crate::join::needs_session("10.0.77.1"));
        live.set_picture(false);
        assert!(live.held_without_page());
        assert!(!live.alone_too_long());
        live.set_picture(true);
        assert!(live.alone_too_long());
        crate::join::test_close_party("10.0.77.1");
        assert!(!crate::join::needs_session("10.0.77.1"));
    }

    #[test]
    fn the_start_and_later_messages_read_the_picture_switch() {
        let m: ClientMsg = serde_json::from_str(r#"{"picture":false}"#).unwrap();
        assert_eq!(m.picture, Some(false));
        let s: StartReq =
            serde_json::from_str(r#"{"host":"h","regist_key":"","rp_key":"","picture":false}"#)
                .unwrap();
        assert_eq!(s.picture, Some(false));
        let s: StartReq =
            serde_json::from_str(r#"{"host":"h","regist_key":"","rp_key":""}"#).unwrap();
        assert_eq!(s.picture, None);
    }

    #[test]
    fn passcode_endings_have_their_own_codes() {
        let code = |t: &str| error_status(&anyhow::anyhow!(t.to_string()))["code"].clone();
        assert_eq!(code(PASSCODE_CANCELLED), "passcode_cancelled");
        assert_eq!(code(PASSCODE_TIMEOUT), "passcode_timeout");
        assert_eq!(code(PASSCODE_CONSOLE_GAVE_UP), "passcode_timeout");
        assert_eq!(code(PASSCODE_TOO_MANY), "passcode_too_many");
    }

    #[test]
    fn watchers_start_at_a_keyframe_and_a_slow_one_skips_ahead() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let (fast_tx, mut fast) = mpsc::channel(8);
        let (slow_tx, mut slow) = mpsc::channel(1);
        live.spectators.lock().unwrap().extend([
            Spectator::watcher(1, fast_tx),
            Spectator::watcher(2, slow_tx),
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
        let later = Instant::now() + GUEST_KEY_GAP * 2;
        live.to_spectators_at(1, b"delta2", false, later);
        assert!(fast.try_recv().is_ok());
        assert!(live.spectators.lock().unwrap()[1].need_key);
        assert!(live.want_key.load(Ordering::Relaxed));
        let _ = slow.try_recv(); // the keyframe it had
        live.to_spectators(1, b"delta3", false);
        assert!(slow.try_recv().is_err());
        live.to_spectators(1, b"key2", true);
        assert!(slow.try_recv().is_ok());
    }

    #[test]
    fn a_player_has_a_shallow_picture_queue_that_skips_to_a_keyframe() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        *live.video_header.lock().unwrap() = Some(b"hdr".to_vec());
        let (tx, mut other) = mpsc::channel(SPECTATOR_QUEUE);
        let (vtx, mut video) = mpsc::channel(2);
        let skipped = Arc::new(AtomicU64::new(0));
        live.add_spectator(
            Spectator {
                video: Some(vtx.clone()),
                skipped: Some(skipped.clone()),
                ..Spectator::watcher(7, tx)
            },
            Some(&vtx),
        );
        // the header waits on the video queue, the rest on the other
        assert_eq!(kinds(&mut video), vec!["bin0"]);
        assert!(kinds(&mut other)[0].contains("watch"));
        live.to_spectators(1, b"delta", false);
        assert!(video.try_recv().is_err(), "no picture before a keyframe");
        live.to_spectators(1, b"key", true);
        live.to_spectators(4, b"opus", false);
        live.to_spectators(1, b"d1", false);
        // full (2): the next picture is skipped, counted, and a keyframe asked for
        live.want_key.store(false, Ordering::Relaxed);
        live.to_spectators_at(1, b"d2", false, Instant::now() + GUEST_KEY_GAP * 2);
        assert_eq!(skipped.load(Ordering::Relaxed), 1);
        assert!(live.want_key.load(Ordering::Relaxed));
        assert_eq!(kinds(&mut video), vec!["bin1", "bin1"]);
        assert_eq!(
            kinds(&mut other),
            vec!["bin4"],
            "sound is not held up by pictures"
        );
        live.to_spectators(1, b"d3", false);
        assert!(video.try_recv().is_err());
        // a header that did not fit goes again ahead of the next keyframe
        let _ = vtx.try_send(Message::Binary(Bytes::from_static(b"x")));
        let _ = vtx.try_send(Message::Binary(Bytes::from_static(b"x")));
        live.to_spectators(0, b"hdr2", false);
        let _ = kinds(&mut video);
        *live.video_header.lock().unwrap() = Some(b"hdr2".to_vec());
        live.to_spectators(1, b"key2", true);
        assert_eq!(kinds(&mut video), vec!["bin0", "bin1"]);
    }

    /// How many keyframes the console is asked for over `secs` of pictures with `live`'s
    /// spectators doing what they do.
    fn asks_over(live: &Live, from: Instant, secs: u64) -> usize {
        let mut asks = 0;
        for f in 0..secs * 60 {
            let now = from + Duration::from_millis(f * 1000 / 60);
            live.to_spectators_at(1, b"delta", false, now);
            if live.want_key.swap(false, Ordering::Relaxed) {
                asks += 1;
            }
        }
        asks
    }

    /// One player who can't keep up: the console's one stream must not turn into keyframes for
    /// everyone. Before, it was one every 500 ms (the stream thread's own limit).
    #[test]
    fn a_lagging_player_does_not_set_the_keyframe_rate() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let (tx, _other) = mpsc::channel(SPECTATOR_QUEUE);
        let (vtx, _video) = mpsc::channel::<Message>(1);
        live.add_spectator(
            Spectator {
                video: Some(vtx),
                ..Spectator::watcher(7, tx)
            },
            None,
        );
        live.want_key.store(false, Ordering::Relaxed);
        // The join's own ask has just gone: fill the queue with the first keyframe.
        let t0 = Instant::now();
        live.to_spectators_at(1, b"key", true, t0);
        let asks = asks_over(&live, t0, 10);
        assert!((3..=6).contains(&asks), "{asks} asks in 10 s");
        // Keyframes that still don't fit: it keeps falling behind, so far fewer.
        for i in 0..3 {
            live.to_spectators_at(1, b"key", true, t0 + Duration::from_secs(10 + i));
        }
        assert!(live.spectators.lock().unwrap()[0].lag_skips >= LAG_SKIPS);
        live.want_key.store(false, Ordering::Relaxed);
        let later = t0 + Duration::from_secs(14);
        // Waiting for a keyframe is no skip: the count stands while it waits.
        let asks = asks_over(&live, later, 20);
        assert!((3..=5).contains(&asks), "{asks} asks in 20 s");
        // Its own asks go through the same limit.
        live.want_key.store(false, Ordering::Relaxed);
        let t = later + Duration::from_secs(60);
        live.guest_key_ask(7, t);
        assert!(live.want_key.swap(false, Ordering::Relaxed));
        live.guest_key_ask(7, t + Duration::from_secs(1));
        assert!(!live.want_key.load(Ordering::Relaxed));
        // Nobody by that id: nothing.
        live.guest_key_ask(99, t + Duration::from_secs(100));
        assert!(!live.want_key.load(Ordering::Relaxed));
    }

    #[test]
    fn a_host_page_asking_is_not_held_to_the_guests_limit() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let (tx, _rx) = mpsc::channel(8);
        live.add_spectator(Spectator::watcher(1, tx), None);
        let t = Instant::now();
        live.guest_key_ask(1, t);
        live.want_key.store(false, Ordering::Relaxed);
        // What the page's own branch and the live feed do.
        let (vtx, _vrx, ctx, _crx) = page();
        live.attach(vtx, ctx, false);
        assert!(live.want_key.load(Ordering::Relaxed));
    }

    /// A header that does not fit a watcher's full queue goes again before its next keyframe: a
    /// slow viewer otherwise decodes on with the old parameter sets after a profile switch.
    #[test]
    fn a_watcher_gets_a_lost_codec_header_again() {
        let live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let (tx, mut rx) = mpsc::channel(2);
        live.spectators
            .lock()
            .unwrap()
            .push(Spectator::watcher(1, tx));
        live.to_spectators(1, b"key", true);
        live.to_spectators(1, b"d1", false);
        // Full: the switch's header is lost.
        *live.video_header.lock().unwrap() = Some(b"hdr2".to_vec());
        live.to_spectators(0, b"hdr2", false);
        assert_eq!(kinds(&mut rx), vec!["bin1", "bin1"]);
        live.to_spectators(1, b"d2", false);
        assert!(rx.try_recv().is_err(), "nothing before a keyframe");
        live.to_spectators(1, b"key2", true);
        let got: Vec<Vec<u8>> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|m| match m {
                Message::Binary(b) => b.to_vec(),
                _ => vec![],
            })
            .collect();
        assert_eq!(got, vec![b"\0hdr2".to_vec(), b"\x01key2".to_vec()]);
    }

    #[test]
    fn pad_states_from_before_the_stream_are_not_replayed() {
        let mut live = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        let st = |b: u32| ControllerState {
            buttons: b,
            ..Default::default()
        };
        // A long wake: only where the pad is now counts.
        for b in 0..300 {
            live.push_pad(PAGE_SOURCE, Some(st(b)));
        }
        assert_eq!(live.pad.lock().unwrap().len(), 1);
        assert_eq!(live.pad.lock().unwrap().back().unwrap().buttons, 299);
        // Playing: all in order, and when the thread is held up the oldest goes, not a release.
        live.playing.store(true, Ordering::Relaxed);
        for b in 0..(PAD_QUEUE as u32 + 10) {
            live.push_pad(PAGE_SOURCE, Some(st(b)));
        }
        live.push_pad(PAGE_SOURCE, Some(st(0)));
        let g = live.pad.get_mut().unwrap();
        assert_eq!(g.len(), PAD_QUEUE);
        assert_eq!(g.back().unwrap().buttons, 0);
    }

    #[test]
    fn another_device_using_the_console_is_not_called_our_last_session() {
        use ps5upload_core::rp_stream::guard::NotReady;
        let e = anyhow::Error::new(NotReady::InUse(Duration::from_secs(30)));
        let v = error_status(&e);
        assert_eq!(v["code"], "in_use");
        assert_eq!(v["retry_s"], 30);
        let e = anyhow::Error::new(NotReady::Busy(Duration::from_secs(12)));
        assert_eq!(error_status(&e)["code"], "busy");
    }

    #[test]
    fn a_console_is_the_same_with_its_port() {
        assert!(same_console("10.0.0.2", "10.0.0.2:9120"));
        assert!(same_console("10.0.0.2:9295", "10.0.0.2:9120"));
        assert!(!same_console("10.0.0.2", "10.0.0.3"));
    }

    // ---- The page's socket, for real ----

    mod socket {
        use super::*;
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as TMsg;

        async fn serve() -> String {
            let app = axum::Router::new().route("/ws", axum::routing::get(ws));
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
            format!("ws://{addr}/ws")
        }

        /// A running session for 10.0.77.2, until dropped.
        struct Session(Arc<Live>);
        impl Session {
            fn start() -> Self {
                let live = Arc::new(Live::new("10.0.77.2".into(), false, DEFAULT_KEEP_ALIVE));
                *LIVE.lock().unwrap() = Some(live.clone());
                Session(live)
            }
        }
        impl Drop for Session {
            fn drop(&mut self) {
                *LIVE.lock().unwrap() = None;
            }
        }

        const ATTACH: &str =
            r#"{"host":"10.0.77.2:9120","regist_key":"","rp_key":"","attach_only":true}"#;

        /// A page that vanished without closing, mid-press: the socket says nothing and answers
        /// no ping. Its pad is let go within a few seconds rather than minutes.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_page_that_vanished_lets_go_of_the_pad() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let s = Session::start();
            let url = serve().await;
            // Named with a port: the same console, attached (not "busy").
            let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            c.send(TMsg::Text(ATTACH.into())).await.unwrap();
            let pad = r#"{"pad":{"buttons":1,"l2":0,"r2":0,"lx":0,"ly":0,"rx":0,"ry":0}}"#;
            c.send(TMsg::Text(pad.into())).await.unwrap();
            let held = async {
                loop {
                    if s.0.sources.set(u64::MAX - 1, None).buttons == 1 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(3), held)
                .await
                .unwrap();
            assert!(s.0.viewer.lock().unwrap().is_some());
            // From here the client reads nothing, so it answers no ping: it is simply gone.
            let released = async {
                loop {
                    if s.0.viewer.lock().unwrap().is_none() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(5), released)
                .await
                .expect("the page was let go");
            assert_eq!(s.0.sources.set(u64::MAX - 1, None).buttons, 0);
            assert!(
                s.0.alone_since.lock().unwrap().is_some(),
                "the session waits"
            );
            drop(c);
        }

        /// A session told to stop is not joined: a start hears "busy, look again", a page coming
        /// back that it has gone. Attaching used to end with it, a minute later, with no word.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_session_on_its_way_out_is_not_joined() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let s = Session::start();
            s.0.stop.store(true, Ordering::Relaxed);
            assert_eq!(s.0.snapshot()["stopping"], true);
            let url = serve().await;
            let hear = |msg: &'static str| {
                let url = url.clone();
                async move {
                    let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
                    c.send(TMsg::Text(msg.into())).await.unwrap();
                    loop {
                        match c.next().await {
                            Some(Ok(TMsg::Text(t))) => {
                                return serde_json::from_str::<serde_json::Value>(t.as_str())
                                    .unwrap()
                            }
                            Some(Ok(_)) => {}
                            other => panic!("closed: {other:?}"),
                        }
                    }
                }
            };
            let v = hear(ATTACH).await;
            assert_eq!(v["stage"], "ended");
            assert_eq!(v["reason"], "no session");
            let v = hear(r#"{"host":"10.0.77.2","regist_key":"","rp_key":"","attach_only":false}"#)
                .await;
            assert_eq!(v["code"], "busy");
            assert_eq!(v["retry_s"], STOPPING_RETRY_S);
        }

        /// A page that answers pings stays, however quiet it is itself.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_quiet_page_that_answers_pings_stays() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let s = Session::start();
            let url = serve().await;
            let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            c.send(TMsg::Text(ATTACH.into())).await.unwrap();
            let t0 = Instant::now();
            let mut pings = 0;
            while t0.elapsed() < PEER_PING_EVERY * (PEER_QUIET_PINGS + 3) {
                // Reading is what makes the client library answer.
                if let Ok(Some(Ok(TMsg::Ping(_)))) =
                    tokio::time::timeout(Duration::from_millis(50), c.next()).await
                {
                    pings += 1;
                }
            }
            assert!(pings >= 3, "{pings} pings");
            assert!(s.0.viewer.lock().unwrap().is_some());
        }

        /// A snapshot above tungstenite's 16 MiB frame limit (an upscaled 4K PNG): it must not end
        /// the player's own socket.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_big_snapshot_does_not_end_the_pages_socket() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let s = Session::start();
            let url = serve().await;
            let (mut c, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
            c.send(TMsg::Text(ATTACH.into())).await.unwrap();
            let mut big = vec![0u8; 20 * 1024 * 1024];
            big[0] = automation::KIND_SNAPSHOT;
            c.send(TMsg::Binary(big.into())).await.unwrap();
            c.send(TMsg::Text(r#"{"ping":1}"#.into())).await.unwrap();
            let pong = async {
                loop {
                    match c.next().await {
                        Some(Ok(TMsg::Text(t))) if t.as_str().contains("pong") => return true,
                        Some(Ok(_)) => {}
                        _ => return false,
                    }
                }
            };
            assert!(tokio::time::timeout(Duration::from_secs(5), pong)
                .await
                .unwrap());
            assert!(s.0.viewer.lock().unwrap().is_some());
        }
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
            Some(Message::Text(t)) => {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                assert_eq!(v["pong"], 1234.5);
                // The engine's clock, for the page to line its own up with.
                assert!(v["engine_us"].is_u64());
            }
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
    fn a_page_that_asks_for_timing_gets_kind_7_and_watchers_still_get_kind_1() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, mut vr, ct, _cr) = page();
        live.attach(vt, ct, true);
        let (st, mut sr) = mpsc::channel(8);
        live.spectators.lock().unwrap().push(Spectator {
            need_key: false,
            ..Spectator::watcher(9, st)
        });
        let before = engine_us();
        assert!(sink.video(KEY, false));
        let after = engine_us();
        let Ok(Message::Binary(b)) = vr.try_recv() else {
            panic!("no video");
        };
        assert_eq!(b[0], KIND_VIDEO_TIMED);
        let t = u64::from_be_bytes(b[1..9].try_into().unwrap());
        assert!((before..=after).contains(&t));
        assert_eq!(&b[9..], KEY);
        let Ok(Message::Binary(w)) = sr.try_recv() else {
            panic!("no watcher video");
        };
        assert_eq!(w[0], 1);
        assert_eq!(&w[1..], KEY);

        // A page that did not ask (an older one) gets kind 1 as before.
        let (vt, mut vr, ct, _cr) = page();
        live.attach(vt, ct, false);
        live.need_key.store(false, Ordering::Relaxed);
        assert!(sink.video(KEY, false));
        assert_eq!(kinds(&mut vr), ["bin1"]);
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
        live.attach(vt, ct, false);
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
        let first = live.attach(vt, ct, false);
        let (vt2, _vr2, ct2, _cr2) = page();
        live.attach(vt2, ct2, false);
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
        live.attach(vt, ct, false);
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
        let id = live.attach(vt, ct, false);
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
        let first = live.attach(vt, ct, false);
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
        let second = live.attach(vt2, ct2, false);
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
    fn a_pad_press_leaves_the_mic_on() {
        // A merge once put the "mic off" of a page leaving into every pad change: talking and
        // pressing a button (or a phone's, or an automation client's) cut the mic.
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        live.mic_enabled.store(true, Ordering::Relaxed);
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, _cr) = page();
        let id = live.attach(vt, ct, false);
        live.set_mic(id, true);
        assert_eq!(sink.mic(), Some(MicInput::On));
        live.push_pad(PAGE_SOURCE, Some(ControllerState::default()));
        live.push_pad(3, Some(ControllerState::default()));
        assert_eq!(sink.mic(), None);
        assert!(live.mic_on.load(Ordering::Relaxed));
        live.detach(id);
        assert_eq!(sink.mic(), Some(MicInput::Off));
    }

    #[test]
    fn each_session_has_its_own_id_and_says_it() {
        let a = Live::new("h".into(), false, DEFAULT_KEEP_ALIVE);
        let b = Live::new("h".into(), false, DEFAULT_KEEP_ALIVE);
        assert!(b.session > a.session);
        assert_eq!(a.snapshot()["session"], a.session);
    }

    #[test]
    fn a_session_without_the_mic_ignores_it() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, _cr) = page();
        let id = live.attach(vt, ct, false);
        live.set_mic(id, true);
        live.mic_frame(id, &[1; 40]);
        assert_eq!(sink.mic(), None);
        let s: StartReq =
            serde_json::from_str(r#"{"host":"h","regist_key":"","rp_key":""}"#).unwrap();
        assert!(!s.mic);
    }

    #[test]
    fn the_keyboard_is_only_on_when_asked_and_only_for_the_attached_page() {
        let s: StartReq =
            serde_json::from_str(r#"{"host":"h","regist_key":"","rp_key":""}"#).unwrap();
        assert!(!s.keyboard, "an older page gets no keyboard");
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        live.keyboard.enabled.store(true, Ordering::Relaxed);
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, _vr, ct, mut cr) = page();
        live.attach(vt, ct, false);
        sink.keyboard(KeyboardEvent::Open("ü".into()));
        assert_eq!(
            kinds(&mut cr),
            vec![r#"{"keyboard":{"open":true,"text":"ü"}}"#]
        );
        // A page that attaches now is told the keyboard is open.
        let (vt2, _vr2, ct2, mut cr2) = page();
        live.attach(vt2, ct2, false);
        assert!(kinds(&mut cr2)
            .iter()
            .any(|m| m == r#"{"keyboard":{"open":true,"text":"ü"}}"#));
        let m: ClientMsg = serde_json::from_str(r#"{"keyboard_text": "üx"}"#).unwrap();
        live.keyboard.page_typed(&m.keyboard);
        let m: ClientMsg = serde_json::from_str(r#"{"keyboard_accept": true}"#).unwrap();
        live.keyboard.page_typed(&m.keyboard);
        assert_eq!(
            sink.keyboard_input(),
            Some(KeyboardInput::Text("üx".into()))
        );
        assert_eq!(sink.keyboard_input(), Some(KeyboardInput::Accept));
        assert_eq!(sink.keyboard_input(), None);
    }

    #[test]
    fn a_slow_page_skips_to_the_next_keyframe() {
        let live = Arc::new(Live::new("h".into(), false, DEFAULT_KEEP_ALIVE));
        let mut sink = Sink {
            live: live.clone(),
            t0: Instant::now(),
        };
        let (vt, mut vr, ct, _cr) = page();
        live.attach(vt, ct, false);
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
