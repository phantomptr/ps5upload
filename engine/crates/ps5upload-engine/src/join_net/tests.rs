//! A fake signalling hub (protocol v1) and a str0m guest that plays the browser, for the internet
//! invite tests here and in join.rs. Everything is on this computer: no real hub, STUN server or
//! router is reached.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message as AxMsg, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use futures_util::{SinkExt, StreamExt};
use str0m::change::{SdpAnswer, SdpPendingOffer};
use str0m::channel::ChannelId;
use str0m::media::{Direction, KeyframeRequestKind, MediaKind, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, Input, Output, Rtc};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as TMsg;

use super::chunk::{self, Piece, Reassembler};

// ── The fake hub ─────────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub(crate) struct HubState {
    /// room → (secret, the host's queue)
    rooms: HashMap<String, (String, Option<mpsc::UnboundedSender<String>>)>,
    /// gid → (room, the guest's queue)
    guests: HashMap<String, (String, mpsc::UnboundedSender<String>)>,
    next_gid: u64,
    /// Every frame a host sent, in order.
    pub host_frames: Vec<serde_json::Value>,
    /// Answer the next N `host` frames with `room_taken`.
    pub refuse_hosts: usize,
    /// Host connections so far.
    pub host_conns: usize,
}

#[derive(Clone)]
pub(crate) struct FakeHub {
    pub url: String,
    pub st: Arc<Mutex<HubState>>,
    /// Closing it drops every host connection (the hub restarting, a network blip).
    kick: Arc<tokio::sync::watch::Sender<u64>>,
}

impl FakeHub {
    pub(crate) async fn start() -> FakeHub {
        let st = Arc::new(Mutex::new(HubState::default()));
        let (kick, _) = tokio::sync::watch::channel(0u64);
        let hub = FakeHub {
            url: String::new(),
            st,
            kick: Arc::new(kick),
        };
        let app = axum::Router::new()
            .route("/ws", axum::routing::get(ws))
            .with_state(hub.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(l, app).await;
        });
        FakeHub {
            url: format!("ws://{addr}/ws"),
            ..hub
        }
    }

    /// Drops every host connection now.
    pub(crate) fn drop_hosts(&self) {
        self.kick.send_modify(|n| *n += 1);
    }

    pub(crate) fn hosts(&self) -> Vec<serde_json::Value> {
        self.st
            .lock()
            .unwrap()
            .host_frames
            .iter()
            .filter(|v| v["t"] == "host")
            .cloned()
            .collect()
    }

    /// Guests that ever joined a room on this hub.
    pub(crate) fn guests_ever(&self) -> u64 {
        self.st.lock().unwrap().next_gid
    }

    pub(crate) fn host_conns(&self) -> usize {
        self.st.lock().unwrap().host_conns
    }

    pub(crate) fn has_host(&self, room: &str) -> bool {
        self.st
            .lock()
            .unwrap()
            .rooms
            .get(room)
            .is_some_and(|r| r.1.as_ref().is_some_and(|tx| !tx.is_closed()))
    }
}

async fn ws(up: WebSocketUpgrade, State(hub): State<FakeHub>) -> axum::response::Response {
    up.max_message_size(16 * 1024)
        .on_upgrade(move |s| conn(s, hub))
}

fn j(v: serde_json::Value) -> String {
    v.to_string()
}

async fn conn(mut s: WebSocket, hub: FakeHub) {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    // Who this connection is: ("host", room) or ("guest", gid).
    let mut me: Option<(&'static str, String)> = None;
    let mut kick = hub.kick.subscribe();
    kick.borrow_and_update();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(t) => if s.send(AxMsg::Text(t.into())).await.is_err() { break },
                None => break,
            },
            _ = kick.changed() => {
                if matches!(me, Some(("host", _))) {
                    break; // dropped without a word, as a dead TCP connection would be
                }
            }
            m = s.recv() => {
                let t = match m {
                    Some(Ok(AxMsg::Text(t))) => t,
                    Some(Ok(AxMsg::Ping(_) | AxMsg::Pong(_))) => continue,
                    _ => break,
                };
                let v: serde_json::Value = match serde_json::from_str(t.as_str()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let mut st = hub.st.lock().unwrap();
                let str_of = |k: &str| v[k].as_str().unwrap_or("").to_string();
                if matches!(me, Some(("host", _))) || v["t"] == "host" {
                    st.host_frames.push(v.clone());
                }
                match v["t"].as_str().unwrap_or("") {
                    "host" => {
                        st.host_conns += 1;
                        let room = str_of("room");
                        let secret = str_of("secret");
                        if st.refuse_hosts > 0 {
                            st.refuse_hosts -= 1;
                            let _ = tx.send(j(serde_json::json!({"t": "error", "why": "room_taken"})));
                            continue;
                        }
                        let taken = st.rooms.get(&room).is_some_and(|r| r.0 != secret);
                        if taken || room.len() != 22 || secret.len() != 43 {
                            let _ = tx.send(j(serde_json::json!({"t": "error", "why": "room_taken"})));
                            continue;
                        }
                        st.rooms.insert(room.clone(), (secret, Some(tx.clone())));
                        let _ = tx.send(j(serde_json::json!({"t": "hosted", "room": room})));
                        me = Some(("host", room));
                    }
                    "join" => {
                        let room = str_of("room");
                        let host = st.rooms.get(&room).and_then(|r| r.1.clone());
                        let Some(host) = host else {
                            let _ = tx.send(j(serde_json::json!({"t": "error", "why": "no_room"})));
                            continue;
                        };
                        st.next_gid += 1;
                        let gid = format!("g{}", st.next_gid);
                        st.guests.insert(gid.clone(), (room, tx.clone()));
                        let _ = tx.send(j(serde_json::json!({"t": "joined", "gid": gid})));
                        let _ = host.send(j(serde_json::json!({"t": "guest", "gid": gid})));
                        me = Some(("guest", gid));
                    }
                    "signal" => {
                        let data = v["data"].clone();
                        match &me {
                            Some(("host", _)) => {
                                if let Some((_, g)) = st.guests.get(&str_of("to")) {
                                    let _ = g.send(j(serde_json::json!({"t": "signal", "from": "host", "data": data})));
                                }
                            }
                            Some(("guest", gid)) => {
                                let room = st.guests.get(gid).map(|g| g.0.clone());
                                if let Some(h) = room.and_then(|r| st.rooms.get(&r)).and_then(|r| r.1.clone()) {
                                    let _ = h.send(j(serde_json::json!({"t": "signal", "from": gid, "data": data})));
                                }
                            }
                            _ => {}
                        }
                    }
                    "leave" => break,
                    _ => {}
                }
            }
        }
    }
    let mut st = hub.st.lock().unwrap();
    match me {
        Some(("host", room)) => {
            if let Some(r) = st.rooms.get_mut(&room) {
                r.1 = None; // kept for a reclaim with the secret
            }
            for (r, g) in st.guests.values() {
                if *r == room {
                    let _ = g.send(j(serde_json::json!({"t": "closed"})));
                }
            }
        }
        Some((_, gid)) => {
            if let Some((room, _)) = st.guests.remove(&gid) {
                if let Some(h) = st.rooms.get(&room).and_then(|r| r.1.clone()) {
                    let _ = h.send(j(serde_json::json!({"t": "left", "gid": gid})));
                }
            }
        }
        None => {}
    }
}

// ── A guest made of str0m (the browser's part) ───────────────────────────────────────────────────

/// What the test guest receives: whole messages, as the page would after reassembly.
#[derive(Debug)]
pub(crate) enum GIn {
    Text(serde_json::Value),
    Binary(Vec<u8>),
    /// The engine's ping (answered already).
    Ping,
    /// A picture off the video track (an access unit, Annex B), and whether nothing was lost
    /// before it.
    Media(Vec<u8>, bool),
    /// A sound frame off the audio track (one Opus packet).
    Audio(Vec<u8>),
}

pub(crate) enum GOut {
    Text(String),
    Binary(Vec<u8>),
    /// The browser's keyframe request on the video track (a PLI).
    Pli,
}

pub(crate) struct TestGuest {
    pub tx: mpsc::UnboundedSender<GOut>,
    pub rx: mpsc::UnboundedReceiver<GIn>,
    /// The engine's answer SDP.
    pub answer: String,
    /// The guest's hub socket (kept open, as a page would until it leaves).
    _hub: tokio::task::JoinHandle<()>,
}

impl TestGuest {
    /// The next text message that has `key`, within 10 s (statuses and binary skipped).
    pub(crate) async fn next_with(&mut self, key: &str) -> serde_json::Value {
        let until = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(until, self.rx.recv()).await {
                Ok(Some(GIn::Text(v))) if v.get(key).is_some() => return v,
                Ok(Some(_)) => {}
                other => panic!("no message with {key}: {other:?}"),
            }
        }
    }

    pub(crate) fn text(&self, v: serde_json::Value) {
        let _ = self.tx.send(GOut::Text(v.to_string()));
    }
}

/// Joins `room` on the hub at `hub_url` and connects to its host over WebRTC on loopback, with a
/// "join" data channel. Returns once the channel is open.
pub(crate) async fn connect_guest(hub_url: &str, room: &str) -> TestGuest {
    connect_guest_on(hub_url, room, "join").await
}

/// [`connect_guest`] with a data channel labelled `label` ("pad": a phone, pad_link.rs).
pub(crate) async fn connect_guest_on(hub_url: &str, room: &str, label: &str) -> TestGuest {
    connect_guest_with(hub_url, room, label, false).await
}

/// [`connect_guest_on`], the offer also with a receive-only video section when `video` (a page
/// that takes the picture as a video track, join/rtcTransport.ts).
pub(crate) async fn connect_guest_with(
    hub_url: &str,
    room: &str,
    label: &str,
    video: bool,
) -> TestGuest {
    let join = serde_json::json!({"v": 1, "t": "join", "room": room});
    connect_guest_join(hub_url, join, label, video, false, "127.0.0.1:0").await
}

/// [`connect_guest`] from an IPv6 address only (`[::1]`): the guest's one candidate is IPv6, as a
/// home behind its provider's shared IPv4 (CGNAT) reaches the host. None when this machine has no
/// IPv6 loopback (some CI containers).
pub(crate) async fn connect_guest_v6(hub_url: &str, room: &str) -> Option<TestGuest> {
    tokio::net::UdpSocket::bind("[::1]:0").await.ok()?;
    let join = serde_json::json!({"v": 1, "t": "join", "room": room});
    Some(connect_guest_join(hub_url, join, "join", false, false, "[::1]:0").await)
}

/// A home-network join signalled on the engine's own socket (`{"t":"join","lan":true}`,
/// join_net/local_signal.rs): no room, a "join" channel, a video and an audio section when
/// `video` (as the pages offer them).
pub(crate) async fn connect_guest_lan(signal_url: &str, video: bool) -> TestGuest {
    let join = serde_json::json!({"v": 1, "t": "join", "lan": true});
    connect_guest_join(signal_url, join, "join", video, video, "127.0.0.1:0").await
}

async fn connect_guest_join(
    hub_url: &str,
    join: serde_json::Value,
    label: &str,
    video: bool,
    audio: bool,
    bind: &str,
) -> TestGuest {
    let (mut hub, _) = tokio_tungstenite::connect_async(hub_url).await.unwrap();
    hub.send(TMsg::Text(join.to_string().into())).await.unwrap();
    let joined = next_hub(&mut hub).await;
    assert_eq!(joined["t"], "joined", "{joined}");

    let sock = tokio::net::UdpSocket::bind(bind).await.unwrap();
    let local = sock.local_addr().unwrap();
    let mut rtc = Rtc::builder()
        .set_crypto_provider(super::crypto::provider())
        .set_reordering_size_video(4000)
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp").unwrap());
    let mut api = rtc.sdp_api();
    let cid = api.add_channel(label.into());
    let mid = video.then(|| api.add_media(MediaKind::Video, Direction::RecvOnly, None, None, None));
    let amid =
        audio.then(|| api.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None));
    let (offer, pending): (_, SdpPendingOffer) = api.apply().unwrap();
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
    .unwrap();
    // The answer (candidates trickled after it go into the running peer below).
    let answer = loop {
        let m = next_hub(&mut hub).await;
        if m["t"] == "signal" && m["data"]["type"] == "answer" {
            break m["data"]["sdp"].as_str().unwrap().to_string();
        }
        assert_ne!(m["data"]["type"], "bye", "{m}");
    };
    rtc.sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap())
        .unwrap();

    let (in_tx, in_rx) = mpsc::unbounded_channel();
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (open_tx, open_rx) = tokio::sync::oneshot::channel();
    let (cand_tx, cand_rx) = mpsc::unbounded_channel::<String>();
    let hub_task = tokio::spawn(async move {
        while let Some(Ok(m)) = hub.next().await {
            if let TMsg::Text(t) = m {
                let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap_or_default();
                if v["data"]["type"] == "candidate" {
                    if let Some(c) = v["data"]["candidate"].as_str() {
                        let _ = cand_tx.send(c.to_string());
                    }
                }
            }
        }
    });
    tokio::spawn(drive(
        rtc, cid, mid, amid, sock, local, in_tx, out_rx, cand_rx, open_tx,
    ));
    tokio::time::timeout(Duration::from_secs(10), open_rx)
        .await
        .expect("the data channel opens over loopback")
        .unwrap();
    TestGuest {
        tx: out_tx,
        rx: in_rx,
        answer,
        _hub: hub_task,
    }
}

async fn next_hub(
    hub: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> serde_json::Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), hub.next())
            .await
            .expect("the hub answers")
            .expect("open")
            .unwrap();
        if let TMsg::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive(
    mut rtc: Rtc,
    cid: ChannelId,
    mid: Option<Mid>,
    amid: Option<Mid>,
    sock: tokio::net::UdpSocket,
    local: SocketAddr,
    in_tx: mpsc::UnboundedSender<GIn>,
    mut out_rx: mpsc::UnboundedReceiver<GOut>,
    mut cand_rx: mpsc::UnboundedReceiver<String>,
    open_tx: tokio::sync::oneshot::Sender<()>,
) {
    let mut open_tx = Some(open_tx);
    let mut reasm = Reassembler::new(64 << 20);
    let mut buf = vec![0u8; 2048];
    let mut pending: Vec<(bool, Vec<u8>)> = Vec::new();
    loop {
        // Out what can go.
        if let Some(mut ch) = rtc.channel(cid) {
            while let Some((b, d)) = pending.first() {
                if ch.write(*b, d).unwrap_or(false) {
                    pending.remove(0);
                } else {
                    break;
                }
            }
        }
        let timeout = loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Transmit(t)) => {
                    let _ = sock.send_to(&t.contents, t.destination).await;
                }
                Ok(Output::Event(e)) => match e {
                    Event::ChannelOpen(id, _) if id == cid => {
                        if let Some(o) = open_tx.take() {
                            let _ = o.send(());
                        }
                    }
                    Event::ChannelData(d) if d.id == cid => {
                        if !d.binary {
                            let v = serde_json::from_slice(&d.data).unwrap();
                            let _ = in_tx.send(GIn::Text(v));
                            continue;
                        }
                        match reasm.push(&d.data).expect("the engine's framing") {
                            Piece::Partial => {}
                            Piece::Message(m) => {
                                let _ = in_tx.send(GIn::Binary(m));
                            }
                            Piece::Ping(b) => {
                                let mut v = vec![chunk::PONG];
                                v.extend_from_slice(&b);
                                pending.push((true, v));
                                let _ = in_tx.send(GIn::Ping);
                            }
                            Piece::Pong(_) => {}
                        }
                    }
                    Event::ChannelClose(id) if id == cid => return,
                    Event::MediaData(d) if Some(d.mid) == amid => {
                        let _ = in_tx.send(GIn::Audio(d.data.to_vec()));
                    }
                    Event::MediaData(d) => {
                        let _ = in_tx.send(GIn::Media(d.data.to_vec(), d.contiguous));
                    }
                    _ => {}
                },
                Err(_) => return,
            }
            if !rtc.is_alive() {
                return;
            }
        };
        let wait = timeout
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                let Ok((n, from)) = r else { return };
                if let Ok(contents) = buf[..n].try_into() {
                    let _ = rtc.handle_input(Input::Receive(Instant::now(), Receive {
                        proto: Protocol::Udp, source: from, destination: local, contents,
                    }));
                }
            }
            m = out_rx.recv() => match m {
                Some(GOut::Text(t)) => pending.push((false, t.into_bytes())),
                Some(GOut::Binary(b)) => pending.extend(chunk::split(&b).into_iter().map(|p| (true, p))),
                Some(GOut::Pli) => {
                    if let Some(mut w) = mid.and_then(|m| rtc.writer(m)) {
                        let _ = w.request_keyframe(None, KeyframeRequestKind::Pli);
                    }
                }
                None => return,
            },
            c = cand_rx.recv() => {
                if let Some(Ok(c)) = c.map(|c| Candidate::from_sdp_string(&c)) {
                    rtc.add_remote_candidate(c);
                }
            }
            _ = tokio::time::sleep(wait) => {}
        }
        if rtc.handle_input(Input::Timeout(Instant::now())).is_err() {
            return;
        }
    }
}

// ── The hub client alone ─────────────────────────────────────────────────────────────────────────

#[test]
fn links_and_hub_addresses() {
    assert!(super::hub_url_ok("wss://hub.phantomptr.com/ws"));
    assert!(super::hub_url_ok("ws://127.0.0.1:9000/ws"));
    assert!(super::hub_url_ok("ws://localhost:9000/ws"));
    assert!(
        !super::hub_url_ok("ws://hub.phantomptr.com/ws"),
        "plain ws off this computer"
    );
    assert!(!super::hub_url_ok("https://hub.phantomptr.com/ws"));
    assert!(!super::hub_url_ok("wss:///ws"));
    assert_eq!(
        super::hub_host("wss://hub.phantomptr.com/ws").as_deref(),
        Some("hub.phantomptr.com")
    );
    assert_eq!(
        super::hub_host("ws://127.0.0.1:9000/ws").as_deref(),
        Some("127.0.0.1:9000")
    );
    let room = super::random_b64(16).unwrap();
    let secret = super::random_b64(32).unwrap();
    assert_eq!((room.len(), secret.len()), (22, 43));
    assert!(room
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    assert_eq!(
        super::link_for(
            super::Kind::Join,
            "R",
            "K",
            "AA:BB",
            "hub.example",
            None,
            None
        ),
        "https://ps5upload.phantomptr.com/join/#room=R&k=K&fp=AA:BB&hub=hub.example"
    );
    // The phone link's: the website's controller page, its own channel label.
    assert_eq!(
        super::link_for(
            super::Kind::Pad,
            "R",
            "K",
            "AA:BB",
            "hub.example",
            None,
            None
        ),
        "https://ps5upload.phantomptr.com/pad/#room=R&k=K&fp=AA:BB&hub=hub.example"
    );
    // The home network's page as a hint (percent-encoded: it is a fragment value).
    assert_eq!(
        super::link_for(
            super::Kind::Join,
            "R",
            "K",
            "AA:BB",
            "hub.example",
            Some("http://192.168.1.10:19113/join"),
            None
        ),
        "https://ps5upload.phantomptr.com/join/#room=R&k=K&fp=AA:BB&hub=hub.example\
         &lan=http%3A%2F%2F192.168.1.10%3A19113%2Fjoin"
    );
    // With this computer's Tailscale page (the Tailscale app's address) beside it: `ts`, once.
    let lan = "http://192.168.1.10:19113/join";
    let ts = "http://100.101.2.3:19113/join";
    assert_eq!(
        super::link_for(super::Kind::Join, "R", "K", "F", "h", Some(lan), Some(ts)),
        "https://ps5upload.phantomptr.com/join/#room=R&k=K&fp=F&hub=h\
         &lan=http%3A%2F%2F192.168.1.10%3A19113%2Fjoin&ts=http%3A%2F%2F100.101.2.3%3A19113%2Fjoin"
    );
    assert!(
        !super::link_for(super::Kind::Join, "R", "K", "F", "h", Some(ts), Some(ts)).contains("ts="),
        "the Tailscale page is the home one already"
    );
    assert!(
        !super::link_for(super::Kind::Join, "R", "K", "F", "h", None, Some("file:x"))
            .contains("ts=")
    );
    // Only an http(s) page is a hint.
    assert!(!super::link_for(
        super::Kind::Pad,
        "R",
        "K",
        "F",
        "h",
        Some("javascript:x"),
        None
    )
    .contains("lan="));
    assert_eq!(
        (super::Kind::Join.label(), super::Kind::Pad.label()),
        ("join", "pad")
    );
    // One certificate per run: the fingerprint never changes.
    let fp = super::fingerprint().unwrap();
    assert_eq!(super::fingerprint().unwrap(), fp);
}

// ── Back-pressure ────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_full_channel_holds_the_session_until_it_drains_or_closes() {
    use crate::join::GuestSocket;
    use std::sync::atomic::Ordering;
    let (_in_tx, rx) = mpsc::channel(4);
    let (up, mut up_rx) = mpsc::unbounded_channel();
    let flow = Arc::new(super::Flow::default());
    let mut s = super::RtcSocket {
        id: 7,
        rx,
        up,
        flow: flow.clone(),
        info: super::PeerInfo::new("room", "g1"),
    };
    // Room: the message goes at once, counted until the driver takes it.
    s.send(AxMsg::Binary(vec![0u8; 1000].into())).await.unwrap();
    assert!(matches!(
        up_rx.try_recv(),
        Ok(super::rtc::Cmd::Up(7, _, 1000))
    ));
    assert_eq!(flow.pending(), 1000);
    // Full past the high water: the session waits (its picture queue then fills and the stream
    // skips it to the next keyframe, as for a slow LAN guest)...
    flow.unwritten.store(0, Ordering::Relaxed);
    flow.set_queued(super::HIGH_WATER + 1, 0);
    let t = tokio::spawn(async move {
        let r = s.send(AxMsg::Text("x".into())).await;
        (r, s)
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!t.is_finished(), "held while the channel is full");
    // ...until it drains.
    flow.set_queued(0, 0);
    let (r, mut s) = tokio::time::timeout(Duration::from_secs(2), t)
        .await
        .unwrap()
        .unwrap();
    assert!(r.is_ok());
    // A peer that ends lets a waiting session go.
    flow.set_queued(super::HIGH_WATER + 1, 0);
    let t = tokio::spawn(async move { s.send(AxMsg::Text("y".into())).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    flow.close();
    let r = tokio::time::timeout(Duration::from_secs(2), t)
        .await
        .unwrap()
        .unwrap();
    assert!(r.is_err());
}
