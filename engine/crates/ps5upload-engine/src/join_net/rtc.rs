//! The engine's one WebRTC endpoint: a UDP socket on each of this computer's IPv4 addresses (one
//! port number for all when it can), shared by every internet guest's peer connection (str0m tells
//! which packet is whose). One task drives them all; the guest sessions talk to it through
//! [`Cmd`].

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::config::DtlsCert;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::chunk::{self, Piece, Reassembler};
use super::{nat, stun, Flow, Kind, PeerInfo, RtcSocket};

/// Peers being set up or connected at once, every room together.
const MAX_PEERS: usize = 32;
/// …and per room.
const MAX_ROOM_PEERS: usize = 8;
/// ICE and DTLS must be through this soon after the offer.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
/// A connection that lost its path for this long has failed.
const DISCONNECTED_WAIT: Duration = Duration::from_secs(10);
/// A session that ended has this long to get its last messages (the bye) out.
const CLOSE_DRAIN: Duration = Duration::from_secs(2);
/// How long STUN servers have to answer.
const STUN_WAIT: Duration = Duration::from_secs(2);
/// What the data channel's SCTP may hold (str0m's default is 128 KiB, below [`super::HIGH_WATER`]).
pub(crate) const SCTP_BUFFER: usize = 1024 * 1024;
/// Messages queued from the channel to the session before the peer is dropped as a flood.
const TO_SESSION: usize = 256;

/// What the driver is asked.
pub(crate) enum Cmd {
    /// A room is open: gather the outside addresses, keep the router mapping. Its kind says which
    /// session its peers' channels get.
    RoomOpen(String, Kind),
    RoomClose(String),
    /// A guest's offer (signal `data.type == "offer"`); `signal` takes what goes back to them.
    Offer {
        room: String,
        gid: String,
        sdp: String,
        signal: mpsc::UnboundedSender<(String, serde_json::Value)>,
    },
    /// A guest's trickled candidate.
    Remote {
        room: String,
        gid: String,
        candidate: String,
    },
    /// The guest left the hub: a peer not connected yet goes (a connected one keeps playing).
    Left {
        room: String,
        gid: String,
    },
    /// A guest session's message (its length as [`Flow`] counted it).
    Up(u64, Message, usize),
    /// A guest session ended.
    Gone(u64),
    /// The router mapping, made or lost.
    Mapped(Option<(SocketAddr, SocketAddr, &'static str)>),
}

/// The engine's certificate and its fingerprint (one per engine run).
pub(crate) fn cert() -> Option<&'static (DtlsCert, String)> {
    static C: OnceLock<Option<(DtlsCert, String)>> = OnceLock::new();
    C.get_or_init(|| {
        let c = super::crypto::new_cert()?;
        let fp = super::crypto::fingerprint_of(&c.certificate);
        Some((c, fp))
    })
    .as_ref()
}

/// The running driver.
pub(crate) struct Net {
    pub(crate) cmd: mpsc::UnboundedSender<Cmd>,
}

/// The router mapping now: outside address and how it was made.
pub(crate) static NET_MAPPED: Mutex<Option<(SocketAddr, &'static str)>> = Mutex::new(None);

static NET: Mutex<Option<Arc<Net>>> = Mutex::new(None);

/// The driver, started on first use (in a Tokio runtime). None when no socket could be opened.
/// A driver whose task ended (its runtime shut down: each test has its own) is started again.
pub(crate) async fn net() -> Option<Arc<Net>> {
    let alive = |n: &Option<Arc<Net>>| n.clone().filter(|n| !n.cmd.is_closed());
    if let Some(n) = alive(&NET.lock().unwrap_or_else(|e| e.into_inner())) {
        return Some(n);
    }
    let n = start().await?;
    let mut g = NET.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(other) = alive(&g) {
        return Some(other); // another start won; this one's sockets close with its task
    }
    *g = Some(n.clone());
    Some(n)
}

/// The tests' engine for the guest page's Playwright test (a browser does not pair with a loopback
/// candidate): the real interfaces, as a running engine has them.
#[cfg(test)]
pub(crate) static TEST_REAL_IPS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The addresses the sockets go on: every IPv4 interface but loopback and link-local (the tests:
/// loopback only).
fn local_ips() -> Vec<Ipv4Addr> {
    #[cfg(test)]
    if !TEST_REAL_IPS.load(Ordering::Relaxed) {
        return vec![Ipv4Addr::LOCALHOST];
    }
    let mut v: Vec<Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() && !a.ip.is_link_local() => Some(a.ip),
            _ => None,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

async fn start() -> Option<Arc<Net>> {
    let mut socks: Vec<Arc<UdpSocket>> = Vec::new();
    let mut port = 0u16;
    for ip in local_ips() {
        // One port number for every address when it is free (one mapping fits them all).
        let s = match UdpSocket::bind((ip, port)).await {
            Ok(s) => s,
            Err(_) => match UdpSocket::bind((ip, 0)).await {
                Ok(s) => s,
                Err(_) => continue,
            },
        };
        if port == 0 {
            port = s.local_addr().map(|a| a.port()).unwrap_or(0);
        }
        socks.push(Arc::new(s));
    }
    if socks.is_empty() {
        crate::log_at!(
            Info,
            "together",
            "join: no address for internet guests (no IPv4 network)"
        );
        return None;
    }
    let local: Vec<SocketAddr> = socks.iter().filter_map(|s| s.local_addr().ok()).collect();
    crate::log_at!(
        Debug,
        "together",
        "join: internet guests' UDP port {port} on {} address(es)",
        local.len()
    );
    let (cmd, cmd_rx) = mpsc::unbounded_channel();
    let (pkt_tx, pkt_rx) = mpsc::channel(1024);
    for (i, s) in socks.iter().enumerate() {
        let s = s.clone();
        let tx = pkt_tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            loop {
                match s.recv_from(&mut buf).await {
                    Ok((n, from)) => {
                        if tx.send((i, from, buf[..n].to_vec())).await.is_err() {
                            return;
                        }
                    }
                    // An ICMP "port unreachable" from an earlier send shows up here on some
                    // systems (Windows): not the socket's end.
                    Err(_) => {
                        if tx.is_closed() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }
            }
        });
    }
    let net = Arc::new(Net { cmd: cmd.clone() });
    let d = Driver {
        socks: socks.into_iter().zip(local).collect(),
        extra: Vec::new(),
        peers: Vec::new(),
        rooms: HashMap::new(),
        stun_tx: HashMap::new(),
        cmd: cmd.clone(),
        nat_want: None,
    };
    tokio::spawn(d.run(cmd_rx, pkt_rx));
    Some(net)
}

struct Peer {
    id: u64,
    /// Its room's kind: the channel label it may open and the session that gets it.
    kind: Kind,
    info: Arc<PeerInfo>,
    rtc: Rtc,
    /// The offer's data section, for trickled candidates.
    mid: String,
    signal: mpsc::UnboundedSender<(String, serde_json::Value)>,
    cid: Option<ChannelId>,
    to_session: Option<mpsc::Sender<Message>>,
    flow: Arc<Flow>,
    /// Data-channel messages (binary pieces already framed) not yet taken by the channel.
    out: VecDeque<(bool, Vec<u8>)>,
    out_bytes: usize,
    reasm: Reassembler,
    started: Instant,
    disconnected_at: Option<Instant>,
    closing_at: Option<Instant>,
    /// Where its packets last came from (the guest's address, for the connect limit).
    last_src: Option<SocketAddr>,
}

static NEXT_PEER: AtomicU64 = AtomicU64::new(1);

struct Driver {
    socks: Vec<(Arc<UdpSocket>, SocketAddr)>,
    /// Outside candidates (STUN's, the router's), added to every new peer.
    extra: Vec<Candidate>,
    peers: Vec<Peer>,
    rooms: HashMap<String, Kind>,
    /// STUN requests in flight: transaction id → socket index.
    stun_tx: HashMap<[u8; 12], usize>,
    cmd: mpsc::UnboundedSender<Cmd>,
    /// The task keeping the router mapping, while one is wanted.
    nat_want: Option<tokio::sync::watch::Sender<bool>>,
}

fn ice_name(s: IceConnectionState) -> &'static str {
    match s {
        IceConnectionState::New => "new",
        IceConnectionState::Checking => "checking",
        IceConnectionState::Connected => "connected",
        IceConnectionState::Completed => "completed",
        IceConnectionState::Disconnected => "disconnected",
    }
}

/// The `a=mid:` of an offer's first section (the data channel's, in a data-only offer).
fn first_mid(sdp: &str) -> String {
    sdp.lines()
        .find_map(|l| l.trim().strip_prefix("a=mid:"))
        .unwrap_or("0")
        .to_string()
}

fn candidate_json(c: &Candidate, mid: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "candidate",
        "candidate": c.to_sdp_string(),
        "sdpMid": mid,
        "sdpMLineIndex": 0,
    })
}

impl Driver {
    async fn run(
        mut self,
        mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
        mut pkt_rx: mpsc::Receiver<(usize, SocketAddr, Vec<u8>)>,
    ) {
        loop {
            let now = Instant::now();
            let mut next = now + Duration::from_millis(250);
            for p in &mut self.peers {
                next = next.min(poll(p, &self.socks, &self.cmd, now));
            }
            self.housekeep(now);
            tokio::select! {
                pkt = pkt_rx.recv() => match pkt {
                    Some((i, from, data)) => self.on_packet(i, from, &data),
                    None => return,
                },
                cmd = cmd_rx.recv() => match cmd {
                    Some(c) => self.on_cmd(c).await,
                    None => return,
                },
                _ = tokio::time::sleep_until(next.into()) => {}
            }
            let now = Instant::now();
            for p in &mut self.peers {
                if p.rtc.is_alive() {
                    if let Err(e) = p.rtc.handle_input(Input::Timeout(now)) {
                        fail(p, "error", &format!("{e}"));
                    }
                }
            }
        }
    }

    fn on_packet(&mut self, i: usize, from: SocketAddr, data: &[u8]) {
        if let Some(tx) = stun::response_tx(data) {
            if self.stun_tx.remove(&tx).is_some() {
                self.on_stun(i, data);
                return;
            }
        }
        let Some(&(_, dest)) = self.socks.get(i) else {
            return;
        };
        let Ok(contents) = data.try_into() else {
            return;
        };
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source: from,
                destination: dest,
                contents,
            },
        );
        if let Some(p) = self
            .peers
            .iter_mut()
            .find(|p| p.rtc.is_alive() && p.rtc.accepts(&input))
        {
            p.last_src = Some(from);
            if let Err(e) = p.rtc.handle_input(input) {
                fail(p, "error", &format!("{e}"));
            }
        }
    }

    fn on_stun(&mut self, i: usize, data: &[u8]) {
        let (Some(addr), Some(&(_, base))) = (stun::mapped(data), self.socks.get(i)) else {
            return;
        };
        if addr == base || !addr.is_ipv4() {
            return; // no NAT in between: the host candidate says it already
        }
        self.add_extra(Candidate::server_reflexive(addr, base, "udp").ok(), "stun");
    }

    /// A new outside candidate: for new peers, and trickled to the ones there are.
    fn add_extra(&mut self, c: Option<Candidate>, how: &str) {
        let Some(c) = c else { return };
        if self.extra.iter().any(|e| e.addr() == c.addr()) {
            return;
        }
        crate::log_at!(
            Debug,
            "together",
            "join: internet candidate from {how}: port {}",
            c.addr().port()
        );
        for p in &mut self.peers {
            if p.closing_at.is_none() && p.rtc.add_local_candidate(c.clone()).is_some() {
                let _ = p
                    .signal
                    .send((p.info.gid.clone(), candidate_json(&c, &p.mid)));
            }
        }
        self.extra.push(c);
    }

    /// Asks the STUN servers in the settings, from every socket.
    async fn gather(&mut self) {
        let servers = crate::join::settings().stun;
        let mut addrs = Vec::new();
        for s in servers.iter().take(4) {
            if cfg!(test) && s.parse::<SocketAddr>().is_err() {
                continue; // the tests never look a name up (nor reach a real server)
            }
            if let Ok(Ok(found)) =
                tokio::time::timeout(STUN_WAIT, tokio::net::lookup_host(s.as_str())).await
            {
                addrs.extend(found.filter(|a| a.is_ipv4()).take(1));
            }
        }
        self.stun_tx.clear();
        for (i, (sock, _)) in self.socks.iter().enumerate() {
            for a in &addrs {
                let mut tx = [0u8; 12];
                if getrandom::fill(&mut tx).is_err() {
                    continue;
                }
                self.stun_tx.insert(tx, i);
                // Awaited, not `try_send_to`: the sockets are new when the first room opens, and a
                // socket the runtime has not yet seen writable refuses a try (WouldBlock), which
                // lost the request, and with it the STUN candidate, whenever the machine was busy.
                let _ = tokio::time::timeout(STUN_WAIT, sock.send_to(&stun::request(tx), *a)).await;
            }
        }
    }

    fn want_mapping(&mut self, want: bool) {
        let want = want && crate::join::settings().upnp;
        match (&self.nat_want, want) {
            (Some(tx), false) => {
                let _ = tx.send(false);
                self.nat_want = None;
            }
            (None, true) => {
                // The address the router sees: the first socket's (the one toward the LAN).
                let Some(&(_, local)) = self.socks.first() else {
                    return;
                };
                let (tx, rx) = tokio::sync::watch::channel(true);
                self.nat_want = Some(tx);
                tokio::spawn(keep_mapping(local, rx, self.cmd.clone()));
            }
            _ => {}
        }
    }

    async fn on_cmd(&mut self, c: Cmd) {
        match c {
            Cmd::RoomOpen(r, kind) => {
                self.rooms.insert(r, kind);
                self.want_mapping(true);
                self.gather().await;
            }
            Cmd::RoomClose(r) => {
                self.rooms.remove(&r);
                if self.rooms.is_empty() {
                    self.want_mapping(false);
                }
            }
            Cmd::Offer {
                room,
                gid,
                sdp,
                signal,
            } => self.on_offer(room, gid, sdp, signal),
            Cmd::Remote {
                room,
                gid,
                candidate,
            } => {
                let c = candidate.trim();
                let c = c.strip_prefix("a=").unwrap_or(c);
                if c.is_empty() || c.contains(".local ") {
                    return;
                }
                if let (Some(p), Ok(cand)) = (
                    self.peers.iter_mut().find(|p| {
                        p.info.room == room && p.info.gid == gid && p.closing_at.is_none()
                    }),
                    Candidate::from_sdp_string(c),
                ) {
                    if cand.addr().is_ipv4() {
                        p.rtc.add_remote_candidate(cand);
                    }
                }
            }
            Cmd::Left { room, gid } => {
                for p in self
                    .peers
                    .iter_mut()
                    .filter(|p| p.info.room == room && p.info.gid == gid && !p.rtc.is_connected())
                {
                    fail(p, "left", "the guest left before connecting");
                }
            }
            Cmd::Up(id, m, len) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    p.flow.unwritten.fetch_sub(len, Ordering::Relaxed);
                    queue(p, m);
                    flush(p);
                }
            }
            Cmd::Gone(id) => {
                if let Some(p) = self.peers.iter_mut().find(|p| p.id == id) {
                    p.to_session = None;
                    if p.closing_at.is_none() {
                        p.closing_at = Some(Instant::now());
                    }
                }
            }
            Cmd::Mapped(m) => {
                *NET_MAPPED.lock().unwrap_or_else(|e| e.into_inner()) =
                    m.map(|(ext, _, how)| (ext, how));
                if let Some((ext, base, how)) = m {
                    self.add_extra(Candidate::server_reflexive(ext, base, "udp").ok(), how);
                }
            }
        }
    }

    fn on_offer(
        &mut self,
        room: String,
        gid: String,
        sdp: String,
        signal: mpsc::UnboundedSender<(String, serde_json::Value)>,
    ) {
        let bye = |why: &str| {
            let _ = signal.send((gid.clone(), serde_json::json!({"type": "bye", "why": why})));
        };
        let Some(&kind) = self.rooms.get(&room) else {
            return;
        };
        // A new offer from the same guest replaces one that did not connect.
        for p in self.peers.iter_mut().filter(|p| {
            p.info.room == room && p.info.gid == gid && p.cid.is_none() && p.closing_at.is_none()
        }) {
            fail(p, "replaced", "a newer offer");
        }
        let live = |p: &&Peer| p.closing_at.is_none();
        if self.peers.iter().filter(live).count() >= MAX_PEERS
            || self
                .peers
                .iter()
                .filter(live)
                .filter(|p| p.info.room == room)
                .count()
                >= MAX_ROOM_PEERS
        {
            bye("full");
            return;
        }
        let Some((cert, _)) = cert() else {
            bye("offer");
            return;
        };
        let Ok(offer) = SdpOffer::from_sdp_string(&sdp) else {
            bye("offer");
            return;
        };
        let now = Instant::now();
        let mut rtc = Rtc::builder()
            .set_crypto_provider(super::crypto::provider())
            .set_dtls_cert(cert.clone())
            .set_stats_interval(Some(Duration::from_secs(1)))
            .set_sctp_max_buffered_amount(SCTP_BUFFER)
            .build(now);
        for (_, a) in &self.socks {
            if let Ok(c) = Candidate::host(*a, "udp") {
                rtc.add_local_candidate(c);
            }
        }
        for c in &self.extra {
            rtc.add_local_candidate(c.clone());
        }
        let answer = match rtc.sdp_api().accept_offer(offer) {
            Ok(a) => a,
            Err(e) => {
                crate::log_at!(Debug, "together", "join: an internet offer refused: {e}");
                bye("offer");
                return;
            }
        };
        let info = PeerInfo::new(&room, &gid);
        info.with(|s| s.ice = "checking");
        let flow = Arc::new(Flow::default());
        let _ = info.flow.set(flow.clone());
        let _ = signal.send((
            gid.clone(),
            serde_json::json!({"type": "answer", "sdp": answer.to_sdp_string()}),
        ));
        crate::log_at!(Debug, "together", "join: internet guest {gid}: answered");
        self.peers.push(Peer {
            id: NEXT_PEER.fetch_add(1, Ordering::Relaxed),
            kind,
            info,
            rtc,
            mid: first_mid(&sdp),
            signal,
            cid: None,
            to_session: None,
            flow,
            out: VecDeque::new(),
            out_bytes: 0,
            reasm: Reassembler::new(crate::join::GUEST_MSG_MAX),
            started: now,
            disconnected_at: None,
            closing_at: None,
            last_src: None,
        });
    }

    /// Ends what timed out, drains what is closing, forgets what is gone.
    fn housekeep(&mut self, now: Instant) {
        for p in &mut self.peers {
            if p.cid.is_none()
                && p.closing_at.is_none()
                && now.duration_since(p.started) > CONNECT_WAIT
            {
                let ice = p.info.with(|s| s.ice);
                fail(p, "timeout", &format!("no connection (ICE {ice})"));
            }
            if p.disconnected_at
                .is_some_and(|t| now.duration_since(t) > DISCONNECTED_WAIT)
            {
                p.disconnected_at = None;
                fail(p, "lost", "the path to the guest was lost");
            }
            if let Some(t) = p.closing_at {
                let drained = p.out.is_empty()
                    && p.cid
                        .and_then(|c| p.rtc.channel(c).map(|mut ch| ch.buffered_amount()))
                        .unwrap_or(0)
                        == 0;
                if p.rtc.is_alive()
                    && (drained || now.duration_since(t) > CLOSE_DRAIN)
                    && p.rtc.close().is_err()
                {
                    p.rtc.disconnect();
                }
            }
        }
        self.peers.retain(|p| {
            let keep = p.rtc.is_alive();
            if !keep {
                p.flow.close();
                p.info.with(|s| {
                    s.ended.get_or_insert(now);
                });
            }
            keep
        });
    }
}

/// Ends a peer: the session (if any) hears its channel close, the panel says why.
fn fail(p: &mut Peer, why: &'static str, detail: &str) {
    let connected = p.cid.is_some();
    p.info.with(|s| {
        s.failed = (!connected && why != "replaced") || why == "lost";
        s.why.get_or_insert(why);
        s.ended.get_or_insert_with(Instant::now);
    });
    crate::log_at!(
        Debug,
        "together",
        "join: internet guest {} ended ({why}): {detail}",
        p.info.gid
    );
    p.to_session = None;
    p.flow.close();
    if p.closing_at.is_none() {
        p.closing_at = Some(Instant::now());
    }
    p.rtc.disconnect();
}

/// A session's message onto the peer's outgoing queue, as data-channel messages.
fn queue(p: &mut Peer, m: Message) {
    let push = |p: &mut Peer, binary: bool, b: Vec<u8>| {
        p.out_bytes += b.len();
        p.out.push_back((binary, b));
    };
    match m {
        Message::Text(t) => push(p, false, t.as_bytes().to_vec()),
        Message::Binary(b) => {
            for piece in chunk::split(&b) {
                push(p, true, piece);
            }
        }
        // The WebSocket ping's stand-in: the page answers it with a pong (chunk.rs).
        Message::Ping(b) if b.len() == 8 => {
            let mut v = vec![chunk::PING];
            v.extend_from_slice(&b);
            push(p, true, v);
        }
        Message::Close(_) => {
            p.closing_at.get_or_insert_with(Instant::now);
        }
        _ => {}
    }
}

/// Moves what the channel takes from the queue into it, and tells the session how full it is.
fn flush(p: &mut Peer) {
    let Some(cid) = p.cid else {
        return;
    };
    let mut buffered = 0;
    if let Some(mut ch) = p.rtc.channel(cid) {
        while let Some((binary, data)) = p.out.front() {
            match ch.write(*binary, data) {
                Ok(true) => {
                    p.out_bytes -= data.len();
                    p.out.pop_front();
                }
                Ok(false) => break,
                Err(_) => {
                    p.out.clear();
                    p.out_bytes = 0;
                    break;
                }
            }
        }
        buffered = ch.buffered_amount();
    }
    p.flow.set_queued(p.out_bytes, buffered);
}

/// Drives one peer until it has nothing more to do now: its packets out, its events handled.
fn poll(
    p: &mut Peer,
    socks: &[(Arc<UdpSocket>, SocketAddr)],
    cmd: &mpsc::UnboundedSender<Cmd>,
    now: Instant,
) -> Instant {
    loop {
        if !p.rtc.is_alive() {
            return now + Duration::from_millis(50);
        }
        let out = match p.rtc.poll_output() {
            Ok(o) => o,
            Err(e) => {
                fail(p, "error", &format!("{e}"));
                return now;
            }
        };
        match out {
            Output::Timeout(t) => return t,
            Output::Transmit(t) => {
                if let Some((s, _)) = socks.iter().find(|(_, a)| *a == t.source) {
                    let _ = s.try_send_to(&t.contents, t.destination);
                }
            }
            Output::Event(e) => on_event(p, e, cmd),
        }
    }
}

fn on_event(p: &mut Peer, e: Event, cmd: &mpsc::UnboundedSender<Cmd>) {
    match e {
        Event::IceConnectionStateChange(s) => {
            p.info.with(|st| st.ice = ice_name(s));
            p.disconnected_at = match s {
                IceConnectionState::Disconnected => Some(Instant::now()),
                _ => None,
            };
        }
        Event::Connected => {
            p.info.with(|s| s.connected = true);
            crate::log_at!(
                Debug,
                "together",
                "join: internet guest {}: connected in {} ms",
                p.info.gid,
                p.started.elapsed().as_millis()
            );
        }
        Event::ChannelOpen(cid, label) => {
            if label != p.kind.label() || p.cid.is_some() || p.closing_at.is_some() {
                return;
            }
            p.cid = Some(cid);
            if let Some(mut ch) = p.rtc.channel(cid) {
                ch.set_buffered_amount_low_threshold(super::HIGH_WATER / 2);
            }
            let ip = p
                .last_src
                .map(|a| a.ip())
                .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
            if !crate::join::connect_allowed(ip, Instant::now()) {
                queue(
                    p,
                    Message::Text(
                        serde_json::json!({"stage": "refused", "why": "slow_down"})
                            .to_string()
                            .into(),
                    ),
                );
                p.closing_at = Some(Instant::now());
                flush(p);
                return;
            }
            let (tx, rx) = mpsc::channel(TO_SESSION);
            p.to_session = Some(tx);
            let socket = RtcSocket {
                id: p.id,
                rx,
                up: cmd.clone(),
                flow: p.flow.clone(),
                info: p.info.clone(),
            };
            crate::log_at!(
                Debug,
                "together",
                "join: internet guest {}: data channel open",
                p.info.gid
            );
            match p.kind {
                Kind::Join => {
                    tokio::spawn(crate::join::guest_session(socket, ip));
                }
                Kind::Pad => {
                    tokio::spawn(crate::pad_link::rtc_session(
                        socket,
                        ip,
                        p.info.room.clone(),
                    ));
                }
            }
        }
        Event::ChannelData(d) => {
            if Some(d.id) != p.cid {
                return;
            }
            let m = if d.binary {
                match p.reasm.push(&d.data) {
                    Ok(Piece::Partial) => return,
                    Ok(Piece::Message(b)) => Message::Binary(b.into()),
                    Ok(Piece::Pong(b)) => Message::Pong(b.to_vec().into()),
                    Ok(Piece::Ping(b)) => {
                        let mut v = vec![chunk::PONG];
                        v.extend_from_slice(&b);
                        p.out_bytes += v.len();
                        p.out.push_back((true, v));
                        flush(p);
                        return;
                    }
                    Err(why) => {
                        fail(p, "protocol", why);
                        return;
                    }
                }
            } else {
                // Over the LAN socket's limit closes it there; so here.
                if d.data.len() > crate::join::GUEST_MSG_MAX {
                    fail(p, "protocol", "a text message over the limit");
                    return;
                }
                match String::from_utf8(d.data) {
                    Ok(t) => Message::Text(t.into()),
                    Err(_) => {
                        fail(p, "protocol", "text that is not UTF-8");
                        return;
                    }
                }
            };
            if let Some(tx) = &p.to_session {
                if tx.try_send(m).is_err() {
                    fail(p, "flood", "the guest sent faster than the session reads");
                }
            }
        }
        Event::ChannelClose(cid) => {
            if Some(cid) == p.cid {
                p.to_session = None;
                if p.closing_at.is_none() {
                    p.closing_at = Some(Instant::now());
                }
            }
        }
        Event::ChannelBufferedAmountLow(_) => flush(p),
        Event::PeerStats(s) => {
            if let Some(rtt) = s
                .selected_candidate_pair
                .as_ref()
                .and_then(|c| c.current_round_trip_time)
                .or(s.rtt)
            {
                p.info
                    .with(|st| st.rtt_ms = Some((rtt.as_millis() as u32).max(1)));
            }
            p.info.sent(s.peer_bytes_tx, s.timestamp);
            flush(p);
            p.flow.second();
        }
        _ => {}
    }
}

/// Keeps the router mapping for `local` while `want` says so: made within the budget, renewed at
/// half its lease, removed at the end.
async fn keep_mapping(
    local: SocketAddr,
    mut want: tokio::sync::watch::Receiver<bool>,
    cmd: mpsc::UnboundedSender<Cmd>,
) {
    let search = nat_search();
    let Some(search) = search else { return };
    let mut m = nat::map(local, &search).await;
    if let Some(mm) = &m {
        let _ = cmd.send(Cmd::Mapped(Some((mm.external, mm.local, mm.how()))));
    }
    loop {
        tokio::select! {
            r = want.changed() => {
                if r.is_err() || !*want.borrow() {
                    break;
                }
            }
            _ = tokio::time::sleep(nat::LEASE / 2) => {
                match &mut m {
                    Some(mm) => {
                        if !nat::renew(mm).await {
                            m = None;
                            let _ = cmd.send(Cmd::Mapped(None));
                        }
                    }
                    None => {
                        m = nat::map(local, &search).await;
                        if let Some(mm) = &m {
                            let _ = cmd.send(Cmd::Mapped(Some((mm.external, mm.local, mm.how()))));
                        }
                    }
                }
            }
        }
    }
    if let Some(mm) = m {
        nat::remove(mm).await;
    }
    let _ = cmd.send(Cmd::Mapped(None));
}

/// Where to look for the router: the LAN's SSDP and NAT-PMP, or (tests) the fake router a test
/// names, never the real one.
fn nat_search() -> Option<nat::Search> {
    #[cfg(test)]
    {
        TEST_NAT.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    #[cfg(not(test))]
    {
        Some(nat::Search::default())
    }
}

#[cfg(test)]
pub(crate) static TEST_NAT: Mutex<Option<nat::Search>> = Mutex::new(None);
