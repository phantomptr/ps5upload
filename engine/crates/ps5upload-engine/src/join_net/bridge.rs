//! The bridge (`GET /api/join/bridge`): PS5Upload Play, a native PS5 app with no WebRTC, joins a
//! friend's Play Together over the internet through its own engine on the home network. The app
//! opens a plain WebSocket here; this engine joins the host's room on the hub as a WebRTC guest,
//! exactly as the guest page does (client/src/join/rtcTransport.ts: the offer, one reliable and
//! ordered data channel labelled `"join"`, the answer checked against the link's fingerprint), and
//! then passes the guest protocol through unchanged.
//!
//! **The app's socket.** First text message within [`FIRST_WAIT`]:
//! `{"bridge":{"v":1,"room":"<22 base64url>","fp":"<SHA-256, hex with colons>","hub":"<host or
//! wss URL, optional>"}}`. Only the hub in the Play Together settings is used: another `hub` is
//! refused. The engine says `{"bridge":"connecting"}`, then `{"bridge":"open","path":"lan" |
//! "tailscale" | "internet","rtt_ms":n | null}` once the channel is open, or
//! `{"bridge":"failed","why":…}` and closes: `bad` (the first message is not one of these),
//! `hub` (another hub, the hub unreachable or another hub error, or the host did not answer
//! within [`TRY_FOR`]), `no_room`, `room_full` (the room, or the host's engine, is full),
//! `no_direct` (answered, but ICE did not connect within [`TRY_FOR`]), `mismatch` (the answer's
//! certificate is not `fp`'s), `offer` (the host's engine refused the offer, or its answer could not
//! be taken), `busy` (this engine's bridge limits). Before "open" nothing else from the app is read
//! (its close ends the try).
//!
//! **After "open"** every WebSocket message from the app goes onto the channel unchanged (text
//! whole, binary in pieces: [`super::chunk`]), and every whole message from the channel goes to
//! the app unchanged. The host's ping (`0x02`) is answered with its pong (`0x03`) here at once and
//! never forwarded. The app closes: the peer connection closes. The channel closes (or the path is
//! lost): the app's socket gets a close frame.
//!
//! **The hub** is used for the join, the offer and this engine's candidates only (UDP with an
//! address, at most [`TRICKLE_MAX`] trickled), and left the moment the channel opens or the try
//! fails: nothing goes through it after that.
//!
//! Limits: [`PER_IP`] bridges at once per address and [`IN_ALL`] in all, 10 connects a minute per
//! address (this computer: the guest socket's own allowance), the app's messages at most a guest
//! socket's ([`crate::join::GUEST_MSG_MAX`]), the host's at most [`FROM_HOST_MAX`] (the page's).
//! When the app does not keep up ([`APP_HIGH_WATER`] waiting for it) the bridge stops reading the
//! connection until it does: the host's channel then fills and its session skips this guest to the
//! next keyframe. Nothing is dropped here. The log has one line per bridge and never the room, the
//! fingerprint, the invite key or an address.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use str0m::change::SdpAnswer;
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Message as WsMsg;

use super::chunk::{self, Piece, Reassembler};
use super::{hub, rtc, stun, Kind};

/// The app's first message must come this soon.
pub(crate) const FIRST_WAIT: Duration = Duration::from_secs(10);
/// One try, from the hub's connect to the open channel (the page gives ICE 15 s too).
pub(crate) const TRY_FOR: Duration = Duration::from_secs(15);
/// Bridges at once from one address (an IPv6 /64), and in all.
pub(crate) const PER_IP: usize = 2;
pub(crate) const IN_ALL: usize = 4;
/// The largest whole message from the host (the guest page's MESSAGE_MAX: a keyframe is far under).
pub(crate) const FROM_HOST_MAX: usize = 16 * 1024 * 1024;
/// Bytes waiting for the app past which the bridge stops reading the connection.
pub(crate) const APP_HIGH_WATER: usize = super::HIGH_WATER;
/// One message to the app taking longer than this: the app is gone.
const APP_SEND_WAIT: Duration = Duration::from_secs(20);
/// A WebSocket ping to the app this often; nothing from it for [`APP_QUIET_MAX`]: it is gone.
const APP_PING_EVERY: Duration = Duration::from_secs(15);
const APP_QUIET_MAX: Duration = Duration::from_secs(45);
/// The app's messages the channel has not taken yet, at most (the host reads them as they come:
/// past this it is not reading).
const TO_HOST_MAX: usize = 256 * 1024;
/// The round trip's ping at the open waits this long for its pong at most.
const RTT_WAIT: Duration = Duration::from_secs(1);
/// ICE disconnected this long after the open: the path is lost.
const DISCONNECTED_WAIT: Duration = Duration::from_secs(10);
/// This engine's candidates trickled through the hub at most (each is a signal the hub counts).
pub(crate) const TRICKLE_MAX: usize = 10;
/// Candidates from the host held until its answer, at most.
const EARLY_MAX: usize = 32;
/// A STUN server's name lookup, and one request's send, wait this long at most.
const STUN_WAIT: Duration = Duration::from_secs(2);
/// A STUN request is sent again after each of these while unanswered.
const STUN_RETRY: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1000),
    Duration::from_millis(2000),
];

// ── The first message ────────────────────────────────────────────────────────────────────────────

/// What the app asks for: the room on the hub, the host engine's certificate fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ask {
    pub room: String,
    /// Upper-case hex with colons.
    pub fp: String,
}

/// The app's first message, against the hub in the settings (`hub_url`). Err: the `why` the app
/// is told (`bad`, `hub`).
pub(crate) fn parse_ask(text: &str, hub_url: &str) -> Result<Ask, &'static str> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| "bad")?;
    let b = v.get("bridge").filter(|b| b.is_object()).ok_or("bad")?;
    if b.get("v").and_then(|x| x.as_u64()) != Some(1) {
        return Err("bad");
    }
    let room = b
        .get("room")
        .and_then(|x| x.as_str())
        .filter(|r| room_ok(r))
        .ok_or("bad")?;
    let fp = b
        .get("fp")
        .and_then(|x| x.as_str())
        .and_then(fingerprint_norm)
        .ok_or("bad")?;
    if !super::hub_url_ok(hub_url) {
        return Err("hub");
    }
    match b.get("hub") {
        None | Some(serde_json::Value::Null) => {}
        Some(h) => {
            let h = h.as_str().ok_or("bad")?;
            if !h.trim().is_empty() && !same_hub(h, hub_url) {
                return Err("hub");
            }
        }
    }
    Ok(Ask {
        room: room.to_string(),
        fp,
    })
}

/// A room id as the hub takes one: 22 base64url characters.
fn room_ok(r: &str) -> bool {
    r.len() == 22
        && r.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// A SHA-256 fingerprint (32 bytes in hex, with colons), upper case; None when it is not one.
pub(crate) fn fingerprint_norm(s: &str) -> Option<String> {
    let up = s.trim().to_ascii_uppercase();
    let parts: Vec<&str> = up.split(':').collect();
    (parts.len() == 32
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|c| c.is_ascii_hexdigit())))
    .then_some(up)
}

/// `given` (a host, `host:port`, or a `ws(s)://` URL) names the hub at `url`.
fn same_hub(given: &str, url: &str) -> bool {
    let Ok(want) = url.parse::<axum::http::Uri>() else {
        return false;
    };
    let tls = want.scheme_str() == Some("wss");
    let norm = |authority: &str, tls: bool| {
        let a = authority.trim().to_ascii_lowercase();
        let default = if tls { ":443" } else { ":80" };
        a.strip_suffix(default).map(str::to_string).unwrap_or(a)
    };
    let Some(want_host) = super::hub_host(url).map(|h| norm(&h, tls)) else {
        return false;
    };
    let given = given.trim();
    if given.contains("://") {
        let Ok(g) = given.parse::<axum::http::Uri>() else {
            return false;
        };
        let g_tls = g.scheme_str() == Some("wss");
        return g.scheme_str() == want.scheme_str()
            && g.authority().map(|a| norm(a.as_str(), g_tls)) == Some(want_host);
    }
    norm(given, tls) == want_host
}

/// The SHA-256 fingerprint an SDP carries (`a=fingerprint:sha-256 <X>`), upper case; None when it
/// has none, more than one that differ, or another hash's (the page's `sdpFingerprint`).
pub(crate) fn sdp_fingerprint(sdp: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for l in sdp.lines() {
        let Some(rest) = l.trim().strip_prefix("a=fingerprint:") else {
            continue;
        };
        let (hash, value) = rest.split_once([' ', '\t'])?;
        if !hash.eq_ignore_ascii_case("sha-256") {
            return None;
        }
        let v = fingerprint_norm(value)?;
        match &found {
            Some(f) if *f != v => return None,
            _ => found = Some(v),
        }
    }
    found
}

/// The `a=mid:` of an SDP's first section (the data channel's, in a data-only offer).
fn first_mid(sdp: &str) -> String {
    sdp.lines()
        .find_map(|l| l.trim().strip_prefix("a=mid:"))
        .unwrap_or("0")
        .to_string()
}

// ── Limits ───────────────────────────────────────────────────────────────────────────────────────

/// The open bridges, by address ([`crate::join::limit_key`]).
static OPEN: Mutex<Vec<IpAddr>> = Mutex::new(Vec::new());
/// Bridge connects per address per minute.
static CONNECTS: crate::join::ConnectLimit = crate::join::ConnectLimit::new();

/// One more bridge from `ip` in `open`, unless that is over [`PER_IP`] or [`IN_ALL`].
pub(crate) fn admit(open: &mut Vec<IpAddr>, ip: IpAddr) -> bool {
    let key = crate::join::limit_key(ip);
    if open.len() >= IN_ALL || open.iter().filter(|k| **k == key).count() >= PER_IP {
        return false;
    }
    open.push(key);
    true
}

fn release(open: &mut Vec<IpAddr>, ip: IpAddr) {
    let key = crate::join::limit_key(ip);
    if let Some(i) = open.iter().position(|k| *k == key) {
        open.swap_remove(i);
    }
}

/// Bridges open now (the host's state shows it).
pub(crate) fn count() -> usize {
    OPEN.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// A bridge's place in [`OPEN`], given back when it ends.
struct Slot(IpAddr);

impl Slot {
    fn take(ip: IpAddr) -> Option<Slot> {
        admit(&mut OPEN.lock().unwrap_or_else(|e| e.into_inner()), ip).then_some(Slot(ip))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        release(&mut OPEN.lock().unwrap_or_else(|e| e.into_inner()), self.0);
    }
}

// ── The settings ─────────────────────────────────────────────────────────────────────────────────

/// What a bridge takes from the Play Together settings.
#[derive(Clone, Debug)]
pub(crate) struct Conf {
    pub hub_url: String,
    pub stun: Vec<String>,
}

/// The tests' hub and STUN servers, in place of the settings (which other tests change).
#[cfg(test)]
pub(crate) static TEST_CONF: Mutex<Option<Conf>> = Mutex::new(None);

impl Conf {
    fn now() -> Conf {
        #[cfg(test)]
        if let Some(c) = TEST_CONF.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return c;
        }
        let s = crate::join::settings();
        Conf {
            hub_url: s.hub_url,
            stun: s.stun,
        }
    }
}

// ── The app's socket ─────────────────────────────────────────────────────────────────────────────

/// The upgrade of `GET /api/join/bridge` from `ip` (join.rs `bridge`).
pub(crate) fn upgrade(up: WebSocketUpgrade, ip: IpAddr) -> Response {
    let allowed = CONNECTS.allow(ip, crate::join::connects_per_min(ip), Instant::now());
    let conf = Conf::now();
    up.max_message_size(crate::join::GUEST_MSG_MAX)
        .max_frame_size(crate::join::GUEST_MSG_MAX)
        .on_upgrade(move |s| session(s, ip, allowed, conf))
}

fn msg_len(m: &Message) -> usize {
    match m {
        Message::Text(t) => t.len(),
        Message::Binary(b) => b.len(),
        _ => 8,
    }
}

fn say(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

/// What goes to the app: its own task writes it, so a slow app never holds the connection's
/// loop (that loop stops reading the connection instead, [`ToApp::full`]).
struct ToApp {
    tx: mpsc::UnboundedSender<Message>,
    queued: Arc<AtomicUsize>,
    drained: Arc<Notify>,
    gone: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl ToApp {
    fn start(mut sink: SplitSink<WebSocket, Message>) -> ToApp {
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let queued = Arc::new(AtomicUsize::new(0));
        let drained = Arc::new(Notify::new());
        let gone = Arc::new(AtomicBool::new(false));
        let (q, d, g) = (queued.clone(), drained.clone(), gone.clone());
        let task = tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                let n = msg_len(&m);
                let last = matches!(m, Message::Close(_));
                let ok = matches!(
                    tokio::time::timeout(APP_SEND_WAIT, sink.send(m)).await,
                    Ok(Ok(()))
                );
                let left = q.fetch_sub(n, Ordering::Relaxed).saturating_sub(n);
                if left <= APP_HIGH_WATER / 2 {
                    d.notify_one();
                }
                if !ok || last {
                    break;
                }
            }
            g.store(true, Ordering::Relaxed);
            d.notify_one();
            let _ = tokio::time::timeout(Duration::from_secs(2), sink.close()).await;
        });
        ToApp {
            tx,
            queued,
            drained,
            gone,
            task,
        }
    }

    fn send(&self, m: Message) {
        self.queued.fetch_add(msg_len(&m), Ordering::Relaxed);
        if self.tx.send(m).is_err() {
            self.gone.store(true, Ordering::Relaxed);
        }
    }

    fn say(&self, v: serde_json::Value) {
        self.send(say(v));
    }

    /// The app has [`APP_HIGH_WATER`] waiting for it.
    fn full(&self) -> bool {
        self.queued.load(Ordering::Relaxed) > APP_HIGH_WATER
    }

    fn gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }

    /// A close frame after what is queued, and the socket closed.
    async fn finish(self) {
        self.send(Message::Close(Some(CloseFrame {
            code: 1000,
            reason: "".into(),
        })));
        drop(self.tx);
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

/// Says `why`, logs it, closes.
async fn refuse(app: ToApp, why: &'static str) {
    crate::log_at!(Info, "together", "join bridge: failed: {why}");
    app.say(serde_json::json!({"bridge": "failed", "why": why}));
    app.finish().await;
}

/// The app's first message (pings and pongs aside), within [`FIRST_WAIT`]. Err(None): it closed or
/// said nothing.
async fn first_message(
    rx: &mut SplitStream<WebSocket>,
    conf: &Conf,
) -> Result<Ask, Option<&'static str>> {
    let until = tokio::time::Instant::now() + FIRST_WAIT;
    loop {
        match tokio::time::timeout_at(until, rx.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                return parse_ask(t.as_str(), &conf.hub_url).map_err(Some)
            }
            Ok(Some(Ok(Message::Binary(_)))) => return Err(Some("bad")),
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => {}
            _ => return Err(None),
        }
    }
}

/// Resolves when the app's socket closes; anything it sends meanwhile is not read (before the
/// channel is open nothing else is taken).
async fn app_gone(rx: &mut SplitStream<WebSocket>) {
    loop {
        match rx.next().await {
            Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
            _ => {}
        }
    }
}

async fn session(ws: WebSocket, ip: IpAddr, allowed: bool, conf: Conf) {
    let (sink, mut app_rx) = ws.split();
    let app = ToApp::start(sink);
    let Some(_slot) = allowed.then(|| Slot::take(ip)).flatten() else {
        return refuse(app, "busy").await;
    };
    let ask = match first_message(&mut app_rx, &conf).await {
        Ok(a) => a,
        Err(Some(why)) => return refuse(app, why).await,
        Err(None) => return app.finish().await,
    };
    app.say(serde_json::json!({"bridge": "connecting"}));
    let started = Instant::now();
    let mut b = match Bridge::connect(&ask, &conf, &mut app_rx).await {
        Ok(b) => b,
        Err(Fail::Why(why)) => return refuse(app, why).await,
        Err(Fail::Gone) => {
            crate::log_at!(
                Debug,
                "together",
                "join bridge: the app left while connecting"
            );
            return;
        }
    };
    let path = b.path();
    crate::log_at!(
        Info,
        "together",
        "join bridge: open to the internet room via {path} in {} ms",
        started.elapsed().as_millis()
    );
    app.say(serde_json::json!({"bridge": "open", "path": path, "rtt_ms": b.rtt_ms}));
    let why = b.pump(&mut app_rx, &app).await;
    crate::log_at!(
        Debug,
        "together",
        "join bridge: closed after {} s ({why})",
        started.elapsed().as_secs()
    );
    app.finish().await;
}

// ── The connection ───────────────────────────────────────────────────────────────────────────────

/// How a try ended without an open channel.
enum Fail {
    /// Said to the app.
    Why(&'static str),
    /// The app closed its socket.
    Gone,
}

/// What one bridge's try asked of the hub (its log, and the tests' measure of the hub's load).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HubUse {
    pub sent: u32,
    pub sent_bytes: usize,
    pub got: u32,
    pub got_bytes: usize,
    pub connected: Duration,
}

/// The last bridge's hub use (tests).
#[cfg(test)]
pub(crate) static LAST_HUB_USE: Mutex<Option<HubUse>> = Mutex::new(None);

/// The bridge's side of the hub while it tries.
struct HubLink {
    ws: Option<hub::Ws>,
    since: Instant,
    used: HubUse,
}

impl HubLink {
    async fn send(&mut self, v: serde_json::Value) {
        let Some(ws) = self.ws.as_mut() else { return };
        let t = v.to_string();
        self.used.sent += 1;
        self.used.sent_bytes += t.len();
        if ws.send(WsMsg::Text(t.into())).await.is_err() {
            self.ws = None;
        }
    }

    async fn signal(&mut self, data: serde_json::Value) {
        self.send(serde_json::json!({"v": 1, "t": "signal", "to": "host", "data": data}))
            .await;
    }

    async fn next(&mut self) -> Option<Result<WsMsg, tokio_tungstenite::tungstenite::Error>> {
        match self.ws.as_mut() {
            Some(ws) => ws.next().await,
            None => std::future::pending().await,
        }
    }

    /// Leaves the room and closes: nothing goes through the hub after this.
    async fn leave(&mut self) {
        if self.ws.is_some() {
            self.send(serde_json::json!({"v": 1, "t": "leave"})).await;
        }
        if let Some(mut ws) = self.ws.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(None)).await;
        }
        self.used.connected = self.since.elapsed();
        let u = self.used;
        crate::log_at!(
            Debug,
            "together",
            "join bridge: the hub: {} messages ({} bytes) out, {} ({} bytes) in, {} ms",
            u.sent,
            u.sent_bytes,
            u.got,
            u.got_bytes,
            u.connected.as_millis()
        );
        #[cfg(test)]
        {
            *LAST_HUB_USE.lock().unwrap_or_else(|e| e.into_inner()) = Some(u);
        }
    }
}

/// The STUN requests in flight: transaction id → socket index.
type StunPending = Arc<Mutex<HashMap<[u8; 12], usize>>>;

/// Tasks that end with the bridge (the sockets' readers, STUN).
#[derive(Default)]
struct Tasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for t in &self.0 {
            t.abort();
        }
    }
}

/// One bridge's peer connection: its sockets, the `Rtc` (the offerer), the channel.
struct Bridge {
    rtc: Rtc,
    socks: Vec<(Arc<UdpSocket>, SocketAddr)>,
    pkt_rx: mpsc::Receiver<(usize, SocketAddr, Vec<u8>)>,
    /// The sockets' readers and the STUN task: they end with the bridge.
    _readers: Tasks,
    _stun_task: Tasks,
    stun: StunPending,
    cid: Option<ChannelId>,
    /// Data-channel messages for the host (binary ones framed already) the channel has not taken.
    out: VecDeque<(bool, Vec<u8>)>,
    out_bytes: usize,
    /// Datagrams to send: socket index, destination.
    sendq: Vec<(usize, SocketAddr, Vec<u8>)>,
    reasm: Reassembler,
    /// Whole messages from the host, not yet handed to the app.
    inbox: VecDeque<Message>,
    /// The pair ICE selected (stats), and the last pair DTLS came over (this socket, the host's).
    pair: Option<(SocketAddr, SocketAddr)>,
    last: Option<(SocketAddr, SocketAddr)>,
    disconnected_at: Option<Instant>,
    closed: bool,
    broken: Option<&'static str>,
    /// The open's ping: its 8 bytes and when it went.
    ping: Option<([u8; 8], Instant)>,
    rtt_ms: Option<u32>,
}

async fn read_socket(i: usize, s: Arc<UdpSocket>, tx: mpsc::Sender<(usize, SocketAddr, Vec<u8>)>) {
    let mut buf = vec![0u8; 2048];
    loop {
        match s.recv_from(&mut buf).await {
            Ok((n, from)) => {
                if tx.send((i, from, buf[..n].to_vec())).await.is_err() {
                    return;
                }
            }
            // An ICMP "port unreachable" from an earlier send shows up here on some systems.
            Err(_) => {
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    }
}

/// The STUN servers named, looked up together (the tests never look a name up): one address of
/// each family per server, at most four servers.
async fn stun_servers(names: &[String]) -> Vec<SocketAddr> {
    let looks = names
        .iter()
        .take(4)
        .filter(|s| !cfg!(test) || s.parse::<SocketAddr>().is_ok())
        .map(|s| async move {
            match tokio::time::timeout(STUN_WAIT, tokio::net::lookup_host(s.as_str())).await {
                Ok(Ok(found)) => {
                    let found: Vec<SocketAddr> = found.collect();
                    [
                        found.iter().find(|a| a.is_ipv4()).copied(),
                        found.iter().find(|a| a.is_ipv6()).copied(),
                    ]
                }
                _ => [None, None],
            }
        });
    futures_util::future::join_all(looks)
        .await
        .into_iter()
        .flatten()
        .flatten()
        .collect()
}

/// Asks the STUN servers from every socket (each request again after [`STUN_RETRY`] while
/// unanswered); the answers come in on the sockets and the bridge takes them off `pending`.
async fn ask_stun(
    socks: Vec<(Arc<UdpSocket>, SocketAddr)>,
    names: Vec<String>,
    pending: StunPending,
) {
    let servers = stun_servers(&names).await;
    let mut sent = Vec::new();
    {
        let mut p = pending.lock().unwrap_or_else(|e| e.into_inner());
        for (i, (_, base)) in socks.iter().enumerate() {
            for a in servers.iter().filter(|a| a.is_ipv6() == base.is_ipv6()) {
                let mut tx = [0u8; 12];
                if getrandom::fill(&mut tx).is_ok() {
                    p.insert(tx, i);
                    sent.push((tx, i, *a));
                }
            }
        }
    }
    for wait in STUN_RETRY {
        let left: Vec<_> = {
            let p = pending.lock().unwrap_or_else(|e| e.into_inner());
            sent.iter()
                .filter(|(tx, ..)| p.contains_key(tx))
                .copied()
                .collect()
        };
        if left.is_empty() {
            return;
        }
        for (tx, i, a) in left {
            let _ =
                tokio::time::timeout(STUN_WAIT, socks[i].0.send_to(&stun::request(tx), a)).await;
        }
        tokio::time::sleep(wait).await;
    }
}

/// A host candidate line as the engine takes it: UDP with an address, no mDNS name.
fn remote_candidate(c: &str) -> Option<Candidate> {
    let c = c.trim();
    let c = c.strip_prefix("a=").unwrap_or(c);
    if c.is_empty() || c.contains(".local ") {
        return None;
    }
    Candidate::from_sdp_string(c).ok()
}

impl Bridge {
    /// Joins the room, offers, and returns once the channel is open (with the round trip, when
    /// the host answered the open's ping in time). The hub is left whatever happens.
    async fn connect(
        ask: &Ask,
        conf: &Conf,
        app_rx: &mut SplitStream<WebSocket>,
    ) -> Result<Bridge, Fail> {
        let mut socks = Vec::new();
        for ip in rtc::local_ips() {
            if let Ok(s) = UdpSocket::bind((ip, 0)).await {
                if let Ok(a) = s.local_addr() {
                    socks.push((Arc::new(s), a));
                }
            }
        }
        if socks.is_empty() {
            return Err(Fail::Why("no_direct"));
        }
        let (pkt_tx, pkt_rx) = mpsc::channel(512);
        let _readers = Tasks(
            socks
                .iter()
                .enumerate()
                .map(|(i, (s, _))| tokio::spawn(read_socket(i, s.clone(), pkt_tx.clone())))
                .collect(),
        );
        drop(pkt_tx);

        let mut cfg = Rtc::builder()
            .set_crypto_provider(super::crypto::provider())
            .set_stats_interval(Some(Duration::from_secs(1)));
        if let Some(cert) = super::crypto::new_cert() {
            cfg = cfg.set_dtls_cert(cert);
        }
        rtc::game_ice(&mut cfg);
        let mut peer = cfg.build(Instant::now());
        rtc::add_host_candidates(&mut peer, socks.iter().map(|s| s.1));
        let mut api = peer.sdp_api();
        api.add_channel(Kind::Join.label().into());
        let Some((offer, pending)) = api.apply() else {
            return Err(Fail::Why("offer"));
        };
        let offer = offer.to_sdp_string();
        let mid = first_mid(&offer);
        let stun: StunPending = Arc::default();
        let _stun_task = Tasks(vec![tokio::spawn(ask_stun(
            socks.clone(),
            conf.stun.clone(),
            stun.clone(),
        ))]);
        let mut b = Bridge {
            rtc: peer,
            socks,
            pkt_rx,
            _readers,
            _stun_task,
            stun,
            cid: None,
            out: VecDeque::new(),
            out_bytes: 0,
            sendq: Vec::new(),
            reasm: Reassembler::new(FROM_HOST_MAX),
            inbox: VecDeque::new(),
            pair: None,
            last: None,
            disconnected_at: None,
            closed: false,
            broken: None,
            ping: None,
            rtt_ms: None,
        };

        let deadline = tokio::time::Instant::now() + TRY_FOR;
        let ws = tokio::select! {
            r = tokio::time::timeout_at(deadline, hub::connect(&conf.hub_url)) => match r {
                Ok(Ok(ws)) => ws,
                Ok(Err(e)) => {
                    crate::log_at!(Debug, "together", "join bridge: hub unreachable: {e}");
                    return Err(Fail::Why("hub"));
                }
                Err(_) => return Err(Fail::Why("hub")),
            },
            _ = app_gone(app_rx) => return Err(Fail::Gone),
        };
        let mut hub = HubLink {
            ws: Some(ws),
            since: Instant::now(),
            used: HubUse::default(),
        };
        hub.send(serde_json::json!({"v": 1, "t": "join", "room": ask.room}))
            .await;
        let r = b
            .signal_until_open(ask, &mut hub, pending, &offer, &mid, deadline, app_rx)
            .await;
        hub.leave().await;
        r?;
        // The STUN answers are of no use any more.
        b._stun_task = Tasks::default();
        b.measure_rtt(app_rx).await?;
        Ok(b)
    }

    /// The hub's part: the offer once in the room, the answer (checked against `fp`), candidates
    /// both ways, until the channel opens or the try fails.
    #[allow(clippy::too_many_arguments)]
    async fn signal_until_open(
        &mut self,
        ask: &Ask,
        hub: &mut HubLink,
        pending: str0m::change::SdpPendingOffer,
        offer: &str,
        mid: &str,
        deadline: tokio::time::Instant,
        app_rx: &mut SplitStream<WebSocket>,
    ) -> Result<(), Fail> {
        let mut pending = Some(pending);
        let mut offer_sent = false;
        let mut answered = false;
        // This engine's candidates found before the offer went, and the host's before its answer.
        let mut own_early: Vec<serde_json::Value> = Vec::new();
        let mut early_remote: Vec<Candidate> = Vec::new();
        let mut trickled: HashSet<SocketAddr> = HashSet::new();
        loop {
            let next = self.drive(Instant::now());
            self.send_datagrams().await;
            if self.cid.is_some() {
                return Ok(());
            }
            if self.broken.is_some() || self.closed {
                return Err(Fail::Why("no_direct"));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Fail::Why(if answered { "no_direct" } else { "hub" }));
            }
            tokio::select! {
                m = hub.next() => match m {
                    Some(Ok(WsMsg::Text(t))) => {
                        hub.used.got += 1;
                        hub.used.got_bytes += t.len();
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) else {
                            continue;
                        };
                        match v.get("t").and_then(|x| x.as_str()).unwrap_or("") {
                            "joined" if !offer_sent => {
                                hub.signal(serde_json::json!({"type": "offer", "sdp": offer})).await;
                                offer_sent = true;
                                for c in own_early.drain(..) {
                                    hub.signal(c).await;
                                }
                            }
                            "signal" if v.get("from").and_then(|x| x.as_str()) == Some("host") => {
                                let d = v.get("data").cloned().unwrap_or_default();
                                let field = |k: &str| d.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                                match field("type").as_str() {
                                    "answer" if !answered => {
                                        let sdp = field("sdp");
                                        // Only the host's own engine: its certificate is the one the link carries.
                                        if sdp_fingerprint(&sdp).as_deref() != Some(ask.fp.as_str()) {
                                            return Err(Fail::Why("mismatch"));
                                        }
                                        let (Some(p), Ok(answer)) = (pending.take(), SdpAnswer::from_sdp_string(&sdp)) else {
                                            return Err(Fail::Why("offer"));
                                        };
                                        if self.rtc.sdp_api().accept_answer(p, answer).is_err() {
                                            return Err(Fail::Why("offer"));
                                        }
                                        answered = true;
                                        for c in early_remote.drain(..) {
                                            self.rtc.add_remote_candidate(c);
                                        }
                                    }
                                    "candidate" => {
                                        if let Some(c) = remote_candidate(&field("candidate")) {
                                            if answered {
                                                self.rtc.add_remote_candidate(c);
                                            } else if early_remote.len() < EARLY_MAX {
                                                early_remote.push(c);
                                            }
                                        }
                                    }
                                    "bye" => {
                                        return Err(Fail::Why(if field("why") == "full" { "room_full" } else { "offer" }));
                                    }
                                    _ => {}
                                }
                            }
                            "closed" => return Err(Fail::Why("no_room")),
                            "error" => {
                                let why = v.get("why").and_then(|x| x.as_str()).unwrap_or("");
                                crate::log_at!(Debug, "together", "join bridge: the hub says {why}");
                                return Err(Fail::Why(match why {
                                    "no_room" => "no_room",
                                    "room_full" => "room_full",
                                    _ => "hub",
                                }));
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(WsMsg::Close(_))) | None | Some(Err(_)) => {
                        hub.ws = None;
                        // With the host's answer in, ICE may still connect without the hub.
                        if !answered {
                            return Err(Fail::Why("hub"));
                        }
                    }
                    Some(Ok(_)) => {}
                },
                p = self.pkt_rx.recv() => match p {
                    Some((i, from, data)) => {
                        if let Some(c) = self.on_packet(i, from, data) {
                            if trickled.len() < TRICKLE_MAX && trickled.insert(c.addr()) {
                                let data = serde_json::json!({
                                    "type": "candidate",
                                    "candidate": c.to_sdp_string(),
                                    "sdpMid": mid,
                                });
                                if offer_sent {
                                    hub.signal(data).await;
                                } else {
                                    own_early.push(data);
                                }
                            }
                        }
                    }
                    None => return Err(Fail::Why("no_direct")),
                },
                _ = app_gone(app_rx) => return Err(Fail::Gone),
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next).min(deadline)) => {}
            }
            self.tick();
        }
    }

    /// The open's ping: the round trip to the host's engine (it answers a guest's ping at once,
    /// rtc.rs), at most [`RTT_WAIT`]. What the host sends meanwhile waits in the inbox.
    async fn measure_rtt(&mut self, app_rx: &mut SplitStream<WebSocket>) -> Result<(), Fail> {
        let mut nonce = [0u8; 8];
        let _ = getrandom::fill(&mut nonce);
        let mut ping = vec![chunk::PING];
        ping.extend_from_slice(&nonce);
        self.ping = Some((nonce, Instant::now()));
        self.for_host(true, ping);
        let until = tokio::time::Instant::now() + RTT_WAIT;
        loop {
            self.flush();
            let next = self.drive(Instant::now());
            self.send_datagrams().await;
            if self.ended().is_some() {
                return Err(Fail::Why("no_direct"));
            }
            if self.rtt_ms.is_some() || tokio::time::Instant::now() >= until {
                self.ping = None;
                return Ok(());
            }
            tokio::select! {
                p = self.pkt_rx.recv() => match p {
                    Some((i, from, data)) => {
                        self.on_packet(i, from, data);
                    }
                    None => return Err(Fail::Why("no_direct")),
                },
                _ = app_gone(app_rx) => {
                    self.close_peer().await;
                    return Err(Fail::Gone);
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(next).min(until)) => {}
            }
            self.tick();
        }
    }

    /// After the open: the app's messages onto the channel, the host's to the app, until either
    /// side ends. Returns why it ended (for the log).
    async fn pump(&mut self, app_rx: &mut SplitStream<WebSocket>, app: &ToApp) -> &'static str {
        let mut ping = tokio::time::interval(APP_PING_EVERY);
        ping.tick().await;
        let mut heard = Instant::now();
        loop {
            self.flush();
            let next = self.drive(Instant::now());
            self.send_datagrams().await;
            for m in self.inbox.drain(..) {
                app.send(m);
            }
            if app.gone() {
                self.close_peer().await;
                return "the app is gone";
            }
            if let Some(why) = self.ended() {
                return why;
            }
            if self.out_bytes > TO_HOST_MAX {
                self.close_peer().await;
                return "the host does not read";
            }
            // The app is behind: the connection is not read until it catches up (the host's
            // channel fills, and its session skips this guest to the next keyframe).
            let paused = app.full();
            tokio::select! {
                p = self.pkt_rx.recv(), if !paused => match p {
                    Some((i, from, data)) => {
                        self.on_packet(i, from, data);
                    }
                    None => return "the sockets closed",
                },
                _ = app.drained.notified() => {}
                m = app_rx.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        heard = Instant::now();
                        self.for_host(false, t.as_bytes().to_vec());
                    }
                    Some(Ok(Message::Binary(b))) => {
                        heard = Instant::now();
                        for piece in chunk::split(&b) {
                            self.for_host(true, piece);
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => heard = Instant::now(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        self.close_peer().await;
                        return "the app closed";
                    }
                },
                _ = ping.tick() => {
                    if heard.elapsed() > APP_QUIET_MAX {
                        self.close_peer().await;
                        return "the app went quiet";
                    }
                    app.send(Message::Ping(Bytes::new()));
                }
                _ = tokio::time::sleep_until(next.into()) => {}
            }
            self.tick();
        }
    }

    /// The way the connection goes: from the pair ICE selected, or the one DTLS came over.
    fn path(&self) -> &'static str {
        self.pair
            .or(self.last)
            .map_or("internet", |(local, remote)| rtc::path_of(local, remote))
    }

    /// Why the connection is over, if it is.
    fn ended(&self) -> Option<&'static str> {
        if let Some(why) = self.broken {
            return Some(why);
        }
        if self.closed {
            return Some("the channel closed");
        }
        if !self.rtc.is_alive() {
            return Some("the connection ended");
        }
        if self
            .disconnected_at
            .is_some_and(|t| t.elapsed() > DISCONNECTED_WAIT)
        {
            return Some("the path was lost");
        }
        None
    }

    fn tick(&mut self) {
        if self.rtc.is_alive()
            && self
                .rtc
                .handle_input(Input::Timeout(Instant::now()))
                .is_err()
        {
            self.broken = Some("error");
            self.rtc.disconnect();
        }
    }

    /// Ends the peer connection (SCTP shutdown, DTLS close_notify sent at once).
    async fn close_peer(&mut self) {
        if self.rtc.is_alive() && self.rtc.close().is_err() {
            self.rtc.disconnect();
        }
        self.drive(Instant::now());
        self.send_datagrams().await;
    }

    /// The `Rtc`'s output until it waits: datagrams into the send queue, events handled. Returns
    /// when it wants its next timeout.
    fn drive(&mut self, now: Instant) -> Instant {
        loop {
            if !self.rtc.is_alive() {
                return now + Duration::from_millis(100);
            }
            match self.rtc.poll_output() {
                Ok(Output::Timeout(t)) => return t,
                Ok(Output::Transmit(t)) => {
                    if let Some(i) = self.socks.iter().position(|(_, a)| *a == t.source) {
                        self.sendq.push((i, t.destination, t.contents.into()));
                    }
                }
                Ok(Output::Event(e)) => self.on_event(e),
                Err(_) => {
                    self.broken = Some("error");
                    self.rtc.disconnect();
                    return now;
                }
            }
        }
    }

    async fn send_datagrams(&mut self) {
        for (i, to, d) in std::mem::take(&mut self.sendq) {
            let s = &self.socks[i].0;
            if let Err(e) = s.try_send_to(&d, to) {
                // A new socket the runtime has not seen writable yet, or a full buffer.
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    let _ =
                        tokio::time::timeout(Duration::from_millis(100), s.send_to(&d, to)).await;
                }
            }
        }
    }

    /// A datagram: a STUN answer (a new outside candidate, returned to be trickled), or the
    /// connection's.
    fn on_packet(&mut self, i: usize, from: SocketAddr, data: Vec<u8>) -> Option<Candidate> {
        let base = self.socks.get(i)?.1;
        if let Some(tx) = stun::response_tx(&data) {
            let ours = self
                .stun
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&tx)
                .is_some();
            if ours {
                let addr = stun::mapped(&data)?;
                if addr == base || addr.is_ipv4() != base.is_ipv4() {
                    return None; // no NAT in between: the host candidate says it already
                }
                let c = Candidate::server_reflexive(addr, base, "udp").ok()?;
                return self.rtc.add_local_candidate(c).cloned();
            }
        }
        // DTLS records (and SCTP inside them) come over the pair ICE selected only.
        if matches!(data.first(), Some(20..=63)) {
            self.last = Some((base, from));
        }
        let contents = data.as_slice().try_into().ok()?;
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source: from,
                destination: base,
                contents,
            },
        );
        if self.rtc.is_alive() && self.rtc.handle_input(input).is_err() {
            self.broken = Some("error");
            self.rtc.disconnect();
        }
        None
    }

    fn on_event(&mut self, e: Event) {
        match e {
            Event::ChannelOpen(cid, label) => {
                if label == Kind::Join.label() && self.cid.is_none() {
                    self.cid = Some(cid);
                }
            }
            Event::ChannelData(d) if Some(d.id) == self.cid => {
                self.on_host_message(d.binary, d.data)
            }
            Event::ChannelClose(id) if Some(id) == self.cid => self.closed = true,
            Event::IceConnectionStateChange(s) => {
                self.disconnected_at = match s {
                    IceConnectionState::Disconnected => {
                        Some(self.disconnected_at.unwrap_or_else(Instant::now))
                    }
                    _ => None,
                };
            }
            Event::PeerStats(s) => {
                if let Some(c) = s.selected_candidate_pair {
                    self.pair = Some((c.local.addr, c.remote.addr));
                }
            }
            _ => {}
        }
    }

    /// One data-channel message from the host.
    fn on_host_message(&mut self, binary: bool, data: Vec<u8>) {
        if !binary {
            match String::from_utf8(data) {
                Ok(t) => self.inbox.push_back(Message::Text(t.into())),
                Err(_) => self.broken = Some("text that is not UTF-8"),
            }
            return;
        }
        match self.reasm.push(&data) {
            Ok(Piece::Partial) => {}
            Ok(Piece::Message(m)) => self.inbox.push_back(Message::Binary(m.into())),
            // At once, here: its round trip is the host's measure of this guest.
            Ok(Piece::Ping(b)) => {
                let mut v = vec![chunk::PONG];
                v.extend_from_slice(&b);
                self.out_bytes += v.len();
                // Ahead of the app's messages still waiting: a pong between another binary
                // message's pieces is fine (both sides' reassemblers take it apart).
                self.out.push_front((true, v));
                self.flush();
            }
            Ok(Piece::Pong(b)) => {
                if let Some((nonce, at)) = self.ping {
                    if nonce == b {
                        self.rtt_ms = Some((at.elapsed().as_millis() as u32).max(1));
                        self.ping = None;
                    }
                }
            }
            Err(why) => self.broken = Some(why),
        }
    }

    /// A message for the host's engine onto the channel's queue.
    fn for_host(&mut self, binary: bool, data: Vec<u8>) {
        self.out_bytes += data.len();
        self.out.push_back((binary, data));
        self.flush();
    }

    /// What the channel takes from the queue.
    fn flush(&mut self) {
        let Some(cid) = self.cid else { return };
        let Some(mut ch) = self.rtc.channel(cid) else {
            return;
        };
        while let Some((binary, data)) = self.out.front() {
            match ch.write(*binary, data) {
                Ok(true) => {
                    self.out_bytes -= data.len();
                    self.out.pop_front();
                }
                Ok(false) => break,
                Err(_) => {
                    self.broken = Some("the channel refused a message");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::join::GuestSocket;
    use tokio_tungstenite::tungstenite::Message as TMsg;

    const ROOM: &str = "AbCdEfGhIjKlMnOpQrSt_-";
    const HUB: &str = "wss://hub.phantomptr.com/ws";

    fn fp() -> String {
        (0..32)
            .map(|i| format!("{:02X}", i * 7 % 256))
            .collect::<Vec<_>>()
            .join(":")
    }

    fn ask(v: serde_json::Value) -> Result<Ask, &'static str> {
        parse_ask(&serde_json::json!({ "bridge": v }).to_string(), HUB)
    }

    #[test]
    fn the_first_message_names_a_room_a_fingerprint_and_maybe_the_hub() {
        let want = Ok(Ask {
            room: ROOM.into(),
            fp: fp(),
        });
        assert_eq!(
            ask(serde_json::json!({"v": 1, "room": ROOM, "fp": fp()})),
            want
        );
        // The link's hub (its host), the hub's URL, a default port, lower-case hex: all the same.
        for hub in [
            "hub.phantomptr.com",
            "HUB.phantomptr.com:443",
            "wss://hub.phantomptr.com/ws",
            "",
        ] {
            let got = ask(serde_json::json!({
                "v": 1, "room": ROOM, "fp": fp().to_lowercase(), "hub": hub
            }));
            assert_eq!(got, want, "{hub}");
        }
        assert_eq!(
            ask(serde_json::json!({"v": 1, "room": ROOM, "fp": fp(), "hub": null})),
            want
        );
    }

    #[test]
    fn a_bad_first_message_or_another_hub_is_refused() {
        let ok = serde_json::json!({"v": 1, "room": ROOM, "fp": fp()});
        let with = |k: &str, v: serde_json::Value| {
            let mut o = ok.clone();
            o[k] = v;
            ask(o)
        };
        // Not the bridge's, or not this version.
        assert_eq!(parse_ask("not json", HUB), Err("bad"));
        assert_eq!(parse_ask(r#"{"hello":{}}"#, HUB), Err("bad"));
        assert_eq!(parse_ask(r#"{"bridge":"x"}"#, HUB), Err("bad"));
        assert_eq!(with("v", serde_json::json!(2)), Err("bad"));
        assert_eq!(
            ask(serde_json::json!({"room": ROOM, "fp": fp()})),
            Err("bad"),
            "no version"
        );
        // A room the hub would not take.
        let long = format!("{ROOM}x");
        for room in [
            "",
            "short",
            &ROOM[..21],
            long.as_str(),
            "AbCdEfGhIjKlMnOpQrSt+/",
            "AbCdEfGhIjKlMnOpQrSt =",
        ] {
            assert_eq!(with("room", serde_json::json!(room)), Err("bad"), "{room}");
        }
        assert_eq!(with("room", serde_json::json!(5)), Err("bad"));
        // No fingerprint, or not a SHA-256 one: nothing to check the host engine against.
        assert_eq!(
            ask(serde_json::json!({"v": 1, "room": ROOM})),
            Err("bad"),
            "no fp"
        );
        let short = fp()[3..].to_string();
        let not_hex = fp().replacen("00", "0G", 1);
        let no_colons = fp().replace(':', "");
        for f in ["", short.as_str(), not_hex.as_str(), no_colons.as_str()] {
            assert_eq!(with("fp", serde_json::json!(f)), Err("bad"), "{f}");
        }
        // Another hub than the settings': refused, whatever form it takes.
        for hub in [
            "hub.example.com",
            "evil.example",
            "hub.phantomptr.com:8443",
            "wss://hub.example.com/ws",
            "ws://hub.phantomptr.com/ws",
            "wss://hub.phantomptr.com.evil.example/ws",
        ] {
            assert_eq!(with("hub", serde_json::json!(hub)), Err("hub"), "{hub}");
        }
        assert_eq!(with("hub", serde_json::json!(7)), Err("bad"));
        // Settings with a hub no bridge may use (plain ws:// off this computer).
        let msg = serde_json::json!({ "bridge": ok }).to_string();
        assert_eq!(parse_ask(&msg, "ws://hub.phantomptr.com/ws"), Err("hub"));
        // A hub under test on this computer, named as the link names it.
        let local = serde_json::json!({"bridge": {"v": 1, "room": ROOM, "fp": fp(), "hub": "127.0.0.1:9000"}});
        assert!(parse_ask(&local.to_string(), "ws://127.0.0.1:9000/ws").is_ok());
        assert_eq!(
            parse_ask(&local.to_string(), "ws://127.0.0.1:9001/ws"),
            Err("hub")
        );
    }

    #[test]
    fn the_answer_carries_one_sha256_fingerprint() {
        let f = fp();
        let one = format!("v=0\r\na=fingerprint:sha-256 {f}\r\nm=application 9\r\n");
        assert_eq!(sdp_fingerprint(&one), Some(f.clone()));
        let twice = format!("{one}a=fingerprint:sha-256 {}\r\n", f.to_lowercase());
        assert_eq!(
            sdp_fingerprint(&twice),
            Some(f.clone()),
            "the same one twice"
        );
        let other = fingerprint_norm(&f.replacen("00", "01", 1)).unwrap();
        let two = format!("{one}a=fingerprint:sha-256 {other}\r\n");
        assert_eq!(sdp_fingerprint(&two), None, "two certificates");
        let sha1 = format!("{one}a=fingerprint:sha-1 AA:BB\r\n");
        assert_eq!(sdp_fingerprint(&sha1), None, "another hash");
        assert_eq!(sdp_fingerprint("v=0\r\n"), None);
        assert_eq!(first_mid("v=0\r\nm=application 9\r\na=mid:0\r\n"), "0");
    }

    #[test]
    fn two_bridges_per_address_four_in_all_ten_connects_a_minute() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let mut open = Vec::new();
        assert!(admit(&mut open, ip("192.168.1.40")));
        assert!(admit(&mut open, ip("192.168.1.40")));
        assert!(
            !admit(&mut open, ip("192.168.1.40")),
            "a third from one PS5"
        );
        // One home's IPv6 /64 is one sender.
        assert!(admit(&mut open, ip("2001:db8:1:2::10")));
        assert!(admit(&mut open, ip("2001:db8:1:2::20")));
        assert!(!admit(&mut open, ip("2001:db8:1:2::30")));
        assert_eq!(open.len(), IN_ALL);
        assert!(!admit(&mut open, ip("192.168.1.41")), "four in all");
        release(&mut open, ip("192.168.1.40"));
        assert!(admit(&mut open, ip("192.168.1.41")));
        assert!(!admit(&mut open, ip("192.168.1.42")));

        let table = crate::join::ConnectLimit::new();
        let lan = ip("192.168.1.40");
        let now = Instant::now();
        let limit = crate::join::connects_per_min(lan);
        assert_eq!(limit, 10);
        for _ in 0..10 {
            assert!(table.allow(lan, limit, now));
        }
        assert!(!table.allow(lan, limit, now), "the eleventh this minute");
        assert!(table.allow(lan, limit, now + Duration::from_secs(61)));
    }

    // ── End to end ───────────────────────────────────────────────────────────────────────────────

    /// The bridge tests that use the bridge's settings (and its per-address limit) take turns.
    static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    type App = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    /// A STUN server on loopback that says every asker is at `who`.
    async fn fake_stun(who: SocketAddr) -> SocketAddr {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        tokio::spawn(async move {
            let mut b = [0u8; 1500];
            while let Ok((n, from)) = s.recv_from(&mut b).await {
                if n >= 20 {
                    let tx: [u8; 12] = b[8..20].try_into().unwrap();
                    let _ = s.send_to(&stun::answer(tx, who), from).await;
                }
            }
        });
        addr
    }

    /// join.rs's routes on loopback; the bridge's socket.
    async fn serve() -> String {
        let app = crate::join::router::<()>();
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(l, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        format!("ws://{addr}/api/join/bridge")
    }

    async fn app(url: &str, first: serde_json::Value) -> App {
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        ws.send(TMsg::Text(first.to_string().into())).await.unwrap();
        ws
    }

    /// The app's next message (pings skipped), within 20 s; None once the socket closed.
    async fn app_next(ws: &mut App) -> Option<TMsg> {
        loop {
            match tokio::time::timeout(Duration::from_secs(20), ws.next())
                .await
                .expect("the bridge says something")
            {
                Some(Ok(TMsg::Ping(_) | TMsg::Pong(_))) => {}
                Some(Ok(TMsg::Close(_))) | None | Some(Err(_)) => return None,
                Some(Ok(m)) => return Some(m),
            }
        }
    }

    async fn app_text(ws: &mut App) -> serde_json::Value {
        match app_next(ws).await {
            Some(TMsg::Text(t)) => serde_json::from_str(t.as_str()).unwrap(),
            other => panic!("not text: {other:?}"),
        }
    }

    async fn host_next(host: &mut super::super::RtcSocket) -> Message {
        tokio::time::timeout(Duration::from_secs(10), host.recv())
            .await
            .expect("the host hears it")
            .expect("open")
            .unwrap()
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..400 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// The whole way: a real hub on loopback, the engine's own room and answerer (hub.rs, rtc.rs)
    /// as the host, the bridge, and a plain WebSocket client as PS5Upload Play.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_ps5_app_joins_an_internet_room_through_the_bridge() {
        let _turn = TURN.lock().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = ps5upload_hub::Hub::start_on(
            ps5upload_hub::Config::default(),
            Arc::new(|_: &str| {}),
            listener,
            None,
        )
        .await
        .unwrap();
        let hub_url = format!("ws://{}/ws", hub.http_addr);
        let stun = fake_stun("203.0.113.5:40000".parse().unwrap()).await;
        *TEST_CONF.lock().unwrap() = Some(Conf {
            hub_url: hub_url.clone(),
            stun: vec![stun.to_string()],
        });

        // The host: an internet room on the hub; the test holds its peers' session ends.
        let room = hub::Room::start(Kind::Join, Box::new(|| true), hub_url.clone()).unwrap();
        let id = room.id();
        let mut sessions = rtc::hold_room(&id);
        until("hosted", || room.json("K", None, None)["state"] == "hosted").await;
        let url = serve().await;
        let fp = super::super::fingerprint().unwrap();
        let hub_host = super::super::hub_host(&hub_url).unwrap();

        // PS5Upload Play: the link's room, fingerprint and hub.
        let mut ps5 = app(
            &url,
            serde_json::json!({"bridge": {"v": 1, "room": id, "fp": fp.to_lowercase(), "hub": hub_host}}),
        )
        .await;
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"bridge": "connecting"})
        );
        let open = app_text(&mut ps5).await;
        assert_eq!(open["bridge"], "open", "{open}");
        assert_eq!(open["path"], "lan", "loopback is the home network: {open}");
        assert!(open["rtt_ms"].as_u64().is_some(), "{open}");
        assert_eq!(count(), 1);
        let mut host = tokio::time::timeout(Duration::from_secs(10), sessions.recv())
            .await
            .expect("the host's channel opened")
            .unwrap();

        // What the bridge asked of the hub: the join, the offer, its STUN candidate, the leave.
        let used = LAST_HUB_USE.lock().unwrap().unwrap();
        eprintln!(
            "hub load per bridge connect: {} messages out ({} bytes), {} in ({} bytes), {} ms connected",
            used.sent,
            used.sent_bytes,
            used.got,
            used.got_bytes,
            used.connected.as_millis()
        );
        assert_eq!(used.sent, 4, "join, offer, one candidate, leave: {used:?}");
        assert!(used.connected < TRY_FOR, "{used:?}");

        // The guest protocol, unchanged both ways: the app's hello…
        let hello = serde_json::json!({"hello": {"invite": "K", "name": "PS5"}}).to_string();
        ps5.send(TMsg::Text(hello.clone().into())).await.unwrap();
        assert_eq!(host_next(&mut host).await, Message::Text(hello.into()));
        // …the host's stage…
        host.send(Message::Text(r#"{"stage":"asking"}"#.into()))
            .await
            .unwrap();
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"stage": "asking"})
        );
        // …a 200 KiB picture, whole (in pieces on the channel)…
        let big: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        host.send(Message::Binary(big.clone().into()))
            .await
            .unwrap();
        match app_next(&mut ps5).await {
            Some(TMsg::Binary(b)) => assert!(b[..] == big[..], "{} bytes", b.len()),
            other => panic!("the picture: {other:?}"),
        }
        // …and the app's binary messages, up to the guest socket's limit.
        let pad: Vec<u8> = (0..crate::join::GUEST_MSG_MAX)
            .map(|i| (i % 7) as u8)
            .collect();
        ps5.send(TMsg::Binary(pad.clone().into())).await.unwrap();
        assert_eq!(host_next(&mut host).await, Message::Binary(pad.into()));

        // The host's ping is answered by the bridge at once, and never reaches the app.
        let nonce = Bytes::from_static(&[1, 2, 3, 4, 5, 6, 7, 8]);
        host.send(Message::Ping(nonce.clone())).await.unwrap();
        assert_eq!(host_next(&mut host).await, Message::Pong(nonce));
        host.send(Message::Text(r#"{"status":1}"#.into()))
            .await
            .unwrap();
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"status": 1}),
            "nothing came before it"
        );

        // The app leaves: the host's channel closes.
        ps5.close(None).await.unwrap();
        let gone = tokio::time::timeout(Duration::from_secs(10), async {
            while host.recv().await.is_some() {}
        })
        .await;
        assert!(gone.is_ok(), "the host's channel closed");
        until("the bridge gone", || count() == 0).await;

        // A certificate other than the link's: refused (the hub left as at an open).
        let first = if &fp[..2] == "00" { "01" } else { "00" };
        let other = format!("{first}{}", &fp[2..]);
        let mut ps5 = app(
            &url,
            serde_json::json!({"bridge": {"v": 1, "room": id, "fp": other}}),
        )
        .await;
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"bridge": "connecting"})
        );
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"bridge": "failed", "why": "mismatch"})
        );
        assert!(app_next(&mut ps5).await.is_none(), "closed");
        let used = LAST_HUB_USE.lock().unwrap().unwrap();
        assert!(used.sent <= 4, "{used:?}");

        // A room that is not there.
        let mut ps5 = app(
            &url,
            serde_json::json!({"bridge": {"v": 1, "room": ROOM, "fp": fp}}),
        )
        .await;
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"bridge": "connecting"})
        );
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"bridge": "failed", "why": "no_room"})
        );
        assert!(app_next(&mut ps5).await.is_none(), "closed");

        // Another hub than the settings': never tried.
        let mut ps5 = app(
            &url,
            serde_json::json!({"bridge": {"v": 1, "room": id, "fp": fp, "hub": "hub.example.com"}}),
        )
        .await;
        assert_eq!(
            app_text(&mut ps5).await,
            serde_json::json!({"bridge": "failed", "why": "hub"})
        );
        assert!(app_next(&mut ps5).await.is_none(), "closed");
        until("no bridge left", || count() == 0).await;

        rtc::release_room(&id);
        *TEST_CONF.lock().unwrap() = None;
        drop(room);
        hub.shutdown();
    }
}
