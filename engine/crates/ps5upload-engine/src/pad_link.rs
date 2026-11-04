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
//! A phone sends pad states and, for its touchpad & keyboard, native keyboard & mouse input
//! (`{"kbm": {"on", "mouse", "keyboard"}}` repeated every 500 ms as a heartbeat, and binary
//! `[8][16-byte report]`). That input is only forwarded to the computer's page as `{"pad_kbm":
//! ..}` (rp_stream_api.rs `push_pad_kbm`): the page stays the one owner of the console's keyboard
//! and mouse and merges a phone's into its own. A phone receives the session's state, rumble,
//! light bar, and `{"kbm_caps": {"mouse", "keyboard", "reason"}}` (what the console being played
//! can make, asked again when that console changes).

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

pub(crate) fn random_key() -> Option<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Equal without leaking where the first difference is.
pub(crate) fn same_key(a: &str, b: &str) -> bool {
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
pub(crate) fn port_of(headers: &HeaderMap) -> u16 {
    headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.rsplit_once(':'))
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or_else(listen_port)
}

/// The port this engine listens on, as the rest of the engine reads it (pkg_install.rs).
fn listen_port() -> u16 {
    std::env::var("PS5UPLOAD_ENGINE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(19113)
}

/// The request came through a reverse proxy: its `Host` port is the proxy's, and nothing listens
/// on that port at this engine's own addresses. (The engine has no TLS of its own, so an https
/// page is always proxied.)
fn proxied(headers: &HeaderMap) -> bool {
    [
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "forwarded",
    ]
    .iter()
    .any(|h| headers.contains_key(*h))
        || headers
            .get("origin")
            .and_then(|o| o.to_str().ok())
            .is_some_and(|o| o.to_ascii_lowercase().starts_with("https://"))
}

/// The port for links that name this engine's own addresses.
pub(crate) fn lan_port(headers: &HeaderMap) -> u16 {
    if proxied(headers) {
        listen_port()
    } else {
        port_of(headers)
    }
}

/// `http://<ip>:<port>`, IPv6 in brackets.
pub(crate) fn ip_base(ip: std::net::IpAddr, port: u16) -> String {
    format!("http://{}", std::net::SocketAddr::new(ip, port))
}

/// What the page's own address says to send people to: scheme, name and port as the browser sees
/// them, from `Origin` (already compared with `Host` by the origin guard), else from `Host` and a
/// proxy's `X-Forwarded-*`. None for this computer's loopback names, which mean something else on
/// every other device, and for what cannot be part of a link.
pub(crate) fn web_base(headers: &HeaderMap) -> Option<String> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let base = text("origin")
        .filter(|o| {
            let o = o.to_ascii_lowercase();
            o.starts_with("http://") || o.starts_with("https://")
        })
        .map(str::to_string)
        .or_else(|| {
            let host = crate::host_guard::first_forwarded(headers, "x-forwarded-host")
                .or_else(|| text("host"))?
                .trim();
            let scheme = crate::host_guard::first_forwarded(headers, "x-forwarded-proto")
                .filter(|p| p.eq_ignore_ascii_case("https"))
                .map_or("http", |_| "https");
            Some(format!("{scheme}://{host}"))
        })?;
    let (_, authority) = base.split_once("://")?;
    let usable = !authority.is_empty()
        && authority.len() <= 255
        && authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b));
    let (name, _) = crate::host_guard::split_authority(authority);
    let local = name == "localhost"
        || name.ends_with(".localhost")
        || name
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified());
    (usable && !local).then_some(base)
}

/// `PS5UPLOAD_PKG_HOST_IP`, when it is an address.
fn pinned_ip() -> Option<std::net::IpAddr> {
    std::env::var("PS5UPLOAD_PKG_HOST_IP")
        .ok()?
        .trim()
        .parse()
        .ok()
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
pub(crate) fn lan_ips(console: Option<&str>) -> Vec<std::net::IpAddr> {
    let toward = console
        .map(str::to_string)
        .or_else(crate::rp_stream_api::live_host);
    let mut out = Vec::new();
    // The address the operator pinned for this machine (a bridged container: the routing guesses
    // below only find the container's own address, which nothing outside it can reach).
    if let Some(ip) = pinned_ip() {
        out.push(ip);
        if crate::pkg_install::in_container() {
            return out;
        }
    }
    if let Some(ip) = toward.as_deref().and_then(lan_ip_toward) {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    // Any outside address picks the default route's interface; nothing is sent.
    if let Some(ip) = lan_ip_toward("1.1.1.1").filter(|ip| is_home_lan(*ip)) {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Tailscale's shared range (100.64.0.0/10), where every tailnet address is.
fn in_tailnet_range(ip: std::net::IpAddr) -> bool {
    matches!(ip, std::net::IpAddr::V4(v4) if v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
}

/// The address `via` (what reaches Tailscale's own 100.100.100.100) is this computer's tailnet
/// address when it is in Tailscale's range and is not simply the default route's address too
/// (a carrier's CGNAT can hand a computer a 100.64/10 address of its own).
fn tailnet_choice(
    via: Option<std::net::IpAddr>,
    default: Option<std::net::IpAddr>,
) -> Option<std::net::IpAddr> {
    via.filter(|ip| in_tailnet_range(*ip) && default != Some(*ip))
}

/// This computer's Tailscale address, while the Tailscale app is connected: friends on Tailscale
/// (this computer shared with them) reach the engine there. The system picks the interface
/// toward Tailscale's 100.100.100.100, which only the Tailscale interface routes; nothing is sent.
pub(crate) fn tailscale_ip() -> Option<std::net::IpAddr> {
    tailnet_choice(lan_ip_toward("100.100.100.100"), lan_ip_toward("1.1.1.1"))
}

/// Virtual adapters nobody joins through (containers, virtual machines): their addresses would
/// only be noise among the choices.
fn virtual_adapter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "docker",
        "br-",
        "veth",
        "vethernet",
        "vmnet",
        "vboxnet",
        "virbr",
        "lxc",
        "lxd",
        "cni",
        "flannel",
        "podman",
        "vmware",
        "virtualbox",
        "hyper-v",
        "default switch",
        "wsl",
        "bridge10",
    ]
    .iter()
    .any(|p| n.starts_with(p) || n.contains(&format!("({p}")))
}

/// A private address someone could reach this computer on through a VPN of their own
/// (WireGuard, OpenVPN, ZeroTier…): home-network ranges and the shared 100.64/10 range.
fn vpn_reachable(ip: std::net::Ipv4Addr) -> bool {
    ip.is_private() || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64)
}

/// This computer's other private addresses, from its network interfaces: a WireGuard (or other
/// VPN) interface a friend reaches it on. Not loopback, not link-local, not virtual adapters, and
/// not `skip` (the addresses already offered).
fn other_private_ips(skip: &[std::net::IpAddr]) -> Vec<std::net::IpAddr> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    let mut out: Vec<std::net::IpAddr> = Vec::new();
    for i in ifaces {
        let std::net::IpAddr::V4(v4) = i.ip() else {
            continue;
        };
        let ip = std::net::IpAddr::V4(v4);
        if i.is_loopback()
            || v4.is_link_local()
            || !vpn_reachable(v4)
            || virtual_adapter(&i.name)
            || skip.contains(&ip)
            || out.contains(&ip)
        {
            continue;
        }
        out.push(ip);
    }
    out
}

/// Every address of this computer a phone or friend may use, best first: the home network's
/// (`lan_ips`), then other private ones (a VPN such as WireGuard), then Tailscale's last.
pub(crate) fn reach_ips(
    console: Option<&str>,
) -> (Vec<std::net::IpAddr>, Option<std::net::IpAddr>) {
    let mut ips = lan_ips(console);
    let tailscale = tailscale_ip();
    let mut skip = ips.clone();
    skip.extend(tailscale);
    // Inside a container with a pinned address, its other addresses are its own.
    if !(pinned_ip().is_some() && crate::pkg_install::in_container()) {
        ips.extend(other_private_ips(&skip));
    }
    if let Some(ip) = tailscale.filter(|ip| !ips.contains(ip)) {
        ips.push(ip);
    }
    (ips, tailscale)
}

/// Where a phone is sent, best first. A QR code goes to this machine's address, since a phone may
/// not resolve the name the computer's browser used (`.local` on Android). The name comes first
/// when those addresses cannot work (a bridged container with no pinned address) or when the page
/// is on https, which only the name's certificate can serve.
fn phone_bases(web: Option<String>, ip_bases: Vec<String>, name_first: bool) -> Vec<String> {
    let mut out = Vec::new();
    let web_first = name_first || web.as_deref().is_some_and(|w| w.starts_with("https://"));
    if web_first {
        out.extend(web.clone());
    }
    for b in ip_bases {
        if !out.contains(&b) {
            out.push(b);
        }
    }
    if let Some(w) = web {
        if !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

fn status(headers: &HeaderMap, console: Option<&str>) -> serde_json::Value {
    let port = lan_port(headers);
    // Home network first (the QR code's), then a VPN's, Tailscale's last: the host picks.
    let (ips, tailscale) = reach_ips(console);
    let ip_bases: Vec<String> = ips.iter().map(|ip| ip_base(*ip, port)).collect();
    let prefix = crate::host_guard::base_path();
    // What the phone app connects to: an address, which its engine's relay accepts as is.
    let pad_ws = ip_bases
        .first()
        .map(|b| format!("{}{prefix}/api/pad/ws", b.replacen("http://", "ws://", 1)));
    let bases = phone_bases(
        web_base(headers),
        ip_bases,
        crate::pkg_install::bridged_container_without_pkg_host_ip(),
    );
    with_link(|l| {
        serde_json::json!({
            "open": l.key.is_some(),
            "url": match (bases.first(), &l.key) {
                (Some(b), Some(k)) => serde_json::Value::String(format!("{b}{prefix}/pad#t={k}")),
                _ => serde_json::Value::Null,
            },
            // This computer's Tailscale address (one of `urls`), for the page to label.
            "tailscale": tailscale.map(|ip| ip.to_string()),
            // Every address, best first (the first is `url`): for a computer on two networks.
            "urls": match &l.key {
                Some(k) => bases
                    .iter()
                    .map(|b| serde_json::Value::String(format!("{b}{prefix}/pad#t={k}")))
                    .collect::<Vec<_>>(),
                None => Vec::new(),
            },
            // What the phone app connects to (it asks; the computer allows).
            "pad_ws": pad_ws,
            "pads": l.pads.iter().map(|p| serde_json::json!({"id": p.id, "name": p.name, "since_ms": p.since_ms, "watch": p.watch, "control": p.control})).collect::<Vec<_>>(),
            "pending": l.pending.iter().map(|p| serde_json::json!({"id": p.id, "name": p.name})).collect::<Vec<_>>(),
        })
    })
}

/// Phone controllers and the watch party for a bug report: counts and switches, never the link's
/// key, an address or a phone's name. `tailscale` says only whether a Tailscale address exists.
pub(crate) fn diag_summary() -> serde_json::Value {
    let tailscale = tailscale_ip().is_some();
    with_link(|l| diag_of(l, tailscale))
}

fn diag_of(l: &Link, tailscale: bool) -> serde_json::Value {
    serde_json::json!({
        "open": l.key.is_some(),
        "phones": l.pads.iter().filter(|p| !p.watch).count(),
        "watchers": l.pads.iter().filter(|p| p.watch).count(),
        "watchers_in_control": l.pads.iter().filter(|p| p.watch && p.control).count(),
        "asking": l.pending.len(),
        "tailscale_ip_present": tailscale,
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
        crate::rp_stream_api::push_pad_kbm(kbm_left(r.id));
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

/// A phone's `{"kbm": ..}` for the page.
fn kbm_control(id: u64, m: &crate::vinput_api::KbmMsg) -> serde_json::Value {
    serde_json::json!({"id": id, "on": m.on, "mouse": m.mouse, "keyboard": m.keyboard})
}

/// A phone's binary `[8][16-byte report]` for the page; None for any other frame.
fn kbm_report(id: u64, frame: &[u8]) -> Option<serde_json::Value> {
    use ps5upload_core::vinput::REPORT_LEN;
    let report = frame.strip_prefix(&[crate::vinput_api::KIND_KBM])?;
    if report.len() != REPORT_LEN {
        return None;
    }
    let hex: String = report.iter().map(|b| format!("{b:02x}")).collect();
    Some(serde_json::json!({"id": id, "r": hex}))
}

/// The phone left (or lost the controller): the page lets go of whatever it held.
fn kbm_left(id: u64) -> serde_json::Value {
    serde_json::json!({"id": id, "left": true})
}

/// What the phone is told about the console's keyboard & mouse (as vinput_api `caps` reads it).
fn kbm_caps(c: anyhow::Result<Option<ps5upload_core::vinput::Caps>>) -> serde_json::Value {
    let (mouse, keyboard, reason) = match c {
        Ok(Some(c)) => (c.mouse, c.keyboard, c.reason),
        Ok(None) => (false, false, Some("vinput_helper_old".to_string())),
        Err(_) => (false, false, Some("unreachable".to_string())),
    };
    serde_json::json!({"kbm_caps": {"mouse": mouse, "keyboard": keyboard, "reason": reason}})
}

/// Ask the console `host` what keyboard & mouse it can make, off the socket loop, and tell the
/// phone through its queue (nothing once it left).
fn ask_kbm_caps(host: String, tx: mpsc::WeakSender<Message>) {
    tokio::spawn(async move {
        let c = tokio::task::spawn_blocking(move || ps5upload_core::vinput::caps(&host))
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!(e.to_string())));
        if let Some(tx) = tx.upgrade() {
            let _ = tx.try_send(say(kbm_caps(c)));
        }
    });
}

/// A watcher plays only while it holds the controller.
fn plays(id: u64) -> bool {
    with_link(|l| l.pads.iter().any(|x| x.id == id && (!x.watch || x.control)))
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
    // Weak: the session still ends when the link drops the phone's sender.
    let caps_tx = tx.downgrade();
    // The console the phone was last told kbm_caps for.
    let mut caps_host: Option<String> = None;
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
                let host = crate::rp_stream_api::live_host();
                if host.is_some() && host != caps_host {
                    caps_host = host.clone();
                    ask_kbm_caps(host.unwrap_or_default(), caps_tx.clone());
                }
                if socket.send(say(serde_json::json!({"stage": "joined", "session": s}))).await.is_err() {
                    break;
                }
            },
            inc = socket.recv() => match inc {
                Some(Ok(Message::Text(t))) => {
                    #[derive(serde::Deserialize)]
                    struct In {
                        #[serde(default)] pad: Option<ControllerState>,
                        #[serde(default)] kbm: Option<crate::vinput_api::KbmMsg>,
                    }
                    if let Ok(m) = serde_json::from_str::<In>(t.as_str()) {
                        if let Some(p) = m.pad {
                            if plays(id) {
                                crate::rp_stream_api::push_pad(id, Some(p));
                            }
                        }
                        if let Some(k) = m.kbm {
                            if plays(id) {
                                crate::rp_stream_api::push_pad_kbm(kbm_control(id, &k));
                            }
                        }
                    }
                }
                Some(Ok(Message::Binary(b))) => {
                    if let Some(v) = kbm_report(id, &b) {
                        if plays(id) {
                            crate::rp_stream_api::push_pad_kbm(v);
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
    crate::rp_stream_api::push_pad(id, None);
    crate::rp_stream_api::push_pad_kbm(kbm_left(id));
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
    /// The computer's link key (a QR code or link opened in the app): in without asking.
    #[serde(default)]
    t: Option<String>,
    /// `w=1`: join to watch.
    #[serde(default)]
    w: Option<u8>,
}

/// The pad socket's query for a relayed phone: its name, then the link's key and watch flag when
/// it came from a link. A key is passed only when it looks like one (hex, as `random_key` makes).
fn relay_query(name: &str, t: Option<&str>, watch: bool) -> String {
    let mut q = format!(
        "?name={}",
        name.bytes()
            .map(|b| if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            })
            .collect::<String>()
    );
    if let Some(t) =
        t.filter(|t| (8..=128).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        q.push_str("&t=");
        q.push_str(t);
    }
    if watch {
        q.push_str("&w=1");
    }
    q
}

/// Only another engine's pad socket on the LAN may be relayed to.
fn relay_target(url: &str) -> Option<String> {
    lan_ws_target(url, "api/pad/ws")
}

/// A private address: a home network, link-local, or a Tailscale-style shared 100.64/10 one.
fn is_lan_ip(ip: std::net::IpAddr) -> bool {
    match ip.to_canonical() {
        std::net::IpAddr::V4(v) => {
            v.is_private()
                || v.is_link_local()
                || (v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64)
        }
        std::net::IpAddr::V6(v) => {
            (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// A DNS name a relay may look up: letters, digits and hyphens in dot-separated labels, and not
/// a number in disguise (`192.168.010.5`, `3232235781`) or this computer's own name.
fn lan_name(name: &str) -> bool {
    let labels_ok = !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    let last_is_word = name
        .rsplit('.')
        .next()
        .is_some_and(|l| l.bytes().any(|b| !b.is_ascii_digit()));
    let lower = name.to_ascii_lowercase();
    labels_ok && last_is_word && lower != "localhost" && !lower.ends_with(".localhost")
}

/// `url` when it is exactly `ws[s]://<private IP or name>[:port][/<prefix>]/<want>`: another
/// engine's socket on the LAN. Rebuilt from what was parsed, so what is connected to is what was
/// checked. Never a public address, user info (`@`), a query or fragment, an escape, or another
/// route. A name is looked up at connect time and every address it has must be private
/// ([`connect_lan`] connects to the address it checked, not to the name again).
pub(crate) fn lan_ws_target(url: &str, want: &str) -> Option<String> {
    let (scheme, rest) = match url.strip_prefix("wss://") {
        Some(rest) => ("wss", rest),
        None => ("ws", url.strip_prefix("ws://")?),
    };
    if !rest
        .bytes()
        .all(|b| b.is_ascii_graphic() && !matches!(b, b'@' | b'?' | b'#' | b'%' | b'\\'))
    {
        return None;
    }
    let (authority, path) = rest.split_once('/')?;
    // An engine hosted below a path (PS5UPLOAD_BASE_URL): its prefix, then the route.
    let prefix = path.strip_suffix(want)?;
    if !prefix.is_empty()
        && crate::host_guard::normalize_base(prefix)
            .is_none_or(|p| format!("{}/", &p[1..]) != prefix)
    {
        return None;
    }
    let (host, port): (String, Option<&str>) = if let Some(v6) = authority.strip_prefix('[') {
        let (h, tail) = v6.split_once(']')?;
        let port = match tail {
            "" => None,
            t => Some(t.strip_prefix(':')?),
        };
        let ip: std::net::Ipv6Addr = h.parse().ok()?;
        if !is_lan_ip(std::net::IpAddr::V6(ip)) {
            return None;
        }
        (format!("[{ip}]"), port)
    } else {
        let (h, port) = match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        };
        match h.parse::<std::net::Ipv4Addr>() {
            Ok(ip) if is_lan_ip(std::net::IpAddr::V4(ip)) => (ip.to_string(), port),
            Ok(_) => return None,
            Err(_) if lan_name(h) => (h.to_ascii_lowercase(), port),
            Err(_) => return None,
        }
    };
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) => {
            Some(p.parse::<u16>().ok().filter(|&n| n != 0)?)
        }
        Some(_) => return None,
    };
    let authority = match port {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    Some(format!("{scheme}://{authority}/{prefix}{want}"))
}

/// A relay's connection refused by its address, or not reached at all.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum RelayRefusal {
    Address,
    Unreachable,
}

impl RelayRefusal {
    fn why(self) -> &'static str {
        match self {
            RelayRefusal::Address => "address",
            RelayRefusal::Unreachable => "unreachable",
        }
    }
}

/// Every address a name has must be private: one public answer among them could be the one that
/// gets used, and a name can be made to answer anything.
fn only_lan(addrs: Vec<std::net::SocketAddr>) -> Result<Vec<std::net::SocketAddr>, RelayRefusal> {
    if addrs.is_empty() {
        return Err(RelayRefusal::Unreachable);
    }
    if addrs.iter().all(|a| is_lan_ip(a.ip())) {
        Ok(addrs)
    } else {
        Err(RelayRefusal::Address)
    }
}

/// A TCP connection to the address `host` was checked to have.
async fn connect_lan(host: &str, port: u16) -> Result<tokio::net::TcpStream, RelayRefusal> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => vec![std::net::SocketAddr::new(ip, port)],
        Err(_) => tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| RelayRefusal::Unreachable)?
            .collect(),
    };
    for addr in only_lan(addrs)? {
        if let Ok(stream) = tokio::net::TcpStream::connect(addr).await {
            let _ = stream.set_nodelay(true);
            return Ok(stream);
        }
    }
    Err(RelayRefusal::Unreachable)
}

/// TLS for a `wss://` engine behind a proxy, checked against the public roots as any browser's
/// visit would be. A private CA's certificate is refused here; the host's LAN address still works.
fn relay_tls() -> Option<tokio_rustls::TlsConnector> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .ok()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Some(tokio_rustls::TlsConnector::from(std::sync::Arc::new(
        config,
    )))
}

/// `GET /api/ps5/rp/pad-relay?url=ws://<computer>/api/pad/ws&name=` (loopback only): the phone
/// app's controller page talks to this, and this to the computer's engine.
pub async fn relay(upgrade: WebSocketUpgrade, Query(q): Query<RelayQuery>) -> Response {
    upgrade.on_upgrade(move |s| relay_session(s, q))
}

async fn relay_session(mut socket: WebSocket, q: RelayQuery) {
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
        "{target}{}",
        relay_query(&name, q.t.as_deref(), q.w == Some(1))
    );
    relay_to(socket, &url).await;
}

/// Connects to `url` (another engine's socket, already checked) and passes every text and binary
/// message both ways unchanged until either side closes; `{"stage": "refused", "why":
/// "unreachable"}` when it cannot connect, `"address"` when a name turned out to lead off the LAN.
pub(crate) async fn relay_to(mut socket: WebSocket, url: &str) {
    let opened = tokio::time::timeout(Duration::from_secs(5), async {
        let uri: axum::http::Uri = url.parse().map_err(|_| RelayRefusal::Address)?;
        let host = uri.host().ok_or(RelayRefusal::Address)?;
        let tls = uri.scheme_str() == Some("wss");
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        let tcp = connect_lan(host, port).await?;
        if !tls {
            let (far, _) = tokio_tungstenite::client_async(url, tcp)
                .await
                .map_err(|_| RelayRefusal::Unreachable)?;
            return Ok(Far::Plain(Box::new(far)));
        }
        let name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| RelayRefusal::Address)?;
        let stream = relay_tls()
            .ok_or(RelayRefusal::Unreachable)?
            .connect(name, tcp)
            .await
            .map_err(|_| RelayRefusal::Unreachable)?;
        let (far, _) = tokio_tungstenite::client_async(url, stream)
            .await
            .map_err(|_| RelayRefusal::Unreachable)?;
        Ok(Far::Tls(Box::new(far)))
    })
    .await
    .unwrap_or(Err(RelayRefusal::Unreachable));
    match opened {
        Ok(Far::Plain(far)) => pump(socket, *far).await,
        Ok(Far::Tls(far)) => pump(socket, *far).await,
        Err(why) => {
            let _ = socket
                .send(say(
                    serde_json::json!({"stage": "refused", "why": why.why()}),
                ))
                .await;
        }
    }
}

enum Far {
    Plain(Box<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>),
    Tls(
        Box<
            tokio_tungstenite::WebSocketStream<
                tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
}

async fn pump<S>(mut socket: WebSocket, far: tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as TMsg;
    let (mut far_tx, mut far_rx) = far.split();
    loop {
        tokio::select! {
            m = far_rx.next() => match m {
                Some(Ok(TMsg::Text(t))) => if socket.send(Message::Text(t.as_str().into())).await.is_err() { break },
                // A watcher's picture and sound.
                Some(Ok(TMsg::Binary(b))) => if socket.send(Message::Binary(b)).await.is_err() { break },
                Some(Ok(TMsg::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            m = socket.recv() => match m {
                Some(Ok(Message::Text(t))) => if far_tx.send(TMsg::Text(t.as_str().into())).await.is_err() { break },
                // The phone's keyboard & mouse reports.
                Some(Ok(Message::Binary(b))) => if far_tx.send(TMsg::Binary(b)).await.is_err() { break },
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
    fn vpn_addresses_are_private_and_not_virtual_adapters() {
        let v4 = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        assert!(vpn_reachable(v4("10.8.0.1"))); // WireGuard's usual server address
        assert!(vpn_reachable(v4("172.16.5.1")));
        assert!(vpn_reachable(v4("100.64.0.7")));
        assert!(!vpn_reachable(v4("8.8.8.8")));
        assert!(!vpn_reachable(v4("100.128.0.1")));
        for name in [
            "docker0",
            "br-1a2b",
            "veth9f",
            "vEthernet (WSL)",
            "vmnet8",
            "vboxnet0",
            "virbr0",
            "bridge100",
        ] {
            assert!(virtual_adapter(name), "{name}");
        }
        for name in [
            "wg0",
            "utun4",
            "WireGuard Tunnel",
            "tun0",
            "en0",
            "Ethernet",
            "Wi-Fi",
        ] {
            assert!(!virtual_adapter(name), "{name}");
        }
    }

    #[test]
    fn this_computer_lists_its_addresses_without_repeats() {
        let (ips, _) = reach_ips(Some("127.0.0.1"));
        let mut seen = ips.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), ips.len());
        assert!(ips.iter().all(|ip| !ip.is_loopback()));
    }

    /// A bug report gets the link's counts, never its key or a phone's name.
    #[test]
    fn the_bug_report_summary_has_no_key_or_names() {
        let (tx, _rx) = mpsc::channel(1);
        let l = Link {
            key: Some("a1b2c3d4e5f60718a1b2c3d4e5f60718".into()),
            pads: vec![
                Pad {
                    id: 1,
                    name: "Ann's phone".into(),
                    since_ms: 0,
                    tx: tx.clone(),
                    watch: false,
                    control: false,
                },
                Pad {
                    id: 2,
                    name: "Bob's tablet".into(),
                    since_ms: 0,
                    tx,
                    watch: true,
                    control: true,
                },
            ],
            pending: Vec::new(),
        };
        let v = diag_of(&l, true);
        let text = v.to_string();
        assert!(!text.contains("a1b2c3d4"), "{text}");
        assert!(!text.contains("Ann") && !text.contains("Bob"), "{text}");
        assert_eq!(v["open"], true);
        assert_eq!(v["phones"], 1);
        assert_eq!(v["watchers"], 1);
        assert_eq!(v["watchers_in_control"], 1);
        assert_eq!(v["tailscale_ip_present"], true);
    }

    #[test]
    fn relayed_link_passes_its_key_and_watch_flag_only_when_sound() {
        assert_eq!(
            relay_query("Ann's phone", None, false),
            "?name=Ann%27s%20phone"
        );
        assert_eq!(
            relay_query("A", Some("0f0f0f0f0f0f0f0f"), true),
            "?name=A&t=0f0f0f0f0f0f0f0f&w=1"
        );
        // not a key: dropped (the computer then asks, as for any unkeyed phone)
        assert_eq!(relay_query("A", Some("0f0f&w=1"), false), "?name=A");
        assert_eq!(relay_query("A", Some("short"), false), "?name=A");
    }

    #[test]
    fn tailnet_address_only_when_tailscale_routes_it() {
        let ip = |s: &str| Some(s.parse::<std::net::IpAddr>().unwrap());
        // Tailscale up: 100.100.100.100 goes out on the tailnet address, the default route on Wi-Fi.
        assert_eq!(
            tailnet_choice(ip("100.101.102.103"), ip("192.168.1.20")),
            ip("100.101.102.103")
        );
        // Tailscale off: both go out on Wi-Fi.
        assert_eq!(tailnet_choice(ip("192.168.1.20"), ip("192.168.1.20")), None);
        // A carrier's CGNAT address on the computer itself is not Tailscale.
        assert_eq!(tailnet_choice(ip("100.70.1.2"), ip("100.70.1.2")), None);
        // Outside 100.64/10 (100.128.x is not shared space).
        assert_eq!(tailnet_choice(ip("100.128.0.1"), ip("192.168.1.20")), None);
        assert_eq!(tailnet_choice(None, ip("192.168.1.20")), None);
    }

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
        assert_eq!(relay_target("wss://8.8.8.8/api/pad/ws"), None);
        assert_eq!(
            relay_target("wss://192.168.1.20/api/pad/ws").as_deref(),
            Some("wss://192.168.1.20/api/pad/ws")
        );
        assert_eq!(
            relay_target("ws://192.168.1.20:19113/ps5/api/pad/ws").as_deref(),
            Some("ws://192.168.1.20:19113/ps5/api/pad/ws")
        );
        assert_eq!(relay_target("ws://192.168.1.20/a//api/pad/ws"), None);
        assert_eq!(relay_target("ws://192.168.1.20/../api/pad/ws"), None);
        assert_eq!(
            lan_ws_target("ws://100.101.2.3:19113/api/join/ws", "api/join/ws").as_deref(),
            Some("ws://100.101.2.3:19113/api/join/ws")
        );
        assert_eq!(
            lan_ws_target("ws://100.128.0.1:1/api/join/ws", "api/join/ws"),
            None
        );
        assert_eq!(
            lan_ws_target("ws://10.0.0.2:1/api/pad/ws", "api/join/ws"),
            None
        );
    }

    #[test]
    fn relay_targets_are_parsed_whole_not_guessed_at() {
        let j = |u: &str| lan_ws_target(u, "api/join/ws");
        // user info: the host is what follows the '@' (8.8.8.8), whatever the private-looking
        // part before it says
        assert_eq!(j("ws://192.168.1.5:80@8.8.8.8/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5@8.8.8.8:19113/api/join/ws"), None);
        assert_eq!(j("ws://user:pw@192.168.1.5:19113/api/join/ws"), None);
        // a query, a fragment, an escape, a backslash, a space, extra path
        assert_eq!(j("ws://192.168.1.5:19113/api/join/ws?x=1"), None);
        assert_eq!(j("ws://192.168.1.5:19113/api/join/ws#x"), None);
        assert_eq!(j("ws://192.168.1.5:19113/api/join/w%73"), None);
        assert_eq!(j("ws://192.168.1.5\\@8.8.8.8/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5 :19113/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5:19113/api/join/ws/"), None);
        assert_eq!(j("ws://192.168.1.5:19113//api/join/ws"), None);
        // ports: digits only, 1-65535
        assert_eq!(j("ws://192.168.1.5:0/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5:65536/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5:+80/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5:/api/join/ws"), None);
        assert_eq!(j("ws://192.168.1.5:1:2/api/join/ws"), None);
        // names are taken as far as this: what they resolve to decides (only_lan)
        assert_eq!(
            j("ws://My-PC.tail1234.ts.net:19113/api/join/ws").as_deref(),
            Some("ws://my-pc.tail1234.ts.net:19113/api/join/ws")
        );
        assert_eq!(
            j("wss://ps5upload.home/api/join/ws").as_deref(),
            Some("wss://ps5upload.home/api/join/ws")
        );
        assert_eq!(j("ws://bad_name!/api/join/ws"), None);
        assert_eq!(j("ws://-x.lan/api/join/ws"), None);
        // and odd address spellings, and this computer
        assert_eq!(j("ws://localhost:19113/api/join/ws"), None);
        assert_eq!(j("ws://a.localhost:19113/api/join/ws"), None);
        assert_eq!(j("ws://192.168.010.5:19113/api/join/ws"), None);
        assert_eq!(j("ws://3232235781:19113/api/join/ws"), None);
        assert_eq!(j("ws://[::ffff:8.8.8.8]:19113/api/join/ws"), None);
        assert_eq!(j("ws://[2001:db8::1]:19113/api/join/ws"), None);
        // the good ones, rebuilt from the parsed address
        assert_eq!(
            j("ws://192.168.1.5/api/join/ws").as_deref(),
            Some("ws://192.168.1.5/api/join/ws")
        );
        assert_eq!(
            j("ws://[fd7a:115c:a1e0::1]:19113/api/join/ws").as_deref(),
            Some("ws://[fd7a:115c:a1e0::1]:19113/api/join/ws")
        );
        assert_eq!(
            j("ws://169.254.3.4:19113/api/join/ws").as_deref(),
            Some("ws://169.254.3.4:19113/api/join/ws")
        );
        // the pad relay reads them the same way
        assert_eq!(relay_target("ws://192.168.1.5:80@8.8.8.8/api/pad/ws"), None);
        assert_eq!(relay_target("ws://192.168.1.5:19113/api/pad/ws?t=k"), None);
    }

    #[test]
    fn a_name_counts_only_when_every_address_it_has_is_private() {
        let a = |s: &str| s.parse::<std::net::SocketAddr>().unwrap();
        let lan = a("192.168.1.5:80");
        assert_eq!(only_lan(vec![lan]), Ok(vec![lan]));
        assert_eq!(
            only_lan(vec![lan, a("[fd00::1]:80")]).map(|v| v.len()),
            Ok(2)
        );
        assert_eq!(
            only_lan(vec![lan, a("8.8.8.8:80")]),
            Err(RelayRefusal::Address)
        );
        assert_eq!(
            only_lan(vec![a("127.0.0.1:80")]),
            Err(RelayRefusal::Address)
        );
        assert_eq!(
            only_lan(vec![a("[::ffff:8.8.8.8]:80")]),
            Err(RelayRefusal::Address)
        );
        assert_eq!(only_lan(Vec::new()), Err(RelayRefusal::Unreachable));
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn links_use_what_the_browser_used_except_this_computers_own_names() {
        let web = |p: &[(&'static str, &'static str)]| web_base(&headers(p));
        assert_eq!(
            web(&[("host", "nas.local:19113")]).as_deref(),
            Some("http://nas.local:19113")
        );
        // the bridged engine used from its own computer: the first invite was localhost
        assert_eq!(web(&[("host", "localhost:19113")]), None);
        assert_eq!(web(&[("host", "127.0.0.1:19113")]), None);
        assert_eq!(web(&[("host", "[::1]:19113")]), None);
        assert_eq!(web(&[("host", "bad host/x")]), None);
        // behind https: scheme and port as the browser sees them
        assert_eq!(
            web(&[("host", "x:80"), ("origin", "https://ps5.home:8443")]).as_deref(),
            Some("https://ps5.home:8443")
        );
        assert_eq!(
            web(&[("host", "ps5.home"), ("x-forwarded-proto", "https")]).as_deref(),
            Some("https://ps5.home")
        );
        assert_eq!(
            web(&[
                ("host", "10.0.0.2:19113"),
                ("x-forwarded-host", "ps5.home"),
                ("x-forwarded-proto", "https, http"),
            ])
            .as_deref(),
            Some("https://ps5.home")
        );
        // the app's own renderer is not an address for anyone else
        assert_eq!(
            web(&[
                ("host", "127.0.0.1:19113"),
                ("origin", "http://tauri.localhost")
            ]),
            None
        );
        assert_eq!(
            web(&[("host", "127.0.0.1:19113"), ("origin", "tauri://localhost")]),
            None
        );
    }

    #[test]
    fn a_proxied_request_says_nothing_about_this_machines_port() {
        let direct = headers(&[("host", "192.168.1.10:8080")]);
        assert_eq!(lan_port(&direct), 8080);
        let proxied = headers(&[("host", "ps5.home:8443"), ("x-forwarded-proto", "https")]);
        assert_eq!(lan_port(&proxied), listen_port());
        let https = headers(&[
            ("host", "ps5.home:8443"),
            ("origin", "https://ps5.home:8443"),
        ]);
        assert_eq!(lan_port(&https), listen_port());
        assert_eq!(
            ip_base("192.168.1.5".parse().unwrap(), 19113),
            "http://192.168.1.5:19113"
        );
        assert_eq!(
            ip_base("fd00::1".parse().unwrap(), 80),
            "http://[fd00::1]:80"
        );
    }

    #[test]
    fn a_qr_code_prefers_the_address_unless_it_cannot_work() {
        let ip = || vec!["http://10.0.0.2:1".to_string()];
        let name = || Some("http://nas.local:1".to_string());
        let https = || Some("https://ps5.home".to_string());
        assert_eq!(
            phone_bases(name(), ip(), false),
            vec!["http://10.0.0.2:1", "http://nas.local:1"]
        );
        assert_eq!(
            phone_bases(name(), ip(), true),
            vec!["http://nas.local:1", "http://10.0.0.2:1"]
        );
        assert_eq!(
            phone_bases(https(), ip(), false),
            vec!["https://ps5.home", "http://10.0.0.2:1"]
        );
        assert_eq!(phone_bases(None, ip(), true), vec!["http://10.0.0.2:1"]);
    }

    #[test]
    fn a_phone_kbm_report_reaches_the_page_as_hex() {
        let mut f = vec![crate::vinput_api::KIND_KBM];
        f.extend(0u8..16);
        assert_eq!(
            kbm_report(3, &f),
            Some(serde_json::json!({"id": 3, "r": "000102030405060708090a0b0c0d0e0f"}))
        );
        // Wrong length or kind: dropped.
        assert_eq!(kbm_report(3, &f[..16]), None);
        let mut long = f.clone();
        long.push(0);
        assert_eq!(kbm_report(3, &long), None);
        f[0] = 5;
        assert_eq!(kbm_report(3, &f), None);
        assert_eq!(kbm_report(3, &[]), None);
    }

    #[test]
    fn kbm_messages_for_the_page() {
        let m: crate::vinput_api::KbmMsg =
            serde_json::from_str(r#"{"on":true,"mouse":true}"#).unwrap();
        assert_eq!(
            kbm_control(2, &m),
            serde_json::json!({"id": 2, "on": true, "mouse": true, "keyboard": false})
        );
        assert_eq!(kbm_left(2), serde_json::json!({"id": 2, "left": true}));
    }

    #[test]
    fn kbm_caps_read_like_vinput_caps() {
        let c: ps5upload_core::vinput::Caps =
            serde_json::from_str(r#"{"mouse":true,"keyboard":false,"open":false,"backend":"x"}"#)
                .unwrap();
        assert_eq!(
            kbm_caps(Ok(Some(c))),
            serde_json::json!({"kbm_caps": {"mouse": true, "keyboard": false, "reason": null}})
        );
        assert_eq!(
            kbm_caps(Ok(None)),
            serde_json::json!({"kbm_caps": {"mouse": false, "keyboard": false, "reason": "vinput_helper_old"}})
        );
        assert_eq!(
            kbm_caps(Err(anyhow::anyhow!("down"))),
            serde_json::json!({"kbm_caps": {"mouse": false, "keyboard": false, "reason": "unreachable"}})
        );
    }

    #[test]
    fn port_comes_from_the_host_header() {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:19114".parse().unwrap());
        assert_eq!(port_of(&h), 19114);
        assert_eq!(port_of(&HeaderMap::new()), 19113);
    }
}
