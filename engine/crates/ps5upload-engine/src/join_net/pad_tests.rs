// The phone link over the internet (pad_link.rs on crate::join_net), included into
// `join::tests::pad_internet` for Play Together's settings and its test turns: a phone anywhere as
// a controller (touchpad, motion, rumble back) and as a watcher, over a WebRTC data channel
// labelled "pad" through a fake hub; New link, internet invites off and the other kind's label.

use super::*;
use crate::join_net::tests::{connect_guest_on, FakeHub, GIn, GOut, TestGuest};
use crate::rp_stream_api::test_live;

/// The host's routes, called as the app calls them.
async fn net_link(new: bool) -> Response {
    crate::pad_link::set_internet(
        HeaderMap::new(),
        Json(serde_json::from_value(serde_json::json!({ "new": new })).unwrap()),
    )
    .await
}

async fn pad_status() -> serde_json::Value {
    crate::pad_link::get_status(HeaderMap::new(), Query(Default::default()))
        .await
        .0
}

async fn close_link() {
    let _ = crate::pad_link::set_open(
        HeaderMap::new(),
        Json(serde_json::from_value(serde_json::json!({ "open": false })).unwrap()),
    )
    .await;
}

fn hub_settings(hub: &FakeHub) {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub.url.clone(),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
}

/// Polls the phone link's status until `f` holds (10 s).
async fn until_pads(what: &str, f: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    for _ in 0..400 {
        let s = pad_status().await;
        if f(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}: {}", pad_status().await);
}

/// The internet link's room and key, as its link carries them.
fn room_and_key(s: &serde_json::Value) -> (String, String) {
    let url = s["internet"]["url"].as_str().unwrap();
    let frag = url.split_once('#').unwrap().1;
    let get = |k: &str| {
        frag.split('&')
            .find_map(|p| p.strip_prefix(&format!("{k}=")))
            .unwrap()
            .to_string()
    };
    (get("room"), get("k"))
}

const DEVICE: &str = "00112233445566778899aabbccddeeff";

/// A phone's hello on its channel, and the stage it is answered.
async fn hello(g: &mut TestGuest, key: &str, watch: bool) -> serde_json::Value {
    hello_as(g, key, watch, DEVICE).await
}

async fn hello_as(g: &mut TestGuest, key: &str, watch: bool, device: &str) -> serde_json::Value {
    let mut h = serde_json::json!({"t": key, "name": "pixel", "d": device});
    if watch {
        h["w"] = 1.into();
    }
    g.text(serde_json::json!({ "hello": h }));
    g.next_with("stage").await
}

/// The host answers the internet phone waiting first (as the Allow over the picture does).
async fn answer_next(allow: bool) {
    let s = until_pads("an internet phone asking", |s| {
        s["pending"][0]["via"] == "internet"
    })
    .await;
    let id = s["pending"][0]["id"].as_u64().unwrap();
    let r = crate::pad_link::decide(Json(
        serde_json::from_value(serde_json::json!({"id": id, "allow": allow})).unwrap(),
    ))
    .await;
    assert_eq!(r.0["ok"], true);
}

/// Asks, is allowed, and is in: the next stage after the host's yes, with the reconnect token the
/// engine gave it first as `rt`.
async fn allowed_in(g: &mut TestGuest, key: &str, watch: bool) -> serde_json::Value {
    assert_eq!(hello(g, key, watch).await["stage"], "asking");
    answer_next(true).await;
    let rt = g.next_with("rt").await["rt"].clone();
    assert!(rt.as_str().is_some_and(|t| t.len() == 32), "{rt}");
    let mut v = g.next_with("stage").await;
    v["rt"] = rt;
    v
}

/// A phone coming back with the reconnect token it was given.
async fn hello_rt(g: &mut TestGuest, key: &str, watch: bool, rt: &serde_json::Value) -> serde_json::Value {
    let mut h = serde_json::json!({"t": key, "name": "pixel", "d": DEVICE, "rt": rt});
    if watch {
        h["w"] = 1.into();
    }
    g.text(serde_json::json!({ "hello": h }));
    g.next_with("stage").await
}

/// Why the phone is told it is out (the "joined" statuses before it skipped).
async fn refused_why(g: &mut TestGuest) -> serde_json::Value {
    loop {
        let v = g.next_with("stage").await;
        if v["stage"] == "refused" {
            return v["why"].clone();
        }
    }
}

/// Text messages within `ms` (none expected: the channel's session never started).
async fn silent_for(g: &mut TestGuest, ms: u64) -> bool {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        match tokio::time::timeout_at(until, g.rx.recv()).await {
            Err(_) => return true,
            Ok(Some(GIn::Text(_) | GIn::Binary(_) | GIn::Media(..) | GIn::Audio(_))) => return false,
            Ok(Some(GIn::Ping)) => {}
            Ok(None) => return true,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_phone_anywhere_is_a_controller_with_touchpad_motion_and_rumble() {
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let hub = FakeHub::start().await;
    hub_settings(&hub);
    close_link().await;
    let live = test_live::Session::start("10.77.0.70");

    // The internet link: the link opens with it, a room of its own, the controller page's link.
    let r = net_link(false).await;
    assert_eq!(r.status(), StatusCode::OK);
    let s = body(r).await;
    assert_eq!(s["open"], true);
    assert_eq!(s["internet_invites"], true);
    let (room, key) = room_and_key(&s);
    let fp = crate::join_net::fingerprint().unwrap();
    let hub_host = crate::join_net::hub_host(&hub.url).unwrap();
    let url = s["internet"]["url"].as_str().unwrap();
    let (link, lan) = url.split_once("&lan=").unwrap_or((url, ""));
    assert_eq!(
        link,
        format!("https://ps5upload.phantomptr.com/pad/#room={room}&k={key}&fp={fp}&hub={hub_host}")
    );
    let lan_url = s["url"].as_str().unwrap_or_default().to_string();
    // "Open on this Wi-Fi": the home network's controller page, without the LAN link's key (a
    // phone there asks, as over the internet).
    if !lan_url.is_empty() {
        let page = lan_url.split('#').next().unwrap();
        assert_eq!(lan, crate::join_net::pct(page), "{url}");
        assert!(!url.contains(&lan_url[lan_url.find("#t=").unwrap() + 3..]));
    }
    assert!(
        !lan_url.contains(&key),
        "the internet link has a key of its own"
    );
    // Asked again: the same link.
    let again = body(net_link(false).await).await;
    assert_eq!(again["internet"]["url"], s["internet"]["url"]);
    until("hosted", || hub.has_host(&room)).await;

    // A wrong key: refused, nothing on the console.
    let mut bad = connect_guest_on(&hub.url, &room, "pad").await;
    let v = hello(&mut bad, "00000000000000000000000000000000", false).await;
    assert_eq!((&v["stage"], &v["why"]), (&"refused".into(), &"key".into()));

    // The link's key only lets a phone ask: the host says No, and it is told so, nothing played.
    let mut nope = connect_guest_on(&hub.url, &room, "pad").await;
    let v = hello_as(&mut nope, &key, false, "ffeeddccbbaa99887766554433221100").await;
    assert_eq!(v["stage"], "asking", "{v}");
    assert!(pad_status().await["pads"].as_array().unwrap().is_empty());
    answer_next(false).await;
    assert_eq!(refused_why(&mut nope).await, "denied");

    // Allowed: in.
    let mut g = connect_guest_on(&hub.url, &room, "pad").await;
    assert!(g.answer.contains(&format!("a=fingerprint:sha-256 {fp}")));
    let v = allowed_in(&mut g, &key, false).await;
    assert_eq!(v["stage"], "joined", "{v}");
    let rt = v["rt"].clone();
    until_pads("an internet controller", |s| {
        s["pads"][0]["via"] == "internet" && s["pads"][0]["watch"] == false
    })
    .await;

    // Buttons, the touchpad and motion reach the console's pad.
    g.text(serde_json::json!({"pad": {
        "buttons": 1, "l2": 0, "r2": 0, "lx": 0, "ly": 0, "rx": 0, "ry": 0,
        "touch": [960, 470],
        "motion": {"gyro": [10, 20, 30], "accel": [0, 1000, 0], "t": 7},
    }}));
    until("the pad", || {
        let m = live.merged();
        m.buttons == 1
            && m.touch == Some([960, 470])
            && m.motion.is_some_and(|x| x.gyro == [10, 20, 30])
    })
    .await;
    // A binary message (the Touchpad & keyboard tab's reports) does not upset the session.
    let _ =
        g.tx.send(GOut::Binary(vec![crate::vinput_api::KIND_KBM; 17]));

    // Rumble and the light bar come back.
    crate::pad_link::broadcast(r#"{"rumble":[200,100]}"#);
    assert_eq!(
        g.next_with("rumble").await["rumble"],
        serde_json::json!([200, 100])
    );
    // The engine's pings go on the channel too (a quiet phone stays while it answers them).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, g.rx.recv()).await {
            Ok(Some(GIn::Ping)) => break,
            Ok(Some(_)) => {}
            other => panic!("no ping: {other:?}"),
        }
    }

    // Dropped (a tunnel, a lift): the phone the host allowed comes back without asking, with the
    // reconnect token the engine gave it. Its device id alone (which a phone makes up itself, so
    // anyone with the link could copy it) only asks again.
    drop(g);
    until_pads("the drop", |s| s["pads"].as_array().unwrap().is_empty()).await;
    let mut copycat = connect_guest_on(&hub.url, &room, "pad").await;
    assert_eq!(hello(&mut copycat, &key, false).await["stage"], "asking");
    answer_next(false).await;
    assert_eq!(refused_why(&mut copycat).await, "denied");
    let mut guess = connect_guest_on(&hub.url, &room, "pad").await;
    let wrong = serde_json::json!("0123456789abcdef0123456789abcdef");
    assert_eq!(hello_rt(&mut guess, &key, false, &wrong).await["stage"], "asking");
    answer_next(false).await;
    assert_eq!(refused_why(&mut guess).await, "denied");
    let mut g = connect_guest_on(&hub.url, &room, "pad").await;
    assert_eq!(hello_rt(&mut g, &key, false, &rt).await["stage"], "joined");
    let id = until_pads("back", |s| s["pads"][0]["via"] == "internet").await["pads"][0]["id"]
        .as_u64()
        .unwrap();
    g.text(serde_json::json!({"pad": {"buttons": 1}}));
    until("pressed again", || live.merged().buttons == 1).await;

    // Disconnect: let go at once, told why, and the device stays out (its token too).
    let k = crate::pad_link::kick(Json(
        serde_json::from_value(serde_json::json!({ "id": id })).unwrap(),
    ))
    .await;
    assert_eq!(k.0["ok"], true);
    assert_eq!(live.merged().buttons, 0);
    assert_eq!(refused_why(&mut g).await, "kicked");
    let mut back = connect_guest_on(&hub.url, &room, "pad").await;
    assert_eq!(hello(&mut back, &key, false).await["why"], "kicked");
    let mut other = connect_guest_on(&hub.url, &room, "pad").await;
    let v = hello_as(&mut other, &key, false, "ffeeddccbbaa99887766554433221100").await;
    assert_eq!(v["stage"], "asking", "a kicked phone's token is no way in: {v}");
    answer_next(false).await;

    close_link().await;
    until("the room closed", || !hub.has_host(&room)).await;
    assert!(pad_status().await["internet"].is_null());
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watcher_anywhere_gets_the_picture_never_the_controller_and_four_at_most() {
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let hub = FakeHub::start().await;
    hub_settings(&hub);
    close_link().await;
    let live = test_live::Session::start("10.77.0.71");
    let s = body(net_link(false).await).await;
    let (room, key) = room_and_key(&s);
    until("hosted", || hub.has_host(&room)).await;

    let mut w = connect_guest_on(&hub.url, &room, "pad").await;
    w.text(serde_json::json!({"hello": {"t": key, "name": "tablet", "w": 1}}));
    // No picture before the host allows it.
    assert_eq!(w.next_with("stage").await["stage"], "asking");
    assert!(!w.rx.try_recv().is_ok_and(|m| matches!(m, GIn::Binary(_))));
    answer_next(true).await;
    // Joined, and what the page's player needs first: the codec (in either order).
    let (mut joined, mut codec) = (false, None);
    while !joined || codec.is_none() {
        match tokio::time::timeout(Duration::from_secs(10), w.rx.recv()).await {
            Ok(Some(GIn::Text(v))) => {
                joined |= v["stage"] == "joined";
                if v.get("watch").is_some() {
                    codec = Some(v["watch"]["hevc"].clone());
                }
            }
            Ok(Some(_)) => {}
            other => panic!("joined and the codec: {other:?}"),
        }
    }
    assert_eq!(codec.unwrap(), false);
    let st = until_pads("an internet watcher", |s| s["pads"][0]["watch"] == true).await;
    assert_eq!(st["pads"][0]["via"], "internet");
    let id = st["pads"][0]["id"].as_u64().unwrap();

    // The picture: a keyframe bigger than a data-channel message comes whole.
    let big: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
    live.picture(&big, true);
    live.picture(&[9, 9, 9], false);
    let mut got = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while got.len() < 2 {
        match tokio::time::timeout_at(deadline, w.rx.recv()).await {
            Ok(Some(GIn::Binary(b))) if b.first() == Some(&1) => got.push(b),
            Ok(Some(_)) => {}
            other => panic!("pictures: {other:?}"),
        }
    }
    assert_eq!(&got[0][1..], &big[..]);
    assert_eq!(got[1], vec![1, 9, 9, 9]);
    // A keyframe ask is taken (and spaced out by the engine).
    w.text(serde_json::json!({"keyframe": true}));

    // Watchers have no controller over the internet: not handed one, and their pad does nothing.
    let c = crate::pad_link::control(Json(
        serde_json::from_value(serde_json::json!({"id": id, "on": true})).unwrap(),
    ))
    .await;
    assert_eq!(c.0["ok"], false);
    w.text(serde_json::json!({"pad": {"buttons": 1}}));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(live.merged().buttons, 0);

    // The watch party's limit holds for watchers from anywhere: four.
    // (One phone, allowed once: with its token the others come in without asking.)
    let mut more = Vec::new();
    let mut rt = serde_json::Value::Null;
    for i in 0..3 {
        let mut x = connect_guest_on(&hub.url, &room, "pad").await;
        let v = if i == 0 {
            let v = allowed_in(&mut x, &key, true).await;
            rt = v["rt"].clone();
            v
        } else {
            hello_rt(&mut x, &key, true, &rt).await
        };
        assert_eq!(v["stage"], "joined");
        more.push(x);
    }
    let mut fifth = connect_guest_on(&hub.url, &room, "pad").await;
    assert_eq!(hello_rt(&mut fifth, &key, true, &rt).await["why"], "full");

    close_link().await;
    assert_eq!(refused_why(&mut w).await, "closed");
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn new_link_revokes_the_old_one_and_internet_invites_off_closes_it() {
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let hub = FakeHub::start().await;
    hub_settings(&hub);
    close_link().await;
    let _live = test_live::Session::start("10.77.0.72");
    let (room1, key1) = room_and_key(&body(net_link(false).await).await);
    until("hosted", || hub.has_host(&room1)).await;
    let mut kept = connect_guest_on(&hub.url, &room1, "pad").await;
    let v = allowed_in(&mut kept, &key1, false).await;
    assert_eq!(v["stage"], "joined");
    let kept_rt = v["rt"].clone();
    // Another phone still waiting for the host's answer when the link is replaced.
    let mut waiting = connect_guest_on(&hub.url, &room1, "pad").await;
    let v = hello_as(
        &mut waiting,
        &key1,
        true,
        "ffeeddccbbaa99887766554433221100",
    )
    .await;
    assert_eq!(v["stage"], "asking");
    until_pads("asking", |s| s["pending"].as_array().unwrap().len() == 1).await;

    // New link: a new room and key; the old room closes (its link stops working), the phone
    // waiting is told so and leaves the list, the one allowed keeps playing.
    let (room2, key2) = room_and_key(&body(net_link(true).await).await);
    assert_eq!(refused_why(&mut waiting).await, "key");
    assert!(pad_status().await["pending"].as_array().unwrap().is_empty());
    assert_ne!(room1, room2);
    assert_ne!(key1, key2);
    until("the old room closed", || !hub.has_host(&room1)).await;
    until("the new room hosted", || hub.has_host(&room2)).await;
    // The old key on the new room: refused.
    let mut old = connect_guest_on(&hub.url, &room2, "pad").await;
    assert_eq!(hello(&mut old, &key1, false).await["why"], "key");
    // A phone already in keeps playing.
    assert_eq!(kept.next_with("stage").await["stage"], "joined");
    // But its reconnect token went with the old link: on the new one it asks again.
    let mut again = connect_guest_on(&hub.url, &room2, "pad").await;
    assert_eq!(hello_rt(&mut again, &key2, false, &kept_rt).await["stage"], "asking");
    answer_next(false).await;
    assert_eq!(refused_why(&mut again).await, "denied");

    // A Play Together channel in the phone link's room gets no session at all.
    let mut other = connect_guest_on(&hub.url, &room2, "join").await;
    other.text(serde_json::json!({"hello": {"invite": key2, "name": "x"}}));
    assert!(
        silent_for(&mut other, 800).await,
        "a join channel in a pad room"
    );

    // Internet invites off: the room closes, and no internet link is made.
    let (ci, h) = local();
    let _ = set_settings(
        ci,
        h,
        Json(SettingsBody {
            addr: None,
            guest_profiles: None,
            block_system_buttons: None,
            max_players: None,
            stream_seat: None,
            internet_invites: Some(false),
            invites: None,
            hub_url: None,
            stun: None,
            upnp: None,
            video_track: None,
        }),
    )
    .await;
    until("the room closed", || !hub.has_host(&room2)).await;
    let s = pad_status().await;
    assert!(s["internet"].is_null());
    assert_eq!(s["internet_invites"], false);
    let r = net_link(false).await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(body(r).await["error"], "internet_off");

    close_link().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The engine for the phone page's Playwright test (client/e2e/phone-pad-internet.spec.ts), not a
/// test of its own: it runs only when the spec starts it with `PS5UPLOAD_E2E_HUB` (the real hub
/// on this computer). The phone link's real routes (`pad_link::router`) on 127.0.0.1, a pretend
/// session (no PS5), no STUN and no router mapping, ICE on this computer's real interfaces.
/// `POST /e2e/stream` with `[kind, key, ...data]` hands the session's stream a message,
/// `GET /e2e/pad` answers the console's pad now, `POST /e2e/rumble` with `{"rumble": [l, r]}`
/// tells every phone, `POST /e2e/stop` ends it. It prints `E2E_ENGINE=http://127.0.0.1:<port>`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "the Playwright spec's engine; run by client/e2e/phone-pad-internet.spec.ts"]
async fn e2e_pad_internet_fixture() {
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
    let live = Arc::new(test_live::Session::start("10.77.0.61"));
    let stop = Arc::new(tokio::sync::Notify::new());

    use axum::routing::{get, post};
    let (l, l2, s) = (live.clone(), live.clone(), stop.clone());
    let app = crate::pad_link::router::<()>()
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
            "/e2e/pad",
            get(move || {
                let m = l2.merged();
                async move {
                    Json(serde_json::json!({
                        "buttons": m.buttons,
                        "touch": m.touch,
                        "motion": m.motion.is_some(),
                    }))
                }
            }),
        )
        .route(
            "/e2e/rumble",
            post(|b: axum::body::Bytes| async move {
                crate::pad_link::broadcast(&String::from_utf8_lossy(&b));
                StatusCode::NO_CONTENT
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
    println!("E2E_ENGINE=http://127.0.0.1:{port}");
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
    close_link().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}
