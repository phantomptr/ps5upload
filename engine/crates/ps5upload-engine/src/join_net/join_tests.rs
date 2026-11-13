// join.rs's tests for internet invites (included into `join::tests::internet`, for its fakes): the guest
// session over a WebRTC data channel, the hub room's life, the settings.

use super::*;
use crate::join_net::tests::{connect_guest, connect_guest_with, FakeHub, GIn, GOut};

async fn internet_invite(console: &str) -> serde_json::Value {
    let (ci, h) = local();
    body(
        invite(
            ci,
            h,
            Json(InviteBody {
                addr: console.into(),
                new: false,
                internet: true,
                share: false,
            }),
        )
        .await,
    )
    .await
}

async fn open_party(console: &str) -> serde_json::Value {
    let (ci, h) = local();
    body(
        open(
            ci,
            h,
            Json(AddrBody {
                addr: console.into(),
            }),
        )
        .await,
    )
    .await
}

async fn close_party(console: &str) {
    let (ci, h) = local();
    close(
        ci,
        h,
        Json(AddrBody {
            addr: console.into(),
        }),
    )
    .await;
}

/// Polls the host's view until `f` holds (10 s).
async fn until_state(
    console: &str,
    what: &str,
    f: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..400 {
        let s = host_state(console).await;
        if f(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "timed out waiting for {what}: {}",
        host_state(console).await
    );
}

/// A STUN server on loopback that says every asker is at `who`.
async fn fake_stun(who: std::net::SocketAddr) -> std::net::SocketAddr {
    let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = s.local_addr().unwrap();
    tokio::spawn(async move {
        let mut b = [0u8; 1500];
        while let Ok((n, from)) = s.recv_from(&mut b).await {
            if n >= 20 {
                let tx: [u8; 12] = b[8..20].try_into().unwrap();
                let _ = s
                    .send_to(&crate::join_net::stun::answer(tx, who), from)
                    .await;
            }
        }
    });
    addr
}

fn settings_body() -> SettingsBody {
    SettingsBody {
        addr: None,
        guest_profiles: None,
        block_system_buttons: None,
        max_players: None,
        stream_seat: None,
        internet_invites: None,
        invites: None,
        hub_url: None,
        stun: None,
        upnp: None,
        video_track: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_internet_guest_joins_over_webrtc_plays_and_gets_the_picture() {
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let hub = FakeHub::start().await;
    let igd = crate::join_net::nat::tests::FakeIgd::start("198.51.100.7").await;
    *crate::join_net::rtc::TEST_NAT.lock().unwrap() = Some(crate::join_net::nat::Search {
        ssdp: igd.ssdp,
        pmp: false,
    });
    let stun = fake_stun("203.0.113.5:40000".parse().unwrap()).await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub.url.clone(),
        stun: vec![stun.to_string()],
        ..Settings::default()
    });
    let console = "10.77.0.41";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    set_live(console, 1);
    let live = crate::rp_stream_api::test_live::Session::start(console);
    let s = open_party(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    assert!(
        s["invite"]["internet"].is_null(),
        "no internet invite until asked"
    );

    // The internet invite: a room on the hub, a link with the key and the fingerprint.
    let inv = internet_invite(console).await;
    let net = &inv["internet"];
    let room = net["room"].as_str().unwrap().to_string();
    let fp = crate::join_net::fingerprint().unwrap();
    let hub_host = crate::join_net::hub_host(&hub.url).unwrap();
    let url = net["url"].as_str().unwrap();
    let (link, lan) = url.split_once("&lan=").unwrap_or((url, ""));
    assert_eq!(
        link,
        format!(
            "https://ps5upload.phantomptr.com/join/#room={room}&k={key}&fp={fp}&hub={hub_host}"
        )
    );
    // The hint for a guest who can't reach the hub: the home network's page (no key of its own;
    // the page adds the link's). None when this computer has no address a guest could use.
    if let Some(home) = inv["url"].as_str() {
        let page = home.split('#').next().unwrap();
        assert_eq!(lan, crate::join_net::pct(page), "{url}");
    }
    // The one link to share: the internet one.
    assert_eq!(inv["share"]["works"], "anywhere", "{inv}");
    assert_eq!(inv["share"]["url"], url);
    assert_eq!(
        inv["key"],
        key.as_str(),
        "one invite, one key for both links"
    );
    until("hosted", || hub.has_host(&room)).await;
    // The router mapped the ICE port (UPnP, the fake router).
    let st = until_state(console, "the router mapping", |s| {
        s["invite"]["internet"]["router"]["via"] == "upnp"
            && s["invite"]["internet"]["state"] == "hosted"
    })
    .await;
    let port = st["invite"]["internet"]["router"]["port"].as_u64().unwrap();
    assert!(igd
        .calls()
        .iter()
        .any(|c| c.starts_with(&format!("add UDP {port} 127.0.0.1:{port}"))));

    // A guest anywhere: ICE over loopback, the data channel open.
    let mut g = connect_guest(&hub.url, &room).await;
    assert!(
        g.answer.contains(&format!("a=fingerprint:sha-256 {fp}")),
        "the answer carries the link's fingerprint"
    );
    assert!(g.answer.contains("198.51.100.7"), "the router's candidate");
    assert!(g.answer.contains("203.0.113.5"), "the STUN candidate");

    // The LAN socket's protocol, on the channel.
    g.text(serde_json::json!({"hello": {"invite": key, "name": "Far"}}));
    assert_eq!(g.next_with("stage").await["stage"], "asking");
    allow_next(console, true).await;
    let j = g.next_with("stage").await;
    assert_eq!(j["stage"], "joined", "{j}");
    assert_eq!(j["player"], 2);
    // A page that offered no video track (an older one): the pictures on the channel, as before.
    assert_eq!(j["track"], false, "{j}");

    // A keyframe bigger than a data-channel message comes whole (in pieces on the wire).
    let big: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    live.picture(&big, true);
    live.picture(&[9, 9, 9], false);
    let mut got = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while got.len() < 2 {
        match tokio::time::timeout_at(deadline, g.rx.recv()).await {
            Ok(Some(GIn::Binary(b))) if b.first() == Some(&1) => got.push(b),
            Ok(Some(_)) => {}
            other => panic!("pictures: {other:?}"),
        }
    }
    assert_eq!(got[0].len(), big.len() + 1);
    assert_eq!(&got[0][1..], &big[..]);
    assert_eq!(got[1], vec![1, 9, 9, 9]);

    // Their controller reaches the console.
    use vpad::{app_button as app, button as pad};
    let _ = g.tx.send(GOut::Binary(frame(1, app::CROSS, &[], None)));
    until("the pad", || {
        f.states()
            .iter()
            .any(|s| s.slot == 2 && s.state.buttons & pad::CROSS != 0)
    })
    .await;

    // The host's panel: an internet guest, direct, with round trips (the channel's ping).
    let st = until_state(console, "a direct guest with a round trip", |s| {
        let g = &s["guests"][0];
        g["via"] == "internet"
            && g["path"] == "direct"
            && g["rtt_ms"].as_u64().is_some()
            && s["invite"]["internet"]["peers"][0]["path"] == "direct"
    })
    .await;
    assert_eq!(st["internet"]["room"], room.as_str());

    // How they are connected, from ICE's selected pair: this test's loopback is the home network.
    let st = until_state(console, "the selected pair", |s| {
        s["guests"][0]["link"]["path"].is_string()
    })
    .await;
    assert_eq!(st["guests"][0]["link"]["path"], "lan");
    assert!(st["guests"][0]["link"]["rtt_ms"].as_u64().is_some());

    // The link's numbers, read on demand: the big picture went through the channel's buffer.
    let link = &st["guests"][0]["link"];
    assert_eq!(link["video"], "channel", "{link}");
    assert!(link["track"].is_null(), "{link}");
    assert!(link["backlog_peak_bytes"].as_u64().unwrap() >= big.len() as u64, "{link}");
    assert_eq!(link["high_water_bytes"], crate::join_net::HIGH_WATER);
    for k in ["queued_bytes", "buffered_bytes", "send_waits"] {
        assert!(link[k].as_u64().is_some(), "{k}: {link}");
    }
    assert!(st["invite"]["internet"]["peers"][0]["link"].is_object());
    // The page's report on its picture: the panel's word for it.
    g.text(serde_json::json!({"stats": {"frames": 120, "stalls": 3, "stall_ms": 900}}));
    let st = until_state(console, "the page's report", |s| {
        s["guests"][0]["page"]["frames"] == 120
    })
    .await;
    assert_eq!(st["guests"][0]["quality"], "poor");
    // Once a stats second passed, the rate out is known.
    until_state(console, "the rate out", |s| {
        s["guests"][0]["link"]["sent_kbps"].as_u64().is_some()
    })
    .await;

    // They leave; Play Together closes: the room goes, and the router mapping with it.
    g.text(serde_json::json!({"leave": 1}));
    until_state(console, "left", |s| s["guests"][0]["state"] == "left").await;
    close_party(console).await;
    until("the room closed", || !hub.has_host(&room)).await;
    until("the mapping removed", || {
        igd.calls()
            .iter()
            .any(|c| c == &format!("delete UDP {port}"))
    })
    .await;
    assert_eq!(
        hub.st.lock().unwrap().host_frames.last().unwrap()["t"],
        "leave"
    );
    *crate::join_net::rtc::TEST_NAT.lock().unwrap() = None;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

/// A guest whose only address is IPv6 (a home behind its provider's shared IPv4, CGNAT, where no
/// port can be forwarded) joins an internet invite directly: the engine offers its IPv6 addresses,
/// takes the guest's IPv6 candidates, and the panel says the connection is IPv6. Before, both were
/// IPv4 only and this guest never connected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_internet_guest_with_only_ipv6_joins_directly() {
    let _turn = PARTY_TESTS.lock().await;
    let hub = FakeHub::start().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub.url.clone(),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let console = "10.77.0.46";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    let s = open_party(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    let inv = internet_invite(console).await;
    let room = inv["internet"]["room"].as_str().unwrap().to_string();
    until("hosted", || hub.has_host(&room)).await;

    let Some(mut g) = crate::join_net::tests::connect_guest_v6(&hub.url, &room).await else {
        eprintln!("no IPv6 loopback here: skipped");
        close_party(console).await;
        *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
        set_live(console, 0);
        return;
    };
    assert!(
        g.answer.contains("::1 "),
        "the answer offers the engine's IPv6 address: {}",
        g.answer
    );
    g.text(serde_json::json!({"hello": {"invite": key, "name": "Six"}}));
    assert_eq!(g.next_with("stage").await["stage"], "asking");
    allow_next(console, true).await;
    assert_eq!(g.next_with("stage").await["stage"], "joined");

    let st = until_state(console, "the selected pair", |s| {
        s["guests"][0]["link"]["path"].is_string()
    })
    .await;
    assert_eq!(st["guests"][0]["link"]["ipv6"], true, "{st}");

    g.text(serde_json::json!({"leave": 1}));
    until_state(console, "left", |s| s["guests"][0]["state"] == "left").await;
    close_party(console).await;
    until("the room closed", || !hub.has_host(&room)).await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

/// An app guest that reaches this engine (the same Wi-Fi, Tailscale) signals the internet invite
/// to it directly (`/api/join/signal`): the same peer connection and certificate, the key only on
/// the channel, and nothing through the hub. A room this engine does not have is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_app_guest_signals_the_internet_invite_to_the_engine_without_the_hub() {
    let _turn = PARTY_TESTS.lock().await;
    let hub = FakeHub::start().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub.url.clone(),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let console = "10.77.0.47";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    let s = open_party(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    let inv = internet_invite(console).await;
    let room = inv["internet"]["room"].as_str().unwrap().to_string();
    assert!(internet_room_open(&room));
    assert!(!internet_room_open("AAAAAAAAAAAAAAAAAAAAAA"));

    let signal = serve().await.replace("/api/join/ws", "/api/join/signal");
    // Not a room here: refused, the socket closed.
    {
        let (mut ws, _) = tokio_tungstenite::connect_async(&signal).await.unwrap();
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as TMsg;
        ws.send(TMsg::Text(
            serde_json::json!({"v": 1, "t": "join", "room": "AAAAAAAAAAAAAAAAAAAAAA"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
        assert_eq!(v, serde_json::json!({"t": "error", "why": "no_room"}));
    }

    // The room: the hub's guest protocol, the engine's own answer with the link's certificate.
    let fp = crate::join_net::fingerprint().unwrap();
    let mut g = connect_guest(&signal, &room).await;
    assert!(g.answer.contains(&format!("a=fingerprint:sha-256 {fp}")));
    g.text(serde_json::json!({"hello": {"invite": key, "name": "Near"}}));
    assert_eq!(g.next_with("stage").await["stage"], "asking");
    allow_next(console, true).await;
    let j = g.next_with("stage").await;
    assert_eq!(j["stage"], "joined", "{j}");
    let st = until_state(console, "the guest's path", |s| {
        s["guests"][0]["link"]["path"].is_string()
    })
    .await;
    assert_eq!(st["guests"][0]["via"], "internet", "an internet invite's guest: {st}");
    assert_eq!(st["guests"][0]["link"]["path"], "lan");
    assert_eq!(hub.guests_ever(), 0, "nothing went through the hub");

    g.text(serde_json::json!({"leave": 1}));
    until_state(console, "left", |s| s["guests"][0]["state"] == "left").await;
    close_party(console).await;
    assert!(!internet_room_open(&room), "closed with Play Together");
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

/// A guest on the home network with the Wi-Fi's own link (no internet invite, no hub at all)
/// joins over WebRTC signalled on the engine's socket: the link carries the engine's
/// certificate, the answer has it, the invite key goes in the hello on the channel, the picture
/// comes on the video track. A home-network join is refused while no Play Together is open.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_home_network_guest_joins_over_webrtc_without_any_hub() {
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        internet_invites: false,
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let signal = serve().await.replace("/api/join/ws", "/api/join/signal");
    {
        // Nothing open: refused.
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message as TMsg;
        let (mut ws, _) = tokio_tungstenite::connect_async(&signal).await.unwrap();
        ws.send(TMsg::Text(r#"{"v":1,"t":"join","lan":true}"#.into()))
            .await
            .unwrap();
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(m.to_text().unwrap(), r#"{"t":"error","why":"no_room"}"#);
    }
    let console = "10.77.0.48";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    let live = crate::rp_stream_api::test_live::Session::start(console);
    let s = open_party(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    assert!(s["invite"]["internet"].is_null(), "no hub, no room: {s}");
    let fp = crate::join_net::fingerprint().unwrap();
    for u in s["invite"]["urls"].as_array().unwrap() {
        assert!(
            u.as_str().unwrap().ends_with(&format!("#i={key}&fp={fp}")),
            "the Wi-Fi's link names the certificate: {u}"
        );
    }

    let mut g = crate::join_net::tests::connect_guest_lan(&signal, true).await;
    assert!(g.answer.contains(&format!("a=fingerprint:sha-256 {fp}")));
    g.text(serde_json::json!({"hello": {"invite": key, "name": "Near"}}));
    assert_eq!(g.next_with("stage").await["stage"], "asking");
    allow_next(console, true).await;
    let j = g.next_with("stage").await;
    assert_eq!(j["stage"], "joined", "{j}");
    assert_eq!(j["track"], true, "the picture on the video track: {j}");
    let (header, key_au, _) = h264();
    live.send(0, &header, false);
    live.picture(&key_au, true);
    let (got, _) = next_track_picture(&mut g).await;
    assert!(got.ends_with(&key_au[key_au.len() - 64..]), "the keyframe on the track");
    // The sound on the Opus audio track beside it (#720), not on the channel: the header (2
    // channels, 16 bits, 48 kHz, 480 samples a frame), then frames [index u16 BE][Opus packet].
    assert_eq!(j["audio"], true, "{j}");
    live.send(3, &[2, 16, 0, 0, 0xbb, 0x80, 0, 0, 0x01, 0xe0], false);
    for i in 0u16..3 {
        let mut f = i.to_be_bytes().to_vec();
        f.extend_from_slice(&[0xfc, 0xff, 0xfe, i as u8]);
        live.send(4, &f, false);
    }
    let mut sound = Vec::new();
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while sound.len() < 3 {
        match tokio::time::timeout_at(until, g.rx.recv()).await {
            Ok(Some(GIn::Audio(p))) => sound.push(p),
            Ok(Some(GIn::Binary(b))) if matches!(b.first(), Some(3 | 4)) => {
                panic!("sound on the channel with the audio track on")
            }
            Ok(Some(_)) => {}
            other => panic!("no sound on the track: {other:?} after {}", sound.len()),
        }
    }
    assert_eq!(sound[0], vec![0xfc, 0xff, 0xfe, 0], "the Opus packet as it came");
    assert_eq!(sound[2], vec![0xfc, 0xff, 0xfe, 2]);
    let st = until_state(console, "the guest's path", |s| {
        s["guests"][0]["link"]["path"].is_string()
    })
    .await;
    assert_eq!(st["guests"][0]["link"]["path"], "lan");

    g.text(serde_json::json!({"leave": 1}));
    until_state(console, "left", |s| s["guests"][0]["state"] == "left").await;
    close_party(console).await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

#[test]
fn the_relay_passes_a_guest_or_a_signalling_socket_only() {
    assert_eq!(
        relay_target("ws://192.168.1.5:19113/api/join/ws"),
        Some(("ws://192.168.1.5:19113/api/join/ws".to_string(), GUEST_MSG_MAX))
    );
    // An offer is several KiB: the hub's frame size for a signalling socket.
    assert_eq!(
        relay_target("ws://100.101.2.3:19113/api/join/signal"),
        Some((
            "ws://100.101.2.3:19113/api/join/signal".to_string(),
            crate::join_net::local_signal::FRAME_MAX
        ))
    );
    for bad in [
        "ws://8.8.8.8:19113/api/join/signal",
        "ws://192.168.1.5:19113/api/join/open",
        "ws://192.168.1.5:19113/api/join/signal?x=1",
        "ws://192.168.1.5:19113/api/join/signalx",
    ] {
        assert_eq!(relay_target(bad), None, "{bad}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hub_room_is_renamed_if_taken_reclaimed_after_a_drop_and_closed_when_turned_off() {
    let _turn = PARTY_TESTS.lock().await;
    let hub = FakeHub::start().await;
    hub.st.lock().unwrap().refuse_hosts = 1;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub.url.clone(),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let console = "10.77.0.42";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    open_party(console).await;
    let first = internet_invite(console).await["internet"]["room"]
        .as_str()
        .unwrap()
        .to_string();
    // Taken (someone else's room by that name): another name, then hosted.
    until("hosted under another name", || hub.hosts().len() >= 2).await;
    let hosts = hub.hosts();
    assert_eq!(hosts[0]["room"], first.as_str());
    let room = hosts[1]["room"].as_str().unwrap().to_string();
    assert_ne!(room, first);
    until("hosted", || hub.has_host(&room)).await;
    let s = until_state(console, "the new name", |s| {
        s["invite"]["internet"]["state"] == "hosted"
    })
    .await;
    assert_eq!(s["invite"]["internet"]["room"], room.as_str());
    assert!(s["invite"]["internet"]["url"]
        .as_str()
        .unwrap()
        .contains(&format!("#room={room}&")));

    // The hub drops the connection: the engine comes back with the same room and secret.
    let conns = hub.host_conns();
    hub.drop_hosts();
    until("reconnected", || {
        hub.host_conns() > conns && hub.has_host(&room)
    })
    .await;
    let hosts = hub.hosts();
    let (a, b) = (&hosts[hosts.len() - 2], &hosts[hosts.len() - 1]);
    assert_eq!((&a["room"], &a["secret"]), (&b["room"], &b["secret"]));
    assert_eq!(b["v"], 1);
    until_state(console, "hosted again", |s| {
        s["invite"]["internet"]["state"] == "hosted"
    })
    .await;

    // Internet invites turned off: the room closes, the LAN invite stays.
    let (ci, h) = local();
    let r = body(
        set_settings(
            ci,
            h,
            Json(SettingsBody {
                internet_invites: Some(false),
                invites: None,
                ..settings_body()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(r["settings"]["internet_invites"], false);
    until("the room closed", || !hub.has_host(&room)).await;
    let s = host_state(console).await;
    assert!(s["invite"]["internet"].is_null() && !s["invite"]["key"].is_null());
    let (ci, h) = local();
    let r = invite(
        ci,
        h,
        Json(InviteBody {
            addr: console.into(),
            new: false,
            internet: true,
            share: false,
        }),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(body(r).await["error"], "internet_off");
    close_party(console).await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

#[tokio::test]
async fn the_settings_check_the_hub_and_stun_addresses() {
    let _turn = PARTY_TESTS.lock().await;
    async fn set(hub: Option<&str>, stun: Option<Vec<&str>>) -> Response {
        let (ci, h) = local();
        set_settings(
            ci,
            h,
            Json(SettingsBody {
                hub_url: hub.map(String::from),
                stun: stun.map(|v| v.into_iter().map(String::from).collect()),
                ..settings_body()
            }),
        )
        .await
    }
    let d = Settings::default();
    assert_eq!(d.hub_url, "wss://hub.phantomptr.com/ws");
    assert_eq!(
        d.stun,
        ["stun.cloudflare.com:3478", "stun.l.google.com:19302"]
    );
    assert!(d.internet_invites && d.upnp);
    assert_eq!(
        set(Some("http://x/ws"), None).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        set(None, Some(vec!["no port"])).await.status(),
        StatusCode::BAD_REQUEST
    );
    let r = body(
        set(
            Some("wss://hub.example.net/ws"),
            Some(vec!["stun:hub.phantomptr.com:3478", "[2001:db8::1]:3478"]),
        )
        .await,
    )
    .await;
    assert_eq!(r["settings"]["hub_url"], "wss://hub.example.net/ws");
    assert_eq!(
        r["settings"]["stun"],
        serde_json::json!(["hub.phantomptr.com:3478", "[2001:db8::1]:3478"])
    );
    // Settings saved before these fields read as the defaults.
    let old: Settings =
        serde_json::from_str(r#"{"block_system_buttons":true,"max_players":3}"#).unwrap();
    assert_eq!(old, Settings::default());
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The engine for the guest page's Playwright test (client/e2e/play-together-internet.spec.ts),
/// not a test of its own: it runs only when the spec starts it with `PS5UPLOAD_E2E_HUB` (the real
/// hub on this computer). Play Together's real routes (`router`) on 127.0.0.1, a pretend session
/// on a pretend console (the fakes above: no PS5), no STUN and no router mapping (nothing leaves
/// this computer), and ICE on this computer's real interfaces as a running engine has them.
/// `POST /e2e/stream` with `[kind, key, ...data]` hands the session's stream a message (0: the
/// codec header, 1: a picture); `POST /e2e/stop` ends it. It prints
/// `E2E_ENGINE=http://127.0.0.1:<port> E2E_CONSOLE=<console>` when ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the Playwright spec's engine; run by client/e2e/play-together-internet.spec.ts"]
async fn e2e_internet_guest_fixture() {
    let Ok(hub_url) = std::env::var("PS5UPLOAD_E2E_HUB") else {
        return;
    };
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    crate::join_net::rtc::TEST_REAL_IPS.store(true, std::sync::atomic::Ordering::Relaxed);
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url,
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let console = "10.77.0.60";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    let live = Arc::new(crate::rp_stream_api::test_live::Session::start(console));
    let stop = Arc::new(tokio::sync::Notify::new());

    use axum::routing::post;
    let (l, s) = (live.clone(), stop.clone());
    let app = router::<()>()
        .route(
            "/e2e/stream",
            post(move |b: axum::body::Bytes| {
                let l = l.clone();
                async move {
                    if b.len() < 2 {
                        return StatusCode::BAD_REQUEST;
                    }
                    l.send(b[0], &b[2..], b[1] == 1);
                    StatusCode::NO_CONTENT
                }
            }),
        )
        .route(
            "/e2e/stop",
            post(move || {
                let s = s.clone();
                async move {
                    s.notify_one();
                    StatusCode::NO_CONTENT
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    println!("E2E_ENGINE=http://127.0.0.1:{port} E2E_CONSOLE={console}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let served = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        // At most ten minutes, whatever happens to the spec.
        let _ = tokio::time::timeout(Duration::from_secs(600), stop.notified()).await;
    });
    let _ = served.await;
    close_party(console).await;
    set_live(console, 0);
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// An H.264 stream as the console's would be: the codec header (SPS, PPS), a keyframe without
/// them, a picture after it. No two zero bytes in a row inside a unit (no start code there).
fn h264() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut header = vec![0, 0, 0, 1, 0x67, 0x64, 0x00, 0x2a, 0xac, 0x2b, 0x40, 0x3c, 0x01];
    header.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x3c, 0xb0]);
    let mut key = vec![0, 0, 0, 1, 0x65, 0x88];
    key.extend((0..30_000u32).map(|i| (i % 200 + 17) as u8));
    let mut p = vec![0, 0, 0, 1, 0x41, 0x9a];
    p.extend((0..900u32).map(|i| (i % 100 + 9) as u8));
    (header, key, p)
}

/// The next picture off the guest's video track (10 s), skipping the channel's messages but
/// failing on a picture there.
async fn next_track_picture(g: &mut crate::join_net::tests::TestGuest) -> (Vec<u8>, bool) {
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(until, g.rx.recv()).await {
            Ok(Some(GIn::Media(au, contiguous))) => return (au, contiguous),
            Ok(Some(GIn::Binary(b))) if b.first() == Some(&1) => {
                panic!("a picture on the channel with the track on")
            }
            Ok(Some(_)) => {}
            other => panic!("no picture on the track: {other:?}"),
        }
    }
}

/// The picture as an RTP video track (#762's design): a page that offers one gets the pictures
/// there (the codec header in front of each keyframe), the sound stays on the channel, the
/// browser's PLI asks the console for a keyframe, and the host's panel reads the track's numbers.
/// The host's setting off answers that same page with the channel (as an engine without the track
/// would), and the page shows the channel's pictures.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_that_offers_a_video_track_gets_the_pictures_on_it_unless_the_host_turned_it_off() {
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let hub = FakeHub::start().await;
    let console = "10.77.0.43";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    set_live(console, 1);
    let live = crate::rp_stream_api::test_live::Session::start(console);
    let (header, key, p) = h264();
    for track_on in [true, false] {
        *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
            hub_url: hub.url.clone(),
            stun: Vec::new(),
            upnp: false,
            video_track: track_on,
            ..Settings::default()
        });
        let s = open_party(console).await;
        let invite = s["invite"]["key"].as_str().unwrap().to_string();
        let room = internet_invite(console).await["internet"]["room"]
            .as_str()
            .unwrap()
            .to_string();
        until("hosted", || hub.has_host(&room)).await;
        let mut g = connect_guest_with(&hub.url, &room, "join", true).await;
        assert!(g.answer.contains("m=video"), "the video section is answered");
        g.text(serde_json::json!({"hello": {"invite": invite, "name": "Far"}}));
        assert_eq!(g.next_with("stage").await["stage"], "asking");
        allow_next(console, true).await;
        let j = g.next_with("stage").await;
        assert_eq!((&j["stage"], &j["track"]), (&"joined".into(), &track_on.into()), "{j}");

        live.send(0, &header, false);
        live.picture(&key, true);
        live.picture(&p, false);
        if !track_on {
            live.send(4, &[0, 1, 0xfc, 0xff], false);
            // Off: everything on the channel, the page's own player shows it.
            let mut kinds = Vec::new();
            let until = tokio::time::Instant::now() + Duration::from_secs(10);
            while kinds.len() < 4 {
                match tokio::time::timeout_at(until, g.rx.recv()).await {
                    Ok(Some(GIn::Binary(b))) => kinds.push(b[0]),
                    Ok(Some(GIn::Media(..))) => panic!("a picture on the track with it off"),
                    Ok(Some(_)) => {}
                    other => panic!("the channel's messages: {other:?} after {kinds:?}"),
                }
            }
            // Sound may come first (it goes before the pictures).
            kinds.sort();
            assert_eq!(kinds, vec![0, 1, 1, 4]);
            let st = until_state(console, "the channel's numbers", |s| {
                s["guests"][0]["link"]["video"] == "channel"
            })
            .await;
            assert!(st["guests"][0]["link"]["track"].is_null());
        } else {
            // The keyframe with the codec header in front (it came without), then the picture.
            let (au, contiguous) = next_track_picture(&mut g).await;
            assert!(contiguous);
            let types: Vec<u8> = au
                .windows(4)
                .filter(|w| w[..3] == [0, 0, 1])
                .map(|w| w[3] & 0x1f)
                .collect();
            assert_eq!(types, vec![7, 8, 5], "SPS, PPS, IDR");
            assert!(au.ends_with(&key[key.len() - 64..]));
            let (au, _) = next_track_picture(&mut g).await;
            assert!(au.ends_with(&p[4..]), "the picture after it, whole");
            // The sound stays on the channel.
            live.send(4, &[0, 1, 0xfc, 0xff], false);
            let until = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                match tokio::time::timeout_at(until, g.rx.recv()).await {
                    Ok(Some(GIn::Binary(b))) if b.first() == Some(&4) => break,
                    Ok(Some(GIn::Binary(b))) if b.first() == Some(&1) => panic!("a picture on the channel"),
                    Ok(Some(_)) => {}
                    other => panic!("no sound on the channel: {other:?}"),
                }
            }
            // The browser's PLI: the session's keyframe ask (its gates), counted on both sides.
            let _ = g.tx.send(GOut::Pli);
            let st = until_state(console, "the PLI and the track's numbers", |s| {
                let gst = &s["guests"][0];
                gst["key_asks"].as_u64() >= Some(1) && gst["link"]["track"]["plis"].as_u64() >= Some(1)
            })
            .await;
            let link = &st["guests"][0]["link"];
            assert_eq!(link["video"], "track", "{link}");
            let t = &link["track"];
            assert_eq!(t["frames_sent"], 2, "{t}");
            let rate = ps5upload_core::rp_stream::StreamConfig::default().bitrate_kbps;
            assert_eq!(t["stream_kbps"], rate, "the session's bitrate: {t}");
            assert_eq!(t["slow"], false, "{t}");
            for k in ["rtp_packets", "nacks", "firs"] {
                assert!(t[k].as_u64().is_some(), "{k}: {t}");
            }
            // The estimate arrives with the transport-cc feedback.
            until_state(console, "the bandwidth estimate", |s| {
                s["guests"][0]["link"]["track"]["bwe_kbps"].as_u64().is_some()
            })
            .await;
        }
        g.text(serde_json::json!({"leave": 1}));
        until_state(console, "left", |s| s["guests"][0]["state"] == "left").await;
        close_party(console).await;
        until("the room closed", || !hub.has_host(&room)).await;
    }
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

async fn share_invite(console: &str) -> serde_json::Value {
    let (ci, h) = local();
    body(
        invite(
            ci,
            h,
            Json(InviteBody {
                addr: console.into(),
                new: false,
                internet: false,
                share: true,
            }),
        )
        .await,
    )
    .await
}

async fn set_invites(word: &str) -> Response {
    let (ci, h) = local();
    set_settings(
        ci,
        h,
        Json(SettingsBody {
            invites: Some(word.into()),
            ..settings_body()
        }),
    )
    .await
}

async fn set_settings_hub(url: &str) -> Response {
    let (ci, h) = local();
    set_settings(
        ci,
        h,
        Json(SettingsBody {
            hub_url: Some(url.into()),
            ..settings_body()
        }),
    )
    .await
}

/// One invite: the host asks for the one link. With the hub there it is the internet link; with
/// no hub to reach (no internet here, or the hub is down) the home network's link instead, said
/// to work on the Wi-Fi only, until the hub hosts the room again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_one_link_is_the_home_networks_while_the_hub_cant_be_reached() {
    let _turn = PARTY_TESTS.lock().await;
    // A hub address nothing answers on (a port just closed): every connect fails at once.
    let dead = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap()
    };
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: format!("ws://{dead}/ws"),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let console = "10.77.0.45";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    open_party(console).await;
    let inv = share_invite(console).await;
    assert!(inv["internet"]["room"].is_string(), "a room was asked for: {inv}");
    let s = until_state(console, "the Wi-Fi's link", |s| {
        s["invite"]["share"]["works"] == "wifi"
    })
    .await;
    let inv = &s["invite"];
    assert_eq!(inv["share"]["why"], "offline", "{inv}");
    assert_eq!(inv["share"]["url"], inv["url"], "the home network's own link");
    assert_eq!(inv["internet"]["offline"], true);

    // The hub is back (here: the settings name one that answers): the internet link again.
    let hub = FakeHub::start().await;
    assert_eq!(set_settings_hub(&hub.url).await.status(), StatusCode::OK);
    let inv = share_invite(console).await;
    let room = inv["internet"]["room"].as_str().unwrap().to_string();
    until("hosted", || hub.has_host(&room)).await;
    let s = until_state(console, "the internet link", |s| {
        s["invite"]["share"]["works"] == "anywhere"
    })
    .await;
    assert_eq!(s["invite"]["share"]["url"], s["invite"]["internet"]["url"]);
    assert!(s["invite"]["share"]["why"].is_null());
    close_party(console).await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

/// Invites work: Only on my Wi-Fi. The one link is the home network's (no room on the hub, no
/// refusal), `invites` and `internet_invites` say the same, and a word it doesn't know is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn invites_work_only_on_the_wifi_keeps_to_the_home_networks_link() {
    let _turn = PARTY_TESTS.lock().await;
    let hub = FakeHub::start().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub.url.clone(),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let console = "10.77.0.46";
    set_helper(console, Arc::new(FakeHelper::default()));
    set_live(console, 1);
    let s = open_party(console).await;
    assert_eq!(s["settings"]["invites"], "anywhere");
    // Before anything is asked: the home network's link, none asked for.
    assert_eq!(s["invite"]["share"]["works"], "wifi");
    assert_eq!(s["invite"]["share"]["why"], "local");

    let r = body(set_invites("wifi").await).await;
    assert_eq!(
        (&r["settings"]["invites"], &r["settings"]["internet_invites"]),
        (&serde_json::json!("wifi"), &serde_json::json!(false))
    );
    let inv = share_invite(console).await;
    assert!(inv["internet"].is_null(), "{inv}");
    assert_eq!(inv["share"]["works"], "wifi");
    assert_eq!(inv["share"]["why"], "setting");
    assert_eq!(inv["share"]["url"], inv["url"]);
    assert!(hub.hosts().is_empty(), "nothing on the hub");
    assert_eq!(
        set_invites("everywhere").await.status(),
        StatusCode::BAD_REQUEST
    );

    // Anywhere again (by the older name): the one link is the internet one.
    let (ci, h) = local();
    let r = body(
        set_settings(
            ci,
            h,
            Json(SettingsBody {
                internet_invites: Some(true),
                ..settings_body()
            }),
        )
        .await,
    )
    .await;
    assert_eq!(r["settings"]["invites"], "anywhere");
    let inv = share_invite(console).await;
    assert_eq!(inv["share"]["works"], "anywhere", "{inv}");
    close_party(console).await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_live(console, 0);
}

/// "Invites work" is kept in the data folder; a file with the older name of the choice
/// (`internet_invites` false) reads as Only on my Wi-Fi, a missing or broken one as nothing saved.
#[test]
fn invites_work_is_saved_and_an_old_off_reads_as_wifi() {
    let dir = std::env::temp_dir().join(format!("ps5u-invites-{}", std::process::id()));
    let p = dir.join(INVITES_FILE);
    assert_eq!(read_invites(&p), None);
    write_invites(&p, false).unwrap();
    assert_eq!(read_invites(&p), Some(false));
    assert!(std::fs::read_to_string(&p).unwrap().contains("\"wifi\""));
    write_invites(&p, true).unwrap();
    assert_eq!(read_invites(&p), Some(true));
    std::fs::write(&p, r#"{"internet_invites": false}"#).unwrap();
    assert_eq!(read_invites(&p), Some(false));
    std::fs::write(&p, "not json").unwrap();
    assert_eq!(read_invites(&p), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The one link's words: the internet link unless the hub can't be reached; the Wi-Fi's link says
/// why it is the one.
#[test]
fn the_one_link_says_where_it_works() {
    let home = "http://192.168.1.10:19113/join#i=k";
    let lan = Some(home);
    let net = serde_json::json!({"url": "https://x/join/#room=r", "offline": false});
    assert_eq!(
        share_json(lan, Some(&net), true),
        serde_json::json!({"url": "https://x/join/#room=r", "works": "anywhere", "why": null})
    );
    let down = serde_json::json!({"url": "https://x/join/#room=r", "offline": true});
    assert_eq!(share_json(lan, Some(&down), true)["why"], "offline");
    assert_eq!(share_json(lan, Some(&down), true)["url"], home);
    assert_eq!(share_json(lan, None, false)["why"], "setting");
    assert_eq!(share_json(lan, None, true)["why"], "local");
    // An older engine's room json has no `offline`: its link stands.
    let old = serde_json::json!({"url": "https://x/join/#room=r"});
    assert_eq!(share_json(lan, Some(&old), true)["works"], "anywhere");
}
