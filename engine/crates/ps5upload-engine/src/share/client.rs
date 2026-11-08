//! The receiving engine's connection to the sender: the link's room on the signalling hub, then a
//! WebRTC data channel straight to the sender (this engine makes the offer, as the guest page does
//! for Play Together). ICE ranks the home network's addresses first, then Tailscale's, then what
//! the internet sees (STUN): the same order the sender's candidates have (rtc.rs). When the hub
//! can't be reached and the link has a home-network hint, the sender's own `/api/share/ws` on the
//! LAN is used instead.
//!
//! The answer must carry the fingerprint the link names: a hub that tried to stand in the middle
//! would have to answer with its own certificate, and is refused.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as TMsg;

use super::wire::{In, Link};
use crate::join_net::chunk::{self, Piece, Reassembler};

/// The hub must have answered by then.
const HUB_WAIT: Duration = Duration::from_secs(15);
/// ICE and DTLS must be through this soon after the answer.
const OPEN_WAIT: Duration = Duration::from_secs(30);
/// STUN servers have this long to answer.
const STUN_WAIT: Duration = Duration::from_millis(1500);
/// The largest message the sender sends (a piece of a file and its header), with room.
const MSG_MAX: usize = super::wire::PIECE + 64 * 1024;
/// Whole messages from the channel waiting for the session (a slow disk holds the driver here).
const TO_SESSION: usize = 32;

/// What a Share link says. None of it is secret: the room only lets someone ask, the sender
/// still allows each friend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShareLink {
    pub room: String,
    /// The sender's DTLS certificate fingerprint.
    pub fp: Option<String>,
    /// The hub's host (and port).
    pub hub: Option<String>,
    /// The sender's page on the home network (`http://192.168.1.10:19113/share`).
    pub lan: Option<String>,
}

fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Reads a Share link: the website's (`https://ps5upload.phantomptr.com/share/#room=…&fp=…&hub=…
/// &lan=…`) or a home network's (`http://192.168.1.10:19113/share#room=…`).
pub(crate) fn parse_link(s: &str) -> Result<ShareLink, &'static str> {
    let s = s.trim();
    let (base, frag) = s.split_once('#').ok_or("not a Share link")?;
    let mut link = ShareLink {
        room: String::new(),
        fp: None,
        hub: None,
        lan: None,
    };
    for kv in frag.split('&') {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        let v = pct_decode(v).ok_or("not a Share link")?;
        match k {
            "room" => link.room = v,
            "fp" => link.fp = Some(v),
            "hub" => link.hub = Some(v),
            "lan" => link.lan = Some(v),
            _ => {}
        }
    }
    let room_ok = (8..=64).contains(&link.room.len())
        && link
            .room
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !room_ok {
        return Err("not a Share link");
    }
    if link
        .fp
        .as_deref()
        .is_some_and(|f| f.len() > 200 || !f.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'))
    {
        return Err("not a Share link");
    }
    if link.hub.as_deref().is_some_and(|h| {
        h.is_empty()
            || h.len() > 255
            || !h
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    }) {
        return Err("not a Share link");
    }
    // A home network's own link: its page is the hint.
    let base_l = base.to_ascii_lowercase();
    if link.lan.is_none()
        && base_l.starts_with("http://")
        && base_l.trim_end_matches('/').ends_with("/share")
    {
        link.lan = Some(base.trim_end_matches('/').to_string());
    }
    if link
        .lan
        .as_deref()
        .is_some_and(|l| !l.starts_with("http://") && !l.starts_with("https://"))
    {
        link.lan = None;
    }
    if link.hub.is_none() && link.lan.is_none() {
        return Err("not a Share link");
    }
    Ok(link)
}

/// The hub's address for a link's hub host: the configured hub when it is that one (a hub under
/// test speaks `ws://`), else `wss://<host>/ws`.
pub(crate) fn hub_url(host: &str) -> String {
    let configured = crate::join::settings().hub_url;
    if crate::join_net::hub_host(&configured).as_deref() == Some(host) {
        configured
    } else {
        format!("wss://{host}/ws")
    }
}

/// The LAN socket for a home network's page: `http://ip:port[/prefix]/share` →
/// `ws://ip:port[/prefix]/api/share/ws`, only a private address (pad_link's check).
pub(crate) fn lan_ws(lan: &str) -> Option<String> {
    let base = lan.trim_end_matches('/');
    let base = base.strip_suffix("/share")?;
    let ws = if let Some(r) = base.strip_prefix("http://") {
        format!("ws://{r}/api/share/ws")
    } else {
        format!("wss://{}/api/share/ws", base.strip_prefix("https://")?)
    };
    crate::pad_link::lan_ws_target(&ws, "api/share/ws")
}

// ── The LAN socket ───────────────────────────────────────────────────────────────────────────────

type LanWs = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

pub(crate) struct WsLink(LanWs);

impl Link for WsLink {
    async fn send_text(&mut self, t: String) -> Result<(), ()> {
        self.0.send(TMsg::Text(t.into())).await.map_err(|_| ())
    }
    async fn recv(&mut self) -> Option<In> {
        loop {
            match self.0.next().await? {
                Ok(TMsg::Text(t)) => return Some(In::Text(t.to_string())),
                Ok(TMsg::Binary(b)) => return Some(In::Binary(b.to_vec())),
                Ok(TMsg::Close(_)) | Err(_) => return None,
                Ok(_) => continue,
            }
        }
    }
    fn path(&self) -> Option<&'static str> {
        Some("lan")
    }
}

/// Connects to the sender's `/api/share/ws` on the home network.
pub(crate) async fn connect_lan(lan: &str) -> Result<WsLink, String> {
    let target = lan_ws(lan).ok_or("the link's home-network address isn't a private one")?;
    let uri: axum::http::Uri = target.parse().map_err(|_| "bad address".to_string())?;
    if uri.scheme_str() != Some("ws") {
        return Err("the home network's page must be http".into());
    }
    let host = uri.host().ok_or("bad address")?.to_string();
    let port = uri.port_u16().unwrap_or(80);
    let tcp = tokio::time::timeout(HUB_WAIT, crate::pad_link::connect_lan(&host, port))
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|_| "the sender's computer can't be reached on this network".to_string())?;
    let (ws, _) = tokio_tungstenite::client_async(target.as_str(), tcp)
        .await
        .map_err(|e| format!("WebSocket: {e}"))?;
    Ok(WsLink(ws))
}

// ── The WebRTC data channel ──────────────────────────────────────────────────────────────────────

pub(crate) struct RtcLink {
    out: mpsc::UnboundedSender<String>,
    rx: mpsc::Receiver<In>,
    path: Arc<Mutex<Option<&'static str>>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for RtcLink {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

impl Link for RtcLink {
    async fn send_text(&mut self, t: String) -> Result<(), ()> {
        self.out.send(t).map_err(|_| ())
    }
    async fn recv(&mut self) -> Option<In> {
        self.rx.recv().await
    }
    fn path(&self) -> Option<&'static str> {
        *self.path.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The `a=fingerprint:sha-256` of an SDP, upper case.
pub(crate) fn sdp_fingerprint(sdp: &str) -> Option<String> {
    sdp.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("a=fingerprint:")?;
        let (alg, fp) = rest.split_once(' ')?;
        alg.eq_ignore_ascii_case("sha-256")
            .then(|| fp.trim().to_ascii_uppercase())
    })
}

async fn next_hub(ws: &mut crate::join_net::hub::Ws) -> Result<serde_json::Value, String> {
    loop {
        let m = tokio::time::timeout(HUB_WAIT, ws.next())
            .await
            .map_err(|_| "the hub did not answer".to_string())?
            .ok_or("the hub closed the connection")?
            .map_err(|e| format!("hub: {e}"))?;
        if let TMsg::Text(t) = m {
            return serde_json::from_str(t.as_str()).map_err(|_| "hub: not JSON".to_string());
        }
    }
}

/// What STUN servers see of each socket: server-reflexive candidates.
async fn stun_candidates(socks: &[(Arc<UdpSocket>, SocketAddr)]) -> Vec<Candidate> {
    let mut servers = Vec::new();
    for s in crate::join::settings().stun.iter().take(2) {
        if cfg!(test) && s.parse::<SocketAddr>().is_err() {
            continue;
        }
        if let Ok(Ok(found)) =
            tokio::time::timeout(Duration::from_secs(2), tokio::net::lookup_host(s.as_str())).await
        {
            servers.extend(found.filter(|a| a.is_ipv4()).take(1));
        }
    }
    let mut out: Vec<Candidate> = Vec::new();
    for (sock, base) in socks {
        let mut txs = Vec::new();
        for srv in &servers {
            let mut tx = [0u8; 12];
            if getrandom::fill(&mut tx).is_err() {
                continue;
            }
            txs.push(tx);
            let _ = sock.send_to(&crate::join_net::stun::request(tx), srv).await;
        }
        if txs.is_empty() {
            continue;
        }
        let until = tokio::time::Instant::now() + STUN_WAIT;
        let mut buf = [0u8; 1500];
        while let Ok(Ok((n, _))) = tokio::time::timeout_at(until, sock.recv_from(&mut buf)).await {
            let b = &buf[..n];
            let Some(tx) = crate::join_net::stun::response_tx(b) else {
                continue;
            };
            if !txs.contains(&tx) {
                continue;
            }
            if let Some(addr) = crate::join_net::stun::mapped(b) {
                if addr != *base && addr.is_ipv4() && !out.iter().any(|c| c.addr() == addr) {
                    if let Ok(c) = Candidate::server_reflexive(addr, *base, "udp") {
                        out.push(c);
                    }
                }
            }
            break;
        }
    }
    out
}

/// Joins the link's room on the hub and opens a "share" data channel to the sender.
pub(crate) async fn connect_rtc(link: &super::client::ShareLink) -> Result<RtcLink, String> {
    let host = link.hub.as_deref().ok_or("the link names no hub")?;
    let want_fp = link
        .fp
        .as_deref()
        .ok_or("the link has no fingerprint")?
        .to_ascii_uppercase();
    let mut hub = crate::join_net::hub::connect(&hub_url(host)).await?;
    let send = |v: serde_json::Value| TMsg::Text(v.to_string().into());
    hub.send(send(
        serde_json::json!({"v": 1, "t": "join", "room": link.room}),
    ))
    .await
    .map_err(|e| format!("hub: {e}"))?;
    loop {
        let m = next_hub(&mut hub).await?;
        match m["t"].as_str() {
            Some("joined") => break,
            Some("error") | Some("closed") => {
                return Err(
                    "The link isn't open: it may have expired or been cancelled, or the sender's \
                     computer is off."
                        .into(),
                )
            }
            _ => {}
        }
    }

    // This computer's sockets, the home network's first.
    let mut socks: Vec<(Arc<UdpSocket>, SocketAddr)> = Vec::new();
    for ip in crate::join_net::rtc::local_ips() {
        if let Ok(s) = UdpSocket::bind((ip, 0)).await {
            if let Ok(a) = s.local_addr() {
                socks.push((Arc::new(s), a));
            }
        }
    }
    if socks.is_empty() {
        return Err("no network to receive on".into());
    }
    let srflx = stun_candidates(&socks).await;
    let mut rtc = Rtc::builder()
        .set_crypto_provider(crate::join_net::crypto::provider())
        .set_stats_interval(Some(Duration::from_secs(1)))
        .build(Instant::now());
    crate::join_net::rtc::add_host_candidates(&mut rtc, socks.iter().map(|s| s.1));
    for c in srflx {
        rtc.add_local_candidate(c);
    }
    let mut api = rtc.sdp_api();
    let cid = api.add_channel("share".into());
    let (offer, pending): (_, SdpPendingOffer) = api.apply().ok_or("could not make an offer")?;
    let signal = |data: serde_json::Value| {
        TMsg::Text(
            serde_json::json!({"v": 1, "t": "signal", "to": "host", "data": data})
                .to_string()
                .into(),
        )
    };
    hub.send(signal(
        serde_json::json!({"type": "offer", "sdp": offer.to_sdp_string()}),
    ))
    .await
    .map_err(|e| format!("hub: {e}"))?;
    let mut early: Vec<String> = Vec::new();
    let answer = loop {
        let m = next_hub(&mut hub).await?;
        if m["t"] == "closed" {
            return Err("The sender's link closed.".into());
        }
        if m["t"] != "signal" {
            continue;
        }
        match m["data"]["type"].as_str() {
            Some("answer") => break m["data"]["sdp"].as_str().unwrap_or("").to_string(),
            Some("candidate") => early.extend(m["data"]["candidate"].as_str().map(str::to_string)),
            Some("bye") => {
                return Err("The sender's computer is busy; try again in a minute.".into())
            }
            _ => {}
        }
    };
    if sdp_fingerprint(&answer).as_deref() != Some(want_fp.as_str()) {
        crate::log_at!(
            Info,
            "together",
            "share: the answer's fingerprint is not the link's"
        );
        return Err(
            "The connection was refused: the sender's certificate isn't the one in the link."
                .into(),
        );
    }
    let ans = SdpAnswer::from_sdp_string(&answer).map_err(|_| "a bad answer".to_string())?;
    rtc.sdp_api()
        .accept_answer(pending, ans)
        .map_err(|e| format!("answer: {e}"))?;
    for c in early {
        if let Ok(c) = Candidate::from_sdp_string(c.trim().trim_start_matches("a=")) {
            if c.addr().is_ipv4() {
                rtc.add_remote_candidate(c);
            }
        }
    }

    let (in_tx, in_rx) = mpsc::channel(TO_SESSION);
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (open_tx, open_rx) = tokio::sync::oneshot::channel();
    let (cand_tx, cand_rx) = mpsc::unbounded_channel::<String>();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let path = Arc::new(Mutex::new(None));
    // The hub stays until the channel is open (late candidates), then goes.
    let (hub_done_tx, mut hub_done_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut hub_done_rx => break,
                m = hub.next() => match m {
                    Some(Ok(TMsg::Text(t))) => {
                        let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                        if v["data"]["type"] == "candidate" {
                            if let Some(c) = v["data"]["candidate"].as_str() {
                                let _ = cand_tx.send(c.to_string());
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
        }
        let _ = hub
            .send(TMsg::Text(
                serde_json::json!({"v": 1, "t": "leave"}).to_string().into(),
            ))
            .await;
        let _ = hub.close(None).await;
    });
    tokio::spawn(drive(Driver {
        rtc,
        cid,
        socks,
        in_tx,
        out_rx,
        cand_rx,
        open_tx: Some(open_tx),
        stop_rx,
        path: path.clone(),
    }));
    let opened = tokio::time::timeout(OPEN_WAIT, open_rx).await;
    let _ = hub_done_tx.send(());
    match opened {
        Ok(Ok(())) => Ok(RtcLink {
            out: out_tx,
            rx: in_rx,
            path,
            stop: Some(stop_tx),
        }),
        _ => {
            let _ = stop_tx.send(());
            Err(
                "No direct connection to the sender could be made (both routers may block it)."
                    .into(),
            )
        }
    }
}

struct Driver {
    rtc: Rtc,
    cid: ChannelId,
    socks: Vec<(Arc<UdpSocket>, SocketAddr)>,
    in_tx: mpsc::Sender<In>,
    out_rx: mpsc::UnboundedReceiver<String>,
    cand_rx: mpsc::UnboundedReceiver<String>,
    open_tx: Option<tokio::sync::oneshot::Sender<()>>,
    stop_rx: tokio::sync::oneshot::Receiver<()>,
    path: Arc<Mutex<Option<&'static str>>>,
}

async fn drive(mut d: Driver) {
    let (pkt_tx, mut pkt_rx) = mpsc::channel::<(SocketAddr, SocketAddr, Vec<u8>)>(1024);
    let mut readers = Vec::new();
    for (s, local) in &d.socks {
        let (s, local, tx) = (s.clone(), *local, pkt_tx.clone());
        readers.push(tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match s.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if tx.send((local, from, buf[..n].to_vec())).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => {
                        if tx.is_closed() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }
            }
        }));
    }
    drop(pkt_tx);
    let mut reasm = Reassembler::new(MSG_MAX);
    let mut pending: std::collections::VecDeque<(bool, Vec<u8>)> = Default::default();
    let mut out_open = true;
    'run: loop {
        if let Some(mut ch) = d.rtc.channel(d.cid) {
            while let Some((b, data)) = pending.front() {
                if ch.write(*b, data).unwrap_or(false) {
                    pending.pop_front();
                } else {
                    break;
                }
            }
        }
        let timeout = loop {
            match d.rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Transmit(t)) => {
                    if let Some((s, _)) = d.socks.iter().find(|(_, a)| *a == t.source) {
                        let _ = s.send_to(&t.contents, t.destination).await;
                    }
                }
                Ok(Output::Event(e)) => match e {
                    Event::ChannelOpen(id, _) if id == d.cid => {
                        if let Some(o) = d.open_tx.take() {
                            let _ = o.send(());
                        }
                    }
                    Event::ChannelData(c) if c.id == d.cid => {
                        let m = if c.binary {
                            match reasm.push(&c.data) {
                                Ok(Piece::Partial) | Ok(Piece::Pong(_)) => continue,
                                Ok(Piece::Message(m)) => In::Binary(m),
                                Ok(Piece::Ping(b)) => {
                                    let mut v = vec![chunk::PONG];
                                    v.extend_from_slice(&b);
                                    pending.push_back((true, v));
                                    continue;
                                }
                                Err(_) => break 'run,
                            }
                        } else {
                            match String::from_utf8(c.data) {
                                Ok(t) => In::Text(t),
                                Err(_) => break 'run,
                            }
                        };
                        // A slow disk holds the connection here (bounded).
                        if d.in_tx.send(m).await.is_err() {
                            break 'run;
                        }
                    }
                    Event::ChannelClose(id) if id == d.cid => break 'run,
                    Event::PeerStats(s) => {
                        if let Some(p) = s.selected_candidate_pair.as_ref() {
                            *d.path.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(crate::join_net::rtc::path_of(p.local.addr, p.remote.addr));
                        }
                    }
                    _ => {}
                },
                Err(_) => break 'run,
            }
            if !d.rtc.is_alive() {
                break 'run;
            }
        };
        let wait = timeout
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        tokio::select! {
            p = pkt_rx.recv() => {
                let Some((local, from, data)) = p else { break 'run };
                if let Ok(contents) = data.as_slice().try_into() {
                    let _ = d.rtc.handle_input(Input::Receive(Instant::now(), Receive {
                        proto: Protocol::Udp, source: from, destination: local, contents,
                    }));
                }
            }
            m = d.out_rx.recv(), if out_open => match m {
                Some(t) => pending.push_back((false, t.into_bytes())),
                None => out_open = false,
            },
            c = d.cand_rx.recv() => {
                if let Some(c) = c {
                    if let Ok(c) = Candidate::from_sdp_string(c.trim().trim_start_matches("a=")) {
                        if c.addr().is_ipv4() && !matches!(c.addr().ip(), IpAddr::V4(a) if a.is_unspecified()) {
                            d.rtc.add_remote_candidate(c);
                        }
                    }
                }
            }
            _ = &mut d.stop_rx => break 'run,
            _ = tokio::time::sleep(wait) => {}
        }
        if !out_open && pending.is_empty() {
            // The session is gone and said all it had to.
            break 'run;
        }
        if d.rtc.handle_input(Input::Timeout(Instant::now())).is_err() {
            break 'run;
        }
    }
    d.rtc.disconnect();
    for r in readers {
        r.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_read_and_checked() {
        let l = parse_link(
            "https://ps5upload.phantomptr.com/share/#room=AbCdEfGhIjKlMnOpQrStUv&fp=AA:BB:cc&hub=hub.phantomptr.com&lan=http%3A%2F%2F192.168.1.10%3A19113%2Fshare",
        )
        .unwrap();
        assert_eq!(l.room, "AbCdEfGhIjKlMnOpQrStUv");
        assert_eq!(l.fp.as_deref(), Some("AA:BB:cc"));
        assert_eq!(l.hub.as_deref(), Some("hub.phantomptr.com"));
        assert_eq!(l.lan.as_deref(), Some("http://192.168.1.10:19113/share"));
        // The home network's own link.
        let l = parse_link("http://192.168.1.10:19113/share#room=AbCdEfGhIjKlMnOpQrStUv").unwrap();
        assert_eq!(l.lan.as_deref(), Some("http://192.168.1.10:19113/share"));
        assert!(l.hub.is_none());
        for bad in [
            "",
            "https://x/share/",
            "https://x/share/#room=short&hub=h",
            "https://x/share/#room=AbCdEfGhIjKlMnOpQrStUv",
            "https://x/share/#room=AbCdEfGhIjKl/../Uv&hub=h",
            "https://x/share/#room=AbCdEfGhIjKlMnOpQrStUv&hub=evil/path",
            "https://x/share/#room=AbCdEfGhIjKlMnOpQrStUv&hub=h&fp=<script>",
        ] {
            assert!(parse_link(bad).is_err(), "{bad} read");
        }
        // A hint that isn't a web page is dropped.
        let l = parse_link("https://x/share/#room=AbCdEfGhIjKlMnOpQrStUv&hub=h&lan=file%3A%2F%2Fx")
            .unwrap();
        assert!(l.lan.is_none());
    }

    #[test]
    fn the_lan_socket_is_only_a_private_address() {
        assert_eq!(
            lan_ws("http://192.168.1.10:19113/share").as_deref(),
            Some("ws://192.168.1.10:19113/api/share/ws")
        );
        assert!(lan_ws("http://8.8.8.8:19113/share").is_none());
        assert!(lan_ws("http://192.168.1.10:19113/other").is_none());
    }

    #[test]
    fn the_answers_fingerprint() {
        let sdp = "v=0\r\na=fingerprint:sha-256 ab:cd:EF\r\n";
        assert_eq!(sdp_fingerprint(sdp).as_deref(), Some("AB:CD:EF"));
        assert!(sdp_fingerprint("v=0\r\na=fingerprint:sha-1 ab\r\n").is_none());
    }
}
