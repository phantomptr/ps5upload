// join.rs's tests for internet invites (included into `join::tests::internet`, for its fakes): the guest
// session over a WebRTC data channel, the hub room's life, the settings.

use super::*;
use crate::join_net::tests::{connect_guest, FakeHub, GIn, GOut};

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
        hub_url: None,
        stun: None,
        upnp: None,
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
    assert_eq!(
        net["url"].as_str().unwrap(),
        format!(
            "https://ps5upload.phantomptr.com/join/#room={room}&k={key}&fp={fp}&hub={hub_host}"
        )
    );
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

    // The link's numbers, read on demand: the big picture went through the channel's buffer.
    let link = &st["guests"][0]["link"];
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
