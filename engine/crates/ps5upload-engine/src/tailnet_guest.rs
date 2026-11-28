//! The guest port on the engine's own tailnet node: how a friend's PS5 running PS5Upload Play
//! reaches this engine over Tailscale when this device is on the tailnet through PS5Upload itself
//! (Settings › Tailscale), with no Tailscale app here.
//!
//! The node ([`crate::tailnet_engine`]) serves ONE TCP port on its tailnet address: the engine's
//! own HTTP port, so the address the friend's app already knows (`100.x.y.z:<port>`, from the
//! mesh) works as it does with the system's Tailscale app. The core splices each connection to a
//! listener of its own here, on 127.0.0.1 at a port picked at start, never the main listener:
//! every spliced connection comes from 127.0.0.1, and the main listener lets a loopback peer reach
//! the whole API. To say who is really asking, the core starts each connection with a PROXY
//! protocol v1 line (`PROXY TCP4 <peer> <our address> <peer port> <port>\r\n`, tailnet.h
//! `TN_SERVE_PROXY_V1`), and this listener:
//!
//! * reads that line first, within [`PROXY_WAIT`] and at most [`PROXY_LINE_MAX`] bytes, and closes
//!   the connection without a word when it is missing, malformed, `UNKNOWN`, not TCP4, or names a
//!   source outside Tailscale's addresses (100.64.0.0/10). A connection is never taken as
//!   loopback, whatever it says;
//! * then serves HTTP on it with the peer's tailnet address as its `ConnectInfo`, through a router
//!   of its own that holds exactly the two sockets a friend's PS5 uses: `GET /api/join/ws` (the
//!   guest socket, [`crate::join::ws`]) and `GET /api/play/app` (PS5Upload Play's channel,
//!   [`crate::play_app::ws`]). Everything else, the host API included, answers 404. The handlers
//!   are the main router's own and treat the 100.x peer as any LAN peer (limits by address, the
//!   invite or token checked by the socket).
//!
//! The browser checks of the main listener (`browser_origin_guard` in lib.rs: the Host name and
//! cross-site Origin) apply here too. Nothing a browser should use is served here, but a web page
//! open on a tailnet device can still point a WebSocket at `100.x:<port>`; with the check it is
//! refused as it is on the home network, so it cannot spend that device's guest limits or ask the
//! TV for pairing codes. PS5Upload Play sends no Origin and is not affected.
//!
//! Trust: anything on this computer can connect to this listener and write a PROXY line naming
//! any 100.x address. It reaches only these two sockets, which it reaches anyway on the main
//! listener (as a loopback peer, with higher limits and the host API besides), so it gains
//! nothing. A tailnet peer reaches only these two sockets, and only when the tailnet's own access
//! rules let it open the port at all.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::routing::get;
use axum::Router;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Semaphore};

/// How long the core's PROXY line may take to arrive (it is written before any byte of the peer's,
/// so a real one is there at once).
pub const PROXY_WAIT: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 3000 });
/// The longest v1 line, CRLF included (the PROXY protocol spec, 2.1).
pub const PROXY_LINE_MAX: usize = 107;
/// Connections still in their PROXY line at once; more are closed (the core itself splices at most
/// 24 connections, so only a local process can get here).
const HANDSHAKES_AT_ONCE: usize = 32;

/// Tailscale's IPv4 addresses (100.64.0.0/10).
pub fn is_tailnet_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 100 && (o[1] & 0xc0) == 64
}

/// A decimal port as the spec writes it: digits only, no leading zero, not 0.
fn port_of(s: &str) -> Option<u16> {
    if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) || s.starts_with('0') {
        return None;
    }
    s.parse().ok()
}

/// The peer a PROXY v1 line names (`PROXY TCP4 <src> <dst> <sport> <dport>\r\n`), when it is a
/// well-formed TCP4 line from one tailnet address to another. Everything else is None: `UNKNOWN`,
/// `TCP6`, extra or missing fields, a loopback, LAN or public source.
pub fn parse_proxy_v1(line: &[u8]) -> Option<SocketAddr> {
    if line.len() > PROXY_LINE_MAX {
        return None;
    }
    let text = std::str::from_utf8(line.strip_suffix(b"\r\n")?).ok()?;
    if text.contains(['\r', '\n']) {
        return None;
    }
    let f: Vec<&str> = text.split(' ').collect();
    let [proxy, tcp4, src, dst, sport, dport] = f.as_slice() else {
        return None;
    };
    if *proxy != "PROXY" || *tcp4 != "TCP4" {
        return None;
    }
    let src: Ipv4Addr = src.parse().ok()?;
    let dst: Ipv4Addr = dst.parse().ok()?;
    let sport = port_of(sport)?;
    port_of(dport)?;
    (is_tailnet_v4(src) && is_tailnet_v4(dst)).then_some(SocketAddr::new(IpAddr::V4(src), sport))
}

/// The first line of a connection, read a byte at a time so nothing after it is taken from the
/// stream. None as soon as it cannot be a PROXY line (wrong start, too long, the peer closed).
async fn read_proxy_line(s: &mut TcpStream) -> Option<Vec<u8>> {
    const START: &[u8] = b"PROXY ";
    let mut line = Vec::with_capacity(64);
    loop {
        let b = s.read_u8().await.ok()?;
        line.push(b);
        if line.len() <= START.len() && START[line.len() - 1] != b {
            return None;
        }
        if line.ends_with(b"\r\n") {
            return Some(line);
        }
        if line.len() >= PROXY_LINE_MAX {
            return None;
        }
    }
}

/// Connections refused so far: logged at the first one and then at each power of two, so a local
/// process knocking all day cannot fill the log.
static REFUSED: AtomicU64 = AtomicU64::new(0);

fn note_refused() {
    let n = REFUSED.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_power_of_two() {
        crate::log_at!(
            Debug,
            "tailnet",
            "tailnet: guest port closed a connection without a valid tailnet PROXY line ({n} so far)"
        );
    }
}

/// Takes connections from the 127.0.0.1 socket, keeps those whose PROXY line names a tailnet
/// peer, and hands them on with that peer's address.
async fn admit(l: TcpListener, tx: mpsc::Sender<(TcpStream, SocketAddr)>) {
    let gate = Arc::new(Semaphore::new(HANDSHAKES_AT_ONCE));
    while !tx.is_closed() {
        let (mut s, from) = match l.accept().await {
            Ok(c) => c,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        // Bound to 127.0.0.1: nothing else can be here. Checked anyway.
        let Ok(permit) = gate.clone().try_acquire_owned() else {
            note_refused();
            continue;
        };
        if !from.ip().is_loopback() {
            note_refused();
            continue;
        }
        let tx = tx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let peer = match tokio::time::timeout(PROXY_WAIT, read_proxy_line(&mut s)).await {
                Ok(Some(line)) => parse_proxy_v1(&line),
                _ => None,
            };
            match peer {
                Some(peer) => {
                    let _ = tx.send((s, peer)).await;
                }
                None => note_refused(), // dropped: closed, no answer
            }
        });
    }
}

/// The admitted connections as axum's listener: each comes with its tailnet peer's address.
struct GuestListener {
    rx: mpsc::Receiver<(TcpStream, SocketAddr)>,
    local: SocketAddr,
}

impl axum::serve::Listener for GuestListener {
    type Io = TcpStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (TcpStream, SocketAddr) {
        match self.rx.recv().await {
            Some(c) => c,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local)
    }
}

#[cfg(test)]
static SEEN: std::sync::Mutex<Vec<(String, SocketAddr)>> = std::sync::Mutex::new(Vec::new());

/// Tests: what `ConnectInfo` the two handlers are given (the same request they extract it from).
#[cfg(test)]
async fn note_peer(
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    SEEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((req.uri().path().to_string(), peer));
    next.run(req).await
}

/// The guest port's whole HTTP surface: the two sockets, and 404 for everything else (another
/// method on those two paths included).
fn router() -> Router {
    let r = Router::new()
        .route(
            "/api/join/ws",
            get(crate::join::ws).fallback(crate::api_errors::fallback),
        )
        .route(
            "/api/play/app",
            get(crate::play_app::ws).fallback(crate::api_errors::fallback),
        );
    #[cfg(test)]
    let r = r.route_layer(axum::middleware::from_fn(note_peer));
    r.fallback(crate::api_errors::fallback)
        .layer(axum::middleware::from_fn(crate::log_requests))
        .layer(axum::middleware::from_fn(crate::browser_origin_guard))
        // Errors in the API's shape, as on the main listener (no base path: the core reaches this
        // port directly, never through a reverse proxy).
        .layer(axum::middleware::from_fn_with_state(
            Arc::<str>::from(""),
            crate::api_errors::api_surface,
        ))
}

/// The running guest port; dropping it stops it.
pub struct Guest {
    pub local_port: u16,
    tasks: Vec<tokio::task::AbortHandle>,
}

impl Drop for Guest {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// Every connection sends at once (as the main listener's, lib.rs `no_delay`).
fn set_nodelay(tcp: &mut TcpStream) {
    let _ = tcp.set_nodelay(true);
}

/// Binds the guest listener on 127.0.0.1 (any free port) and serves it. None when it cannot be
/// bound: the node then serves nothing.
pub async fn start() -> Option<Guest> {
    use axum::serve::ListenerExt;
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.ok()?;
    let local = l.local_addr().ok()?;
    let (tx, rx) = mpsc::channel(16);
    let admit = tokio::spawn(admit(l, tx));
    let listener = GuestListener { rx, local }.tap_io(set_nodelay as fn(&mut TcpStream));
    let serve = tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            router().into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Some(Guest {
        local_port: local.port(),
        tasks: vec![admit.abort_handle(), serve.abort_handle()],
    })
}

/// At engine start, before the tailnet node: the guest listener, and what the node serves (the
/// engine's HTTP port on its tailnet address, spliced here). Keep the result for as long as the
/// engine serves.
pub async fn boot(http_port: u16) -> Option<Guest> {
    let guest = start().await;
    match &guest {
        Some(g) => crate::tailnet_engine::set_guest_port(http_port, Some(g.local_port)),
        None => {
            crate::tailnet_engine::set_guest_port(http_port, None);
            crate::log_at!(
                Warn,
                "tailnet",
                "tailnet: no guest port (127.0.0.1 could not be bound): a friend's PS5 cannot \
                 reach this engine through PS5Upload's own Tailscale"
            );
        }
    }
    guest
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::tungstenite::Message as TMsg;

    const PEER: &str = "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113\r\n";

    #[test]
    fn a_proxy_line_names_only_a_tailnet_peer() {
        let ok = |l: &str| parse_proxy_v1(l.as_bytes());
        assert_eq!(
            ok(PEER),
            Some("100.101.2.3:41000".parse::<SocketAddr>().unwrap())
        );
        assert_eq!(
            ok("PROXY TCP4 100.127.255.254 100.64.0.1 1 65535\r\n"),
            Some("100.127.255.254:1".parse::<SocketAddr>().unwrap())
        );
        for bad in [
            "PROXY UNKNOWN\r\n",
            "PROXY UNKNOWN 100.101.2.3 100.64.0.1 41000 19113\r\n",
            "PROXY TCP6 fd7a:115c:a1e0::1 fd7a:115c:a1e0::2 41000 19113\r\n",
            "PROXY TCP4 127.0.0.1 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 192.168.1.5 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 100.128.0.1 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 100.63.255.255 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 8.8.8.8 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 100.101.2.3 127.0.0.1 41000 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113",
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113 x\r\n",
            "PROXY TCP4  100.101.2.3 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 041000 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 0 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 +4100 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 65536 19113\r\n",
            "PROXY TCP4 100.101.002.3 100.64.0.1 41000 19113\r\n",
            "proxy tcp4 100.101.2.3 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113\r\n\r\n",
            "GET / HTTP/1.1\r\n",
            "",
        ] {
            assert_eq!(ok(bad), None, "{bad:?}");
        }
        let long = format!(
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113{}\r\n",
            " ".repeat(70)
        );
        assert_eq!(ok(&long), None);
    }

    /// A connection to the guest port as the core makes one: `first` is written before anything.
    async fn dial(port: u16, first: &str) -> TcpStream {
        let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        s.write_all(first.as_bytes()).await.unwrap();
        s
    }

    /// The whole answer to a plain request (the server closes after it).
    async fn http(port: u16, first: &str, req: &str) -> String {
        let mut s = dial(port, first).await;
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    fn get_req(path: &str) -> String {
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
    }

    /// Closed with nothing said: EOF (or a reset) before a single byte.
    async fn closed_silently(mut s: TcpStream, then: &str) {
        let _ = s.write_all(then.as_bytes()).await;
        let mut b = [0u8; 64];
        let r = tokio::time::timeout(Duration::from_secs(5), s.read(&mut b))
            .await
            .expect("the guest port should close the connection");
        assert!(matches!(r, Ok(0) | Err(_)), "answered: {r:?}");
    }

    #[tokio::test]
    async fn only_the_two_sockets_answer_and_they_see_the_tailnet_peer() {
        let g = start().await.unwrap();
        let port = g.local_port;
        // The host's API, pages, and the other LAN routes: not here.
        for path in [
            "/",
            "/join",
            "/pad",
            "/share",
            "/api/join",
            "/api/join/signal",
            "/api/join/relay?url=x",
            "/api/play/app/paired",
            "/api/ps5/status",
            "/api/jobs",
            "/api/settings",
            "/api/tailnet/status",
            "/api/pad/ws",
            "/api/share/ws",
            "/pkg-host/x",
            "/app-icon/x",
            "/api/join/ws/x",
            "/api/play/appx",
        ] {
            let a = http(port, PEER, &get_req(path)).await;
            assert!(a.starts_with("HTTP/1.1 404"), "{path}: {a}");
        }
        for (method, path) in [
            ("POST", "/api/join/open"),
            ("POST", "/api/join/close"),
            ("POST", "/api/join/decide"),
            ("POST", "/api/play/app/pair"),
            ("POST", "/api/play/app/unpair"),
            ("POST", "/api/play/app/moved"),
            ("POST", "/api/play/app/pair_confirm"),
            ("POST", "/api/join/ws"),
            ("DELETE", "/api/play/app"),
        ] {
            let req = format!(
                "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            let a = http(port, PEER, &req).await;
            assert!(a.starts_with("HTTP/1.1 404"), "{method} {path}: {a}");
        }
        // A plain GET of a socket's path (no upgrade) reaches the handler and is not a socket.
        let a = http(port, PEER, &get_req("/api/play/app")).await;
        assert!(!a.starts_with("HTTP/1.1 404") && !a.is_empty(), "{a}");

        // The two sockets work, and their handlers are given the peer's tailnet address.
        SEEN.lock().unwrap().clear();
        let s = dial(port, "PROXY TCP4 100.101.2.3 100.64.0.1 41001 19113\r\n").await;
        let (mut app, _) = tokio_tungstenite::client_async("ws://127.0.0.1/api/play/app", s)
            .await
            .unwrap();
        app.send(TMsg::Text(r#"{"hello":{}}"#.into()))
            .await
            .unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match app.next().await {
                    Some(Ok(TMsg::Text(t))) => return t.to_string(),
                    Some(Ok(_)) => continue,
                    other => panic!("socket ended: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&first).unwrap();
        // Without a token: only that an engine is there (play_app.rs), as a LAN peer gets.
        assert_eq!(v["doc"]["paired"], false, "{v}");
        let s = dial(port, "PROXY TCP4 100.101.2.4 100.64.0.1 41002 19113\r\n").await;
        let (guest, _) = tokio_tungstenite::client_async("ws://127.0.0.1/api/join/ws", s)
            .await
            .unwrap();
        drop(guest);
        drop(app);
        let seen = SEEN.lock().unwrap().clone();
        assert!(
            seen.contains(&("/api/play/app".into(), "100.101.2.3:41001".parse().unwrap())),
            "{seen:?}"
        );
        assert!(
            seen.contains(&("/api/join/ws".into(), "100.101.2.4:41002".parse().unwrap())),
            "{seen:?}"
        );
        assert!(seen.iter().all(|(_, p)| !p.ip().is_loopback()), "{seen:?}");
    }

    #[tokio::test]
    async fn a_connection_without_a_good_tailnet_line_is_closed_unanswered() {
        let g = start().await.unwrap();
        let port = g.local_port;
        let req = get_req("/api/play/app");
        // No line at all: straight HTTP, as a local browser or process would send.
        closed_silently(dial(port, &req).await, "").await;
        for first in [
            "PROXY garbage\r\n",
            "PROXY UNKNOWN\r\n",
            "PROXY TCP6 fd7a:115c:a1e0::1 fd7a:115c:a1e0::2 41000 19113\r\n",
            "PROXY TCP4 127.0.0.1 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 192.168.1.5 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 10.0.0.2 100.64.0.1 41000 19113\r\n",
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 19113\n",
        ] {
            closed_silently(dial(port, first).await, &req).await;
        }
        // Too long: no CRLF within the spec's 107 bytes.
        let long = format!(
            "PROXY TCP4 100.101.2.3 100.64.0.1 41000 {}",
            "1".repeat(120)
        );
        closed_silently(dial(port, &long).await, "\r\n").await;
        // Too slow: nothing, or half a line, past the wait.
        let t0 = std::time::Instant::now();
        closed_silently(dial(port, "").await, "").await;
        assert!(t0.elapsed() >= PROXY_WAIT);
        let half = dial(port, "PROXY TCP4 100.101.2.3").await;
        closed_silently(half, "").await;
        // The listener still serves a good one afterwards.
        let a = http(port, PEER, &get_req("/")).await;
        assert!(a.starts_with("HTTP/1.1 404"), "{a}");
    }

    #[tokio::test]
    async fn a_cross_site_page_is_refused_as_on_the_main_listener() {
        let g = start().await.unwrap();
        let req = "GET /api/play/app HTTP/1.1\r\nHost: 100.64.0.1:19113\r\nOrigin: http://evil.example\r\nSec-Fetch-Site: cross-site\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        let a = http(g.local_port, PEER, req).await;
        assert!(a.starts_with("HTTP/1.1 403"), "{a}");
    }

    #[tokio::test]
    async fn dropping_the_guest_port_closes_it() {
        let g = start().await.unwrap();
        let port = g.local_port;
        drop(g);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .is_err());
    }
}
