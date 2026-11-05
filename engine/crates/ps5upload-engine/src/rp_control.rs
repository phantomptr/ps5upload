//! Remote Play with no app page: start, stop and drive a session over plain HTTP.
//!
//! A child module of rp_stream_api.rs (it reads the running session's private state). Same rule as
//! the rest of the automation API: this computer only (`vinput_api::local_allowed`,
//! `host_is_local`), unless the operator opened it with `PS5UPLOAD_ALLOW_AUTOMATION`.
//!
//! * `POST /api/ps5/rp/start {host, width?, height?, fps?, bitrate_kbps?, codec?, keep_alive_s?,
//!   mic?, dualsense?, keyboard?, replay_s?, wait_s?, regist_key?, rp_key?}`: starts the session,
//!   or joins the one running for that console. `host` may be a console's address, id or name.
//!   The keys come from the engine's store (`consoles_api`, filled by pairing) unless sent here.
//!   Answers `{ok, started, host, session, keep_alive_s, playing, passcode}` at once, or with
//!   `wait_s` (at most 60) once the first picture came, the console asks for the passcode, or the
//!   start failed. Progress and how it ended are in `GET /api/ps5/rp/session`.
//!
//!   With no page attached, a session started here ends after `keep_alive_s` (300 by default, 600
//!   at most) without a call to this API and without a reader of the live feed (Go live, OBS), so a
//!   script that dies does not hold the console's one Remote Play slot.
//! * `POST /api/ps5/rp/stop {host, session?}`: ends it. The console holds its slot for 12-30 s after.
//! * `POST /api/ps5/rp/settings {host, session?, keyframe?, replay_s?}`: what can change while it
//!   runs. Resolution, frame rate, bitrate and codec are set when a session starts, so asking for
//!   one answers 409 `restart_needed`: stop, then start with the new values.
//! * `POST /api/ps5/rp/passcode {host, session, digits | cancel}`: the user's sign-in passcode, when
//!   the session says `passcode: true`. Passed to the console, never kept or logged.
//! * `POST /api/ps5/rp/keyboard {host, session, text? , accept?, reject?}`: the console's on-screen
//!   keyboard, for a session started with `keyboard: true`, while it is open.
//! * `POST /api/ps5/rp/mic {host, session, on?, opus_b64?}`: the mic on or off, for a session started
//!   with `mic: true`; with no page attached also Opus packets (48 kHz stereo, 10 ms). A page that
//!   is attached keeps sending its own.
//!
//! Errors are JSON `{"error": code, "message"}`: local_only, host_not_local (403), bad_request
//! (400), no_session (404), stale_session, busy_other, no_credentials, restart_needed,
//! no_passcode_asked, keyboard_not_enabled, keyboard_closed, mic_not_enabled (409),
//! start_failed (502, with `end`).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use ps5upload_core::rp_stream::{valid_passcode, MicInput, Passcode};
use serde_json::{json, Value};

use super::{Live, Secret, StartReq, LAST_SESSION, LIVE, MAX_KEEP_ALIVE, MIC_QUEUE};

/// A session started here waits this long for the next call when nobody watches it.
/// How long a start waits for a session that was just stopped to go.
const STOPPING_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(5)
};
const API_KEEP_ALIVE: Duration = Duration::from_secs(300);
/// The longest `wait_s`.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// The most text accepted for the console's keyboard at once.
const MAX_KEYBOARD_TEXT: usize = 16 * 1024;

/// The session started through this API (0: none): the only kind kept alive by calls and readers,
/// so a session a page started and left still ends the way it always did.
static API_SESSION: AtomicU64 = AtomicU64::new(0);

fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

/// Only this computer (or what the operator opened), as for the rest of the automation API.
fn gate(peer: &SocketAddr, headers: &HeaderMap) -> Option<Response> {
    if !crate::vinput_api::host_is_local(headers) {
        return Some(crate::vinput_api::host_not_local());
    }
    if !crate::vinput_api::local_allowed(peer.ip()) {
        return Some(fail(
            StatusCode::FORBIDDEN,
            "local_only",
            "This is only for the computer the engine runs on. PS5UPLOAD_ALLOW_AUTOMATION=1 opens it to the addresses in PS5UPLOAD_ALLOW_IP.",
        ));
    }
    None
}

/// A call to this API, or a reader of the feed: the session started here is in use.
/// A session a start for `host` can join: that console's, and not stopping.
fn joinable(live: &Live, host: &str) -> bool {
    same_console(&live.host, host) && !live.stop.load(Ordering::Relaxed)
}

/// That console's session is on its way out.
fn stopping_here(host: &str) -> bool {
    super::live().is_some_and(|l| same_console(&l.host, host) && l.stop.load(Ordering::Relaxed))
}

pub(super) fn touch(live: &Live) {
    if API_SESSION.load(Ordering::Relaxed) != live.session {
        return;
    }
    if let Ok(mut a) = live.alone_since.lock() {
        if a.is_some() {
            *a = Some(Instant::now());
        }
    }
}

/// The running session named by `host` and `session`, marked as in use.
fn session_for(host: &str, session: Option<u64>) -> Result<Arc<Live>, Box<Response>> {
    let host = crate::consoles_api::resolve(host);
    let live = super::automation::session_for(super::live(), &host, session)?;
    touch(&live);
    Ok(live)
}

/// Commands that act on the console must name their session: a replacement is never driven by
/// mistake.
fn driving(host: &str, session: Option<u64>) -> Result<Arc<Live>, Box<Response>> {
    if session.is_none() {
        return Err(Box::new(fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "session is required: take it from the start answer or GET /api/ps5/rp/session",
        )));
    }
    session_for(host, session)
}

/// No Debug: it can carry keys.
#[derive(serde::Deserialize)]
pub struct StartBody {
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    #[serde(default)]
    regist_key: Option<String>,
    #[serde(default)]
    rp_key: Option<String>,
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
    #[serde(default)]
    keep_alive_s: Option<u64>,
    #[serde(default)]
    mic: bool,
    #[serde(default)]
    dualsense: bool,
    #[serde(default)]
    keyboard: bool,
    #[serde(default)]
    replay_s: Option<u64>,
    #[serde(default)]
    wait_s: Option<u64>,
}

/// The session's keys: as sent, or the engine's own for this console.
fn keys_for(host: &str, b: &StartBody) -> Result<(String, String), Box<Response>> {
    match (b.regist_key.as_deref(), b.rp_key.as_deref()) {
        (Some(r), Some(k)) => Ok((r.to_string(), k.to_string())),
        (None, None) => crate::consoles_api::rp_keys(host).ok_or_else(|| {
            Box::new(fail(
                StatusCode::CONFLICT,
                "no_credentials",
                "This console has no Remote Play keys here. Pair it first with POST /api/ps5/power/pair.",
            ))
        }),
        _ => Err(Box::new(fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "regist_key and rp_key go together",
        ))),
    }
}

/// `POST /api/ps5/rp/start`.
pub async fn start(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<StartBody>,
) -> Response {
    if let Some(r) = gate(&peer, &headers) {
        return r;
    }
    let host = crate::console_addr(&crate::consoles_api::resolve(&b.host));
    if host.is_empty() {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "host is required");
    }
    if let Some(c) = b.codec.as_deref() {
        if ps5upload_core::rp_stream::Codec::from_name(c).is_none() {
            return fail(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "codec must be h264, h265 or h265_hdr",
            );
        }
    }
    // A session already running is joined: its keys are not needed.
    // Unless it is on its way out: joining that would end with it, a moment later. Its console
    // needs it gone first, so a start right after a stop waits for that, briefly.
    let until = Instant::now() + STOPPING_WAIT;
    while stopping_here(&host) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let running = super::live().filter(|l| joinable(l, &host));
    let creds = if running.is_some() {
        None
    } else {
        let (r, k) = match keys_for(&host, &b) {
            Ok(v) => v,
            Err(e) => return *e,
        };
        match ps5upload_core::rp_session::SessionCreds::from_hex(&r, &k) {
            Ok(c) => Some(c),
            Err(e) => {
                return fail(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    &format!("the keys are not usable: {e:#}"),
                )
            }
        }
    };
    let keep = b
        .keep_alive_s
        .map(Duration::from_secs)
        .unwrap_or(API_KEEP_ALIVE)
        .min(MAX_KEEP_ALIVE);
    // Read by field name, so a new field of the socket's start message has its default here.
    let req: StartReq = match serde_json::from_value(json!({
        "host": host,
        "regist_key": "",
        "rp_key": "",
        "width": b.width,
        "height": b.height,
        "fps": b.fps,
        "bitrate_kbps": b.bitrate_kbps,
        "codec": b.codec,
        "keep_alive_s": keep.as_secs(),
        "mic": b.mic,
        "dualsense": b.dualsense,
        "keyboard": b.keyboard,
        "replay_s": b.replay_s,
    })) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                &format!("{e}"),
            )
        }
    };
    let (live, started) =
        {
            let Ok(mut g) = LIVE.lock() else {
                return fail(StatusCode::INTERNAL_SERVER_ERROR, "internal", "lock");
            };
            match g.as_ref() {
                Some(l) if joinable(l, &host) => (l.clone(), false),
                Some(l) if same_console(&l.host, &host) => return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": "busy",
                        "retry_s": super::STOPPING_RETRY_S,
                        "message": "The last session is still finishing. Try again in a moment.",
                    })),
                )
                    .into_response(),
                Some(l) => {
                    return (
                        StatusCode::CONFLICT,
                        Json(json!({
                            "error": "busy_other",
                            "host": l.host,
                            "message": format!("Remote Play is already streaming {}", l.host),
                        })),
                    )
                        .into_response()
                }
                None => {
                    let Some(creds) = creds else {
                        return fail(
                            StatusCode::CONFLICT,
                            "no_session",
                            "The session ended just now. Try again.",
                        );
                    };
                    let l = super::begin(&req, creds);
                    *g = Some(l.clone());
                    (l, true)
                }
            }
        };
    if started {
        adopt(&live);
    } else {
        touch(&live);
    }
    let wait = b
        .wait_s
        .map(Duration::from_secs)
        .unwrap_or_default()
        .min(MAX_WAIT);
    if let Some(end) = wait_for_picture(&live, wait).await {
        return (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": "start_failed",
                "message": end.get("error").cloned().unwrap_or(Value::Null),
                "end": end,
            })),
        )
            .into_response();
    }
    Json(json!({
        "ok": true,
        "started": started,
        "host": live.host,
        "session": live.session,
        "keep_alive_s": live.keep_alive.as_secs(),
        "playing": live.playing.load(Ordering::Relaxed),
        "passcode": live.passcode_ask.lock().map(|g| g.is_some()).unwrap_or(false),
    }))
    .into_response()
}

fn same_console(a: &str, b: &str) -> bool {
    crate::vinput_api::host_key(a) == crate::vinput_api::host_key(b)
}

/// The session is this API's: nobody watches yet, and a reader of the live feed counts as use.
fn adopt(live: &Arc<Live>) {
    API_SESSION.store(live.session, Ordering::Relaxed);
    if let Ok(mut a) = live.alone_since.lock() {
        if live.viewer.lock().map(|v| v.is_none()).unwrap_or(false) {
            *a = Some(Instant::now());
        }
    }
    let l = live.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if !super::live().is_some_and(|now| Arc::ptr_eq(&now, &l)) {
                return;
            }
            if l.feed.readers() > 0 {
                touch(&l);
            }
        }
    });
}

/// Waits until there is a picture, the console asks for the passcode, or `wait` passes. `Some`:
/// the session ended first, and how.
async fn wait_for_picture(live: &Arc<Live>, wait: Duration) -> Option<Value> {
    let until = Instant::now() + wait;
    while Instant::now() < until {
        let asked = live
            .passcode_ask
            .lock()
            .map(|g| g.is_some())
            .unwrap_or(false);
        if live.playing.load(Ordering::Relaxed) || asked {
            return None;
        }
        if !super::live().is_some_and(|now| Arc::ptr_eq(&now, live)) {
            return Some(last_end().unwrap_or_else(|| json!({"stage": "ended"})));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// How the last session ended, when it has.
fn last_end() -> Option<Value> {
    LAST_SESSION
        .lock()
        .ok()?
        .as_ref()?
        .get("end")
        .filter(|e| !e.is_null())
        .cloned()
}

/// What a local script sees beyond the session's id, for `GET /api/ps5/rp/session`: there is no page
/// to tell it each stage. Nothing secret: no keys, no passcode, and the typed text is what the
/// console's own keyboard already shows.
pub(super) fn add_details(v: &mut Value, live: Option<&Live>) {
    let last = LAST_SESSION.lock().ok().and_then(|g| g.clone());
    let Some(l) = live else {
        if let Some(end) = last_end() {
            v["last_end"] = end;
        }
        return;
    };
    let stage = last
        .as_ref()
        .and_then(|s| s.get("stages"))
        .and_then(|s| s.as_array())
        .and_then(|s| s.last())
        .and_then(|s| s.get("stage"))
        .cloned();
    v["stage"] = stage.unwrap_or(Value::Null);
    v["started_by"] = if API_SESSION.load(Ordering::Relaxed) == l.session {
        "api"
    } else {
        "page"
    }
    .into();
    v["passcode_retry"] = l
        .passcode_ask
        .lock()
        .ok()
        .and_then(|g| *g)
        .map(Value::from)
        .unwrap_or(Value::Null);
    v["mic_enabled"] = l.mic_enabled.load(Ordering::Relaxed).into();
    v["mic_on"] = l.mic_on.load(Ordering::Relaxed).into();
    v["keyboard_enabled"] = l.keyboard.enabled.load(Ordering::Relaxed).into();
    v["keyboard"] = l
        .keyboard
        .on_attach()
        .and_then(|k| k.get("keyboard").cloned())
        .unwrap_or_else(|| json!({"open": false}));
    v["keep_alive_s"] = l.keep_alive.as_secs().into();
    v["alone_s"] = l
        .alone_since
        .lock()
        .ok()
        .and_then(|a| a.map(|t| t.elapsed().as_secs()))
        .map(Value::from)
        .unwrap_or(Value::Null);
    if let Some(info) = super::live_feed::session_info() {
        v["video"] = serde_json::to_value(info).unwrap_or(Value::Null);
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct StopReq {
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    #[serde(default)]
    session: Option<u64>,
}

/// `POST /api/ps5/rp/stop`.
pub async fn stop(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<StopReq>,
) -> Response {
    if let Some(res) = gate(&peer, &headers) {
        return res;
    }
    let live = match session_for(&r.host, r.session) {
        Ok(l) => l,
        Err(e) => return *e,
    };
    live.stop.store(true, Ordering::Relaxed);
    Json(json!({"ok": true, "host": live.host, "session": live.session})).into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct SettingsReq {
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    #[serde(default)]
    session: Option<u64>,
    #[serde(default)]
    keyframe: bool,
    #[serde(default)]
    replay_s: Option<u64>,
    /// The picture on the attached page on or off (see rp_stream_api's module note).
    #[serde(default)]
    picture: Option<bool>,
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
}

impl SettingsReq {
    /// The asked-for values the console only takes when a session starts.
    fn start_only(&self) -> Vec<&'static str> {
        let mut f = Vec::new();
        if self.width.is_some() {
            f.push("width");
        }
        if self.height.is_some() {
            f.push("height");
        }
        if self.fps.is_some() {
            f.push("fps");
        }
        if self.bitrate_kbps.is_some() {
            f.push("bitrate_kbps");
        }
        if self.codec.is_some() {
            f.push("codec");
        }
        f
    }
}

/// `POST /api/ps5/rp/settings`.
pub async fn settings(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<SettingsReq>,
) -> Response {
    if let Some(res) = gate(&peer, &headers) {
        return res;
    }
    let live = match session_for(&r.host, r.session) {
        Ok(l) => l,
        Err(e) => return *e,
    };
    let fields = r.start_only();
    if !fields.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "restart_needed",
                "fields": fields,
                "message": "The console takes these only when a session starts. Stop it, then start it with the new values. The console holds its slot for up to 30 s after a stop.",
            })),
        )
            .into_response();
    }
    if r.replay_s.is_some() {
        live.set_replay(r.replay_s);
    }
    if r.keyframe {
        live.want_key.store(true, Ordering::Relaxed);
    }
    if let Some(on) = r.picture {
        live.set_picture(on);
    }
    Json(json!({
        "ok": true,
        "session": live.session,
        "picture": !live.picture_off(),
        "replay_on": live.replay.lock().map(|g| g.is_some()).unwrap_or(false),
    }))
    .into_response()
}

/// No Debug: it carries the passcode.
#[derive(serde::Deserialize)]
pub struct PasscodeReq {
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    #[serde(default)]
    session: Option<u64>,
    #[serde(default, alias = "passcode")]
    digits: Option<Secret>,
    #[serde(default)]
    cancel: bool,
}

impl Live {
    /// An answer from this API. Same rules as a page's, without the page: only while a passcode is
    /// asked for, and only four digits (or a cancel).
    fn api_passcode(&self, answer: Passcode) -> bool {
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
}

/// `POST /api/ps5/rp/passcode`.
pub async fn passcode(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<PasscodeReq>,
) -> Response {
    if let Some(res) = gate(&peer, &headers) {
        return res;
    }
    let live = match driving(&r.host, r.session) {
        Ok(l) => l,
        Err(e) => return *e,
    };
    let answer = match (r.cancel, r.digits) {
        (true, _) => Passcode::Cancel,
        (false, Some(Secret(d))) if valid_passcode(&d) => Passcode::Digits(d),
        _ => {
            return fail(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "digits must be the four digits of the passcode (or send cancel: true)",
            )
        }
    };
    if !live.api_passcode(answer) {
        return fail(
            StatusCode::CONFLICT,
            "no_passcode_asked",
            "The console is not asking for a passcode now.",
        );
    }
    Json(json!({"ok": true, "session": live.session})).into_response()
}

/// No Debug: what is typed may be private.
#[derive(serde::Deserialize)]
pub struct KeyboardReq {
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    #[serde(default)]
    session: Option<u64>,
    /// The whole text of the console's field.
    #[serde(default)]
    text: Option<String>,
    /// Enter.
    #[serde(default)]
    accept: bool,
    /// Cancel.
    #[serde(default)]
    reject: bool,
}

/// `POST /api/ps5/rp/keyboard`.
pub async fn keyboard(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<KeyboardReq>,
) -> Response {
    if let Some(res) = gate(&peer, &headers) {
        return res;
    }
    let live = match driving(&r.host, r.session) {
        Ok(l) => l,
        Err(e) => return *e,
    };
    if r.text.is_none() && !r.accept && !r.reject {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "send text, accept or reject",
        );
    }
    if r.text.as_ref().is_some_and(|t| t.len() > MAX_KEYBOARD_TEXT) {
        return fail(StatusCode::BAD_REQUEST, "bad_request", "text is too long");
    }
    if !live.keyboard.enabled.load(Ordering::Relaxed) {
        return fail(
            StatusCode::CONFLICT,
            "keyboard_not_enabled",
            "This session was not started with keyboard: true.",
        );
    }
    if live.keyboard.on_attach().is_none() {
        return fail(
            StatusCode::CONFLICT,
            "keyboard_closed",
            "The console's keyboard is not open.",
        );
    }
    live.keyboard
        .page_typed(&crate::rp_keyboard::PageKeyboard::new(
            r.text, r.accept, r.reject,
        ));
    Json(json!({"ok": true, "session": live.session})).into_response()
}

/// A local script's mic: on or off, and Opus packets while no page is attached.
#[derive(Debug, Default, serde::Deserialize)]
pub struct MicReq {
    #[serde(default, alias = "addr", alias = "console")]
    host: String,
    #[serde(default)]
    session: Option<u64>,
    #[serde(default)]
    on: Option<bool>,
    #[serde(default)]
    opus_b64: Vec<String>,
}

impl Live {
    /// Packets from this API, in order: dropped when the mic is off or a page owns it. Returns how
    /// many went on the queue.
    fn api_mic_frames(&self, frames: Vec<Vec<u8>>) -> usize {
        let page = self.viewer.lock().map(|v| v.is_some()).unwrap_or(true);
        if page || !self.mic_on.load(Ordering::Relaxed) {
            return 0;
        }
        let Ok(mut q) = self.mic.lock() else {
            return 0;
        };
        let waiting = q.iter().filter(|m| matches!(m, MicInput::Frame(_))).count();
        let room = MIC_QUEUE.saturating_sub(waiting);
        let taken = frames.len().min(room);
        q.extend(frames.into_iter().take(room).map(MicInput::Frame));
        taken
    }
}

/// `POST /api/ps5/rp/mic`.
pub async fn mic(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<MicReq>,
) -> Response {
    if let Some(res) = gate(&peer, &headers) {
        return res;
    }
    let live = match driving(&r.host, r.session) {
        Ok(l) => l,
        Err(e) => return *e,
    };
    if !live.mic_enabled.load(Ordering::Relaxed) {
        return fail(
            StatusCode::CONFLICT,
            "mic_not_enabled",
            "This session was not started with mic: true.",
        );
    }
    if r.on.is_none() && r.opus_b64.is_empty() {
        return fail(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "send on or opus_b64",
        );
    }
    let mut frames = Vec::new();
    for f in &r.opus_b64 {
        match base64::engine::general_purpose::STANDARD.decode(f) {
            Ok(b) if !b.is_empty() => frames.push(b),
            _ => {
                return fail(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "opus_b64 must be a list of base64 Opus packets",
                )
            }
        }
    }
    if let Some(on) = r.on {
        live.mic_switch(on);
    }
    let queued = live.api_mic_frames(frames);
    Json(json!({
        "ok": true,
        "session": live.session,
        "on": live.mic_on.load(Ordering::Relaxed),
        "queued": queued,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Live {
        Live::new("10.0.0.9".into(), false, Duration::from_secs(60))
    }

    #[test]
    fn a_passcode_is_taken_only_while_the_console_asks() {
        let l = session();
        assert!(!l.api_passcode(Passcode::Digits("1234".into())));
        assert!(l.take_passcode().is_none());
        l.ask_passcode(false);
        assert!(l.api_passcode(Passcode::Digits("1234".into())));
        assert!(matches!(l.take_passcode(), Some(Passcode::Digits(d)) if d == "1234"));
        // Taken once: the console is no longer asking.
        assert!(!l.api_passcode(Passcode::Cancel));
    }

    #[test]
    fn the_scripts_mic_packets_need_the_mic_on_and_no_page() {
        let l = session();
        l.mic_enabled.store(true, Ordering::Relaxed);
        assert_eq!(l.api_mic_frames(vec![vec![1]]), 0, "off");
        l.mic_switch(true);
        assert_eq!(l.api_mic_frames(vec![vec![1], vec![2]]), 2);
        // The queue is bounded: a stalled stream drops voice rather than playing it late.
        assert_eq!(
            l.api_mic_frames(vec![vec![3]; MIC_QUEUE + 5]),
            MIC_QUEUE - 2
        );
        // A page owns the mic once it is attached.
        let (vt, _vr) = tokio::sync::mpsc::channel(4);
        let (ct, _cr) = tokio::sync::mpsc::channel(64);
        let _id = l.attach(vt, ct, false);
        l.mic_switch(true);
        assert_eq!(l.api_mic_frames(vec![vec![4]]), 0);
    }

    #[test]
    fn quality_changes_are_named_as_start_only() {
        let r: SettingsReq = serde_json::from_value(json!({
            "host": "h", "keyframe": true, "bitrate_kbps": 5000, "codec": "h264",
        }))
        .unwrap();
        assert_eq!(r.start_only(), vec!["bitrate_kbps", "codec"]);
        let r: SettingsReq =
            serde_json::from_value(json!({"host": "h", "keyframe": true})).unwrap();
        assert!(r.start_only().is_empty());
    }

    /// Only a session this API started is kept alive by calls: one a page started and left ends
    /// the way it always did.
    #[test]
    fn only_a_session_started_here_is_kept_alive_by_calls() {
        let mine = session();
        let page = session();
        let old = Instant::now() - Duration::from_secs(30);
        *mine.alone_since.lock().unwrap() = Some(old);
        *page.alone_since.lock().unwrap() = Some(old);
        API_SESSION.store(mine.session, Ordering::Relaxed);
        touch(&mine);
        touch(&page);
        assert!(mine.alone_since.lock().unwrap().unwrap().elapsed() < Duration::from_secs(5));
        assert!(page.alone_since.lock().unwrap().unwrap().elapsed() >= Duration::from_secs(30));
        // With a page attached there is nothing to keep alive.
        let (vt, _vr) = tokio::sync::mpsc::channel(4);
        let (ct, _cr) = tokio::sync::mpsc::channel(64);
        mine.attach(vt, ct, false);
        touch(&mine);
        assert!(mine.alone_since.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn asking_for_the_session_counts_as_use_unless_passive() {
        let _g = super::super::LIVE_TEST_LOCK.lock().await;
        let live = std::sync::Arc::new(session());
        *LIVE.lock().unwrap() = Some(live.clone());
        API_SESSION.store(live.session, Ordering::Relaxed);
        let mut local = HeaderMap::new();
        local.insert(axum::http::header::HOST, "127.0.0.1:19113".parse().unwrap());
        let ask = |passive: Option<&str>| {
            axum::extract::Query(super::super::CurrentQuery {
                passive: passive.map(str::to_string),
            })
        };
        let old = Instant::now() - Duration::from_secs(30);
        *live.alone_since.lock().unwrap() = Some(old);
        let _ = super::super::current(local.clone(), ask(Some("1"))).await;
        assert!(live.alone_since.lock().unwrap().unwrap().elapsed() >= Duration::from_secs(30));
        let _ = super::super::current(local, ask(None)).await;
        assert!(live.alone_since.lock().unwrap().unwrap().elapsed() < Duration::from_secs(5));
        *LIVE.lock().unwrap() = None;
    }

    #[test]
    fn a_stopping_session_is_not_joined() {
        let l = Live::new("10.0.0.9".into(), false, Duration::from_secs(60));
        assert!(joinable(&l, "10.0.0.9"));
        assert!(!joinable(&l, "10.0.0.8"));
        l.stop.store(true, Ordering::Relaxed);
        assert!(!joinable(&l, "10.0.0.9"));
    }

    #[test]
    fn start_reads_the_console_by_any_of_its_names() {
        let b: StartBody = serde_json::from_value(json!({"console": "10.0.0.9"})).unwrap();
        assert_eq!(b.host, "10.0.0.9");
        let b: StartBody = serde_json::from_value(json!({"addr": "10.0.0.8"})).unwrap();
        assert_eq!(b.host, "10.0.0.8");
    }

    #[tokio::test]
    async fn half_a_pair_of_keys_is_refused() {
        let b: StartBody =
            serde_json::from_value(json!({"host": "10.0.0.7", "regist_key": "aa"})).unwrap();
        let r = keys_for("10.0.0.7", &b).err().unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let b: StartBody = serde_json::from_value(json!({"host": "10.0.0.7"})).unwrap();
        let r = keys_for("10.0.0.77", &b).err().unwrap();
        assert_eq!(r.status(), StatusCode::CONFLICT);
    }
}
