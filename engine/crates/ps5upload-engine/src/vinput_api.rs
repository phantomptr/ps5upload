//! The PC's keyboard and mouse as the console's own (native keyboard & mouse).
//!
//! Pages send input two ways, both checked for authority before anything reaches this module:
//!
//! * on the Remote Play socket (`rp_stream_api.rs`): `{"kbm": {"on", "mouse", "keyboard"}}` (sent
//!   again every [`vinput::HEARTBEAT`] as the heartbeat; `on: false` turns it off) and binary
//!   `[8][16-byte report]` (`ps5upload_core::vinput::Report`). Only the page the session is shown
//!   on counts ([`Owner::Page`] with its viewer id); the engine answers `{"kbm": {...}}` with what
//!   the PS5 sees (see [`status_json`]).
//! * without a stream, on `GET /api/ps5/vinput/ws?host=` ([`ws`]): the same messages, accepted only
//!   from this machine (a loopback peer, whatever `PS5UPLOAD_ALLOW_IP` admits), and refused while a
//!   Remote Play session for that console has a page (that page owns the input).
//!
//! One link per console runs on its own thread: it opens the helper's virtual devices
//! (`vinput.session`), sends the reports in order (merged by `vinput::coalesce`, one call in flight,
//! so a slow round trip batches instead of queueing), keeps the session with a heartbeat while the
//! page's heartbeat is fresh, and closes it (the helper lets go of every key and button) when the
//! page turns it off, leaves, is replaced by another owner, or goes quiet for [`vinput::TIMEOUT`].
//! The helper lets go on its own too when the engine goes quiet.
//!
//! `GET /api/ps5/vinput/caps?addr=` says what the console can make.

use std::collections::HashMap;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ps5upload_core::vinput::{
    self, coalesce, Report, ReportAck, SessionOp, SessionState, REPORT_LEN,
};

/// Page → engine binary kind on the Remote Play socket (and the local socket): one report.
pub(crate) const KIND_KBM: u8 = 8;
/// After a failed open (console unreachable), the next attempt waits this long.
const RETRY_AFTER: Duration = Duration::from_secs(3);
/// After the page turns the mode off, the devices stay (everything let go) this long before they
/// are removed, so switching modes back and forth does not add and remove them each time. Leaving
/// the stream, or going quiet, removes them at once.
pub(crate) const LINGER: Duration = Duration::from_secs(5);
/// While the console's keyboard & mouse process starts (`vinput_starting`), the open is asked
/// again this often.
const STARTING_RETRY: Duration = Duration::from_millis(300);
/// A link with no owner ends after this long.
const IDLE_END: Duration = Duration::from_secs(5);
/// Reports waiting for the link thread: past this the page is far ahead of the console and the
/// oldest motion is merged away (never a change of what is held).
const PENDING_MAX: usize = 1024;

/// Who sends input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Owner {
    /// The page showing the Remote Play session (its viewer id).
    Page(u64),
    /// A page on this machine with no stream (its socket's id).
    Local(u64),
}

/// Where a link's status goes (the owner's socket).
pub(crate) type StatusFn = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// The helper side (a test seam: [`HelperPort`] is the real one).
pub(crate) trait Port: Send + Sync + 'static {
    fn session(&self, host: &str, op: SessionOp, id: u64) -> Result<Option<SessionState>>;
    fn report(&self, host: &str, id: u64, r: &[Report]) -> Result<Option<ReportAck>>;
}

struct HelperPort;

impl Port for HelperPort {
    fn session(&self, host: &str, op: SessionOp, id: u64) -> Result<Option<SessionState>> {
        vinput::session(host, op, id)
    }
    fn report(&self, host: &str, id: u64, r: &[Report]) -> Result<Option<ReportAck>> {
        vinput::report(host, id, r)
    }
}

pub(crate) enum Cmd {
    /// The owner wants these devices (also its heartbeat).
    Want {
        owner: Owner,
        mouse: bool,
        keyboard: bool,
        status: StatusFn,
    },
    Report {
        owner: Owner,
        report: Report,
    },
    /// The owner is done (`why` goes to its status).
    Off {
        owner: Owner,
        why: &'static str,
    },
}

/// A fresh owner token for the helper (never 0).
fn new_token() -> u64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_ok() {
        let t = u64::from_le_bytes(b);
        if t != 0 {
            return t;
        }
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    (t ^ N
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        .rotate_left(32))
        | 1
}

/// What the owner's page is told: `on` (input is wanted), what the PS5 sees (`mouse`, `keyboard`:
/// its virtual devices are open), and why not (`err`: `vinput_unsupported` (this console cannot
/// make them yet), `vinput_starting` (its keyboard & mouse process is starting: asked again on its
/// own), `vinput_needs_loader`, `vinput_companion_failed`, `vinput_helper_old`, `unreachable`,
/// `vinput_failed`, `vinput_no_user` (nobody is signed in on the PS5), `rp_session_owns_input`), or why
/// it ended (`reason`: `off`, `timeout`, `taken`, `left`).
fn status_json(
    on: bool,
    s: &SessionState,
    err: Option<&str>,
    reason: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({"kbm": {
        "on": on,
        "mouse": s.open && s.mouse,
        "keyboard": s.open && s.keyboard,
        "err": err,
        "reason": reason,
    }})
}

/// One console's link: the state machine the thread drives (time passed in, for tests).
pub(crate) struct LinkState<P: Port> {
    host: String,
    port: Arc<P>,
    owner: Option<Owner>,
    status: Option<StatusFn>,
    token: u64,
    /// What the owner wants, while it wants anything.
    want: Option<(bool, bool)>,
    /// What was last asked of the helper (an open is not repeated for every heartbeat).
    asked: Option<(bool, bool)>,
    helper: SessionState,
    err: Option<String>,
    page_hb: Instant,
    helper_hb: Instant,
    retry_at: Option<Instant>,
    pending: Vec<Report>,
    last_sent: Option<String>,
    /// When the link last had an owner.
    owned_at: Instant,
    /// Turned off: the devices are removed at this time unless it is turned on again.
    linger_until: Option<Instant>,
}

impl<P: Port> LinkState<P> {
    pub(crate) fn new(host: &str, port: Arc<P>, now: Instant) -> Self {
        LinkState {
            host: host.to_string(),
            port,
            owner: None,
            status: None,
            token: 0,
            want: None,
            asked: None,
            helper: SessionState::default(),
            err: None,
            page_hb: now,
            helper_hb: now,
            retry_at: None,
            pending: Vec::new(),
            last_sent: None,
            owned_at: now,
            linger_until: None,
        }
    }

    fn tell(&mut self, reason: Option<&str>) {
        let Some(st) = self.status.clone() else {
            return;
        };
        let v = status_json(
            self.want.is_some(),
            &self.helper,
            self.err.as_deref(),
            reason,
        );
        let s = v.to_string();
        if self.last_sent.as_deref() == Some(s.as_str()) {
            return;
        }
        self.last_sent = Some(s);
        st(v);
    }

    /// Lets go of the helper's session (it lets go of every key and button) and forgets the owner.
    fn end(&mut self, reason: &'static str) {
        if self.helper.open || self.asked.is_some() {
            let _ = self.port.session(&self.host, SessionOp::Close, self.token);
        }
        self.pending.clear();
        self.helper = SessionState::default();
        self.want = None;
        self.asked = None;
        self.retry_at = None;
        self.err = None;
        self.linger_until = None;
        self.tell(Some(reason));
        self.owner = None;
        self.status = None;
        self.last_sent = None;
    }

    fn open(&mut self, mouse: bool, keyboard: bool, now: Instant) {
        if self.retry_at.is_some_and(|t| now < t) {
            return;
        }
        self.asked = Some((mouse, keyboard));
        self.retry_at = None;
        match self
            .port
            .session(&self.host, SessionOp::Open { mouse, keyboard }, self.token)
        {
            Ok(Some(s)) if s.err.as_deref() == Some("vinput_starting") && !s.open => {
                // The console starts its keyboard & mouse process: ask again shortly.
                self.err = s.err.clone();
                self.helper = s;
                self.asked = None;
                self.retry_at = Some(now + STARTING_RETRY);
            }
            Ok(Some(s)) => {
                self.err = s.err.clone();
                self.helper = s;
                self.helper_hb = now;
            }
            Ok(None) => {
                self.helper = SessionState::default();
                self.err = Some("vinput_helper_old".into());
            }
            Err(e) => {
                crate::log_info!("vinput: open on {} failed: {e:#}", self.host);
                self.helper = SessionState::default();
                self.err = Some("unreachable".into());
                self.asked = None;
                self.retry_at = Some(now + RETRY_AFTER);
            }
        }
    }

    /// The helper no longer has our session (it timed out, or was taken): open again on the next
    /// heartbeat.
    fn lost(&mut self, err: Option<String>) {
        self.helper = SessionState::default();
        self.asked = None;
        self.pending.clear();
        self.err = err;
    }

    /// The helper answered that our session is gone: it timed out (opened again with the next
    /// heartbeat), or another computer took the devices (`vinput_taken`: not taken back on our own,
    /// which would only take turns with it; turning the mode off and on again does).
    fn lost_session(&mut self, err: Option<String>) {
        match err.as_deref() {
            Some("vinput_not_owner") => {
                let asked = self.asked;
                self.lost(Some("vinput_taken".into()));
                self.asked = asked;
            }
            Some("vinput_not_open") | None => self.lost(None),
            Some(_) => self.lost(err),
        }
    }

    pub(crate) fn on_cmd(&mut self, cmd: Cmd, now: Instant) {
        match cmd {
            Cmd::Want {
                owner,
                mouse,
                keyboard,
                status,
            } => {
                if self.owner != Some(owner) {
                    if self.owner.is_some() {
                        self.end("taken");
                    }
                    self.owner = Some(owner);
                    self.token = new_token();
                }
                self.status = Some(status);
                self.owned_at = now;
                if !mouse && !keyboard {
                    self.end("off");
                    return;
                }
                self.page_hb = now;
                self.want = Some((mouse, keyboard));
                self.linger_until = None;
                if self.asked != Some((mouse, keyboard)) {
                    self.open(mouse, keyboard, now);
                }
                self.tell(None);
            }
            Cmd::Report { owner, report } => {
                if self.owner != Some(owner) || !self.helper.open || self.want.is_none() {
                    return;
                }
                if self.pending.len() >= PENDING_MAX {
                    let merged = coalesce(self.pending.drain(..));
                    self.pending = merged;
                }
                self.pending.push(report);
            }
            Cmd::Off { owner, why } => {
                if self.owner != Some(owner) {
                    return;
                }
                if why == "off" && self.helper.open {
                    self.linger(now);
                } else {
                    self.end(why);
                }
            }
        }
    }

    /// Turned off: everything is let go now (what was queued is dropped), the devices stay for
    /// [`LINGER`].
    fn linger(&mut self, now: Instant) {
        self.pending.clear();
        let _ = self
            .port
            .report(&self.host, self.token, &[Report::RELEASED]);
        self.want = None;
        self.linger_until = Some(now + LINGER);
        self.helper_hb = now;
        self.tell(Some("off"));
    }

    /// Sends what is waiting, keeps the helper's session, and ends one whose page went quiet.
    pub(crate) fn tick(&mut self, now: Instant) {
        if self.owner.is_some() {
            self.owned_at = now;
        }
        if let Some(t) = self.linger_until {
            if now >= t {
                self.end("off");
                return;
            }
        }
        if self.want.is_some() && now.duration_since(self.page_hb) > vinput::TIMEOUT {
            self.end("timeout");
            return;
        }
        // A failed open is tried again once its wait is over.
        if let (Some((m, k)), None) = (self.want, self.asked) {
            if self.retry_at.is_some_and(|t| now >= t) {
                self.open(m, k, now);
                self.tell(None);
            }
        }
        if !self.helper.open {
            return;
        }
        if !self.pending.is_empty() {
            let batch = coalesce(self.pending.drain(..));
            match self.port.report(&self.host, self.token, &batch) {
                Ok(Some(a)) if a.ok => {
                    self.helper_hb = now;
                    self.err = None;
                }
                Ok(Some(a)) => {
                    let err = a.err.unwrap_or_else(|| "vinput_failed".into());
                    if err == "vinput_not_open" || err == "vinput_not_owner" {
                        self.lost_session(Some(err));
                    } else {
                        self.err = Some(err);
                        self.helper_hb = now;
                    }
                }
                Ok(None) => self.lost(Some("vinput_helper_old".into())),
                Err(_) => self.lost(Some("unreachable".into())),
            }
            self.tell(None);
            return;
        }
        if now.duration_since(self.helper_hb) >= vinput::HEARTBEAT {
            match self
                .port
                .session(&self.host, SessionOp::Heartbeat, self.token)
            {
                Ok(Some(s)) if s.ok => {
                    // Still ours; `vinput_starting` while the console restarts its process.
                    self.helper_hb = now;
                    self.err = s.err;
                }
                Ok(Some(s)) => self.lost_session(s.err),
                Ok(None) => self.lost(Some("vinput_helper_old".into())),
                Err(_) => self.lost(Some("unreachable".into())),
            }
            self.tell(None);
        }
    }

    fn idle_for(&self, now: Instant) -> Option<Duration> {
        self.owner
            .is_none()
            .then(|| now.duration_since(self.owned_at))
    }
}

/// The running links, by console host.
/// A running link: its generation (to tell a newer one apart) and its queue.
type LinkTx = (u64, mpsc::Sender<Cmd>);
static LINKS: Mutex<Option<HashMap<String, LinkTx>>> = Mutex::new(None);

fn spawn_link(host: &str) -> LinkTx {
    static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let gen = GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Cmd>();
    let h = host.to_string();
    std::thread::Builder::new()
        .name("ps5upload-vinput".into())
        .spawn(move || run_link(h, gen, rx))
        .ok();
    (gen, tx)
}

fn run_link(host: String, gen: u64, rx: mpsc::Receiver<Cmd>) {
    let mut st = LinkState::new(&host, Arc::new(HelperPort), Instant::now());
    loop {
        let wait = if st.owner.is_some() {
            Duration::from_millis(50)
        } else {
            Duration::from_secs(1)
        };
        match rx.recv_timeout(wait) {
            Ok(c) => {
                st.on_cmd(c, Instant::now());
                // Everything already waiting goes in the same batch.
                while let Ok(c) = rx.try_recv() {
                    st.on_cmd(c, Instant::now());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                st.end("left");
                return;
            }
        }
        st.tick(Instant::now());
        if st.idle_for(Instant::now()).is_some_and(|d| d > IDLE_END) {
            let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(map) = g.as_mut() {
                if map.get(&host).is_some_and(|(g2, _)| *g2 == gen) {
                    map.remove(&host);
                }
            }
            drop(g);
            // Anything sent between the last look and the removal is still served.
            while let Ok(c) = rx.try_recv() {
                st.on_cmd(c, Instant::now());
            }
            if st.owner.is_none() {
                return;
            }
        }
    }
}

/// The console without a port: `10.0.0.2` and `10.0.0.2:9120` are one console (one link).
pub(crate) fn host_key(console: &str) -> String {
    if let Some(rest) = console.strip_prefix('[') {
        if let Some(i) = rest.find(']') {
            return format!("[{}]", &rest[..i]);
        }
        return console.to_string();
    }
    match console.split_once(':') {
        Some((h, port)) if !port.contains(':') => h.to_string(),
        _ => console.to_string(),
    }
}

/// Sends `cmd` to `host`'s link; `spawn` starts one when none runs.
fn send(host: &str, cmd: Cmd, spawn: bool) {
    let host = host_key(host);
    let host = host.as_str();
    let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
    let map = g.get_or_insert_with(HashMap::new);
    let cmd = match map.get(host) {
        Some((_, tx)) => match tx.send(cmd) {
            Ok(()) => return,
            Err(mpsc::SendError(c)) => c, // that link ended: start another
        },
        None => cmd,
    };
    if !spawn {
        return;
    }
    let (gen, tx) = spawn_link(host);
    let _ = tx.send(cmd);
    map.insert(host.to_string(), (gen, tx));
}

/// The owner wants input sent to `host` (also its heartbeat); both false turns it off.
pub(crate) fn want(host: &str, owner: Owner, mouse: bool, keyboard: bool, status: StatusFn) {
    send(
        host,
        Cmd::Want {
            owner,
            mouse,
            keyboard,
            status,
        },
        true,
    );
}

/// One report from the owner (binary `[8][16 bytes]` without the kind). False when malformed.
pub(crate) fn report(host: &str, owner: Owner, bytes: &[u8]) -> bool {
    let Some(report) = Report::from_bytes(bytes) else {
        return false;
    };
    send(host, Cmd::Report { owner, report }, false);
    true
}

/// The owner is done (left, or turned it off).
pub(crate) fn off(host: &str, owner: Owner, why: &'static str) {
    send(host, Cmd::Off { owner, why }, false);
}

/// A `{"kbm": {...}}` message from a page: `on` false (or both devices false) turns it off.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct KbmMsg {
    #[serde(default)]
    pub on: bool,
    #[serde(default)]
    pub mouse: bool,
    #[serde(default)]
    pub keyboard: bool,
}

/// Applies a page's `{"kbm": ...}` for `owner`.
pub(crate) fn control(host: &str, owner: Owner, m: &KbmMsg, status: StatusFn) {
    if m.on && (m.mouse || m.keyboard) {
        want(host, owner, m.mouse, m.keyboard, status);
    } else {
        off(host, owner, "off");
    }
}

// ── HTTP ─────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct CapsQuery {
    addr: String,
}

/// `GET /api/ps5/vinput/caps?addr=`: `{"supported": true, "mouse", "keyboard", "open", "backend",
/// "reason"?}`, or `{"supported": false}` from a helper without the methods.
pub async fn caps(Query(q): Query<CapsQuery>) -> Response {
    let addr = q.addr;
    match tokio::task::spawn_blocking(move || vinput::caps(&addr)).await {
        Ok(Ok(Some(c))) => {
            let mut v = serde_json::to_value(c).unwrap_or_default();
            v["supported"] = true.into();
            Json(v).into_response()
        }
        Ok(Ok(None)) => Json(serde_json::json!({"supported": false})).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": format!("{e:#}")})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct WsQuery {
    host: String,
}

/// Only this machine may send input without a stream.
pub(crate) fn local_allowed(peer: std::net::IpAddr) -> bool {
    peer.is_loopback()
}

/// The request addresses this computer by a loopback name (`localhost`, `127.0.0.1`, `[::1]`,
/// any port), as the app and a local script do (the app fetches `http://127.0.0.1:<port>`).
/// The peer being loopback is not enough on its own: a page whose own name was re-pointed at
/// 127.0.0.1 (DNS rebinding) reaches the engine from this computer as its own origin, but its
/// requests still carry its own name. Only for the routes that answer to this computer alone;
/// the web UI elsewhere is reached by any name (a NAS hostname, a reverse proxy).
pub(crate) fn host_is_local(headers: &axum::http::HeaderMap) -> bool {
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let host = host.trim();
    let (name, port) = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((name, "")) => (name, None),
            Some((name, port)) => match port.strip_prefix(':') {
                Some(p) => (name, Some(p)),
                None => return false,
            },
            None => return false,
        },
        None => match host.split_once(':') {
            Some((name, p)) => (name, Some(p)),
            None => (host, None),
        },
    };
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    port_ok && (name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1")
}

/// The refusal of a request that does not address this computer by a loopback name.
pub(crate) fn host_not_local() -> Response {
    (
        StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({"error": "host_not_local"})),
    )
        .into_response()
}

/// `GET /api/ps5/vinput/ws?host=`: this computer as the console's keyboard and mouse with no
/// stream. Loopback peers only, addressing it as localhost.
pub async fn ws(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(q): Query<WsQuery>,
) -> Response {
    if !host_is_local(&headers) {
        return host_not_local();
    }
    if !local_allowed(peer.ip()) {
        return (
            StatusCode::FORBIDDEN,
            "keyboard and mouse input is accepted from this computer only",
        )
            .into_response();
    }
    upgrade.on_upgrade(move |s| local_session(s, q.host))
}

async fn local_session(mut socket: WebSocket, host: String) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let owner = Owner::Local(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<serde_json::Value>(32);
    let status: StatusFn = Arc::new(move |v| {
        let _ = tx.try_send(v);
    });
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(v) => {
                    if socket.send(Message::Text(v.to_string().into())).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            inc = socket.recv() => match inc {
                Some(Ok(Message::Text(t))) => {
                    #[derive(serde::Deserialize)]
                    struct M {
                        kbm: Option<KbmMsg>,
                    }
                    let Ok(M { kbm: Some(m) }) = serde_json::from_str::<M>(t.as_str()) else {
                        continue;
                    };
                    // A Remote Play page showing this console owns its input.
                    if m.on && crate::rp_stream_api::page_attached(&host) {
                        off(&host, owner, "off");
                        let v = status_json(false, &SessionState::default(), Some("rp_session_owns_input"), None);
                        if socket.send(Message::Text(v.to_string().into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    control(&host, owner, &m, status.clone());
                }
                Some(Ok(Message::Binary(b))) if b.first() == Some(&KIND_KBM) && b.len() == 1 + REPORT_LEN => {
                    report(&host, owner, &b[1..]);
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
    off(&host, owner, "left");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_host(h: Option<&str>) -> axum::http::HeaderMap {
        let mut m = axum::http::HeaderMap::new();
        if let Some(h) = h {
            m.insert(axum::http::header::HOST, h.parse().unwrap());
        }
        m
    }

    /// Loopback names at any port only: a rebound name (DNS rebinding), a LAN address or
    /// none at all is refused.
    #[test]
    fn only_a_loopback_host_is_local() {
        for h in [
            "localhost",
            "LOCALHOST:19113",
            "127.0.0.1",
            "127.0.0.1:19114",
            "[::1]",
            "[::1]:19113",
        ] {
            assert!(host_is_local(&with_host(Some(h))), "{h}");
        }
        for h in [
            "evil.example:19113",
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.nip.io:19113",
            "192.168.1.10:19113",
            "nas.local:19113",
            "tauri.localhost",
            "127.0.0.2:19113",
            "[::2]:19113",
            "[::1]x",
            "::1",
            "localhost:",
            "localhost:80x",
            "",
        ] {
            assert!(!host_is_local(&with_host(Some(h))), "{h}");
        }
        assert!(!host_is_local(&with_host(None)));
        assert_eq!(host_not_local().status(), StatusCode::FORBIDDEN);
    }

    #[derive(Default)]
    struct Fake {
        log: Mutex<Vec<String>>,
        /// What the helper can make.
        can: Mutex<(bool, bool)>,
        unreachable: Mutex<bool>,
        old: Mutex<bool>,
        /// The helper forgot the session (it timed out).
        forgot: Mutex<bool>,
        /// Opens still answered `vinput_starting` (the console's process is starting).
        starting: Mutex<u32>,
        /// Heartbeats say the process is being started again.
        restarting: Mutex<bool>,
    }

    impl Fake {
        fn log(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }
    }

    impl Port for Fake {
        fn session(&self, _h: &str, op: SessionOp, id: u64) -> Result<Option<SessionState>> {
            assert_ne!(id, 0);
            self.log.lock().unwrap().push(format!("{op:?}"));
            if *self.unreachable.lock().unwrap() {
                anyhow::bail!("no route");
            }
            if *self.old.lock().unwrap() {
                return Ok(None);
            }
            let (cm, ck) = *self.can.lock().unwrap();
            Ok(Some(match op {
                SessionOp::Open { .. } if *self.starting.lock().unwrap() > 0 => {
                    *self.starting.lock().unwrap() -= 1;
                    SessionState {
                        err: Some("vinput_starting".into()),
                        ..Default::default()
                    }
                }
                SessionOp::Heartbeat if *self.restarting.lock().unwrap() => SessionState {
                    ok: true,
                    open: true,
                    mouse: cm,
                    keyboard: ck,
                    err: Some("vinput_starting".into()),
                },
                SessionOp::Open { mouse, keyboard } => {
                    let (m, k) = (mouse && cm, keyboard && ck);
                    SessionState {
                        ok: m == mouse && k == keyboard,
                        open: m || k,
                        mouse: m,
                        keyboard: k,
                        err: (m != mouse || k != keyboard).then(|| "vinput_unsupported".into()),
                    }
                }
                SessionOp::Heartbeat if *self.forgot.lock().unwrap() => SessionState {
                    err: Some("vinput_not_open".into()),
                    ..Default::default()
                },
                SessionOp::Heartbeat => SessionState {
                    ok: true,
                    open: true,
                    mouse: cm,
                    keyboard: ck,
                    err: None,
                },
                SessionOp::Close => SessionState {
                    ok: true,
                    ..Default::default()
                },
            }))
        }
        fn report(&self, _h: &str, _id: u64, r: &[Report]) -> Result<Option<ReportAck>> {
            self.log
                .lock()
                .unwrap()
                .push(format!("report {}", vinput::to_hex(r)));
            Ok(Some(ReportAck {
                ok: true,
                applied: r.len() as u32,
                err: None,
            }))
        }
    }

    type Seen = Arc<Mutex<Vec<serde_json::Value>>>;

    fn sink() -> (StatusFn, Seen) {
        let seen: Seen = Default::default();
        let s = seen.clone();
        (
            Arc::new(move |v| s.lock().unwrap().push(v["kbm"].clone())),
            seen,
        )
    }

    fn want_cmd(owner: Owner, m: bool, k: bool, st: &StatusFn) -> Cmd {
        Cmd::Want {
            owner,
            mouse: m,
            keyboard: k,
            status: st.clone(),
        }
    }

    fn rep(buttons: u8, dx: i16) -> Report {
        Report {
            buttons,
            dx,
            ..Default::default()
        }
    }

    const P1: Owner = Owner::Page(1);

    #[test]
    fn a_starting_console_is_asked_again_until_its_devices_open() {
        let port = Arc::new(Fake::default());
        *port.can.lock().unwrap() = (true, true);
        *port.starting.lock().unwrap() = 2;
        let t0 = Instant::now();
        let mut l = LinkState::new("10.0.0.2", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        let s = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(s["err"], "vinput_starting");
        assert_eq!(s["mouse"], false);
        // a heartbeat right away does not ask again; after the wait it does, twice
        l.on_cmd(
            want_cmd(P1, true, true, &st),
            t0 + Duration::from_millis(100),
        );
        l.tick(t0 + Duration::from_millis(100));
        assert_eq!(port.log().len(), 1);
        let mut now = t0;
        for _ in 0..4 {
            now += STARTING_RETRY;
            l.on_cmd(want_cmd(P1, true, true, &st), now);
            l.tick(now);
        }
        assert_eq!(
            port.log().iter().filter(|e| e.starts_with("Open")).count(),
            2
        );
        let s = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(s["mouse"], true);
        assert_eq!(s["keyboard"], true);
        assert!(s["err"].is_null());
        // the console restarting its process: the session stays, the page sees it, then it clears
        *port.restarting.lock().unwrap() = true;
        now += vinput::HEARTBEAT;
        l.on_cmd(want_cmd(P1, true, true, &st), now);
        l.tick(now);
        assert_eq!(
            seen.lock().unwrap().last().unwrap()["err"],
            "vinput_starting"
        );
        assert_eq!(seen.lock().unwrap().last().unwrap()["mouse"], true);
        *port.restarting.lock().unwrap() = false;
        l.on_cmd(
            Cmd::Report {
                owner: P1,
                report: rep(1, 3),
            },
            now,
        );
        l.tick(now);
        assert!(seen.lock().unwrap().last().unwrap()["err"].is_null());
    }

    #[test]
    fn the_stub_console_says_not_supported_and_nothing_is_sent() {
        let port = Arc::new(Fake::default()); // can make nothing: today's helper
        let t0 = Instant::now();
        let mut l = LinkState::new("10.0.0.2", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        assert_eq!(
            port.log(),
            vec!["Open { mouse: true, keyboard: true }".to_string()]
        );
        let s = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(s["on"], true);
        assert_eq!(s["mouse"], false);
        assert_eq!(s["keyboard"], false);
        assert_eq!(s["err"], "vinput_unsupported");
        // heartbeats do not ask again, reports go nowhere, and nothing is kept open
        for i in 1..5 {
            let now = t0 + Duration::from_millis(500 * i);
            l.on_cmd(want_cmd(P1, true, true, &st), now);
            l.on_cmd(
                Cmd::Report {
                    owner: P1,
                    report: rep(1, 3),
                },
                now,
            );
            l.tick(now);
        }
        assert!(port.log().is_empty());
        // one status, not one per heartbeat
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn reports_go_in_order_merged_and_the_session_is_kept_then_closed() {
        let port = Arc::new(Fake::default());
        *port.can.lock().unwrap() = (true, true);
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, false, &st), t0);
        assert_eq!(seen.lock().unwrap().last().unwrap()["mouse"], true);
        port.log();
        for r in [rep(0, 1), rep(0, 2), rep(1, 0), rep(0, 0)] {
            l.on_cmd(
                Cmd::Report {
                    owner: P1,
                    report: r,
                },
                t0,
            );
        }
        // another owner's report is dropped
        l.on_cmd(
            Cmd::Report {
                owner: Owner::Page(2),
                report: rep(4, 9),
            },
            t0,
        );
        l.tick(t0);
        let log = port.log();
        assert_eq!(log.len(), 1);
        assert_eq!(
            log[0],
            format!(
                "report {}",
                vinput::to_hex(&[rep(0, 3), rep(1, 0), rep(0, 0)])
            )
        );
        // quiet but wanted: a heartbeat every 500 ms
        l.on_cmd(
            want_cmd(P1, true, false, &st),
            t0 + Duration::from_millis(400),
        );
        l.tick(t0 + Duration::from_millis(400));
        assert!(port.log().is_empty());
        l.tick(t0 + Duration::from_millis(500));
        assert_eq!(port.log(), vec!["Heartbeat".to_string()]);
        // off: everything is let go at once, the devices stay 5 s (kept alive), then go
        let t1 = t0 + Duration::from_millis(600);
        l.on_cmd(
            Cmd::Off {
                owner: P1,
                why: "off",
            },
            t1,
        );
        assert_eq!(
            port.log(),
            vec![format!("report {}", vinput::to_hex(&[Report::RELEASED]))]
        );
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last["on"], false);
        assert_eq!(last["reason"], "off");
        // reports while off go nowhere
        l.on_cmd(
            Cmd::Report {
                owner: P1,
                report: rep(1, 1),
            },
            t1,
        );
        l.tick(t1 + vinput::HEARTBEAT);
        assert_eq!(port.log(), vec!["Heartbeat".to_string()]);
        l.tick(t1 + LINGER);
        assert_eq!(port.log(), vec!["Close".to_string()]);
    }

    #[test]
    fn turned_on_again_within_five_seconds_the_devices_are_kept() {
        let port = Arc::new(Fake::default());
        *port.can.lock().unwrap() = (true, true);
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, _seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        l.on_cmd(
            Cmd::Off {
                owner: P1,
                why: "off",
            },
            t0,
        );
        port.log();
        l.on_cmd(want_cmd(P1, true, true, &st), t0 + Duration::from_secs(2));
        assert!(port.log().is_empty(), "no new open");
        let t6 = t0 + LINGER + Duration::from_secs(1);
        l.on_cmd(want_cmd(P1, true, true, &st), t6);
        l.tick(t6);
        assert!(!port.log().contains(&"Close".to_string()));
        // leaving the stream removes them at once
        l.on_cmd(
            Cmd::Off {
                owner: P1,
                why: "left",
            },
            t0 + Duration::from_secs(7),
        );
        assert_eq!(port.log(), vec!["Close".to_string()]);
    }

    #[test]
    fn a_page_that_goes_quiet_is_let_go_after_two_seconds() {
        let port = Arc::new(Fake::default());
        *port.can.lock().unwrap() = (true, true);
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        port.log();
        let mut t = t0;
        // the engine keeps the helper alive only while the page's heartbeat is fresh
        while t < t0 + Duration::from_millis(1900) {
            t += Duration::from_millis(100);
            l.tick(t);
        }
        assert!(port.log().iter().all(|s| s == "Heartbeat"));
        l.tick(t0 + vinput::TIMEOUT + Duration::from_millis(1));
        assert_eq!(port.log(), vec!["Close".to_string()]);
        assert_eq!(seen.lock().unwrap().last().unwrap()["reason"], "timeout");
        assert!(l.idle_for(t0 + Duration::from_secs(3)).is_some());
    }

    #[test]
    fn a_new_owner_takes_over_and_the_old_one_is_let_go_first() {
        let port = Arc::new(Fake::default());
        *port.can.lock().unwrap() = (true, true);
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (a, seen_a) = sink();
        let (b, seen_b) = sink();
        l.on_cmd(want_cmd(P1, true, true, &a), t0);
        let first = l.token;
        port.log();
        l.on_cmd(want_cmd(Owner::Local(5), true, false, &b), t0);
        assert_eq!(
            port.log(),
            vec![
                "Close".to_string(),
                "Open { mouse: true, keyboard: false }".to_string()
            ]
        );
        assert_ne!(l.token, first, "a new owner gets a new token");
        assert_eq!(seen_a.lock().unwrap().last().unwrap()["reason"], "taken");
        assert_eq!(seen_b.lock().unwrap().last().unwrap()["mouse"], true);
        // the old owner's off does nothing now
        l.on_cmd(
            Cmd::Off {
                owner: P1,
                why: "left",
            },
            t0,
        );
        assert!(port.log().is_empty());
    }

    #[test]
    fn an_older_helper_and_an_unreachable_one_are_said_plainly() {
        let port = Arc::new(Fake::default());
        *port.old.lock().unwrap() = true;
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        assert_eq!(
            seen.lock().unwrap().last().unwrap()["err"],
            "vinput_helper_old"
        );

        let port = Arc::new(Fake::default());
        *port.unreachable.lock().unwrap() = true;
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        assert_eq!(seen.lock().unwrap().last().unwrap()["err"], "unreachable");
        port.log();
        // not asked again on every heartbeat, but after the wait
        l.on_cmd(
            want_cmd(P1, true, true, &st),
            t0 + Duration::from_millis(500),
        );
        l.tick(t0 + Duration::from_millis(500));
        assert!(port.log().is_empty());
        *port.unreachable.lock().unwrap() = false;
        *port.can.lock().unwrap() = (true, true);
        l.on_cmd(want_cmd(P1, true, true, &st), t0 + RETRY_AFTER);
        l.tick(t0 + RETRY_AFTER);
        assert!(port.log().iter().any(|s| s.starts_with("Open")));
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(
            (last["mouse"].clone(), last["err"].clone()),
            (true.into(), serde_json::Value::Null)
        );
    }

    #[test]
    fn a_helper_that_forgot_the_session_is_opened_again() {
        let port = Arc::new(Fake::default());
        *port.can.lock().unwrap() = (true, true);
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, _seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        *port.forgot.lock().unwrap() = true;
        l.tick(t0 + vinput::HEARTBEAT);
        assert!(!l.helper.open);
        *port.forgot.lock().unwrap() = false;
        port.log();
        l.on_cmd(want_cmd(P1, true, true, &st), t0 + vinput::HEARTBEAT);
        assert_eq!(
            port.log(),
            vec!["Open { mouse: true, keyboard: true }".to_string()]
        );
    }

    #[test]
    fn another_computer_taking_the_devices_is_not_fought_over() {
        struct Taken(Mutex<Vec<String>>);
        impl Port for Taken {
            fn session(&self, _h: &str, op: SessionOp, _id: u64) -> Result<Option<SessionState>> {
                self.0.lock().unwrap().push(format!("{op:?}"));
                Ok(Some(match op {
                    SessionOp::Open { .. } => SessionState {
                        ok: true,
                        open: true,
                        mouse: true,
                        keyboard: true,
                        err: None,
                    },
                    _ => SessionState {
                        err: Some("vinput_not_owner".into()),
                        ..Default::default()
                    },
                }))
            }
            fn report(&self, _h: &str, _id: u64, _r: &[Report]) -> Result<Option<ReportAck>> {
                Ok(None)
            }
        }
        let port = Arc::new(Taken(Mutex::new(vec![])));
        let t0 = Instant::now();
        let mut l = LinkState::new("h", port.clone(), t0);
        let (st, seen) = sink();
        l.on_cmd(want_cmd(P1, true, true, &st), t0);
        l.tick(t0 + vinput::HEARTBEAT);
        assert_eq!(seen.lock().unwrap().last().unwrap()["err"], "vinput_taken");
        port.0.lock().unwrap().clear();
        l.on_cmd(want_cmd(P1, true, true, &st), t0 + vinput::HEARTBEAT);
        assert!(
            port.0.lock().unwrap().is_empty(),
            "not opened again by itself"
        );
    }

    #[test]
    fn a_console_is_one_link_with_or_without_its_port() {
        assert_eq!(host_key("10.0.0.2:9120"), "10.0.0.2");
        assert_eq!(host_key("10.0.0.2"), "10.0.0.2");
        assert_eq!(host_key("[fe80::1]:9120"), "[fe80::1]");
        assert_eq!(host_key("fe80::1"), "fe80::1");
    }

    #[test]
    fn only_this_machine_may_send_input_without_a_stream() {
        assert!(local_allowed("127.0.0.1".parse().unwrap()));
        assert!(local_allowed("::1".parse().unwrap()));
        assert!(!local_allowed("192.168.1.20".parse().unwrap()));
    }

    #[test]
    fn a_kbm_message_with_nothing_wanted_is_off() {
        let m: KbmMsg = serde_json::from_str(r#"{"on":true}"#).unwrap();
        assert!(m.on && !m.mouse && !m.keyboard);
    }
}
