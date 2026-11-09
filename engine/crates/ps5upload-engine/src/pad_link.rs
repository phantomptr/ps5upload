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
//! * **Internet** (`POST /api/ps5/rp/pad-link/internet`): a phone anywhere, over WebRTC through the
//!   signalling hub, as an internet Play Together guest connects (crate::join_net). The link is
//!   `https://ps5upload.phantomptr.com/pad/#room=…&k=…&fp=…&hub=…` (`&w=1` to watch): a room of
//!   its own on the hub and a key of its own, both new with each New link (the old link then stops
//!   working). The website's copy of `/pad` opens a `"pad"` data channel to this engine, checks its
//!   certificate against `fp`, and sends this socket's messages on it after a first `{"hello":
//!   {"t", "name", "d", "w"}}` (the query below). The key only lets it ask: the computer allows
//!   it (`decide`) before it gets the pad or the picture, as with an internet Play Together guest,
//!   and one it allowed comes back after a drop without asking: on the yes the engine sends it a
//!   reconnect token of its own (`{"rt": …}`, random), which the page keeps for that link's room
//!   and sends back as `rt` in its hello. A device id is never enough over the internet (the phone
//!   makes it up). New link voids every token and refuses the phones still waiting. A
//!   watcher there gets the picture as an internet Play Together guest does (a short queue that
//!   skips to the next keyframe), and only watches: it cannot be handed the controller. Internet
//!   invites off (Settings › Play Together) closes the room; phones already in keep playing.
//!
//! `/pad` and `/api/pad/` are the only routes a LAN peer reaches besides `/pkg-host/`
//! (`loopback_allows` in lib.rs), and the socket checks the key or the computer's yes itself.
//! A phone also names its device (`d=`, random, kept on the phone): on the LAN, a device the computer allowed
//! comes back after a dropped socket without asking again, and one it disconnected stays out until
//! the link is closed.
//!
//! A phone that vanishes (out of Wi-Fi range, asleep with a thumb on the stick) must not stay on
//! the console's pad: the socket hears a WebSocket ping every second and ends when nothing at all
//! comes back for a few, and a page that says it keeps sending (`"ka"` on its pad messages, every
//! 250 ms) is let go after a second without one.
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
use axum::extract::{ConnectInfo, Query};
use axum::http::{HeaderMap, StatusCode};
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
/// The least time between a watcher's keyframe requests that reach the console.
const WATCH_KEY_GAP: Duration = Duration::from_secs(1);
/// How often a phone hears whether the session is playing (and a WebSocket ping).
const STATE_EVERY: Duration = crate::rp_stream_api::PEER_PING_EVERY;
/// A page that keeps sending (`"ka"`) and has not for this long: its pad is let go. The socket may
/// well be fine (a hidden page's timers, a busy phone): it comes back with its next state.
const PAD_SILENT_AFTER: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 1000 });
/// The largest message a phone's socket takes. Its pad states are small JSON, its keyboard and
/// mouse reports 17 bytes; what it receives (a watcher's pictures) is not limited by this.
const PHONE_MSG_MAX: usize = crate::join::GUEST_MSG_MAX;
/// Asks waiting from one address.
const MAX_PENDING_PER_IP: usize = 2;
/// An internet watcher's pictures queued before it skips to the next keyframe (Play Together's
/// internet guests have the same, join.rs): never far behind, whatever its connection.
const NET_WATCH_QUEUE: usize = 6;
/// An internet phone must say hello this soon after its channel opens.
const NET_HELLO_WAIT: Duration = Duration::from_secs(10);

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
        m.turbo |= s.turbo;
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
    pub fn is_empty(&self) -> bool {
        self.0.lock().map(|g| g.is_empty()).unwrap_or(true)
    }

    /// Whether `source` has a state recorded.
    pub fn contains(&self, source: u64) -> bool {
        self.0
            .lock()
            .map(|g| g.contains_key(&source))
            .unwrap_or(false)
    }

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
    /// The device's own id, when it gave one.
    device: Option<String>,
    since_ms: u128,
    tx: mpsc::Sender<Message>,
    /// A watcher (watch party): it sees the stream, and plays only once handed the controller.
    watch: bool,
    /// The computer handed this watcher the controller.
    control: bool,
    /// Its connection when it came over the internet (None on the LAN).
    net: Option<std::sync::Arc<crate::join_net::PeerInfo>>,
    /// An internet phone's reconnect token (see [`NetLink::tokens`]).
    rt: Option<String>,
}

/// The internet link: a room on the signalling hub, and the key its link carries.
struct NetLink {
    room: crate::join_net::hub::Room,
    key: String,
    /// Reconnect tokens this engine gave the internet phones it allowed (random, made here: never
    /// what a phone says about itself). Only one of these lets a phone back in without asking
    /// again; they go with the room, so a New link (or closing the link) voids them all.
    tokens: std::collections::HashSet<String>,
}

struct Pending {
    id: u64,
    name: String,
    ip: std::net::IpAddr,
    /// It asks over the internet (an internet link's phone).
    net: bool,
    /// It asks to watch.
    watch: bool,
    answer: oneshot::Sender<Answer>,
}

/// The computer's answer to a phone that asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Allow,
    Deny,
    /// Its internet link was replaced by a New link before anyone answered: told `key`.
    Revoked,
}

#[derive(Default)]
struct Link {
    /// The QR code's key, while the link is open.
    key: Option<String>,
    pads: Vec<Pad>,
    pending: Vec<Pending>,
    /// Devices the computer allowed: back in without asking on a new socket.
    allowed: std::collections::HashSet<String>,
    /// Devices the computer disconnected: out until the link closes.
    denied: std::collections::HashSet<String>,
    /// The internet link, once asked for (until the link closes or a New link replaces it).
    net: Option<NetLink>,
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
    let internet_invites = crate::join::settings().internet_invites;
    // The home network's page, the internet link's hint for a phone that cannot reach the hub
    // ("Open on this Wi-Fi"). It carries no key: the phone asks there, and the computer allows it.
    let lan_page = bases.first().map(|b| format!("{b}{prefix}/pad"));
    with_link(|l| {
        serde_json::json!({
            "open": l.key.is_some(),
            // Internet links may be made (Settings › Play Together › Allow internet invites).
            "internet_invites": internet_invites,
            // The internet link once asked for: `url` (the link), `room`, `hub`, `fp`, `state`
            // (the hub connection), `error`, `router`, `peers` (as Play Together's invite has).
            "internet": l.net.as_ref().map(|n| n.room.json(&n.key, lan_page.as_deref(), None)),
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
            // `via`: "lan" or "internet" (with the connection's round trip, `rtt_ms`).
            "pads": l.pads.iter().map(|p| serde_json::json!({
                "id": p.id, "name": p.name, "since_ms": p.since_ms, "watch": p.watch, "control": p.control,
                "via": if p.net.is_some() { "internet" } else { "lan" },
                "rtt_ms": p.net.as_ref().and_then(|n| n.rtt_ms()),
                // An internet phone's way: "lan", "tailscale", "internet" (ICE's selected pair).
                "path": p.net.as_ref().and_then(|n| n.net_path()),
            })).collect::<Vec<_>>(),
            "pending": l.pending.iter().map(|p| serde_json::json!({
                "id": p.id, "name": p.name, "via": if p.net { "internet" } else { "lan" }, "watch": p.watch,
            })).collect::<Vec<_>>(),
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
        "internet_link": l.net.is_some(),
        "internet_phones": l.pads.iter().filter(|p| p.net.is_some()).count(),
        "tailscale_ip_present": tailscale,
    })
}

#[derive(serde::Deserialize, Default)]
pub struct StatusQuery {
    #[serde(default)]
    #[serde(alias = "addr", alias = "host")]
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
    #[serde(alias = "addr", alias = "host")]
    console: Option<String>,
}

/// `pad`'s input off the console now, whatever its socket is doing: a stalled one may not read
/// its Close for minutes (or its queue is full and the Close was dropped).
fn release(id: u64, watch: bool) {
    crate::rp_stream_api::push_pad(id, None);
    crate::rp_stream_api::push_pad_kbm(kbm_left(id));
    if watch {
        crate::rp_stream_api::remove_spectator(id);
    }
}

/// `p` is told why, and goes.
fn let_go(p: &Pad, why: &str) {
    let _ =
        p.tx.try_send(say(serde_json::json!({"stage": "refused", "why": why})));
    let _ = p.tx.try_send(Message::Close(None));
}

/// `POST /api/ps5/rp/pad-link {open, console?}`: open the QR link (a new key), or close it,
/// which also lets every phone go.
pub async fn set_open(headers: HeaderMap, Json(r): Json<OpenReq>) -> Json<serde_json::Value> {
    let gone = with_link(|l| {
        if r.open {
            if l.key.is_none() {
                l.key = random_key();
            }
            return Vec::new();
        }
        l.key = None;
        // Its room on the hub closes with it.
        l.net = None;
        l.allowed.clear();
        l.denied.clear();
        for p in l.pending.drain(..) {
            let _ = p.answer.send(Answer::Deny);
        }
        l.pads
            .drain(..)
            .map(|p| {
                let_go(&p, "closed");
                (p.id, p.watch)
            })
            .collect()
    });
    for (id, watch) in gone {
        release(id, watch);
    }
    Json(status(&headers, r.console.as_deref()))
}

#[derive(serde::Deserialize)]
pub struct InternetReq {
    /// A new room and key: the old internet link stops working.
    #[serde(default)]
    new: bool,
    #[serde(default)]
    #[serde(alias = "addr", alias = "host")]
    console: Option<String>,
}

fn net_refused(code: StatusCode, err: &str) -> Response {
    (code, Json(serde_json::json!({ "error": err }))).into_response()
}

/// `POST /api/ps5/rp/pad-link/internet {new?, console?}`: the link for a phone anywhere
/// (`internet.url` in the answer, the status as `GET` has it; the link opens too). A room on the
/// signalling hub in Play Together's settings, with a key of its own; the same one again unless
/// `new`, which closes the old room (its link stops working; internet phones still waiting to be
/// allowed are told so, phones already in keep playing). An internet phone asks like the phone
/// app does: the computer allows it (`pending`, `decide`) before it gets the pad or the picture.
/// 409 `internet_off` when the settings turned internet invites off, 409 `hub_url` when the hub
/// address is not a `wss://` one.
pub async fn set_internet(headers: HeaderMap, Json(r): Json<InternetReq>) -> Response {
    let s = crate::join::settings();
    if !s.internet_invites {
        return net_refused(StatusCode::CONFLICT, "internet_off");
    }
    if !crate::join_net::hub_url_ok(&s.hub_url) {
        return net_refused(StatusCode::CONFLICT, "hub_url");
    }
    if crate::join_net::fingerprint().is_none() {
        return net_refused(StatusCode::INTERNAL_SERVER_ERROR, "no_certificate");
    }
    let made = with_link(|l| {
        if l.key.is_none() {
            l.key = random_key();
        }
        if r.new || l.net.as_ref().is_none_or(|n| n.room.url() != s.hub_url) {
            // The old room goes first: its link stops working, and the internet phones still
            // waiting for an answer go with it (those allowed keep playing).
            l.net = None;
            let (revoked, kept) = std::mem::take(&mut l.pending)
                .into_iter()
                .partition::<Vec<_>, _>(|p| p.net);
            l.pending = kept;
            for p in revoked {
                let _ = p.answer.send(Answer::Revoked);
            }
            let key = random_key()?;
            let room = crate::join_net::hub::Room::start(
                crate::join_net::Kind::Pad,
                Box::new(|| crate::join::settings().internet_invites),
                s.hub_url.clone(),
            )?;
            l.net = Some(NetLink {
                room,
                key,
                tokens: Default::default(),
            });
        }
        Some(())
    });
    if made.is_none() {
        return net_refused(StatusCode::INTERNAL_SERVER_ERROR, "no_random");
    }
    crate::log_at!(
        Info,
        "together",
        "pad_link: internet link open (new: {})",
        r.new
    );
    Json(status(&headers, r.console.as_deref())).into_response()
}

/// Internet invites were turned off: the internet link's room closes, and the internet phones
/// still waiting for an answer go with it (phones in keep playing).
pub(crate) fn internet_off() {
    let revoked = with_link(|l| {
        l.net = None;
        let (revoked, kept) = std::mem::take(&mut l.pending)
            .into_iter()
            .partition::<Vec<_>, _>(|p| p.net);
        l.pending = kept;
        revoked
    });
    for p in revoked {
        let _ = p.answer.send(Answer::Revoked);
    }
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
    let ok = found.is_some();
    if let Some(p) = found {
        let _ = p
            .answer
            .send(if r.allow { Answer::Allow } else { Answer::Deny });
    }
    Json(serde_json::json!({ "ok": ok }))
}

#[derive(serde::Deserialize)]
pub struct KickReq {
    id: u64,
}

/// `POST /api/ps5/rp/pad-link/kick {id}`: let a phone go, and keep it out until the link closes
/// (the link's key alone would let it back in with one tap).
pub async fn kick(Json(r): Json<KickReq>) -> Json<serde_json::Value> {
    let gone = with_link(|l| {
        let i = l.pads.iter().position(|p| p.id == r.id)?;
        let p = l.pads.remove(i);
        let_go(&p, "kicked");
        if let Some(d) = &p.device {
            l.allowed.remove(d);
            l.denied.insert(d.clone());
        }
        // Its way back in over the internet goes too.
        if let (Some(rt), Some(n)) = (&p.rt, l.net.as_mut()) {
            n.tokens.remove(rt);
        }
        Some((p.id, p.watch))
    });
    let ok = gone.is_some();
    if let Some((id, watch)) = gone {
        release(id, watch);
    }
    Json(serde_json::json!({ "ok": ok }))
}

#[derive(serde::Deserialize)]
pub struct ControlReq {
    id: u64,
    on: bool,
}

/// `POST /api/ps5/rp/pad-link/control {id, on}`: hand a watcher the controller, or take it back.
/// Not one watching over the internet (`ok: false`): those only watch.
pub async fn control(Json(r): Json<ControlReq>) -> Json<serde_json::Value> {
    let ok = with_link(|l| {
        let Some(p) = l
            .pads
            .iter_mut()
            .find(|p| p.id == r.id && p.watch && p.net.is_none())
        else {
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
    /// This device's own id (random, kept on the device).
    #[serde(default)]
    d: Option<String>,
    /// An internet phone's reconnect token, as this engine gave it when it was allowed.
    #[serde(default)]
    rt: Option<String>,
}

/// A phone's name as it gave it: printable, short. Empty when it gave none, and the /pad page sends
/// a word for what it is (`iphone`, `android`...): the computer's own app says those in its language.
fn clean_name(n: Option<&str>) -> String {
    let n: String = n
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    n.trim().to_string()
}

/// A device id when it looks like one (hex, as the page makes them).
fn clean_device(d: Option<&str>) -> Option<String> {
    d.filter(|d| (8..=128).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
}

static CONNECTS: crate::join::ConnectLimit = crate::join::ConnectLimit::new();

/// A socket that has the computer's word already: the link's key, or a device it allowed. Those
/// come back as often as a flaky Wi-Fi drops them; the limit is for everyone else.
fn is_expected(q: &PadQuery) -> bool {
    let device = clean_device(q.d.as_deref());
    with_link(|l| {
        q.t.as_deref()
            .is_some_and(|t| l.key.as_deref().is_some_and(|k| same_key(k, t)))
            || device.is_some_and(|d| l.allowed.contains(&d))
    })
}

/// `GET /api/pad/ws?t=&name=&d=`: a phone's controller socket. Any device on the LAN reaches it, so
/// the same limits as a Play Together guest's (join.rs): sockets per address, and small messages.
pub async fn ws(
    upgrade: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    Query(q): Query<PadQuery>,
) -> Response {
    let ip = peer.ip();
    if !is_expected(&q)
        && !CONNECTS.allow(
            ip,
            crate::join::connects_per_min(ip),
            std::time::Instant::now(),
        )
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many tries, wait a minute",
        )
            .into_response();
    }
    upgrade
        .max_message_size(PHONE_MSG_MAX)
        .max_frame_size(PHONE_MSG_MAX)
        .on_upgrade(move |s| pad_session(s, q, ip))
}

#[derive(serde::Deserialize)]
struct NetHello {
    hello: PadQuery,
}

/// An internet phone's data channel (crate::join_net, a room of the phone link): its first message
/// is `{"hello": {"t", "name", "d", "w"}}`, the LAN socket's query. In only with the internet link's
/// key, and only from that link's room (a New link's old room lets nobody in); then the LAN socket's
/// session.
pub(crate) async fn rtc_session(
    mut socket: crate::join_net::RtcSocket,
    ip: std::net::IpAddr,
    room: String,
) {
    use crate::join::GuestSocket;
    use crate::rp_stream_api::send_limited;
    let refuse = |why: &str| say(serde_json::json!({"stage": "refused", "why": why}));
    let hello = tokio::time::timeout(NET_HELLO_WAIT, async {
        loop {
            match socket.recv().await {
                Some(Ok(Message::Text(t))) => {
                    return serde_json::from_str::<NetHello>(t.as_str())
                        .ok()
                        .map(|h| h.hello);
                }
                Some(Ok(Message::Pong(_) | Message::Ping(_))) => {}
                _ => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();
    let Some(q) = hello else {
        send_limited(&mut socket, refuse("hello")).await;
        return;
    };
    let ok = with_link(|l| {
        l.net.as_ref().is_some_and(|n| {
            n.room.id() == room && q.t.as_deref().is_some_and(|t| same_key(&n.key, t))
        })
    });
    if !ok {
        crate::log_at!(
            Debug,
            "together",
            "pad_link: an internet phone with an old or wrong link"
        );
        send_limited(&mut socket, refuse("key")).await;
        return;
    }
    pad_session(socket, q, ip).await
}

/// `t` when it is a reconnect token the current internet link gave (compared whole).
fn net_token_known(t: &str) -> Option<String> {
    with_link(|l| {
        l.net
            .as_ref()
            .and_then(|n| n.tokens.iter().find(|k| same_key(k, t)).cloned())
    })
}

/// A new reconnect token for an internet phone just allowed, kept with the current internet link.
/// None when the link went meanwhile (the phone then has to ask again after a drop).
fn new_net_token() -> Option<String> {
    let t = random_key()?;
    with_link(|l| {
        let n = l.net.as_mut()?;
        n.tokens.insert(t.clone());
        Some(t)
    })
}

fn say(v: serde_json::Value) -> Message {
    Message::Text(v.to_string().into())
}

/// A ping's payload: the time, eight bytes (what an internet phone's channel carries as a ping).
fn ping_bytes() -> [u8; 8] {
    std::time::UNIX_EPOCH
        .elapsed()
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        .to_le_bytes()
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

impl Pad {
    /// A watcher plays only while it holds the controller.
    fn plays(&self) -> bool {
        !self.watch || self.control
    }
}

/// A watcher plays only while it holds the controller.
fn plays(id: u64) -> bool {
    with_link(|l| l.pads.iter().any(|x| x.id == id && x.plays()))
}

/// A linked phone plays on the stream's controller (a controller, or a watcher handed it), quiet
/// or not: a Play Together guest never gets that controller from under it (join.rs, #720).
pub(crate) fn phones_playing() -> bool {
    with_link(|l| any_playing(&l.pads))
}

fn any_playing(pads: &[Pad]) -> bool {
    pads.iter().any(Pad::plays)
}

/// A phone's session, on the LAN socket or an internet phone's data channel (whose key
/// `rtc_session` checked already).
async fn pad_session<S: crate::join::GuestSocket>(
    mut socket: S,
    q: PadQuery,
    ip: std::net::IpAddr,
) {
    use crate::rp_stream_api::send_limited;
    let net = socket.net();
    let name = clean_name(q.name.as_deref());
    let device = clean_device(q.d.as_deref());
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let refuse = |why: &str| say(serde_json::json!({"stage": "refused", "why": why}));
    let out = device
        .as_ref()
        .is_some_and(|d| with_link(|l| l.denied.contains(d)));
    if out {
        send_limited(&mut socket, refuse("kicked")).await;
        return;
    }
    // The QR code's key lets a phone on the LAN in at once. Over the internet the link's key only
    // lets a phone ask (`rtc_session` checked it): the computer allows it, as it allows an
    // internet Play Together guest, and one it allowed comes back after a drop with the reconnect
    // token this engine gave it then. Never by its device id: the phone makes that up itself.
    let mut rt = match &net {
        Some(_) => q.rt.as_deref().and_then(net_token_known),
        None => None,
    };
    let keyed = match &net {
        Some(_) => rt.is_some(),
        None => is_expected(&q),
    };
    if !keyed {
        if net.is_none() && q.t.is_some() {
            // A QR code from a link closed since (or a guess).
            send_limited(&mut socket, refuse("key")).await;
            return;
        }
        // Asking: the computer's page shows it; wait for the answer.
        let (tx, rx) = oneshot::channel();
        let queued = with_link(|l| {
            if l.pending.len() >= MAX_PENDING
                || l.pending.iter().filter(|p| p.ip == ip).count() >= MAX_PENDING_PER_IP
            {
                return false;
            }
            l.pending.push(Pending {
                id,
                name: name.clone(),
                ip,
                net: net.is_some(),
                watch: q.w == Some(1),
                answer: tx,
            });
            true
        });
        if !queued {
            send_limited(&mut socket, refuse("busy")).await;
            return;
        }
        crate::log_at!(
            Info,
            "together",
            "pad_link: phone {id} asks to {} ({})",
            if q.w == Some(1) { "watch" } else { "play" },
            if net.is_some() { "internet" } else { "LAN" }
        );
        send_limited(&mut socket, say(serde_json::json!({"stage": "asking"}))).await;
        let answer = tokio::select! {
            a = rx => a.ok(),
            _ = tokio::time::sleep(ASK_WAIT) => None,
            // The phone gave up.
            _ = async { loop { match socket.recv().await { Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break, _ => {} } } } => Some(Answer::Deny),
        };
        with_link(|l| l.pending.retain(|p| p.id != id));
        match answer {
            None => {
                send_limited(&mut socket, refuse("no_answer")).await;
                return;
            }
            Some(Answer::Deny) => {
                send_limited(&mut socket, refuse("denied")).await;
                return;
            }
            Some(Answer::Revoked) => {
                send_limited(&mut socket, refuse("key")).await;
                return;
            }
            Some(Answer::Allow) => {
                if net.is_some() {
                    // Its way back after a drop: a token of ours, kept by the phone for this link.
                    rt = new_net_token();
                    if let Some(t) = &rt {
                        send_limited(&mut socket, say(serde_json::json!({ "rt": t }))).await;
                    }
                } else if let Some(d) = &device {
                    with_link(|l| l.allowed.insert(d.clone()));
                }
            }
        }
    }
    let watch = q.w == Some(1);
    // A watcher gets the stream itself (rp_stream_api's copy for it); none without a session. Over
    // the internet as a Play Together guest gets it: pictures through a short queue of their own
    // (`video`) that skips to the next keyframe when the connection can't keep up.
    let (mut frames, mut video) = if watch {
        let feed = if net.is_some() {
            crate::rp_stream_api::live_host()
                .and_then(|host| {
                    crate::rp_stream_api::add_player(
                        id,
                        &host,
                        NET_WATCH_QUEUE,
                        std::sync::Arc::new(AtomicU64::new(0)),
                        true,
                    )
                })
                .map(|f| (Some(f.other), Some(f.video)))
        } else {
            crate::rp_stream_api::add_spectator(id).map(|r| (Some(r), None))
        };
        match feed {
            Some(f) => f,
            None => {
                send_limited(&mut socket, refuse("no_stream")).await;
                return;
            }
        }
    } else {
        (None, None)
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
            device: device.clone(),
            since_ms,
            tx,
            watch,
            control: false,
            net: net.clone(),
            rt: rt.clone(),
        });
        true
    });
    if !joined {
        if watch {
            crate::rp_stream_api::remove_spectator(id);
        }
        send_limited(&mut socket, refuse("full")).await;
        return;
    }
    // By number at this level: a bug report is posted in public (names under Detailed logs).
    crate::log_at!(
        Info,
        "together",
        "pad_link: phone {id} joined as {} ({})",
        if watch { "watcher" } else { "controller" },
        if net.is_some() { "internet" } else { "LAN" }
    );
    crate::log_at!(Debug, "together", "pad_link: phone {id} is {name}");
    let mut tick = tokio::time::interval(STATE_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Rounds since anything came from the phone (counted, not timed: see rp_stream_api's page).
    let mut quiet = 0u32;
    // When a page that keeps sending is let go of if it hasn't.
    let mut pad_due: Option<tokio::time::Instant> = None;
    let mut key_asked: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(Message::Close(_)) | None => break,
                Some(m) => if !send_limited(&mut socket, m).await { break },
            },
            // A watcher's picture and sound (pending forever for a controller).
            f = async {
                match frames.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => match f {
                Some(m) => if !send_limited(&mut socket, m).await { break },
                None => frames = None,
            },
            // An internet watcher's pictures (their own short queue).
            f = async {
                match video.as_mut() {
                    Some(r) => r.recv().await,
                    None => std::future::pending().await,
                }
            } => match f {
                Some(m) => if !send_limited(&mut socket, m).await { break },
                None => video = None,
            },
            _ = tick.tick() => {
                quiet += 1;
                if quiet > crate::rp_stream_api::PEER_QUIET_PINGS {
                    crate::log_at!(Info, "together", "pad_link: phone {id} went silent; letting it go");
                    break;
                }
                let s = crate::rp_stream_api::live_state();
                let host = crate::rp_stream_api::live_host();
                if host.is_some() && host != caps_host {
                    caps_host = host.clone();
                    ask_kbm_caps(host.unwrap_or_default(), caps_tx.clone());
                }
                if !send_limited(&mut socket, say(serde_json::json!({"stage": "joined", "session": s}))).await
                    // Eight bytes: an internet phone's channel carries only those (join_net/chunk.rs).
                    || !send_limited(&mut socket, Message::Ping(ping_bytes().to_vec().into())).await
                {
                    break;
                }
            },
            _ = async {
                match pad_due {
                    Some(t) => tokio::time::sleep_until(t).await,
                    None => std::future::pending().await,
                }
            } => {
                pad_due = None;
                crate::rp_stream_api::push_pad(id, None);
            },
            inc = socket.recv() => { quiet = 0; match inc {
                Some(Ok(Message::Text(t))) => {
                    #[derive(serde::Deserialize)]
                    struct In {
                        #[serde(default)] pad: Option<ControllerState>,
                        #[serde(default)] kbm: Option<crate::vinput_api::KbmMsg>,
                        /// This page sends its pad at least every 250 ms.
                        #[serde(default)] ka: Option<serde_json::Value>,
                        /// A watcher's picture needs a keyframe.
                        #[serde(default)] keyframe: bool,
                    }
                    if let Ok(m) = serde_json::from_str::<In>(t.as_str()) {
                        if m.keyframe && watch && key_asked.is_none_or(|t| t.elapsed() >= WATCH_KEY_GAP) {
                            key_asked = Some(std::time::Instant::now());
                            crate::rp_stream_api::request_spectator_keyframe(id);
                        }
                        if let Some(p) = m.pad {
                            if plays(id) {
                                crate::rp_stream_api::push_pad(id, Some(p));
                                pad_due = m.ka.is_some().then(|| tokio::time::Instant::now() + PAD_SILENT_AFTER);
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
            }},
        }
    }
    release(id, watch);
    with_link(|l| l.pads.retain(|p| p.id != id));
    crate::log_at!(Info, "together", "pad_link: phone {id} left");
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
    /// The device's own id.
    #[serde(default)]
    d: Option<String>,
}

/// The pad socket's query for a relayed phone: its name, then the link's key and watch flag when
/// it came from a link. A key is passed only when it looks like one (hex, as `random_key` makes);
/// so is the device's id.
fn relay_query(name: &str, t: Option<&str>, d: Option<&str>, watch: bool) -> String {
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
    if let Some(d) = clean_device(d) {
        q.push_str("&d=");
        q.push_str(&d);
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
            .is_none_or(|p| p.is_empty() || format!("{}/", &p[1..]) != prefix)
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
pub(crate) enum RelayRefusal {
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
pub(crate) async fn connect_lan(
    host: &str,
    port: u16,
) -> Result<tokio::net::TcpStream, RelayRefusal> {
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
pub(crate) fn relay_tls() -> Option<tokio_rustls::TlsConnector> {
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
    upgrade
        .max_message_size(PHONE_MSG_MAX)
        .max_frame_size(PHONE_MSG_MAX)
        .on_upgrade(move |s| relay_session(s, q))
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
        relay_query(&name, q.t.as_deref(), q.d.as_deref(), q.w == Some(1))
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

/// The phone link's host routes (lib.rs registers the same ones), for tests that serve them.
#[cfg(test)]
pub(crate) fn router<S: Clone + Send + Sync + 'static>() -> axum::Router<S> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/ps5/rp/pad-link", get(get_status).post(set_open))
        .route("/api/ps5/rp/pad-link/decide", post(decide))
        .route("/api/ps5/rp/pad-link/kick", post(kick))
        .route("/api/ps5/rp/pad-link/control", post(control))
        .route("/api/ps5/rp/pad-link/internet", post(set_internet))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ping_carries_eight_bytes() {
        // An internet phone's channel passes a ping of eight bytes only (join_net/chunk.rs).
        assert_eq!(ping_bytes().len(), 8);
        assert!(u64::from_le_bytes(ping_bytes()) > 0);
    }

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
                    device: None,
                    since_ms: 0,
                    tx: tx.clone(),
                    watch: false,
                    control: false,
                    net: None,
                    rt: None,
                },
                Pad {
                    id: 2,
                    name: "Bob's tablet".into(),
                    device: None,
                    since_ms: 0,
                    tx,
                    watch: true,
                    control: true,
                    net: None,
                    rt: None,
                },
            ],
            ..Default::default()
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
            relay_query("Ann's phone", None, None, false),
            "?name=Ann%27s%20phone"
        );
        assert_eq!(
            relay_query("A", Some("0f0f0f0f0f0f0f0f"), None, true),
            "?name=A&t=0f0f0f0f0f0f0f0f&w=1"
        );
        assert_eq!(
            relay_query("A", None, Some("abcdef0123456789"), false),
            "?name=A&d=abcdef0123456789"
        );
        assert_eq!(relay_query("A", None, Some("ab&w=1zzzz"), false), "?name=A");
        // not a key: dropped (the computer then asks, as for any unkeyed phone)
        assert_eq!(relay_query("A", Some("0f0f&w=1"), None, false), "?name=A");
        assert_eq!(relay_query("A", Some("short"), None, false), "?name=A");
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
        assert_eq!(
            clean_name(None),
            "",
            "the computer's app names it in its language"
        );
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
        assert_eq!(relay_target("ws://192.168.1.20//api/pad/ws"), None);
        assert_eq!(relay_target("ws://192.168.1.20/../api/pad/ws"), None);
        for bad in [
            "ws://192.168.1.20:19113/../api/pad/ws",
            "ws://192.168.1.20:19113/a/../api/pad/ws",
            "ws://192.168.1.20:19113/xapi/pad/ws",
            "ws://192.168.1.20:19113/api/pad/ws/x",
            "ws://192.168.1.20:19113/api/other",
        ] {
            assert_eq!(relay_target(bad), None, "{bad}");
        }
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
    fn a_linked_phone_plays_quiet_or_not_and_a_watcher_only_with_the_controller() {
        let pad = |watch: bool, control: bool| Pad {
            id: 1,
            name: String::new(),
            device: None,
            since_ms: 0,
            tx: mpsc::channel(1).0,
            watch,
            control,
            net: None,
            rt: None,
        };
        assert!(!any_playing(&[]));
        assert!(
            any_playing(&[pad(false, false)]),
            "a controller, whatever it sent lately"
        );
        assert!(!any_playing(&[pad(true, false)]), "a watcher only watches");
        assert!(any_playing(&[pad(true, false), pad(true, true)]));
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

    // ---- A phone's socket, for real ----

    mod socket {
        use super::*;
        use crate::rp_stream_api::{test_live, LIVE_TEST_LOCK};
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as TMsg;

        type Phone = tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >;

        async fn serve() -> String {
            let app = axum::Router::new().route("/api/pad/ws", axum::routing::get(ws));
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = l.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(
                    l,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .unwrap();
            });
            format!("ws://{addr}/api/pad/ws")
        }

        /// The link as a new engine has it, its key open.
        fn fresh_link() -> String {
            let key = random_key().unwrap();
            let k = key.clone();
            *LINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Link {
                key: Some(k),
                ..Default::default()
            });
            key
        }

        async fn phone(url: &str) -> Phone {
            tokio_tungstenite::connect_async(url).await.unwrap().0
        }

        const PRESS: &str = r#"{"pad":{"buttons":1,"l2":0,"r2":0,"lx":0,"ly":0,"rx":0,"ry":0}}"#;
        const PRESS_KA: &str =
            r#"{"pad":{"buttons":1,"l2":0,"r2":0,"lx":0,"ly":0,"rx":0,"ry":0},"ka":1}"#;

        /// Reads until `key`'s value says what is wanted (answering pings as a browser does).
        async fn hear(
            ws: &mut Phone,
            want: impl Fn(&serde_json::Value) -> bool,
        ) -> serde_json::Value {
            let f = async {
                loop {
                    match ws.next().await {
                        Some(Ok(TMsg::Text(t))) => {
                            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                            if want(&v) {
                                return v;
                            }
                        }
                        Some(Ok(_)) => {}
                        other => panic!("closed: {other:?}"),
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(5), f)
                .await
                .expect("in time")
        }

        async fn until(what: &str, f: impl Fn() -> bool) {
            let end = std::time::Instant::now() + Duration::from_secs(5);
            while !f() {
                assert!(std::time::Instant::now() < end, "timed out: {what}");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        fn joined(v: &serde_json::Value) -> bool {
            v["stage"] == "joined"
        }

        fn pads() -> usize {
            with_link(|l| l.pads.len())
        }

        /// In the game, holding ✕, with the session's merged pad saying so.
        async fn holding(url: &str, live: &test_live::Session) -> (Phone, u64) {
            let mut ws = phone(url).await;
            hear(&mut ws, joined).await;
            let id = with_link(|l| l.pads.last().unwrap().id);
            ws.send(TMsg::Text(PRESS.into())).await.unwrap();
            until("pressed", || live.merged().buttons == 1).await;
            (ws, id)
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn disconnecting_a_phone_lets_go_at_once_and_keeps_it_out() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let live = test_live::Session::start("10.0.77.3");
            let key = fresh_link();
            let base = serve().await;
            let url = format!("{base}?t={key}&name=iphone&d=00112233445566778899aabbccddeeff");
            let (mut ws, id) = holding(&url, &live).await;
            let r = kick(Json(KickReq { id })).await;
            assert_eq!(r.0["ok"], true);
            // The engine itself, not the socket's loop.
            assert_eq!(live.merged().buttons, 0);
            let v = hear(&mut ws, |v| v["stage"] == "refused").await;
            assert_eq!(v["why"], "kicked");
            // Unknown now: says so.
            assert_eq!(kick(Json(KickReq { id })).await.0["ok"], false);
            assert_eq!(
                decide(Json(DecideReq {
                    id: 999_999,
                    allow: true
                }))
                .await
                .0["ok"],
                false
            );
            // One tap on Try again: out, while the link's key still works for others.
            let mut again = phone(&url).await;
            assert_eq!(
                hear(&mut again, |v| v["stage"] == "refused").await["why"],
                "kicked"
            );
            let other = format!("{base}?t={key}&name=android&d=ffeeddccbbaa99887766554433221100");
            let mut ws2 = phone(&other).await;
            hear(&mut ws2, joined).await;
            // The link closed: everyone told why, and the key void; a new link starts afresh.
            let h = HeaderMap::new();
            let _ = set_open(
                h.clone(),
                Json(OpenReq {
                    open: false,
                    console: Some("127.0.0.1".into()),
                }),
            )
            .await;
            assert_eq!(
                hear(&mut ws2, |v| v["stage"] == "refused").await["why"],
                "closed"
            );
            let mut after = phone(&url).await;
            assert_eq!(
                hear(&mut after, |v| v["stage"] == "refused").await["why"],
                "key",
                "the old code; the kicked mark went with the link"
            );
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn closing_the_link_lets_go_of_every_phone_at_once() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let live = test_live::Session::start("10.0.77.3");
            let key = fresh_link();
            let base = serve().await;
            let (_ws, _id) = holding(&format!("{base}?t={key}"), &live).await;
            let _ = set_open(
                HeaderMap::new(),
                Json(OpenReq {
                    open: false,
                    console: Some("127.0.0.1".into()),
                }),
            )
            .await;
            assert_eq!(live.merged().buttons, 0);
        }

        /// Out of Wi-Fi range mid-press: no close ever comes, and it answers no ping (the client
        /// library answers only while it is read).
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_phone_that_vanished_lets_go_within_seconds() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let live = test_live::Session::start("10.0.77.3");
            let key = fresh_link();
            let base = serve().await;
            let (ws, _id) = holding(&format!("{base}?t={key}"), &live).await;
            until("let go", || live.merged().buttons == 0 && pads() == 0).await;
            drop(ws);
        }

        /// A phone that answers pings stays, as long as it wants.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_quiet_phone_that_answers_pings_stays_holding() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let live = test_live::Session::start("10.0.77.3");
            let key = fresh_link();
            let base = serve().await;
            let (mut ws, _id) = holding(&format!("{base}?t={key}"), &live).await;
            let end = std::time::Instant::now()
                + STATE_EVERY * (crate::rp_stream_api::PEER_QUIET_PINGS + 3);
            while std::time::Instant::now() < end {
                let _ = tokio::time::timeout(Duration::from_millis(20), ws.next()).await;
            }
            assert_eq!(pads(), 1);
            // An older app's page sends only changes: held is held.
            assert_eq!(live.merged().buttons, 1);
        }

        /// A page that says it keeps sending and stopped (hidden, stuck): the pad goes, the socket
        /// stays, and the next state brings it back.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_page_that_stopped_sending_is_let_go_of_and_comes_back() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let live = test_live::Session::start("10.0.77.3");
            let key = fresh_link();
            let base = serve().await;
            let mut ws = phone(&format!("{base}?t={key}")).await;
            hear(&mut ws, joined).await;
            ws.send(TMsg::Text(PRESS_KA.into())).await.unwrap();
            until("pressed", || live.merged().buttons == 1).await;
            // Reading (so pings are answered), not sending.
            let f = async {
                while live.merged().buttons == 1 {
                    let _ = tokio::time::timeout(Duration::from_millis(20), ws.next()).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(3), f)
                .await
                .expect("let go");
            assert_eq!(pads(), 1, "the socket is fine");
            ws.send(TMsg::Text(PRESS_KA.into())).await.unwrap();
            until("back", || live.merged().buttons == 1).await;
        }

        /// A device the computer allowed comes back without asking again.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn an_allowed_phone_comes_back_without_asking() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let _live = test_live::Session::start("10.0.77.3");
            fresh_link();
            let base = serve().await;
            let url = format!("{base}?name=Pixel&d=00112233445566778899aabbccddeeff");
            let mut ws = phone(&url).await;
            hear(&mut ws, |v| v["stage"] == "asking").await;
            let ask = with_link(|l| l.pending[0].id);
            assert_eq!(
                decide(Json(DecideReq {
                    id: ask,
                    allow: true
                }))
                .await
                .0["ok"],
                true
            );
            hear(&mut ws, joined).await;
            drop(ws);
            until("gone", || pads() == 0).await;
            let mut ws = phone(&url).await;
            hear(&mut ws, joined).await;
            // Someone else, no id: still asks.
            let mut other = phone(&format!("{base}?name=Other")).await;
            hear(&mut other, |v| v["stage"] == "asking").await;
        }

        /// Any device on the LAN reaches this socket: asks and sizes have a limit.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn the_lan_cannot_flood_the_asks_or_send_huge_messages() {
            let _turn = LIVE_TEST_LOCK.lock().await;
            let _live = test_live::Session::start("10.0.77.3");
            fresh_link();
            let base = serve().await;
            let mut a = phone(&base).await;
            hear(&mut a, |v| v["stage"] == "asking").await;
            let mut b = phone(&base).await;
            hear(&mut b, |v| v["stage"] == "asking").await;
            // A third from the same address, however the list stands.
            let mut c = phone(&base).await;
            assert_eq!(
                hear(&mut c, |v| v["stage"] == "refused").await["why"],
                "busy"
            );
            // An ask that sends a huge message is cut off.
            let huge = "x".repeat(PHONE_MSG_MAX + 1);
            let _ = a.send(TMsg::Text(huge.into())).await;
            until("cut off", || with_link(|l| l.pending.len()) == 1).await;
            // Sockets per address: over the limit an upgrade is refused outright.
            let ip: std::net::IpAddr = "192.0.2.92".parse().unwrap();
            let t = std::time::Instant::now();
            let limit = crate::join::connects_per_min(ip);
            for _ in 0..limit {
                assert!(CONNECTS.allow(ip, limit, t));
            }
            assert!(!CONNECTS.allow(ip, limit, t));
        }

        #[test]
        fn a_device_id_is_hex_or_nothing() {
            assert_eq!(clean_device(None), None);
            assert_eq!(clean_device(Some("short")), None);
            assert_eq!(clean_device(Some("00112233&w=1")), None);
            assert_eq!(clean_device(Some("00112233")).as_deref(), Some("00112233"));
        }
    }

    #[test]
    fn port_comes_from_the_host_header() {
        let mut h = HeaderMap::new();
        h.insert("host", "127.0.0.1:19114".parse().unwrap());
        assert_eq!(port_of(&h), 19114);
        assert_eq!(port_of(&HeaderMap::new()), 19113);
    }
}
