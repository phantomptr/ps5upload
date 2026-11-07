//! Rooms and the `/ws` protocol (v1, `PROTOCOL.md`).
//!
//! One task per WebSocket. Each owns its socket and a bounded queue other tasks push messages
//! into; the room table is one mutex held only for map updates (never across an await). A peer
//! whose queue is full is too slow to read and is dropped rather than buffered.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State as AxState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Notify};

use crate::config::Config;
use crate::limits::{canonical_ip, IpKey, RateTable, Window};
use crate::metrics::{ErrorKind, Gauges, Metrics};
use crate::{Field, Shared};

pub const PROTOCOL_VERSION: u64 = 1;
pub const ROOM_LEN: usize = 22;
pub const SECRET_LEN: usize = 43;
pub const GID_LEN: usize = 12;
/// Messages queued for one peer before it counts as too slow.
const PEER_QUEUE: usize = 64;
/// The hard frame cap given to the WebSocket layer. Frames between the protocol limit and this
/// get a `too_large` error; larger ones are cut off by the WebSocket layer itself.
const HARD_FRAME_FACTOR: usize = 4;

/// WebSocket close codes the hub uses (4000-4999 are for applications).
pub const CLOSE_PROTOCOL: u16 = 4000;
pub const CLOSE_REPLACED: u16 = 4001;
pub const CLOSE_DONE: u16 = 1000;
pub const CLOSE_GOING_AWAY: u16 = 1001;

pub(crate) struct State {
    rooms: HashMap<String, Room>,
    rooms_per_ip: HashMap<IpKey, usize>,
    joins: RateTable,
}

impl State {
    pub(crate) fn new(c: &Config) -> Self {
        Self {
            rooms: HashMap::new(),
            rooms_per_ip: HashMap::new(),
            joins: RateTable::new(c.max_tracked_ips),
        }
    }

    fn remove_room(&mut self, id: &str) -> Option<Room> {
        let room = self.rooms.remove(id)?;
        if let Some(n) = self.rooms_per_ip.get_mut(&room.owner) {
            *n -= 1;
            if *n == 0 {
                self.rooms_per_ip.remove(&room.owner);
            }
        }
        Some(room)
    }
}

struct Room {
    secret_hash: [u8; 32],
    /// Who opened it: counts toward that client's room limit until the room is removed.
    owner: IpKey,
    host: Option<Peer>,
    host_signals: Window,
    hostless_since: Option<Instant>,
    guests: HashMap<String, Guest>,
}

struct Guest {
    peer: Peer,
    signals: Window,
}

#[derive(Clone)]
struct Peer {
    conn: u64,
    tx: mpsc::Sender<Out>,
    kill: Arc<Notify>,
}

impl Peer {
    /// Queues a message; a peer that cannot take it is dropped.
    fn send(&self, out: Out, m: &Metrics) {
        if self.tx.try_send(out).is_err() {
            m.error(ErrorKind::SlowPeer);
            self.kill.notify_one();
        }
    }
}

enum Out {
    Text(String),
    Close(u16, &'static str),
}

enum Role {
    None,
    Host { room: String },
    Guest { room: String, gid: String },
}

/// What a handled message asks the connection to do.
enum Action {
    Reply(Vec<String>),
    ReplyThenClose(Vec<String>),
}

pub(crate) fn gauges(shared: &Shared) -> Gauges {
    let st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    Gauges {
        rooms: st.rooms.len(),
        hosted_rooms: st.rooms.values().filter(|r| r.host.is_some()).count(),
        guests: st.rooms.values().map(|r| r.guests.len()).sum(),
        connections: shared.connections.load(Ordering::Relaxed),
    }
}

pub(crate) fn router(shared: Arc<Shared>, with_metrics: bool) -> Router {
    let r = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(|| async { "ok\n" }));
    let r = if with_metrics {
        r.route("/metrics", get(metrics_handler))
    } else {
        r
    };
    r.with_state(shared)
}

/// The private metrics listener's routes.
pub(crate) fn metrics_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok\n" }))
        .route("/metrics", get(metrics_handler))
        .with_state(shared)
}

async fn metrics_handler(AxState(shared): AxState<Arc<Shared>>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        shared.metrics_text(),
    )
        .into_response()
}

/// The client's address: the TCP peer, or the trusted proxy's header when the peer is a trusted
/// proxy (its last entry, the one the proxy itself added).
pub fn client_ip(c: &Config, peer: IpAddr, headers: &HeaderMap) -> IpAddr {
    let peer = canonical_ip(peer);
    if !c.trusted_proxies.iter().any(|p| p.contains(peer)) {
        return peer;
    }
    headers
        .get(c.client_ip_header.as_str())
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit(',').next())
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
        .map(canonical_ip)
        .unwrap_or(peer)
}

async fn ws_handler(
    AxState(shared): AxState<Arc<Shared>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(origin) = headers.get(header::ORIGIN) {
        let ok = origin
            .to_str()
            .map(|o| shared.config.allowed_origins.iter().any(|a| a == o))
            .unwrap_or(false);
        if !ok {
            shared.metrics.error(ErrorKind::BadOrigin);
            return (StatusCode::FORBIDDEN, "origin not allowed\n").into_response();
        }
    }
    let ip = IpKey::of(client_ip(&shared.config, peer.ip(), &headers));
    let hard = shared.config.max_frame_bytes * HARD_FRAME_FACTOR;
    ws.max_message_size(hard)
        .max_frame_size(hard)
        .on_upgrade(move |socket| connection(socket, shared, ip))
}

fn random_b64(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).expect("OS randomness");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn is_b64url(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn error_text(kind: ErrorKind) -> String {
    json!({"t": "error", "why": kind.as_str()}).to_string()
}

fn close_msg(code: u16, reason: &'static str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

async fn connection(socket: WebSocket, shared: Arc<Shared>, ip: IpKey) {
    shared.connections.fetch_add(1, Ordering::Relaxed);
    let conn = shared.next_conn.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Out>(PEER_QUEUE);
    let kill = Arc::new(Notify::new());
    let me = Peer {
        conn,
        tx,
        kill: kill.clone(),
    };
    let mut role = Role::None;
    let mut last_seen = Instant::now();
    let cfg = &shared.config;
    let mut ping = tokio::time::interval_at(
        tokio::time::Instant::now() + cfg.ping_interval,
        cfg.ping_interval,
    );
    // A peer that never says anything still has to be checked before the first ping is due.
    let mut shutdown = shared.shutdown.subscribe();

    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { break };
                last_seen = Instant::now();
                let res = match msg {
                    Message::Text(t) => handle_text(&shared, &mut role, &me, ip, t.as_str()),
                    Message::Binary(_) => Err(ErrorKind::BadFrame),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_) => break,
                };
                match res {
                    Ok(Action::Reply(lines)) => {
                        if send_all(&mut sink, lines).await.is_err() {
                            break;
                        }
                    }
                    Ok(Action::ReplyThenClose(lines)) => {
                        let _ = send_all(&mut sink, lines).await;
                        let _ = sink.send(close_msg(CLOSE_DONE, "bye")).await;
                        break;
                    }
                    Err(kind) => {
                        shared.metrics.error(kind);
                        if kind == ErrorKind::NoPeer {
                            if sink.send(Message::Text(error_text(kind).into())).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        shared.log("protocol_error", &[("kind", Field::S(kind.as_str()))]);
                        let _ = sink.send(Message::Text(error_text(kind).into())).await;
                        let _ = sink.send(close_msg(CLOSE_PROTOCOL, kind.as_str())).await;
                        break;
                    }
                }
            }
            out = rx.recv() => match out {
                Some(Out::Text(s)) => {
                    if sink.send(Message::Text(s.into())).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close(code, reason)) => {
                    let _ = sink.send(close_msg(code, reason)).await;
                    break;
                }
                None => break,
            },
            _ = kill.notified() => break,
            _ = ping.tick() => {
                if last_seen.elapsed() >= cfg.peer_timeout {
                    shared.metrics.error(ErrorKind::Timeout);
                    let _ = sink.send(close_msg(CLOSE_GOING_AWAY, "timeout")).await;
                    break;
                }
                if sink.send(Message::Ping(Default::default())).await.is_err() {
                    break;
                }
            }
            _ = stopped(&mut shutdown) => {
                let _ = sink.send(close_msg(CLOSE_GOING_AWAY, "shutdown")).await;
                break;
            }
        }
    }
    let _ = sink.close().await;
    disconnect(&shared, role, conn);
    shared.connections.fetch_sub(1, Ordering::Relaxed);
}

async fn send_all(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    lines: Vec<String>,
) -> Result<(), axum::Error> {
    for l in lines {
        sink.send(Message::Text(l.into())).await?;
    }
    Ok(())
}

/// The socket is gone: a host leaves its room hostless (its guests are closed), a guest is
/// removed and its host told.
fn disconnect(shared: &Shared, role: Role, conn: u64) {
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    match role {
        Role::None => {}
        Role::Host { room } => {
            let Some(r) = st.rooms.get_mut(&room) else {
                return;
            };
            // A reclaim already put a new host in.
            if r.host.as_ref().map(|h| h.conn) != Some(conn) {
                return;
            }
            r.host = None;
            r.hostless_since = Some(Instant::now());
            for (_, g) in r.guests.drain() {
                close_guest(&g.peer, &shared.metrics);
            }
        }
        Role::Guest { room, gid } => {
            let Some(r) = st.rooms.get_mut(&room) else {
                return;
            };
            if r.guests.get(&gid).map(|g| g.peer.conn) != Some(conn) {
                return;
            }
            r.guests.remove(&gid);
            if let Some(h) = &r.host {
                h.send(
                    Out::Text(json!({"t": "left", "gid": gid}).to_string()),
                    &shared.metrics,
                );
            }
        }
    }
}

fn close_guest(p: &Peer, m: &Metrics) {
    p.send(Out::Text(json!({"t": "closed"}).to_string()), m);
    p.send(Out::Close(CLOSE_DONE, "closed"), m);
}

fn str_field<'a>(v: &'a Value, k: &str) -> Result<Option<&'a str>, ErrorKind> {
    match v.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ErrorKind::BadJson),
    }
}

fn handle_text(
    shared: &Shared,
    role: &mut Role,
    me: &Peer,
    ip: IpKey,
    text: &str,
) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    if text.len() > cfg.max_frame_bytes {
        return Err(ErrorKind::TooLarge);
    }
    let msg: Value = serde_json::from_str(text).map_err(|_| ErrorKind::BadJson)?;
    if !msg.is_object() {
        return Err(ErrorKind::BadJson);
    }
    let t = str_field(&msg, "t")?.ok_or(ErrorKind::BadJson)?;
    let v = msg.get("v");
    match (t, v) {
        ("host" | "join", None) => return Err(ErrorKind::BadVersion),
        (_, Some(v)) if v.as_u64() != Some(PROTOCOL_VERSION) => return Err(ErrorKind::BadVersion),
        _ => {}
    }
    match t {
        "host" => on_host(shared, role, me, ip, &msg),
        "join" => on_join(shared, role, me, ip, &msg),
        "signal" => on_signal(shared, role, &msg),
        "leave" => on_leave(shared, role, me, &msg),
        _ => Err(ErrorKind::UnknownType),
    }
}

fn on_host(
    shared: &Shared,
    role: &mut Role,
    me: &Peer,
    ip: IpKey,
    msg: &Value,
) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    if !matches!(role, Role::None) {
        return Err(ErrorKind::BadState);
    }
    let room_id = str_field(msg, "room")?.unwrap_or_default();
    if !is_b64url(room_id, ROOM_LEN) {
        return Err(ErrorKind::BadRoom);
    }
    let secret = str_field(msg, "secret")?.unwrap_or_default();
    if !is_b64url(secret, SECRET_LEN) {
        return Err(ErrorKind::BadSecret);
    }
    let hash = hash_secret(secret);
    let now = Instant::now();
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let mut replies = vec![json!({"t": "hosted", "room": room_id}).to_string()];
    if let Some(room) = st.rooms.get_mut(room_id) {
        if !ct_eq(&room.secret_hash, &hash) {
            return Err(ErrorKind::RoomTaken);
        }
        if let Some(old) = room.host.replace(me.clone()) {
            old.send(Out::Close(CLOSE_REPLACED, "replaced"), &shared.metrics);
        }
        room.hostless_since = None;
        // The new socket learns about guests still waiting from the old one.
        for gid in room.guests.keys() {
            replies.push(json!({"t": "guest", "gid": gid}).to_string());
        }
        Metrics::inc(&shared.metrics.reclaims);
    } else {
        if st.rooms.len() >= cfg.max_rooms {
            return Err(ErrorKind::ServerFull);
        }
        let owned = st.rooms_per_ip.get(&ip).copied().unwrap_or(0);
        if owned >= cfg.max_rooms_per_ip {
            return Err(ErrorKind::TooManyRooms);
        }
        *st.rooms_per_ip.entry(ip).or_insert(0) += 1;
        st.rooms.insert(
            room_id.to_string(),
            Room {
                secret_hash: hash,
                owner: ip,
                host: Some(me.clone()),
                host_signals: Window::new(now),
                hostless_since: None,
                guests: HashMap::new(),
            },
        );
        Metrics::inc(&shared.metrics.hosts);
    }
    *role = Role::Host {
        room: room_id.to_string(),
    };
    Ok(Action::Reply(replies))
}

fn on_join(
    shared: &Shared,
    role: &mut Role,
    me: &Peer,
    ip: IpKey,
    msg: &Value,
) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    if !matches!(role, Role::None) {
        return Err(ErrorKind::BadState);
    }
    let now = Instant::now();
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    // Every attempt counts, found or not: this is what makes guessing room ids pointless.
    if !st.joins.hit(ip, now, cfg.rate_window, cfg.max_joins_per_ip) {
        return Err(ErrorKind::RateLimited);
    }
    let room_id = str_field(msg, "room")?.unwrap_or_default();
    if !is_b64url(room_id, ROOM_LEN) {
        return Err(ErrorKind::BadRoom);
    }
    let Some(room) = st.rooms.get_mut(room_id) else {
        return Err(ErrorKind::NoRoom);
    };
    let Some(host) = room.host.clone() else {
        return Err(ErrorKind::NoRoom);
    };
    if room.guests.len() >= cfg.max_guests_per_room {
        return Err(ErrorKind::RoomFull);
    }
    let gid = loop {
        let g = random_b64(GID_LEN * 3 / 4);
        if !room.guests.contains_key(&g) {
            break g;
        }
    };
    room.guests.insert(
        gid.clone(),
        Guest {
            peer: me.clone(),
            signals: Window::new(now),
        },
    );
    host.send(
        Out::Text(json!({"t": "guest", "gid": gid}).to_string()),
        &shared.metrics,
    );
    Metrics::inc(&shared.metrics.joins);
    let reply = json!({"t": "joined", "gid": gid}).to_string();
    *role = Role::Guest {
        room: room_id.to_string(),
        gid,
    };
    Ok(Action::Reply(vec![reply]))
}

fn on_signal(shared: &Shared, role: &mut Role, msg: &Value) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    let to = str_field(msg, "to")?.ok_or(ErrorKind::BadTarget)?;
    let data = msg.get("data").ok_or(ErrorKind::BadData)?;
    if !data.is_object() {
        return Err(ErrorKind::BadData);
    }
    let data_text = data.to_string();
    if data_text.len() > cfg.max_signal_data_bytes {
        return Err(ErrorKind::BadData);
    }
    let now = Instant::now();
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    match role {
        Role::None => Err(ErrorKind::BadState),
        Role::Guest { room, gid } => {
            if to != "host" {
                return Err(ErrorKind::BadTarget);
            }
            let Some(r) = st.rooms.get_mut(room.as_str()) else {
                return Err(ErrorKind::NoPeer);
            };
            let Some(g) = r.guests.get_mut(gid.as_str()) else {
                return Err(ErrorKind::NoPeer);
            };
            if !g
                .signals
                .hit(now, cfg.rate_window, cfg.max_signals_per_guest)
            {
                return Err(ErrorKind::RateLimited);
            }
            let Some(h) = &r.host else {
                return Err(ErrorKind::NoPeer);
            };
            h.send(Out::Text(forward(gid, &data_text)), &shared.metrics);
            Metrics::inc(&shared.metrics.signals);
            Ok(Action::Reply(Vec::new()))
        }
        Role::Host { room } => {
            if !is_b64url(to, GID_LEN) {
                return Err(ErrorKind::BadTarget);
            }
            let Some(r) = st.rooms.get_mut(room.as_str()) else {
                return Err(ErrorKind::NoPeer);
            };
            if !r
                .host_signals
                .hit(now, cfg.rate_window, cfg.max_signals_per_host)
            {
                return Err(ErrorKind::RateLimited);
            }
            let Some(g) = r.guests.get(to) else {
                return Err(ErrorKind::NoPeer);
            };
            g.peer
                .send(Out::Text(forward("host", &data_text)), &shared.metrics);
            Metrics::inc(&shared.metrics.signals);
            Ok(Action::Reply(Vec::new()))
        }
    }
}

/// `{"t":"signal","from":<from>,"data":<data>}`, with `data` spliced in as the JSON it already is.
fn forward(from: &str, data_json: &str) -> String {
    format!(
        "{{\"t\":\"signal\",\"from\":{},\"data\":{}}}",
        Value::String(from.to_string()),
        data_json
    )
}

fn on_leave(shared: &Shared, role: &mut Role, me: &Peer, msg: &Value) -> Result<Action, ErrorKind> {
    let gid_arg = str_field(msg, "gid")?;
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    match std::mem::replace(role, Role::None) {
        Role::None => Err(ErrorKind::BadState),
        Role::Guest { room, gid } => {
            if let Some(r) = st.rooms.get_mut(&room) {
                if r.guests.get(&gid).map(|g| g.peer.conn) == Some(me.conn) {
                    r.guests.remove(&gid);
                    if let Some(h) = &r.host {
                        h.send(
                            Out::Text(json!({"t": "left", "gid": gid}).to_string()),
                            &shared.metrics,
                        );
                    }
                }
            }
            Ok(Action::ReplyThenClose(Vec::new()))
        }
        Role::Host { room } => {
            let is_host = st
                .rooms
                .get(&room)
                .and_then(|r| r.host.as_ref())
                .map(|h| h.conn)
                == Some(me.conn);
            if !is_host {
                return Ok(Action::ReplyThenClose(Vec::new()));
            }
            match gid_arg {
                // Kick one guest; the host stays.
                Some(gid) => {
                    *role = Role::Host { room: room.clone() };
                    let r = st.rooms.get_mut(&room).expect("checked above");
                    match r.guests.remove(gid) {
                        Some(g) => {
                            close_guest(&g.peer, &shared.metrics);
                            Ok(Action::Reply(Vec::new()))
                        }
                        None => Err(ErrorKind::NoPeer),
                    }
                }
                // Close the room for good.
                None => {
                    if let Some(r) = st.remove_room(&room) {
                        for g in r.guests.values() {
                            close_guest(&g.peer, &shared.metrics);
                        }
                    }
                    Ok(Action::ReplyThenClose(Vec::new()))
                }
            }
        }
    }
}

/// Removes rooms whose host stayed away past the idle limit, forgets expired rate counters and
/// logs a count summary now and then.
pub(crate) async fn sweeper(shared: Arc<Shared>, stun_table: Arc<Mutex<RateTable>>) {
    let cfg = &shared.config;
    let mut tick = tokio::time::interval(cfg.sweep_interval);
    let mut shutdown = shared.shutdown.subscribe();
    let mut last_summary = Instant::now();
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = stopped(&mut shutdown) => return,
        }
        let now = Instant::now();
        {
            let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let expired: Vec<String> = st
                .rooms
                .iter()
                .filter(|(_, r)| {
                    r.hostless_since
                        .is_some_and(|t| now.duration_since(t) >= cfg.room_idle_ttl)
                })
                .map(|(k, _)| k.clone())
                .collect();
            for id in expired {
                st.remove_room(&id);
                Metrics::inc(&shared.metrics.rooms_expired);
            }
            st.joins.sweep(now, cfg.rate_window);
        }
        stun_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sweep(now, cfg.rate_window);
        if !cfg.summary_interval.is_zero() && last_summary.elapsed() >= cfg.summary_interval {
            last_summary = now;
            let g = gauges(&shared);
            let m = &shared.metrics;
            shared.log(
                "summary",
                &[
                    ("rooms", Field::N(g.rooms as u64)),
                    ("guests", Field::N(g.guests as u64)),
                    ("connections", Field::N(g.connections as u64)),
                    ("joins", Field::N(m.joins.load(Ordering::Relaxed))),
                    ("signals", Field::N(m.signals.load(Ordering::Relaxed))),
                    ("stun", Field::N(m.stun_requests.load(Ordering::Relaxed))),
                ],
            );
        }
    }
}

/// Resolves once shutdown is asked for (holds no borrow of the channel across the await).
async fn stopped(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}
