//! Rooms and the `/ws` protocol (v1, `PROTOCOL.md`).
//!
//! One task per WebSocket. Each owns its socket and a bounded queue other tasks push messages
//! into; the room table is one mutex held only for map updates (never across an await). A peer
//! whose queue is full is too slow to read and is dropped rather than buffered.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State as AxState};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::value::RawValue;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::BTreeMap;
use tokio::sync::{mpsc, Notify};

use crate::config::Config;
use crate::limits::{canonical_ip, v6_site, IpKey, RateTable, Window};
use crate::metrics::{ErrorKind, Gauges, Limit, Metrics, Stray};
use crate::{Field, Shared};

pub const PROTOCOL_VERSION: u64 = 1;
pub const ROOM_LEN: usize = 22;
pub const SECRET_LEN: usize = 43;
pub const GID_LEN: usize = 12;
/// The last character of a room id made from its host's secret ([`room_for_secret`]). A room id
/// of 16 random bytes (what older apps make) never ends in it: the last of its 22 base64url
/// characters holds 2 bits only, so it is always `A`, `Q`, `g` or `w`.
pub const BOUND_ROOM_MARK: char = 'h';
/// What a bound room id hashes before the secret: the id is never a prefix of the hash the hub
/// keeps for reclaims.
const BOUND_ROOM_DOMAIN: &[u8] = b"ps5upload-hub room\0";

/// The room id a host's secret gives: 21 base64url characters of a SHA-256 of the secret (126
/// bits), then [`BOUND_ROOM_MARK`]. The hub opens a room with an id of this form only for the
/// secret it was made from, so a link holder cannot open it with a secret of their own, also
/// after a hub restart (when the hub has no rooms at all): the id is the proof, nothing is stored.
pub fn room_for_secret(secret: &str) -> String {
    let h = Sha256::new()
        .chain_update(BOUND_ROOM_DOMAIN)
        .chain_update(secret.as_bytes())
        .finalize();
    let mut id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h);
    id.truncate(ROOM_LEN - 1);
    id.push(BOUND_ROOM_MARK);
    id
}

/// A room id of the bound form ([`room_for_secret`]): only its own secret may host it.
pub fn is_bound_room(room: &str) -> bool {
    room.len() == ROOM_LEN && room.ends_with(BOUND_ROOM_MARK)
}
/// Messages queued for one peer before it counts as too slow.
const PEER_QUEUE: usize = 64;
/// The hard frame cap given to the WebSocket layer. Frames between the protocol limit and this
/// get a `too_large` error; larger ones are cut off by the WebSocket layer itself.
const HARD_FRAME_FACTOR: usize = 2;
/// The WebSocket layer's read buffer. Its default (128 KiB, allocated and touched for every
/// socket) made an idle connection cost about 140 KiB, so 2,000 of them would not fit the 256 MB
/// machine. Signalling messages are small; a larger frame just takes several reads.
const READ_BUFFER: usize = 4 * 1024;
/// A peer that does not take a frame for this long (it stopped reading) is dropped, so a socket
/// that never reads cannot hold its task, and its connection slot, forever.
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// WebSocket close codes the hub uses (4000-4999 are for applications).
pub const CLOSE_PROTOCOL: u16 = 4000;
pub const CLOSE_REPLACED: u16 = 4001;
pub const CLOSE_DONE: u16 = 1000;
pub const CLOSE_GOING_AWAY: u16 = 1001;

pub(crate) struct State {
    rooms: HashMap<String, Room>,
    rooms_per_ip: HashMap<IpKey, usize>,
    conns_per_ip: HashMap<IpKey, usize>,
    /// Open connections per IPv6 /48 (see [`v6_site`]).
    conns_per_v6_site: HashMap<u64, usize>,
    joins: RateTable,
}

impl State {
    pub(crate) fn new(c: &Config) -> Self {
        Self {
            rooms: HashMap::new(),
            rooms_per_ip: HashMap::new(),
            conns_per_ip: HashMap::new(),
            conns_per_v6_site: HashMap::new(),
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
    /// Each guest client's signals in this room ([`Config::max_signals_per_guest_client`]),
    /// kept when its guest goes so that joining again does not refill them. Bounded: at most
    /// [`GUEST_CLIENTS_PER_SEAT`] per seat, and windows that ran out are dropped.
    guest_clients: HashMap<IpKey, Window>,
}

/// Guest clients a room keeps a signal budget for, per seat (8 seats: 32 clients a minute).
const GUEST_CLIENTS_PER_SEAT: usize = 4;

struct Guest {
    peer: Peer,
    signals: Window,
    /// When it joined: a guest still here [`Config::guest_max_age`] later is removed.
    joined: Instant,
    /// Who it is (the address [`client_ip`] gave): its signals count toward that client's
    /// budget in the room too.
    client: IpKey,
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

/// What `/robots.txt` says: nothing here is for crawlers.
pub const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /\n";

/// The public routes. `/ws/` is `/ws` (a hub address typed with a slash at the end). Everything
/// else answers 404 and is counted by the class of its path ([`Stray`]), never logged: the hub's
/// name is public (certificate logs list it), so scanners and crawlers ask for `/`, `/.env` and
/// the like all day. `/robots.txt` tells well-behaved crawlers to stay away.
pub(crate) fn router(shared: Arc<Shared>, with_metrics: bool) -> Router {
    let r = Router::new()
        // Every method reaches the handler, so a request that is not a WebSocket upgrade is
        // counted; it gets the same answer as before (405, 400 or 426).
        .route("/ws", any(ws_handler))
        .route("/ws/", any(ws_handler))
        .route("/health", get(|| async { "ok\n" }))
        .route("/robots.txt", get(robots_handler));
    let r = if with_metrics {
        r.route("/metrics", get(metrics_handler))
    } else {
        r
    };
    r.fallback(not_found).with_state(shared)
}

async fn robots_handler(AxState(shared): AxState<Arc<Shared>>) -> Response {
    shared.metrics.stray(Stray::Robots);
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        ROBOTS_TXT,
    )
        .into_response()
}

async fn not_found(AxState(shared): AxState<Arc<Shared>>, uri: Uri) -> Response {
    shared.metrics.stray(Stray::of_path(uri.path()));
    (StatusCode::NOT_FOUND, "not found\n").into_response()
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
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => {
            shared.metrics.stray(Stray::WsNotWebSocket);
            return rejection.into_response();
        }
    };
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
    let client = client_ip(&shared.config, peer.ip(), &headers);
    let slot = match ConnSlot::take(&shared, IpKey::of(client), v6_site(client)) {
        Ok(s) => s,
        Err(kind) => {
            shared.metrics.error(kind);
            let status = if kind == ErrorKind::ServerFull {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::TOO_MANY_REQUESTS
            };
            return (status, "too many connections\n").into_response();
        }
    };
    let hard = shared.config.max_frame_bytes * HARD_FRAME_FACTOR;
    // The slot moves into the callback: it is given back when the connection ends, or when the
    // upgrade fails and the callback is dropped without running.
    ws.max_message_size(hard)
        .max_frame_size(hard)
        .read_buffer_size(READ_BUFFER)
        .on_upgrade(move |socket| connection(socket, slot))
}

/// One open connection, counted against the hub's and its client's limits until dropped.
struct ConnSlot {
    shared: Arc<Shared>,
    ip: IpKey,
    site: Option<u64>,
}

impl ConnSlot {
    fn take(shared: &Arc<Shared>, ip: IpKey, site: Option<u64>) -> Result<ConnSlot, ErrorKind> {
        let cfg = &shared.config;
        let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let m = &shared.metrics;
        if shared.connections.load(Ordering::Relaxed) >= cfg.max_connections {
            m.limited(Limit::Connections);
            return Err(ErrorKind::ServerFull);
        }
        // Both limits are checked before either count moves, so a refusal leaves no entry.
        if st.conns_per_ip.get(&ip).copied().unwrap_or(0) >= cfg.max_connections_per_ip {
            m.limited(Limit::ConnectionsPerClient);
            return Err(ErrorKind::TooManyConnections);
        }
        if let Some(site) = site {
            let open = st.conns_per_v6_site.get(&site).copied().unwrap_or(0);
            if open >= cfg.max_connections_per_v6_site {
                m.limited(Limit::ConnectionsPerV6Site);
                return Err(ErrorKind::TooManyConnections);
            }
            *st.conns_per_v6_site.entry(site).or_insert(0) += 1;
        }
        *st.conns_per_ip.entry(ip).or_insert(0) += 1;
        shared.connections.fetch_add(1, Ordering::Relaxed);
        Ok(ConnSlot {
            shared: shared.clone(),
            ip,
            site,
        })
    }
}

impl Drop for ConnSlot {
    fn drop(&mut self) {
        let mut st = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = st.conns_per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                st.conns_per_ip.remove(&self.ip);
            }
        }
        if let Some(site) = self.site {
            if let Some(n) = st.conns_per_v6_site.get_mut(&site) {
                *n -= 1;
                if *n == 0 {
                    st.conns_per_v6_site.remove(&site);
                }
            }
        }
        self.shared.connections.fetch_sub(1, Ordering::Relaxed);
    }
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

type Sink = futures_util::stream::SplitSink<WebSocket, Message>;

/// Sends one frame, giving up after [`SEND_TIMEOUT`] (a peer that stopped reading).
async fn send_frame(sink: &mut Sink, m: Message) -> Result<(), ()> {
    match tokio::time::timeout(SEND_TIMEOUT, sink.send(m)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

async fn send_all(sink: &mut Sink, lines: Vec<String>) -> Result<(), ()> {
    for l in lines {
        send_frame(sink, Message::Text(l.into())).await?;
    }
    Ok(())
}

async fn connection(socket: WebSocket, slot: ConnSlot) {
    let shared = slot.shared.clone();
    let ip = slot.ip;
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
    let mut shutdown = shared.shutdown.subscribe();
    // A socket must take a role soon: an idle one would otherwise hold a connection slot for as
    // long as it answers pings.
    let hello = tokio::time::sleep(cfg.hello_timeout);
    tokio::pin!(hello);

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
                        let _ = send_frame(&mut sink, close_msg(CLOSE_DONE, "bye")).await;
                        break;
                    }
                    // Counted in the metrics, not logged: any client can send garbage, and a log
                    // line for each would be volume nobody reads.
                    Err(kind) => {
                        shared.metrics.error(kind);
                        let text = Message::Text(error_text(kind).into());
                        if kind == ErrorKind::NoPeer {
                            if send_frame(&mut sink, text).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        let _ = send_frame(&mut sink, text).await;
                        let _ = send_frame(&mut sink, close_msg(CLOSE_PROTOCOL, kind.as_str())).await;
                        break;
                    }
                }
            }
            out = rx.recv() => match out {
                Some(Out::Text(s)) => {
                    if send_frame(&mut sink, Message::Text(s.into())).await.is_err() {
                        break;
                    }
                }
                Some(Out::Close(code, reason)) => {
                    let _ = send_frame(&mut sink, close_msg(code, reason)).await;
                    break;
                }
                None => break,
            },
            _ = kill.notified() => break,
            _ = ping.tick() => {
                let quiet = last_seen.elapsed();
                if quiet >= cfg.peer_timeout {
                    shared.metrics.error(ErrorKind::Timeout);
                    let _ = send_frame(&mut sink, close_msg(CLOSE_GOING_AWAY, "timeout")).await;
                    break;
                }
                // Heard from within the interval (the engine pings the hub every 20 s): no ping
                // needed, one frame fewer each way.
                if quiet >= cfg.ping_interval
                    && send_frame(&mut sink, Message::Ping(Default::default())).await.is_err()
                {
                    break;
                }
            }
            _ = &mut hello, if matches!(role, Role::None) => {
                shared.metrics.error(ErrorKind::Timeout);
                let _ = send_frame(&mut sink, close_msg(CLOSE_PROTOCOL, "timeout")).await;
                break;
            }
            _ = stopped(&mut shutdown) => {
                let _ = send_frame(&mut sink, close_msg(CLOSE_GOING_AWAY, "shutdown")).await;
                break;
            }
        }
    }
    let _ = tokio::time::timeout(SEND_TIMEOUT, sink.close()).await;
    disconnect(&shared, role, conn);
    drop(slot);
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
            // On shutdown every guest's own task closes it as going away (1001), so it comes
            // back; closing it here as done (1000) would race that and tell it the room ended.
            if *shared.shutdown.borrow() {
                return;
            }
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

/// A message's top-level fields, each as the JSON text it came as. Nothing under them is parsed
/// (a signal's `data`, nearly all of its bytes, is checked to be an object and goes on as it
/// came): before, the whole message became a tree of values and `data` was written out again,
/// which cost about 14 µs for a 5 KB offer against about 3 µs now (`forward_cost` below). A field
/// that comes twice counts as the last one, as before.
type Msg<'a> = BTreeMap<String, &'a RawValue>;

/// A string field: None when absent, `bad_json` when it is not a string (null included).
fn str_field<'a>(m: &Msg<'a>, k: &str) -> Result<Option<Cow<'a, str>>, ErrorKind> {
    let Some(raw) = m.get(k) else {
        return Ok(None);
    };
    let t: &'a str = raw.get();
    // A plain string (no escape) is its text between the quotes: no copy.
    if let Some(inner) = t.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        if !inner.contains(['\\', '"']) {
            return Ok(Some(Cow::Borrowed(inner)));
        }
    }
    serde_json::from_str::<String>(t)
        .map(|s| Some(Cow::Owned(s)))
        .map_err(|_| ErrorKind::BadJson)
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
    // Not an object (an array, a string, a number): no map, `bad_json` as before.
    let msg: Msg = serde_json::from_str(text).map_err(|_| ErrorKind::BadJson)?;
    let t = str_field(&msg, "t")?.ok_or(ErrorKind::BadJson)?;
    let t: &str = &t;
    let v = msg.get("v").map(|raw| {
        serde_json::from_str::<Value>(raw.get())
            .ok()
            .and_then(|v| v.as_u64())
    });
    match (t, v) {
        ("host" | "join", None) => return Err(ErrorKind::BadVersion),
        (_, Some(v)) if v != Some(PROTOCOL_VERSION) => return Err(ErrorKind::BadVersion),
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
    msg: &Msg,
) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    if !matches!(role, Role::None) {
        return Err(ErrorKind::BadState);
    }
    let room_id = str_field(msg, "room")?.unwrap_or_default();
    let room_id: &str = &room_id;
    if !is_b64url(room_id, ROOM_LEN) {
        return Err(ErrorKind::BadRoom);
    }
    let secret = str_field(msg, "secret")?.unwrap_or_default();
    let secret: &str = &secret;
    if !is_b64url(secret, SECRET_LEN) {
        return Err(ErrorKind::BadSecret);
    }
    // A bound id opens for its own secret only. Before, the first `host` for an id after a hub
    // restart took it with any secret: anyone with the link could open the room as theirs, its
    // host got `room_taken` from then on, and the guests' offers went to them.
    if is_bound_room(room_id) && room_for_secret(secret) != room_id {
        return Err(ErrorKind::RoomTaken);
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
            shared.metrics.limited(Limit::Rooms);
            return Err(ErrorKind::ServerFull);
        }
        let owned = st.rooms_per_ip.get(&ip).copied().unwrap_or(0);
        if owned >= cfg.max_rooms_per_ip {
            shared.metrics.limited(Limit::RoomsPerClient);
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
                guest_clients: HashMap::new(),
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
    msg: &Msg,
) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    if !matches!(role, Role::None) {
        return Err(ErrorKind::BadState);
    }
    let now = Instant::now();
    let mut st = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    // Every attempt counts, found or not: this is what makes guessing room ids pointless.
    if !st.joins.hit(ip, now, cfg.rate_window, cfg.max_joins_per_ip) {
        shared.metrics.limited(Limit::JoinRate);
        return Err(ErrorKind::RateLimited);
    }
    let room_id = str_field(msg, "room")?.unwrap_or_default();
    let room_id: &str = &room_id;
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
    // The client's signal budget here outlives its guests: before, each join came with a fresh
    // one, so one client joining again and again (30 times a minute) sent the host 600 signals
    // a minute where one guest may send 20. A client that has used it up is refused now rather
    // than at its first signal. The table holds clients that joined within the window, at most
    // a few per seat: past that, a new client waits for windows to run out.
    let cap = cfg
        .max_guests_per_room
        .saturating_mul(GUEST_CLIENTS_PER_SEAT);
    if !room.guest_clients.contains_key(&ip) && room.guest_clients.len() >= cap {
        room.guest_clients
            .retain(|_, w| !w.expired(now, cfg.rate_window));
        if room.guest_clients.len() >= cap {
            shared.metrics.limited(Limit::RoomClients);
            return Err(ErrorKind::RateLimited);
        }
    }
    let budget = room
        .guest_clients
        .entry(ip)
        .or_insert_with(|| Window::new(now));
    if budget.used_up(now, cfg.rate_window, cfg.max_signals_per_guest_client) {
        shared.metrics.limited(Limit::ClientBudget);
        return Err(ErrorKind::RateLimited);
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
            joined: now,
            client: ip,
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

fn on_signal(shared: &Shared, role: &mut Role, msg: &Msg) -> Result<Action, ErrorKind> {
    let cfg = &shared.config;
    let to = str_field(msg, "to")?.ok_or(ErrorKind::BadTarget)?;
    let to: &str = &to;
    // The object's text as it came (valid JSON already: the whole message parsed). Its size is as
    // sent; the engine and the pages send compact JSON, so that is what it always was.
    let data_text = msg.get("data").ok_or(ErrorKind::BadData)?.get();
    if !data_text.starts_with('{') || data_text.len() > cfg.max_signal_data_bytes {
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
                shared.metrics.limited(Limit::GuestSignals);
                return Err(ErrorKind::RateLimited);
            }
            // (An entry the sweeper dropped as run out starts again: a guest in the room has a
            // seat, so this adds at most one per seat past the join's bound.)
            let client = r
                .guest_clients
                .entry(g.client)
                .or_insert_with(|| Window::new(now));
            if !client.hit(now, cfg.rate_window, cfg.max_signals_per_guest_client) {
                shared.metrics.limited(Limit::ClientSignals);
                return Err(ErrorKind::RateLimited);
            }
            let Some(h) = &r.host else {
                return Err(ErrorKind::NoPeer);
            };
            h.send(Out::Text(forward(gid, data_text)), &shared.metrics);
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
            // Over the host's limit: the signal is dropped and the host stays. Before, the host
            // was closed (and every guest with it, and again on each reconnect while the window
            // lasted), and guests make a host send: it answers each offer. So anyone with the
            // link could cut the host off by offering fast enough.
            if !r
                .host_signals
                .hit(now, cfg.rate_window, cfg.max_signals_per_host)
            {
                Metrics::inc(&shared.metrics.host_signals_dropped);
                shared.metrics.limited(Limit::HostSignals);
                return Ok(Action::Reply(Vec::new()));
            }
            let Some(g) = r.guests.get(to) else {
                return Err(ErrorKind::NoPeer);
            };
            g.peer
                .send(Out::Text(forward("host", data_text)), &shared.metrics);
            Metrics::inc(&shared.metrics.signals);
            Ok(Action::Reply(Vec::new()))
        }
    }
}

/// `{"t":"signal","from":<from>,"data":<data>}`, with `data` spliced in as the JSON it already is.
/// `from` is "host" or a gid (base64url): nothing in it needs escaping.
fn forward(from: &str, data_json: &str) -> String {
    debug_assert!(from == "host" || is_b64url(from, GID_LEN));
    const HEAD: &str = "{\"t\":\"signal\",\"from\":\"";
    const MID: &str = "\",\"data\":";
    let mut s = String::with_capacity(HEAD.len() + from.len() + MID.len() + data_json.len() + 1);
    s.push_str(HEAD);
    s.push_str(from);
    s.push_str(MID);
    s.push_str(data_json);
    s.push('}');
    s
}

fn on_leave(shared: &Shared, role: &mut Role, me: &Peer, msg: &Msg) -> Result<Action, ErrorKind> {
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
                    match r.guests.remove(&*gid) {
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
                        Metrics::inc(&shared.metrics.rooms_closed);
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
    let mut last_activity = shared.metrics.activity();
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
            expire_guests(&mut st, now, cfg.guest_max_age, &shared.metrics);
            for r in st.rooms.values_mut() {
                r.guest_clients
                    .retain(|_, w| !w.expired(now, cfg.rate_window));
            }
            st.joins.sweep(now, cfg.rate_window);
        }
        stun_table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sweep(now, cfg.rate_window);
        if !cfg.summary_interval.is_zero() && last_summary.elapsed() >= cfg.summary_interval {
            last_summary = now;
            let m = &shared.metrics;
            let activity = m.activity();
            // Nothing happened since the last one: no line (an idle hub logs nothing).
            if activity != last_activity {
                last_activity = activity;
                let g = gauges(&shared);
                shared.log(
                    "summary",
                    &[
                        ("rooms", Field::N(g.rooms as u64)),
                        ("guests", Field::N(g.guests as u64)),
                        ("connections", Field::N(g.connections as u64)),
                        ("hosts", Field::N(m.hosts.load(Ordering::Relaxed))),
                        ("joins", Field::N(m.joins.load(Ordering::Relaxed))),
                        ("signals", Field::N(m.signals.load(Ordering::Relaxed))),
                        ("stun", Field::N(m.stun_requests.load(Ordering::Relaxed))),
                        ("errors", Field::N(m.errors_total())),
                        ("refused", Field::N(m.refused_total())),
                        ("strays", Field::N(m.strays_total())),
                    ],
                );
            }
        }
    }
}

/// Removes the guests that joined `max_age` ago or more. The hub only introduces a guest: every
/// client leaves its room once its connection to the host is up (the guest page and the share
/// client from the start), well within a minute and a half, so one still here has not made a
/// session and will not. Before, a few sockets that joined and then sat idle (answering pings)
/// kept a room full for as long as they liked. The guest gets `timeout` (4000), its host `left`.
fn expire_guests(st: &mut State, now: Instant, max_age: Duration, m: &Metrics) {
    for r in st.rooms.values_mut() {
        let old: Vec<String> = r
            .guests
            .iter()
            .filter(|(_, g)| now.duration_since(g.joined) >= max_age)
            .map(|(gid, _)| gid.clone())
            .collect();
        for gid in old {
            let Some(g) = r.guests.remove(&gid) else {
                continue;
            };
            m.error(ErrorKind::Timeout);
            Metrics::inc(&m.guests_expired);
            g.peer.send(Out::Text(error_text(ErrorKind::Timeout)), m);
            g.peer.send(Out::Close(CLOSE_PROTOCOL, "timeout"), m);
            if let Some(h) = &r.host {
                h.send(Out::Text(json!({"t": "left", "gid": gid}).to_string()), m);
            }
        }
    }
}

/// Resolves once shutdown is asked for (holds no borrow of the channel across the await).
async fn stopped(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    fn shared(c: Config) -> Shared {
        let (shutdown, _) = tokio::sync::watch::channel(false);
        Shared {
            state: Mutex::new(State::new(&c)),
            metrics: Metrics::default(),
            shutdown,
            next_conn: AtomicU64::new(1),
            connections: AtomicUsize::new(0),
            log: Arc::new(|_: &str| {}),
            config: c,
        }
    }

    fn peer(conn: u64) -> (Peer, mpsc::Receiver<Out>) {
        let (tx, rx) = mpsc::channel(PEER_QUEUE);
        let p = Peer {
            conn,
            tx,
            kill: Arc::new(Notify::new()),
        };
        (p, rx)
    }

    fn text(o: Option<Out>) -> String {
        match o {
            Some(Out::Text(t)) => t,
            _ => panic!("no text queued"),
        }
    }

    /// A host and one guest in a room, handled in process (no sockets): what `connection` does
    /// with each text frame.
    struct Room1 {
        sh: Shared,
        host: (Role, Peer, mpsc::Receiver<Out>),
        guest: (Role, Peer, mpsc::Receiver<Out>),
        gid: String,
    }

    fn ip() -> IpKey {
        IpKey::of("192.0.2.1".parse().unwrap())
    }

    fn room1(c: Config) -> Room1 {
        let sh = shared(c);
        let (hp, mut hrx) = peer(1);
        let (gp, grx) = peer(2);
        let mut hrole = Role::None;
        let mut grole = Role::None;
        let host = json!({"v": 1, "t": "host", "room": "A".repeat(ROOM_LEN), "secret": "S".repeat(SECRET_LEN)});
        assert!(handle_text(&sh, &mut hrole, &hp, ip(), &host.to_string()).is_ok());
        let join = json!({"v": 1, "t": "join", "room": "A".repeat(ROOM_LEN)});
        assert!(handle_text(&sh, &mut grole, &gp, ip(), &join.to_string()).is_ok());
        let guest: Value = serde_json::from_str(&text(hrx.try_recv().ok())).unwrap();
        let gid = guest["gid"].as_str().unwrap().to_string();
        Room1 {
            sh,
            host: (hrole, hp, hrx),
            guest: (grole, gp, grx),
            gid,
        }
    }

    /// An offer the size a browser makes (a video, an audio and a data section: about 5 KB).
    fn offer_data() -> Value {
        let mut sdp =
            String::from("v=0\r\no=- 4611731400430051336 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n");
        while sdp.len() < 5000 {
            sdp.push_str(
                "a=rtpmap:96 H264/90000\r\na=rtcp-fb:96 nack pli\r\na=fmtp:96 \
                 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
            );
        }
        json!({"type": "offer", "sdp": sdp})
    }

    #[test]
    fn a_signal_is_forwarded_with_its_data_unchanged() {
        let mut r = room1(Config::default());
        // Guest → host: the data as sent.
        let data = r#"{"type":"candidate","candidate":"candidate:1 1 udp 2122260223 192.0.2.9 5000 typ host","sdpMid":"0","x":"é\n"}"#;
        let msg = format!(r#"{{"t":"signal","to":"host","data":{data}}}"#);
        let (role, me, _) = &mut r.guest;
        assert!(matches!(
            handle_text(&r.sh, role, me, ip(), &msg),
            Ok(Action::Reply(v)) if v.is_empty()
        ));
        let got: Value = serde_json::from_str(&text(r.host.2.try_recv().ok())).unwrap();
        assert_eq!(
            got,
            json!({"t": "signal", "from": r.gid, "data": serde_json::from_str::<Value>(data).unwrap()})
        );
        // Host → guest.
        let offer = offer_data();
        let msg = json!({"t": "signal", "to": r.gid, "data": offer}).to_string();
        let (role, me, _) = &mut r.host;
        assert!(handle_text(&r.sh, role, me, ip(), &msg).is_ok());
        let got: Value = serde_json::from_str(&text(r.guest.2.try_recv().ok())).unwrap();
        assert_eq!(got, json!({"t": "signal", "from": "host", "data": offer}));
        // Not an object, too large, or a field of the wrong type: refused as before.
        let (role, me, _) = &mut r.guest;
        for (bad, why) in [
            (
                json!({"t": "signal", "to": "host", "data": [1]}),
                ErrorKind::BadData,
            ),
            (
                json!({"t": "signal", "to": "host", "data": "x"}),
                ErrorKind::BadData,
            ),
            (
                json!({"t": "signal", "to": "host", "data": null}),
                ErrorKind::BadData,
            ),
            (json!({"t": "signal", "to": "host"}), ErrorKind::BadData),
            (
                json!({"t": "signal", "to": "host", "data": {"x": "y".repeat(13_000)}}),
                ErrorKind::BadData,
            ),
            (
                json!({"t": "signal", "to": 7, "data": {}}),
                ErrorKind::BadJson,
            ),
            (
                json!({"t": "signal", "to": null, "data": {}}),
                ErrorKind::BadJson,
            ),
            (json!({"t": "signal", "data": {}}), ErrorKind::BadTarget),
            (json!({"t": 1}), ErrorKind::BadJson),
            (json!({"to": "host"}), ErrorKind::BadJson),
            (
                json!({"t": "signal", "to": "host", "data": {}, "v": 2}),
                ErrorKind::BadVersion,
            ),
            (
                json!({"t": "signal", "to": "host", "data": {}, "v": "1"}),
                ErrorKind::BadVersion,
            ),
            (json!({"t": "nope", "v": 1}), ErrorKind::UnknownType),
            (json!(["signal"]), ErrorKind::BadJson),
        ] {
            let res = handle_text(&r.sh, role, me, ip(), &bad.to_string());
            assert!(matches!(res, Err(k) if k == why), "{bad}");
        }
        for bad in ["", "nul", "{\"t\":\"signal\"", "1"] {
            let res = handle_text(&r.sh, role, me, ip(), bad);
            assert!(matches!(res, Err(ErrorKind::BadJson)), "{bad}");
        }
        // A signal with `v` 1 is fine (the engine sends one), and so are spaces and escapes.
        let ok = json!({"v": 1, "t": "signal", "to": "host", "data": {"a": 1}}).to_string();
        assert!(handle_text(&r.sh, role, me, ip(), &ok).is_ok());
        let spaced =
            "{ \"t\" : \"sig\\u006eal\", \"to\":\"ho\\u0073t\" , \"data\" :  {\"a\": [1, 2]} }";
        assert!(handle_text(&r.sh, role, me, ip(), spaced).is_ok());
        let got: Value = serde_json::from_str(&text(r.host.2.try_recv().ok())).unwrap();
        // (the one forwarded before, `ok`'s)
        assert_eq!(got["data"], json!({"a": 1}));
        let got: Value = serde_json::from_str(&text(r.host.2.try_recv().ok())).unwrap();
        assert_eq!(got["data"], json!({"a": [1, 2]}));
        // A field that comes twice counts as the last one, as before.
        let twice = r#"{"t":"leave","t":"signal","to":"host","data":{"b":2}}"#;
        assert!(handle_text(&r.sh, role, me, ip(), twice).is_ok());
        let got: Value = serde_json::from_str(&text(r.host.2.try_recv().ok())).unwrap();
        assert_eq!(got["data"], json!({"b": 2}));
    }

    #[test]
    fn a_bound_room_id_is_the_secrets_and_a_random_one_is_never_bound() {
        let a = random_b64(32);
        let b = random_b64(32);
        let ra = room_for_secret(&a);
        assert_eq!(ra, room_for_secret(&a));
        assert_ne!(ra, room_for_secret(&b));
        assert!(is_b64url(&ra, ROOM_LEN) && is_bound_room(&ra));
        // Not a piece of the hash kept for reclaims.
        let kept = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash_secret(&a));
        assert!(!kept.starts_with(&ra[..ROOM_LEN - 1]));
        // What older apps make: 16 random bytes. Never of the bound form.
        for _ in 0..2_000 {
            let old = random_b64(16);
            assert_eq!(old.len(), ROOM_LEN);
            assert!(!is_bound_room(&old), "{old}");
        }
        // In process: a bound id with another secret is refused, with its own taken.
        let sh = shared(Config::default());
        let (p, _rx) = peer(1);
        let host = |room: &str, secret: &str| {
            let msg = json!({"v": 1, "t": "host", "room": room, "secret": secret}).to_string();
            handle_text(&sh, &mut Role::None, &p, ip(), &msg).map(|_| ())
        };
        assert!(matches!(host(&ra, &b), Err(ErrorKind::RoomTaken)));
        assert!(host(&ra, &a).is_ok());
    }

    /// The per-client budgets a room keeps are bounded: past a few clients per seat in one window
    /// a new client is refused (an old one is not) until windows run out.
    #[test]
    fn a_rooms_client_budgets_are_bounded() {
        let window = Duration::from_millis(200);
        let sh = shared(Config {
            max_guests_per_room: 1,
            rate_window: window,
            ..Config::default()
        });
        let (hp, _hrx) = peer(1);
        let room = "A".repeat(ROOM_LEN);
        let host = json!({"v": 1, "t": "host", "room": room, "secret": "S".repeat(SECRET_LEN)});
        assert!(handle_text(&sh, &mut Role::None, &hp, ip(), &host.to_string()).is_ok());
        let join = json!({"v": 1, "t": "join", "room": room}).to_string();
        let client = |n: u32| IpKey::V4(0xc633_6400 + n);
        // Join and leave: one seat, one client at a time.
        let visit = |n: u32| {
            let (gp, _grx) = peer(10 + u64::from(n));
            let mut role = Role::None;
            let res = handle_text(&sh, &mut role, &gp, client(n), &join).map(|_| ());
            if res.is_ok() {
                let leave = json!({"t": "leave"}).to_string();
                assert!(handle_text(&sh, &mut role, &gp, client(n), &leave).is_ok());
            }
            res
        };
        let cap = GUEST_CLIENTS_PER_SEAT as u32;
        for n in 0..cap {
            assert!(visit(n).is_ok(), "client {n}");
        }
        assert!(matches!(visit(cap), Err(ErrorKind::RateLimited)));
        assert!(visit(0).is_ok(), "a client it has may come back");
        let kept = |sh: &Shared| {
            let st = sh.state.lock().unwrap();
            st.rooms[&room].guest_clients.len()
        };
        assert_eq!(kept(&sh), GUEST_CLIENTS_PER_SEAT);
        std::thread::sleep(window);
        assert!(visit(cap).is_ok(), "windows ran out");
        assert_eq!(kept(&sh), 1);
    }

    /// `cargo test -p ps5upload-hub --release forward_cost -- --ignored --nocapture`: the time the
    /// hub spends on one forwarded offer (about 5 KB) and one ICE candidate, in process.
    #[test]
    #[ignore = "a measurement: prints the cost per forwarded signal"]
    fn forward_cost() {
        let c = Config {
            max_signals_per_guest: u32::MAX,
            max_signals_per_guest_client: u32::MAX,
            max_signals_per_host: u32::MAX,
            ..Config::default()
        };
        let mut r = room1(c);
        let offer = json!({"t": "signal", "to": "host", "data": offer_data()}).to_string();
        let cand = json!({"t": "signal", "to": "host", "data": {"type": "candidate",
            "candidate": "candidate:842163049 1 udp 1677729535 203.0.113.5 40000 typ srflx raddr \
                          0.0.0.0 rport 0 generation 0 ufrag abcd network-cost 999",
            "sdpMid": "0"}})
        .to_string();
        for (name, msg) in [("offer", &offer), ("candidate", &cand)] {
            let n = 20_000u32;
            let t0 = Instant::now();
            for _ in 0..n {
                let (role, me, _) = &mut r.guest;
                let _ = handle_text(&r.sh, role, me, ip(), msg).unwrap();
                let _ = r.host.2.try_recv().unwrap();
            }
            let per = t0.elapsed() / n;
            println!(
                "hub forward {name} ({} bytes): {:.2} us per message",
                msg.len(),
                per.as_secs_f64() * 1e6
            );
        }
    }
}
