//! Phones as controllers for the Remote Play session this engine runs.
//!
//! A phone joins the session as one more controller, next to whatever the computer's page
//! plays with: every controller's state is merged into the one pad the console gets
//! (`merge_pads`). Two ways in, both on the LAN:
//!
//! * **QR code**: the computer's page opens the link (`POST /api/ps5/rp/pad-link {open:true}`)
//!   and shows `http://<this computer>:<port>/pad#t=<key>` as a QR code. Any phone camera opens
//!   the controller page (`GET /pad`), which connects to `GET /api/pad/ws?t=<key>&name=..`.
//!   The key is 128 random bits, valid until the link is closed or the engine restarts.
//! * **Ask**: the ps5upload app on a phone sees "<computer> is using Remote Play" (mesh presence,
//!   with this engine's pad address) and connects without a key; the socket waits until the
//!   computer's page allows it (`POST /api/ps5/rp/pad-link/decide`). The phone app connects
//!   through its own engine (`/api/ps5/rp/pad-relay`): an app page may not open a plain
//!   `ws://` to another machine (Android blocks cleartext, iOS ATS), its own loopback it may.
//!
//! `/pad` and `/api/pad/` are the only routes a LAN peer reaches besides `/pkg-host/`
//! (`loopback_allows` in lib.rs), and the socket checks the key or the computer's yes itself.
//! A phone only ever sends pad states; it receives the session's state, rumble and light bar.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use ps5upload_core::rp_stream::ControllerState;
use tokio::sync::{mpsc, oneshot};

/// How long an ask waits for the computer's answer.
const ASK_WAIT: Duration = Duration::from_secs(90);
/// Asks waiting at once: more are refused (nobody clicks through a flood).
const MAX_PENDING: usize = 4;
/// Phones playing at once.
const MAX_PADS: usize = 4;
/// Watchers at once (watch party): each is a full copy of the stream on the LAN.
const MAX_WATCHERS: usize = 4;
/// How often a phone hears whether the session is playing.
const STATE_EVERY: Duration = Duration::from_secs(1);

/// Merge controller states into the one pad the console gets: buttons from all, the deeper
/// trigger, and for each stick axis the one pushed furthest (a resting controller never cancels
/// another). The first finger on a touchpad, and the first controller with motion, win.
pub fn merge_pads<'a>(states: impl IntoIterator<Item = &'a ControllerState>) -> ControllerState {
    let mut m = ControllerState::default();
    let far = |cur: i16, v: i16| {
        if v.unsigned_abs() > cur.unsigned_abs() {
            v
        } else {
            cur
        }
    };
    for s in states {
        m.buttons |= s.buttons;
        m.l2 = m.l2.max(s.l2);
        m.r2 = m.r2.max(s.r2);
        m.lx = far(m.lx, s.lx);
        m.ly = far(m.ly, s.ly);
        m.rx = far(m.rx, s.rx);
        m.ry = far(m.ry, s.ry);
        if m.touch.is_none() {
            m.touch = s.touch;
        }
        // Motion comes from the one controller that has sensors (a phone).
        if m.motion.is_none() {
            m.motion = s.motion;
        }
    }
    m
}

/// Every controller's latest state, by source (0 = the computer's page, then one per phone).
#[derive(Default)]
pub struct PadSources(Mutex<BTreeMap<u64, ControllerState>>);

impl PadSources {
    /// Record `source`'s state (None: it left) and return the merged pad.
    pub fn set(&self, source: u64, state: Option<ControllerState>) -> ControllerState {
        let Ok(mut g) = self.0.lock() else {
            return ControllerState::default();
        };
        match state {
            Some(s) => {
                g.insert(source, s);
            }
            None => {
                g.remove(&source);
            }
        }
        merge_pads(g.values())
    }
}

struct Pad {
    id: u64,
    name: String,
    since_ms: u128,
    tx: mpsc::Sender<Message>,
    /// A watcher (watch party): it sees the stream, and plays only once handed the controller.
    watch: bool,
    /// The computer handed this watcher the controller.
    control: bool,
}

struct Pending {
    id: u64,
    name: String,
    answer: oneshot::Sender<bool>,
}

#[derive(Default)]
struct Link {
    /// The QR code's key, while the link is open.
    key: Option<String>,
    pads: Vec<Pad>,
    pending: Vec<Pending>,
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);
/// Source ids for phones (0 is the computer's page).
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn with_link<R>(f: impl FnOnce(&mut Link) -> R) -> R {
    let mut g = LINK.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Link::default))
}

fn random_key() -> Option<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Equal without leaking where the first difference is.
fn same_key(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// This machine's address on the network that reaches `toward` (the console: phones are on
/// that network). A UDP connect sends nothing; it only makes the system pick the interface.
fn lan_ip_toward(toward: &str) -> Option<std::net::IpAddr> {
    let host = ps5upload_core::events::console_host(toward);
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((host.as_str(), 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// The port this engine answers on, from the request the computer's page made to it.
fn port_of(headers: &HeaderMap) -> u16 {
    headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.rsplit_once(':'))
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(19113)
}

/// A home network's address (10/8, 172.16/12, 192.168/16): where a phone on the same Wi-Fi
/// could reach this computer. Not a VPN's or a public one.
fn is_home_lan(ip: std::net::IpAddr) -> bool {
    matches!(ip, std::net::IpAddr::V4(v4) if v4.is_private())
}

/// This machine's addresses a phone may reach, best first: on the console's network (where
/// phones usually are), then on the network the default route goes out on. A computer on two
/// networks (the PS5 on one, the phone's Wi-Fi on the other) needs the second: the QR code
/// carried only the first, which the phone could not reach.
fn lan_ips(console: Option<&str>) -> Vec<std::net::IpAddr> {
    let toward = console
        .map(str::to_string)
        .or_else(crate::rp_stream_api::live_host);
    let mut out = Vec::new();
    if let Some(ip) = toward.as_deref().and_then(lan_ip_toward) {
        out.push(ip);
    }
    // Any outside address picks the default route's interface; nothing is sent.
    if let Some(ip) = lan_ip_toward("1.1.1.1").filter(|ip| is_home_lan(*ip)) {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Where phones reach this engine, toward the console being played (or `console`).
fn base_url(headers: &HeaderMap, console: Option<&str>) -> Option<String> {
    let ip = *lan_ips(console).first()?;
    Some(format!("http://{ip}:{}", port_of(headers)))
}

fn status(headers: &HeaderMap, console: Option<&str>) -> serde_json::Value {
    let base = base_url(headers, console);
    let port = port_of(headers);
    let ips = lan_ips(console);
    with_link(|l| {
        serde_json::json!({
            "open": l.key.is_some(),
            "url": match (&base, &l.key) {
                (Some(b), Some(k)) => serde_json::Value::String(format!("{b}/pad#t={k}")),
                _ => serde_json::Value::Null,
            },
            // Every address, best first (the first is `url`): for a computer on two networks.
            "urls": match &l.key {
                Some(k) => ips
                    .iter()
                    .map(|ip| serde_json::Value::String(format!("http://{ip}:{port}/pad#t={k}")))
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            },
            // What the phone app connects to (it asks; the computer allows).
            "pad_ws": base.as_ref().map(|b| format!("{}/api/pad/ws", b.replacen("http://", "ws://", 1))),
            "pads": l.pads.iter().map(|p| serde_json::json!({"id": p.id, "name": p.name, "since_ms": p.since_ms, "watch": p.watch, "control": p.control})).collect::<Vec<_>>(),
            "pending": l.pending.iter().map(|p| serde_json::json!({"id": p.id, "name": p.name})).collect::<Vec<_>>(),
        })
    })
}

#[derive(serde::Deserialize, Default)]
pub struct StatusQuery {
    #[serde(default)]
    console: Option<String>,
}

/// `GET /api/ps5/rp/pad-link?console=`: the link, the phones playing and the phones asking.
pub async fn get_status(
    headers: HeaderMap,
    Query(q): Query<StatusQuery>,
) -> Json<serde_json::Value> {
    Json(status(&headers, q.console.as_deref()))
}

#[derive(serde::Deserialize)]
pub struct OpenReq {
    open: bool,
    #[serde(default)]
    console: Option<String>,
}

/// `POST /api/ps5/rp/pad-link {open, console?}`: open the QR link (a new key), or close it,
/// which also lets every phone go.
pub async fn set_open(headers: HeaderMap, Json(r): Json<OpenReq>) -> Json<serde_json::Value> {
    with_link(|l| {
        if r.open {
            if l.key.is_none() {
                l.key = random_key();
            }
        } else {
            l.key = None;
            for p in l.pads.drain(..) {
                let _ = p.tx.try_send(Message::Close(None));
            }
            for p in l.pending.drain(..) {
                let _ = p.answer.send(false);
            }
        }
    });
    Json(status(&headers, r.console.as_deref()))
}

#[derive(serde::Deserialize)]
pub struct DecideReq {
    id: u64,
    allow: bool,
}

/// `POST /api/ps5/rp/pad-link/decide {id, allow}`: answer a phone that asked.
pub async fn decide(Json(r): Json<DecideReq>) -> Json<serde_json::Value> {
    let found = with_link(|l| {
        let i = l.pending.iter().position(|p| p.id == r.id)?;
        Some(l.pending.remove(i))
    });
    if let Some(p) = found {
        let _ = p.answer.send(r.allow);
    }
    Json(serde_json::json!({"ok": true}))
}

#[derive(serde::Deserialize)]
pub struct KickReq {
    id: u64,
}

/// `POST /api/ps5/rp/pad-link/kick {id}`: let a phone go.
pub async fn kick(Json(r): Json<KickReq>) -> Json<serde_json::Value> {
    with_link(|l| {
        if let Some(i) = l.pads.iter().position(|p| p.id == r.id) {
            let p = l.pads.remove(i);
            let _ = p.tx.try_send(Message::Close(None));
        }
    });
    Json(serde_json::json!({"ok": true}))
}

#[derive(serde::Deserialize)]
pub struct ControlReq {
    id: u64,
    on: bool,
}

/// `POST /api/ps5/rp/pad-link/control {id, on}`: hand a watcher the controller, or take it back.
pub async fn control(Json(r): Json<ControlReq>) -> Json<serde_json::Value> {
    let ok = with_link(|l| {
        let Some(p) = l.pads.iter_mut().find(|p| p.id == r.id && p.watch) else {
            return false;
        };
        p.control = r.on;
        let _ = p.tx.try_send(say(serde_json::json!({ "control": r.on })));
        true
    });
    if ok && !r.on {
        // Let go of whatever it held.
        crate::rp_stream_api::push_pad(r.id, None);
    }
    Json(serde_json::json!({ "ok": ok }))
}

/// Tell every phone something (rumble, light bar).
pub fn broadcast(text: &str) {
    let Ok(g) = LINK.lock() else {
        return;
    };
    if let Some(l) = g.as_ref() {
        for p in &l.pads {
            let _ = p.tx.try_send(Message::Text(text.to_string().into()));
        }
    }
}

/// `GET /pad`: the controller page a phone opens from the QR code.
pub async fn page() -> impl IntoResponse {
    Html(include_str!("pad_page.html"))
}

#[derive(serde::Deserialize)]
pub struct PadQuery {
    #[serde(default)]
    t: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// `w=1`: join to watch (watch party), not to play.
    #[serde(default)]
    w: Option<u8>,
}

/// A phone's name as it gave it: printable, short.
fn clean_name(n: Option<&str>) -> String {
    let n: String = n
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    let n = n.trim();
    if n.is_empty() {
        "Phone".into()
    } else {
        n.to_string()
    }
}

/// `GET /api/pad/ws?t=&name=`: a phone's controller socket.
pub async fn ws(upgrade: WebSocketUpgrade, Query(q): Query<PadQuery>) -> Response {
    upgrade.on_upgrade(move |s| pad_session(s, q))
}

fn say(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

async fn pad_session(mut socket: WebSocket, q: PadQuery) {
    let name = clean_name(q.name.as_deref());
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let keyed =
        q.t.as_deref()
            .is_some_and(|t| with_link(|l| l.key.as_deref().is_some_and(|k| same_key(k, t))));
    if !keyed {
        if q.t.is_some() {
            // A QR code from a link closed since (or a guess).
            let _ = socket
                .send(say(serde_json::json!({"stage": "refused", "why": "key"})))
                .await;
            return;
        }
        // Asking: the computer's page shows it; wait for the answer.
        let (tx, rx) = oneshot::channel();
        let queued = with_link(|l| {
            if l.pending.len() >= MAX_PENDING {
                return false;
            }
            l.pending.push(Pending {
                id,
                name: name.clone(),
                answer: tx,
            });
            true
        });
        if !queued {
            let _ = socket
                .send(say(serde_json::json!({"stage": "refused", "why": "busy"})))
                .await;
            return;
        }
        let _ = socket
            .send(say(serde_json::json!({"stage": "asking"})))
            .await;
        let allowed = tokio::select! {
            a = rx => a.unwrap_or(false),
            _ = tokio::time::sleep(ASK_WAIT) => false,
            // The phone gave up.
            _ = async { loop { match socket.recv().await { Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break, _ => {} } } } => false,
        };
        with_link(|l| l.pending.retain(|p| p.id != id));
        if !allowed {
            let _ = socket
                .send(say(
                    serde_json::json!({"stage": "refused", "why": "denied"}),
                ))
                .await;
            return;
        }
    }
    let watch = q.w == Some(1);
    // A watcher gets the stream itself (rp_stream_api's copy for it); none without a session.
    let mut frames = if watch {
        match crate::rp_stream_api::add_spectator(id) {
            Some(r) => Some(r),
            None => {
                let _ = socket
                    .send(say(
                        serde_json::json!({"stage": "refused", "why": "no_stream"}),
                    ))
                    .await;
                return;
            }
        }
    } else {
        None
    };
    let (tx, mut rx) = mpsc::channel::<Message>(64);
    let joined = with_link(|l| {
        let (cap, count) = if watch {
            (MAX_WATCHERS, l.pads.iter().filter(|p| p.watch).count())
        } else {
            (MAX_PADS, l.pads.iter().filter(|p| !p.watch).count())
        };
        if count >= cap {
            return false;
        }
        let since_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        l.pads.push(Pad {
            id,
            name: name.clone(),
            since_ms,
            tx,
            watch,
            control: false,
        });
        true
    });
    if !joined {
        if watch {
            crate::rp_stream_api::remove_spectator(id);
        }
        let _ = socket
            .send(say(serde_json::json!({"stage": "refused", "why": "full"})))
            .await;
        return;
    }
    crate::log_info!(
        "pad_link: {name} joined as {} {id}",
        if watch { "watcher" } else { "controller" }
    );
    let mut tick = tokio::time::interval(STATE_EVERY);
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(Message::Close(_)) | None => break,
                Some(m) => if socket.send(m).await.is_err() { break },
            },
            // A watcher's picture and sound (pending forever for a controller).
            f = async {
                match frames.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => match f {
                Some(m) => if socket.send(m).await.is_err() { break },
                None => frames = None,
            },
            _ = tick.tick() => {
                let s = crate::rp_stream_api::live_state();
                if socket.send(say(serde_json::json!({"stage": "joined", "session": s}))).await.is_err() {
                    break;
                }
            },
            inc = socket.recv() => match inc {
                Some(Ok(Message::Text(t))) => {
                    #[derive(serde::Deserialize)]
                    struct In { #[serde(default)] pad: Option<ControllerState> }
                    if let Ok(In { pad: Some(p) }) = serde_json::from_str::<In>(t.as_str()) {
                        // A watcher plays only while it holds the controller.
                        let plays = with_link(|l| l.pads.iter().any(|x| x.id == id && (!x.watch || x.control)));
                        if plays {
                            crate::rp_stream_api::push_pad(id, Some(p));
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
    crate::rp_stream_api::push_pad(id, None);
    if watch {
        crate::rp_stream_api::remove_spectator(id);
    }
    with_link(|l| l.pads.retain(|p| p.id != id));
    crate::log_info!("pad_link: controller {id} ({name}) left");
}

#[derive(serde::Deserialize)]
pub struct RelayQuery {
    url: String,
    #[serde(default)]
    name: Option<String>,
}

/// Only another engine's pad socket on the LAN may be relayed to.
fn relay_target(url: &str) -> Option<String> {
    let rest = url.strip_prefix("ws://")?;
    let (authority, path) = rest.split_once('/')?;
    if path.split('?').next() != Some("api/pad/ws") {
        return None;
    }
    let host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority);
    let ip: std::net::IpAddr = host.trim_matches(['[', ']']).parse().ok()?;
    let private = match ip {
        std::net::IpAddr::V4(v) => v.is_private() || v.is_link_local(),
        std::net::IpAddr::V6(v) => {
            (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    };
    private.then(|| format!("ws://{authority}/api/pad/ws"))
}

/// `GET /api/ps5/rp/pad-relay?url=ws://<computer>/api/pad/ws&name=` (loopback only): the phone
/// app's controller page talks to this, and this to the computer's engine.
pub async fn relay(upgrade: WebSocketUpgrade, Query(q): Query<RelayQuery>) -> Response {
    upgrade.on_upgrade(move |s| relay_session(s, q))
}

async fn relay_session(mut socket: WebSocket, q: RelayQuery) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMsg;
    let Some(target) = relay_target(&q.url) else {
        let _ = socket
            .send(say(
                serde_json::json!({"stage": "refused", "why": "address"}),
            ))
            .await;
        return;
    };
    // The computer shows the asking phone by the name the consoles list it under.
    let name = clean_name(Some(
        q.name.unwrap_or_else(crate::mesh_api::device_name).as_str(),
    ));
    let url = format!(
        "{target}?name={}",
        name.bytes()
            .map(|b| if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            })
            .collect::<String>()
    );
    let far = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(url.as_str()),
    )
    .await
    {
        Ok(Ok((s, _))) => s,
        _ => {
            let _ = socket
                .send(say(
                    serde_json::json!({"stage": "refused", "why": "unreachable"}),
                ))
                .await;
            return;
        }
    };
    let (mut far_tx, mut far_rx) = far.split();
    loop {
        tokio::select! {
            m = far_rx.next() => match m {
                Some(Ok(TMsg::Text(t))) => if socket.send(Message::Text(t.as_str().into())).await.is_err() { break },
                Some(Ok(TMsg::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            m = socket.recv() => match m {
                Some(Ok(Message::Text(t))) => if far_tx.send(TMsg::Text(t.as_str().into())).await.is_err() { break },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
    let _ = far_tx.send(TMsg::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_is_offered_home_network_addresses_only() {
        use std::net::IpAddr;
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert!(is_home_lan(ip("192.168.0.34")));
        assert!(is_home_lan(ip("10.0.0.5")));
        assert!(is_home_lan(ip("172.20.1.2")));
        assert!(!is_home_lan(ip("100.64.1.2"))); // a VPN (CGNAT range)
        assert!(!is_home_lan(ip("8.8.8.8")));
        assert!(!is_home_lan(ip("fe80::1")));
    }

    #[test]
    fn the_console_network_address_comes_first() {
        // Toward a console on this machine: loopback is never offered, and the list never repeats.
        let ips = lan_ips(Some("127.0.0.1"));
        assert!(ips.iter().all(|ip| !ip.is_loopback()));
        let mut seen = ips.clone();
        seen.dedup();
        assert_eq!(seen.len(), ips.len());
    }

    fn st(buttons: u32, l2: u8, lx: i16, ry: i16) -> ControllerState {
        ControllerState {
            buttons,
            l2,
            lx,
            ry,
            ..Default::default()
        }
    }

    #[test]
    fn merged_pad_takes_every_press_and_the_furthest_stick() {
        let page = st(1, 0, 0, 0); // ✕ on the computer
        let phone = st(4, 200, -20000, 3000); // □, L2, stick left
        let m = merge_pads([&page, &phone]);
        assert_eq!(m.buttons, 5);
        assert_eq!(m.l2, 200);
        assert_eq!(m.lx, -20000);
        assert_eq!(m.ry, 3000);
        // A resting controller never cancels one in use.
        let rest = ControllerState::default();
        assert_eq!(merge_pads([&phone, &rest]).lx, -20000);
    }

    #[test]
    fn a_source_that_leaves_lets_go_of_its_buttons() {
        let s = PadSources::default();
        s.set(0, Some(st(1, 0, 0, 0)));
        assert_eq!(s.set(7, Some(st(2, 0, 0, 0))).buttons, 3);
        assert_eq!(s.set(7, None).buttons, 1);
        assert_eq!(s.set(0, None).buttons, 0);
    }

    #[test]
    fn keys_compare_whole() {
        assert!(same_key("ab12", "ab12"));
        assert!(!same_key("ab12", "ab13"));
        assert!(!same_key("ab12", "ab1"));
        assert_eq!(random_key().map(|k| k.len()), Some(32));
    }

    #[test]
    fn names_are_printable_and_short() {
        assert_eq!(clean_name(Some("  Pixel 8\u{7}  ")), "Pixel 8");
        assert_eq!(clean_name(None), "Phone");
        assert_eq!(clean_name(Some(&"x".repeat(100))).len(), 40);
    }

    #[test]
    fn relay_goes_only_to_a_pad_socket_on_the_lan() {
        assert_eq!(
            relay_target("ws://192.168.1.20:19113/api/pad/ws"),
            Some("ws://192.168.1.20:19113/api/pad/ws".into())
        );
        assert_eq!(relay_target("ws://192.168.1.20:19113/api/ps5/rp/ws"), None);
        assert_eq!(relay_target("ws://8.8.8.8:19113/api/pad/ws"), None);
        assert_eq!(relay_target("wss://192.168.1.20/api/pad/ws"), None);
        assert_eq!(relay_target("ws://example.com/api/pad/ws"), None);
    }

    #[test]
    fn port_comes_from_the_host_header() {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:19114".parse().unwrap());
        assert_eq!(port_of(&h), 19114);
        assert_eq!(port_of(&HeaderMap::new()), 19113);
    }
}
