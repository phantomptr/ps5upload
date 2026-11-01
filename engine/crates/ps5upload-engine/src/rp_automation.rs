//! Remote Play for a local script or agent (#615): see the picture, hold a controller.
//!
//! A child module of rp_stream_api.rs (it reads the running session's private state). Nothing
//! here starts a stream, attaches a viewer, or moves one: it works on the session a page
//! already runs, and only from this computer (`vinput_api::local_allowed`, the same rule as the
//! keyboard & mouse socket).
//!
//! * `GET /api/ps5/rp/frame?host=&session=&format=png|jpeg&max_width=&after=&json=1`: the
//!   picture the attached page shows now. The engine never decodes video, so it asks the page
//!   (`{"snapshot": {"id", "format", "max_width", "after_seq"?}}` on its status queue) and the
//!   page answers with binary `[KIND_SNAPSHOT][u32 BE meta length][meta JSON][image]`, meta
//!   `{"id", "format", "width", "height", "captured_at_ms", "frame_at_ms", "frame_seq"}` or
//!   `{"id", "error"}`. Only the page the session is shown on is asked, never a watcher or a
//!   phone, and only its answer to that request counts. The image comes back as it is, with
//!   `X-Frame-*` headers; `json=1` gives JSON with the image in base64.
//! * `POST /api/ps5/rp/input {host, session, buttons, left, right, l2, r2, duration_ms}`: hold
//!   that controller state for `duration_ms` (at most MAX_HOLD), then let go. It is one more
//!   controller (AUTOMATION_SOURCE), merged with the page's (source 0) and phones' (1 and up),
//!   never in their place. A newer input replaces the held one; `POST /api/ps5/rp/input/release
//!   {host, session}` lets go at once. Nothing is ever sent twice.
//!
//! Errors are JSON `{"error": code}`: local_only (403), bad_request (400), no_session (404),
//! stale_session / no_viewer (409), busy / rate_limited (429), viewer_busy / no_picture (503),
//! timeout / no_newer_frame (504), page_error (502).

use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use ps5upload_core::rp_stream::{button, ControllerState};
use tokio::sync::oneshot;

use super::{text, Live};

/// Page → engine binary kind: the picture a frame request asked for.
pub(crate) const KIND_SNAPSHOT: u8 = 9;
/// The automation client's controller among the session's controllers (0 is the page, phones
/// count up from 1 and never get near this).
pub(crate) const AUTOMATION_SOURCE: u64 = u64::MAX;
/// How long the page has to answer a frame request.
const SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
/// How long the page may wait for a newer picture than `after` (inside SNAPSHOT_WAIT).
const AFTER_WAIT_MS: u64 = 1500;
/// Frame requests at most this often (5 a second): a picture costs the page an encode.
const SNAPSHOT_EVERY: Duration = Duration::from_millis(200);
/// The longest an input may be held before it is let go.
const MAX_HOLD_MS: u64 = 5000;
/// The longest side a frame request may ask to scale to (and the smallest).
const MAX_WIDTH_RANGE: std::ops::RangeInclusive<u32> = 16..=7680;
/// The largest picture a page may answer with.
const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

struct Pending {
    id: u64,
    viewer: u64,
    answer: oneshot::Sender<Answer>,
}

/// What the page said: its metadata, and the image when there is one.
#[derive(Debug)]
struct Answer {
    meta: serde_json::Value,
    image: Bytes,
}

/// A session's automation state: the frame request in flight, the last one's time, and which
/// input hold is current.
#[derive(Default)]
pub(crate) struct AutoState {
    pending: Mutex<Option<Pending>>,
    last_request: Mutex<Option<Instant>>,
    next_request: AtomicU64,
    hold: AtomicU64,
}

impl AutoState {
    /// The page `viewer` no longer shows the session: a request it was asked is dropped (the
    /// waiting client hears no_viewer at once rather than a timeout).
    pub(crate) fn viewer_gone(&self, viewer: u64) {
        if let Ok(mut g) = self.pending.lock() {
            if g.as_ref().is_some_and(|p| p.viewer == viewer) {
                *g = None;
            }
        }
    }

    /// The page `viewer` answered (`payload` without the kind byte). Kept only when it is the
    /// page that was asked and the id is the one asked for.
    pub(crate) fn page_answered(&self, viewer: u64, payload: Bytes) -> bool {
        let Some(answer) = parse_answer(payload) else {
            return false;
        };
        let Some(id) = answer.meta.get("id").and_then(|v| v.as_u64()) else {
            return false;
        };
        let Ok(mut g) = self.pending.lock() else {
            return false;
        };
        if !g.as_ref().is_some_and(|p| p.viewer == viewer && p.id == id) {
            return false;
        }
        let p = g.take().expect("checked above");
        p.answer.send(answer).is_ok()
    }
}

/// `[u32 BE meta length][meta JSON][image]`.
fn parse_answer(b: Bytes) -> Option<Answer> {
    if b.len() < 4 || b.len() > MAX_SNAPSHOT_BYTES {
        return None;
    }
    let n = u32::from_be_bytes(b[..4].try_into().ok()?) as usize;
    let end = 4usize.checked_add(n)?;
    if end > b.len() {
        return None;
    }
    let meta: serde_json::Value = serde_json::from_slice(&b[4..end]).ok()?;
    if !meta.is_object() {
        return None;
    }
    Some(Answer {
        meta,
        image: b.slice(end..),
    })
}

fn fail(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

fn fail_with(status: StatusCode, v: serde_json::Value) -> Response {
    (status, Json(v)).into_response()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The running session when it is `host`'s; otherwise the error to answer with.
fn session_for(
    live: Option<Arc<Live>>,
    host: &str,
    session: Option<u64>,
) -> Result<Arc<Live>, Box<Response>> {
    let key = crate::vinput_api::host_key(host.trim());
    let Some(l) = live.filter(|l| !key.is_empty() && crate::vinput_api::host_key(&l.host) == key)
    else {
        return Err(Box::new(fail(StatusCode::NOT_FOUND, "no_session")));
    };
    if let Some(s) = session {
        if s != l.session {
            return Err(Box::new(fail_with(
                StatusCode::CONFLICT,
                serde_json::json!({"error": "stale_session", "session": l.session}),
            )));
        }
    }
    Ok(l)
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct FrameQuery {
    #[serde(default)]
    host: String,
    #[serde(default)]
    session: Option<u64>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    max_width: Option<u32>,
    /// A `frame_id` from an earlier answer: wait (a little) for a newer picture than that one.
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    json: Option<String>,
}

/// `GET /api/ps5/rp/frame`.
pub async fn frame(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<FrameQuery>,
) -> Response {
    if !crate::vinput_api::host_is_local(&headers) {
        return crate::vinput_api::host_not_local();
    }
    frame_for(peer.ip(), super::live(), q).await
}

async fn frame_for(peer: IpAddr, live: Option<Arc<Live>>, q: FrameQuery) -> Response {
    if !crate::vinput_api::local_allowed(peer) {
        return fail(StatusCode::FORBIDDEN, "local_only");
    }
    let format = match q.format.as_deref().unwrap_or("png") {
        "png" => "png",
        "jpeg" | "jpg" => "jpeg",
        _ => return fail(StatusCode::BAD_REQUEST, "bad_request"),
    };
    let max_width = match q.max_width {
        None | Some(0) => None,
        Some(w) if MAX_WIDTH_RANGE.contains(&w) => Some(w),
        Some(_) => return fail(StatusCode::BAD_REQUEST, "bad_request"),
    };
    let as_json = matches!(q.json.as_deref(), Some("1" | "true"));
    let live = match session_for(live, &q.host, q.session) {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let Some((viewer, ctl)) = live
        .viewer
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|v| (v.id, v.ctl.clone())))
    else {
        return fail(StatusCode::CONFLICT, "no_viewer");
    };
    // `after` names a picture as "<viewer>.<seq>": only the same page can tell newer from older;
    // a picture from another page is newer by definition.
    let after_seq = q.after.as_deref().and_then(|a| {
        let (v, s) = a.split_once('.')?;
        (v.parse::<u64>().ok()? == viewer).then_some(s.parse::<u64>().ok()?)
    });
    let auto = &live.auto;
    let id = auto.next_request.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = oneshot::channel();
    {
        let Ok(mut p) = auto.pending.lock() else {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        };
        if p.is_some() {
            return fail(StatusCode::TOO_MANY_REQUESTS, "busy");
        }
        let Ok(mut last) = auto.last_request.lock() else {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "internal");
        };
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < SNAPSHOT_EVERY {
                let wait = (SNAPSHOT_EVERY - since).as_millis() as u64 + 1;
                return fail_with(
                    StatusCode::TOO_MANY_REQUESTS,
                    serde_json::json!({"error": "rate_limited", "retry_ms": wait}),
                );
            }
        }
        *last = Some(Instant::now());
        *p = Some(Pending {
            id,
            viewer,
            answer: tx,
        });
    }
    let mut ask = serde_json::json!({ "id": id, "format": format });
    if let Some(w) = max_width {
        ask["max_width"] = w.into();
    }
    if let Some(s) = after_seq {
        ask["after_seq"] = s.into();
        ask["wait_ms"] = AFTER_WAIT_MS.into();
    }
    if ctl
        .try_send(text(serde_json::json!({ "snapshot": ask })))
        .is_err()
    {
        auto.viewer_gone(viewer);
        return fail(StatusCode::SERVICE_UNAVAILABLE, "viewer_busy");
    }
    let answer = match tokio::time::timeout(SNAPSHOT_WAIT, rx).await {
        Ok(Ok(a)) => a,
        // The page left or lost the stream before answering.
        Ok(Err(_)) => return fail(StatusCode::CONFLICT, "no_viewer"),
        Err(_) => {
            if let Ok(mut p) = auto.pending.lock() {
                if p.as_ref().is_some_and(|p| p.id == id) {
                    *p = None;
                }
            }
            return fail(StatusCode::GATEWAY_TIMEOUT, "timeout");
        }
    };
    respond(&live, viewer, answer, as_json)
}

/// The page's answer as the client gets it.
fn respond(live: &Live, viewer: u64, a: Answer, as_json: bool) -> Response {
    let m = &a.meta;
    if let Some(e) = m.get("error").and_then(|v| v.as_str()) {
        return match e {
            "no_picture" => fail(StatusCode::SERVICE_UNAVAILABLE, "no_picture"),
            "no_newer_frame" => fail(StatusCode::GATEWAY_TIMEOUT, "no_newer_frame"),
            _ => fail_with(
                StatusCode::BAD_GATEWAY,
                serde_json::json!({"error": "page_error", "detail": e}),
            ),
        };
    }
    let format = match m.get("format").and_then(|v| v.as_str()) {
        Some("jpeg") => "jpeg",
        Some("png") => "png",
        _ => return fail(StatusCode::BAD_GATEWAY, "page_error"),
    };
    if a.image.is_empty() {
        return fail(StatusCode::BAD_GATEWAY, "page_error");
    }
    let num = |k: &str| m.get(k).and_then(|v| v.as_u64());
    let width = num("width").unwrap_or(0);
    let height = num("height").unwrap_or(0);
    let captured = num("captured_at_ms");
    let frame_at = num("frame_at_ms");
    // Both on the page's clock: how old the picture was when it was taken.
    let age = match (captured, frame_at) {
        (Some(c), Some(f)) => Some(c.saturating_sub(f)),
        _ => None,
    };
    let frame_id = num("frame_seq").map(|s| format!("{viewer}.{s}"));
    let mime = if format == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    if as_json {
        return Json(serde_json::json!({
            "host": live.host,
            "session": live.session,
            "format": format,
            "width": width,
            "height": height,
            "captured_at_ms": captured,
            "frame_at_ms": frame_at,
            "frame_age_ms": age,
            "frame_id": frame_id,
            "engine_at_ms": now_ms(),
            "image_base64": base64::engine::general_purpose::STANDARD.encode(&a.image),
        }))
        .into_response();
    }
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let mut put = |k: &'static str, v: String| {
        if let Ok(v) = HeaderValue::from_str(&v) {
            h.insert(HeaderName::from_static(k), v);
        }
    };
    put("x-frame-host", live.host.clone());
    put("x-frame-session", live.session.to_string());
    put("x-frame-width", width.to_string());
    put("x-frame-height", height.to_string());
    if let Some(c) = captured {
        put("x-frame-captured-at", c.to_string());
    }
    if let Some(f) = frame_at {
        put("x-frame-at", f.to_string());
    }
    if let Some(a) = age {
        put("x-frame-age-ms", a.to_string());
    }
    if let Some(id) = frame_id {
        put("x-frame-id", id);
    }
    (h, a.image).into_response()
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct InputReq {
    #[serde(default)]
    host: String,
    #[serde(default)]
    session: Option<u64>,
    #[serde(default)]
    buttons: Vec<String>,
    /// Left stick [x, y], each -1..1 (right and down positive).
    #[serde(default)]
    left: Option<[f64; 2]>,
    #[serde(default)]
    right: Option<[f64; 2]>,
    /// Triggers 0..1.
    #[serde(default)]
    l2: Option<f64>,
    #[serde(default)]
    r2: Option<f64>,
    #[serde(default)]
    duration_ms: Option<u64>,
}

/// A button's bit by the name a client uses.
fn button_bit(name: &str) -> Option<u32> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "cross" | "x" => button::CROSS,
        "circle" => button::CIRCLE,
        "square" => button::SQUARE,
        "triangle" => button::TRIANGLE,
        "left" | "dpad_left" => button::DPAD_LEFT,
        "right" | "dpad_right" => button::DPAD_RIGHT,
        "up" | "dpad_up" => button::DPAD_UP,
        "down" | "dpad_down" => button::DPAD_DOWN,
        "l1" => button::L1,
        "r1" => button::R1,
        "l3" => button::L3,
        "r3" => button::R3,
        "options" => button::OPTIONS,
        "create" | "share" => button::SHARE,
        "touchpad" => button::TOUCHPAD,
        "ps" => button::PS,
        _ => return None,
    })
}

fn axis(v: f64) -> Option<i16> {
    (v.is_finite() && (-1.0..=1.0).contains(&v)).then(|| (v * 32767.0).round() as i16)
}

fn trigger(v: f64) -> Option<u8> {
    (v.is_finite() && (0.0..=1.0).contains(&v)).then(|| (v * 255.0).round() as u8)
}

/// The controller state and hold an input asks for, or what is wrong with it.
fn parse_input(r: &InputReq) -> Result<(ControllerState, u64), serde_json::Value> {
    let bad = |field: &str| serde_json::json!({"error": "bad_request", "field": field});
    let mut s = ControllerState::default();
    for b in &r.buttons {
        s.buttons |= button_bit(b)
            .ok_or_else(|| serde_json::json!({"error": "bad_request", "button": b}))?;
    }
    if let Some([x, y]) = r.left {
        s.lx = axis(x).ok_or_else(|| bad("left"))?;
        s.ly = axis(y).ok_or_else(|| bad("left"))?;
    }
    if let Some([x, y]) = r.right {
        s.rx = axis(x).ok_or_else(|| bad("right"))?;
        s.ry = axis(y).ok_or_else(|| bad("right"))?;
    }
    if let Some(v) = r.l2 {
        s.l2 = trigger(v).ok_or_else(|| bad("l2"))?;
    }
    if let Some(v) = r.r2 {
        s.r2 = trigger(v).ok_or_else(|| bad("r2"))?;
    }
    let ms = r.duration_ms.unwrap_or(100);
    if !(1..=MAX_HOLD_MS).contains(&ms) {
        return Err(bad("duration_ms"));
    }
    Ok((s, ms))
}

/// `POST /api/ps5/rp/input`.
pub async fn input(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<InputReq>,
) -> Response {
    if !crate::vinput_api::host_is_local(&headers) {
        return crate::vinput_api::host_not_local();
    }
    input_for(peer.ip(), super::live(), r)
}

fn input_for(peer: IpAddr, live: Option<Arc<Live>>, r: InputReq) -> Response {
    if !crate::vinput_api::local_allowed(peer) {
        return fail(StatusCode::FORBIDDEN, "local_only");
    }
    // Input must name its session: a replacement session is never driven by mistake.
    if r.session.is_none() {
        return fail_with(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "bad_request", "field": "session"}),
        );
    }
    let live = match session_for(live, &r.host, r.session) {
        Ok(l) => l,
        Err(resp) => return *resp,
    };
    let (state, ms) = match parse_input(&r) {
        Ok(v) => v,
        Err(e) => return fail_with(StatusCode::BAD_REQUEST, e),
    };
    let hold = hold(&live, state, Duration::from_millis(ms));
    let sent = now_ms();
    Json(serde_json::json!({
        "ok": true,
        "host": live.host,
        "session": live.session,
        "hold": hold,
        "sent_at_ms": sent,
        "release_at_ms": sent + ms,
    }))
    .into_response()
}

/// Hold `state` as the automation controller for `d`, then let go unless a newer hold or a
/// release came meanwhile. Returns the hold's number.
fn hold(live: &Arc<Live>, state: ControllerState, d: Duration) -> u64 {
    let n = live.auto.hold.fetch_add(1, Ordering::SeqCst) + 1;
    live.push_pad(AUTOMATION_SOURCE, Some(state));
    let l = live.clone();
    tokio::spawn(async move {
        tokio::time::sleep(d).await;
        if l.auto.hold.load(Ordering::SeqCst) == n {
            l.push_pad(AUTOMATION_SOURCE, None);
        }
    });
    n
}

#[derive(Debug, Default, serde::Deserialize)]
pub struct ReleaseReq {
    #[serde(default)]
    host: String,
    #[serde(default)]
    session: Option<u64>,
}

/// `POST /api/ps5/rp/input/release`.
pub async fn release(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(r): Json<ReleaseReq>,
) -> Response {
    if !crate::vinput_api::host_is_local(&headers) {
        return crate::vinput_api::host_not_local();
    }
    release_for(peer.ip(), super::live(), r)
}

fn release_for(peer: IpAddr, live: Option<Arc<Live>>, r: ReleaseReq) -> Response {
    if !crate::vinput_api::local_allowed(peer) {
        return fail(StatusCode::FORBIDDEN, "local_only");
    }
    let live = match session_for(live, &r.host, r.session) {
        Ok(l) => l,
        Err(resp) => return *resp,
    };
    live.auto.hold.fetch_add(1, Ordering::SeqCst);
    live.push_pad(AUTOMATION_SOURCE, None);
    Json(serde_json::json!({"ok": true, "session": live.session})).into_response()
}

#[cfg(test)]
mod tests {
    use super::super::{mpsc, Message, Spectator, CTL_QUEUE, PAGE_SOURCE, VIDEO_QUEUE};
    use super::*;

    const LOCAL: &str = "127.0.0.1";

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn session(host: &str) -> Arc<Live> {
        Arc::new(Live::new(host.into(), false, Duration::from_secs(60)))
    }

    struct Page {
        id: u64,
        ctl: mpsc::Receiver<Message>,
        _video: mpsc::Receiver<Message>,
    }

    fn attach(l: &Live) -> Page {
        let (vt, vr) = mpsc::channel(VIDEO_QUEUE);
        let (ct, cr) = mpsc::channel(CTL_QUEUE);
        let id = l.attach(vt, ct, false);
        let mut p = Page {
            id,
            ctl: cr,
            _video: vr,
        };
        while p.ctl.try_recv().is_ok() {}
        p
    }

    fn q(host: &str) -> FrameQuery {
        FrameQuery {
            host: host.into(),
            ..Default::default()
        }
    }

    async fn body_json(r: Response) -> (StatusCode, serde_json::Value) {
        let st = r.status();
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            st,
            serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
        )
    }

    fn answer(meta: serde_json::Value, image: &[u8]) -> Bytes {
        let m = meta.to_string().into_bytes();
        let mut v = (m.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(&m);
        v.extend_from_slice(image);
        Bytes::from(v)
    }

    /// The page's side: wait for the snapshot request and return it.
    async fn next_request(p: &mut Page) -> serde_json::Value {
        loop {
            match tokio::time::timeout(Duration::from_secs(1), p.ctl.recv()).await {
                Ok(Some(Message::Text(t))) => {
                    let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                    if let Some(s) = v.get("snapshot") {
                        return s.clone();
                    }
                }
                Ok(Some(_)) => continue,
                other => panic!("no snapshot request: {other:?}"),
            }
        }
    }

    /// From this computer but not addressed as localhost (a page whose name was re-pointed at
    /// 127.0.0.1): refused before anything else.
    #[tokio::test]
    async fn a_request_not_addressed_to_localhost_is_refused() {
        let peer = ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 50000)));
        let hdr = |h: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::HOST, h.parse().unwrap());
            m
        };
        let refused = |(st, v): (StatusCode, serde_json::Value)| {
            st == StatusCode::FORBIDDEN && v["error"] == "host_not_local"
        };
        for h in ["evil.example:19113", "192.168.1.10:19113"] {
            assert!(refused(
                body_json(frame(peer, hdr(h), Query(q("10.0.0.9"))).await).await
            ));
            let r = input(
                peer,
                hdr(h),
                Json(InputReq {
                    host: "10.0.0.9".into(),
                    session: Some(1),
                    ..Default::default()
                }),
            )
            .await;
            assert!(refused(body_json(r).await), "{h}");
            let r = release(
                peer,
                hdr(h),
                Json(ReleaseReq {
                    host: "10.0.0.9".into(),
                    session: Some(1),
                }),
            )
            .await;
            assert!(refused(body_json(r).await), "{h}");
        }
        // Addressed as localhost it gets past that check (to whatever the session says).
        for h in ["127.0.0.1:19113", "localhost:19113", "[::1]:19113"] {
            let got = body_json(frame(peer, hdr(h), Query(q("10.0.0.9"))).await).await;
            assert!(!refused(got), "{h}");
        }
    }

    #[tokio::test]
    async fn only_this_computer_may_ask() {
        let l = session("10.0.0.9");
        let _p = attach(&l);
        for peer in ["192.168.1.20", "10.0.0.9"] {
            let (st, v) =
                body_json(frame_for(ip(peer), Some(l.clone()), q("10.0.0.9")).await).await;
            assert_eq!(
                (st, v["error"].as_str()),
                (StatusCode::FORBIDDEN, Some("local_only"))
            );
            let r = input_for(
                ip(peer),
                Some(l.clone()),
                InputReq {
                    host: "10.0.0.9".into(),
                    session: Some(l.session),
                    ..Default::default()
                },
            );
            assert_eq!(r.status(), StatusCode::FORBIDDEN);
            let r = release_for(
                ip(peer),
                Some(l.clone()),
                ReleaseReq {
                    host: "10.0.0.9".into(),
                    session: Some(l.session),
                },
            );
            assert_eq!(r.status(), StatusCode::FORBIDDEN);
        }
        assert!(
            l.pad.lock().unwrap().is_empty(),
            "a refused input reached the console"
        );
    }

    #[tokio::test]
    async fn no_session_stale_session_and_no_viewer() {
        let (st, v) = body_json(frame_for(ip(LOCAL), None, q("10.0.0.9")).await).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("no_session"))
        );
        let l = session("10.0.0.9");
        // Another console's session is not this one's.
        let (st, _) = body_json(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.8")).await).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // An old session id: refused, and told the current one.
        let mut stale = q("10.0.0.9:9295");
        stale.session = Some(l.session + 1000);
        let (st, v) = body_json(frame_for(ip(LOCAL), Some(l.clone()), stale).await).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some("stale_session"))
        );
        assert_eq!(v["session"], l.session);
        // Detached: nobody to ask, and no stream is started or moved.
        let (st, v) = body_json(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")).await).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some("no_viewer"))
        );
        assert!(l.viewer.lock().unwrap().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_page_that_does_not_answer_times_out() {
        let l = session("10.0.0.9");
        let mut p = attach(&l);
        let (st, v) = body_json(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")).await).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::GATEWAY_TIMEOUT, Some("timeout"))
        );
        // It was asked, and a late answer is not kept for anyone.
        let req = next_request(&mut p).await;
        assert!(l.auto.pending.lock().unwrap().is_none());
        let late = answer(
            serde_json::json!({"id": req["id"], "format": "png", "width": 1, "height": 1}),
            b"x",
        );
        assert!(!l.auto.page_answered(p.id, late));
    }

    #[tokio::test]
    async fn the_picture_comes_back_from_the_page_with_its_metadata() {
        let l = session("10.0.0.9");
        let mut p = attach(&l);
        // A watcher (watch party) must never be asked.
        let (stx, mut srx) = mpsc::channel(8);
        l.spectators.lock().unwrap().push(Spectator {
            id: 77,
            tx: stx,
            need_key: true,
        });
        let mut want = q("10.0.0.9");
        want.session = Some(l.session);
        want.format = Some("jpeg".into());
        want.max_width = Some(640);
        let req = tokio::spawn(frame_for(ip(LOCAL), Some(l.clone()), want));
        let ask = next_request(&mut p).await;
        assert_eq!(ask["format"], "jpeg");
        assert_eq!(ask["max_width"], 640);
        // Another page id, or another request id: ignored.
        let meta = serde_json::json!({
            "id": ask["id"], "format": "jpeg", "width": 640, "height": 360,
            "captured_at_ms": 1_000_050u64, "frame_at_ms": 1_000_034u64, "frame_seq": 42,
        });
        assert!(!l
            .auto
            .page_answered(p.id + 1, answer(meta.clone(), b"JPEG")));
        let mut wrong = meta.clone();
        wrong["id"] = 999.into();
        assert!(!l.auto.page_answered(p.id, answer(wrong, b"JPEG")));
        assert!(l.auto.page_answered(p.id, answer(meta, b"JPEG")));
        let r = req.await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let h = r.headers().clone();
        assert_eq!(h["content-type"], "image/jpeg");
        assert_eq!(h["x-frame-width"], "640");
        assert_eq!(h["x-frame-height"], "360");
        assert_eq!(h["x-frame-age-ms"], "16");
        assert_eq!(h["x-frame-session"], l.session.to_string());
        assert_eq!(h["x-frame-host"], "10.0.0.9");
        assert_eq!(h["x-frame-id"], format!("{}.42", p.id));
        let b = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&b[..], b"JPEG");
        assert!(srx.try_recv().is_err(), "a watcher was asked for a picture");
    }

    #[tokio::test]
    async fn json_answers_carry_the_image_in_base64_and_after_reaches_the_page() {
        let l = session("10.0.0.9");
        let mut p = attach(&l);
        let mut want = q("10.0.0.9");
        want.json = Some("1".into());
        want.after = Some(format!("{}.41", p.id));
        let req = tokio::spawn(frame_for(ip(LOCAL), Some(l.clone()), want));
        let ask = next_request(&mut p).await;
        assert_eq!(ask["format"], "png");
        assert_eq!(ask["after_seq"], 41);
        assert!(ask.get("max_width").is_none());
        let meta = serde_json::json!({
            "id": ask["id"], "format": "png", "width": 1920, "height": 1080,
            "captured_at_ms": 5u64, "frame_at_ms": 5u64, "frame_seq": 43,
        });
        assert!(l.auto.page_answered(p.id, answer(meta, b"\x89PNG")));
        let (st, v) = body_json(req.await.unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["width"], 1920);
        assert_eq!(v["frame_age_ms"], 0);
        assert_eq!(v["session"], l.session);
        assert_eq!(v["frame_id"], format!("{}.43", p.id));
        assert_eq!(v["image_base64"], "iVBORw==");

        // `after` from another page: that page's sequence means nothing here.
        tokio::time::sleep(SNAPSHOT_EVERY).await;
        let mut want = q("10.0.0.9");
        want.after = Some(format!("{}.41", p.id + 5));
        let req = tokio::spawn(frame_for(ip(LOCAL), Some(l.clone()), want));
        let ask = next_request(&mut p).await;
        assert!(ask.get("after_seq").is_none());
        let meta = serde_json::json!({"id": ask["id"], "error": "no_newer_frame"});
        assert!(l.auto.page_answered(p.id, answer(meta, b"")));
        let (st, v) = body_json(req.await.unwrap()).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::GATEWAY_TIMEOUT, Some("no_newer_frame"))
        );
    }

    #[tokio::test]
    async fn requests_are_rate_limited_and_one_at_a_time() {
        let l = session("10.0.0.9");
        let mut p = attach(&l);
        let first = tokio::spawn(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")));
        let ask = next_request(&mut p).await;
        let (st, v) = body_json(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")).await).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::TOO_MANY_REQUESTS, Some("busy"))
        );
        let meta = serde_json::json!({"id": ask["id"], "error": "no_picture"});
        assert!(l.auto.page_answered(p.id, answer(meta, b"")));
        let (st, v) = body_json(first.await.unwrap()).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("no_picture"))
        );
        // Right after: too soon.
        let (st, v) = body_json(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")).await).await;
        assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(v["error"], "rate_limited");
        assert!(v["retry_ms"].as_u64().unwrap() <= SNAPSHOT_EVERY.as_millis() as u64 + 1);
    }

    #[tokio::test]
    async fn a_page_that_leaves_or_loses_the_stream_ends_the_wait() {
        let l = session("10.0.0.9");
        let mut p = attach(&l);
        let req = tokio::spawn(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")));
        let ask = next_request(&mut p).await;
        // Another page takes the stream: the first one's answer would be of nothing current.
        let mut p2 = attach(&l);
        let (st, v) = body_json(req.await.unwrap()).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some("no_viewer"))
        );
        let meta = serde_json::json!({"id": ask["id"], "format": "png", "width": 1, "height": 1});
        assert!(!l.auto.page_answered(p.id, answer(meta, b"x")));
        // The new page is the one asked next; then it leaves too.
        tokio::time::sleep(SNAPSHOT_EVERY).await;
        let req = tokio::spawn(frame_for(ip(LOCAL), Some(l.clone()), q("10.0.0.9")));
        let _ = next_request(&mut p2).await;
        assert!(p.ctl.try_recv().map_or(
            true,
            |m| !matches!(&m, Message::Text(t) if t.as_str().contains("snapshot"))
        ));
        l.detach(p2.id);
        let (st, _) = body_json(req.await.unwrap()).await;
        assert_eq!(st, StatusCode::CONFLICT);
    }

    #[test]
    fn answers_must_be_well_formed() {
        assert!(parse_answer(Bytes::from_static(b"")).is_none());
        assert!(parse_answer(Bytes::from_static(&[0, 0, 0, 9, b'{'])).is_none());
        assert!(parse_answer(answer(serde_json::json!([1]), b"")).is_none());
        let a = parse_answer(answer(serde_json::json!({"id": 1}), b"img")).unwrap();
        assert_eq!(&a.image[..], b"img");
    }

    #[test]
    fn inputs_are_bounded_and_named() {
        let r = InputReq {
            buttons: vec!["cross".into(), "DPAD_RIGHT".into(), "l1".into()],
            left: Some([1.0, -1.0]),
            right: Some([0.5, 0.0]),
            l2: Some(1.0),
            r2: Some(0.0),
            duration_ms: Some(150),
            ..Default::default()
        };
        let (s, ms) = parse_input(&r).unwrap();
        assert_eq!(ms, 150);
        assert_eq!(s.buttons, button::CROSS | button::DPAD_RIGHT | button::L1);
        assert_eq!((s.lx, s.ly, s.rx, s.ry), (32767, -32767, 16384, 0));
        assert_eq!((s.l2, s.r2), (255, 0));
        assert_eq!(parse_input(&InputReq::default()).unwrap().1, 100);
        for bad in [
            InputReq {
                buttons: vec!["jump".into()],
                ..Default::default()
            },
            InputReq {
                left: Some([1.5, 0.0]),
                ..Default::default()
            },
            InputReq {
                right: Some([f64::NAN, 0.0]),
                ..Default::default()
            },
            InputReq {
                l2: Some(-0.1),
                ..Default::default()
            },
            InputReq {
                duration_ms: Some(0),
                ..Default::default()
            },
            InputReq {
                duration_ms: Some(MAX_HOLD_MS + 1),
                ..Default::default()
            },
        ] {
            assert!(parse_input(&bad).is_err(), "{bad:?}");
        }
    }

    /// Every merged state the console would get, in order.
    fn sent(l: &Live) -> Vec<ControllerState> {
        l.pad.lock().unwrap().drain(..).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn input_is_held_then_let_go_and_never_takes_the_pages_place() {
        let l = session("10.0.0.9");
        let _p = attach(&l);
        // The user holds Square on the page (source 0).
        let page = ControllerState {
            buttons: button::SQUARE,
            ..Default::default()
        };
        l.push_pad(PAGE_SOURCE, Some(page));
        sent(&l);
        // Input must name the session, the current one.
        let mut r = InputReq {
            host: "10.0.0.9".into(),
            buttons: vec!["right".into()],
            duration_ms: Some(150),
            ..Default::default()
        };
        let (st, v) = body_json(input_for(ip(LOCAL), Some(l.clone()), r.clone())).await;
        assert_eq!(
            (st, v["field"].as_str()),
            (StatusCode::BAD_REQUEST, Some("session"))
        );
        r.session = Some(l.session + 1);
        let (st, v) = body_json(input_for(ip(LOCAL), Some(l.clone()), r.clone())).await;
        assert_eq!(
            (st, v["error"].as_str()),
            (StatusCode::CONFLICT, Some("stale_session"))
        );
        assert!(sent(&l).is_empty(), "a refused input reached the console");
        r.session = Some(l.session);
        let (st, v) = body_json(input_for(ip(LOCAL), Some(l.clone()), r.clone())).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(
            v["release_at_ms"].as_u64().unwrap() - v["sent_at_ms"].as_u64().unwrap(),
            150
        );
        // Merged with the page's Square, which stays the page's.
        assert_eq!(
            sent(&l),
            [ControllerState {
                buttons: button::SQUARE | button::DPAD_RIGHT,
                ..Default::default()
            }]
        );
        assert_eq!(
            l.sources.set(PAGE_SOURCE, Some(page)).buttons & button::SQUARE,
            button::SQUARE
        );
        sent(&l);
        tokio::time::sleep(Duration::from_millis(149)).await;
        assert!(sent(&l).is_empty());
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            sent(&l),
            [page],
            "let go of Right only; the page still holds Square"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_newer_input_replaces_the_held_one_and_release_lets_go_at_once() {
        let l = session("10.0.0.9");
        let input = |b: &str, ms: u64| InputReq {
            host: "10.0.0.9".into(),
            session: Some(l.session),
            buttons: vec![b.into()],
            duration_ms: Some(ms),
            ..Default::default()
        };
        assert_eq!(
            input_for(ip(LOCAL), Some(l.clone()), input("up", 100)).status(),
            StatusCode::OK
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            input_for(ip(LOCAL), Some(l.clone()), input("down", 100)).status(),
            StatusCode::OK
        );
        sent(&l);
        // The first hold's end must not let go of the second.
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(sent(&l).is_empty());
        let r = release_for(
            ip(LOCAL),
            Some(l.clone()),
            ReleaseReq {
                host: "10.0.0.9".into(),
                session: Some(l.session),
            },
        );
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(sent(&l), [ControllerState::default()]);
        // Nor does the second's, after the release.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(sent(&l).is_empty());
        let stale = release_for(
            ip(LOCAL),
            Some(l.clone()),
            ReleaseReq {
                host: "10.0.0.9".into(),
                session: Some(l.session + 1),
            },
        );
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let none = release_for(
            ip(LOCAL),
            None,
            ReleaseReq {
                host: "10.0.0.9".into(),
                session: None,
            },
        );
        assert_eq!(none.status(), StatusCode::NOT_FOUND);
        assert_ne!(AUTOMATION_SOURCE, PAGE_SOURCE);
    }
}
