// join.rs's tests for Play Rooms (included into `join::tests::rooms`, for its fakes): the room's
// settings and rules over the real guest socket, host controls, the preview, and the lobby on a
// real hub (knock, password sealed to the room key, the host's answer, the card and the relayed
// preview).

use super::*;
use crate::join_room as jr;

async fn open_room(console: &str) -> serde_json::Value {
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

async fn set_room(console: &str, set: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let (ci, h) = local();
    let mut set = set;
    set["addr"] = console.into();
    let b: room_api::RoomBody = serde_json::from_value(set).unwrap();
    let r = room(ci, h, Json(b)).await;
    let code = r.status();
    (code, body(r).await)
}

fn fresh_settings() {
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let _ = jr::set_password(None);
    jr::test_clear_blocks();
}

/// The room's settings, size, lock, unique names and the refusal words, over the guest socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn room_settings_size_lock_and_names() {
    let _turn = PARTY_TESTS.lock().await;
    fresh_settings();
    let console = "10.77.0.41";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    let url = serve().await;
    set_live(console, 1);
    let s = open_room(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    assert_eq!(s["room"]["visibility"], "private");
    assert_eq!(s["room"]["size"], 4);
    assert_eq!(s["room"]["proto"], jr::PROTO);

    // A name the rules refuse, a size out of range: refused, nothing applied.
    let (code, v) = set_room(console, serde_json::json!({"name": "PS5Upload night"})).await;
    assert_eq!((code, v["error"].as_str()), (StatusCode::CONFLICT, Some("name")));
    let (code, _) = set_room(console, serde_json::json!({"size": 9})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let (code, v) = set_room(console, serde_json::json!({"password": "abc"})).await;
    assert_eq!(
        (code, v["error"].as_str()),
        (StatusCode::CONFLICT, Some("password_short"))
    );
    // Room size 2: the host and one friend.
    let (code, v) = set_room(console, serde_json::json!({"size": 2, "name": "Couch chaos"})).await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(v["room"]["size"], 2);
    assert_eq!(v["room"]["name"], "Couch chaos");

    let mut sam = guest(
        &url,
        serde_json::json!({"hello": {"invite": key, "name": "Sam", "proto": 2}}),
    )
    .await;
    assert_eq!(next(&mut sam).await["stage"], "asking");
    allow_next(console, true).await;
    assert_eq!(next(&mut sam).await["stage"], "joined");

    // Full: the room says how many may play and how many are in.
    let mut kim = guest(
        &url,
        serde_json::json!({"hello": {"invite": key, "name": "Kim", "proto": 2}}),
    )
    .await;
    let v = next(&mut kim).await;
    assert_eq!(
        (&v["why"], &v["allowed"], &v["in"]),
        (&"full".into(), &1.into(), &1.into()),
        "{v}"
    );
    // A size below the people in is refused with the count.
    let (code, v) = set_room(console, serde_json::json!({"size": 1})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{v}");
    let (code, _) = set_room(console, serde_json::json!({"size": 4})).await;
    assert_eq!(code, StatusCode::OK);

    // A look-alike of Sam's name: a rooms-aware guest is offered "(2)"; an older one plays as it.
    let mut sam2 = guest(
        &url,
        serde_json::json!({"hello": {"invite": key, "name": "SΑM", "proto": 2}}),
    )
    .await;
    let v = next(&mut sam2).await;
    assert_eq!(
        (&v["why"], &v["suggest"]),
        (&"name_taken".into(), &"SΑM (2)".into()),
        "{v}"
    );
    let mut old = guest(
        &url,
        serde_json::json!({"hello": {"invite": key, "name": "sam"}}),
    )
    .await;
    assert_eq!(next(&mut old).await["stage"], "asking");
    let s = host_state(console).await;
    assert_eq!(s["pending"][0]["name"], "sam (2)", "{s}");
    assert_eq!(s["pending"][0]["came"], "invite");
    allow_next(console, false).await;
    assert_eq!(next(&mut old).await["why"], "denied");

    // Locked: every way in, invites included.
    let (code, v) = set_room(console, serde_json::json!({"lock": true})).await;
    assert_eq!((code, &v["room"]["lock"]), (StatusCode::OK, &true.into()));
    let mut c = guest(
        &url,
        serde_json::json!({"hello": {"invite": key, "name": "Lee", "proto": 2}}),
    )
    .await;
    assert_eq!(next(&mut c).await["why"], "locked");
    set_room(console, serde_json::json!({"lock": false})).await;

    // A wrong code (typed) vs a lapsed link.
    let mut w = guest(
        &url,
        serde_json::json!({"hello": {"code": "ZZZZZZ", "name": "W", "proto": 2}}),
    )
    .await;
    assert_eq!(next(&mut w).await["why"], "code");

    // Close asks first while friends are in, and names them.
    let (ci, h) = local();
    let r = close(
        ci,
        h,
        Query(Default::default()),
        Json(CloseBody {
            addr: console.into(),
            confirm_friends: false,
        }),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let v = body(r).await;
    assert_eq!(
        (&v["error"], &v["friends"]),
        (&"friends_playing".into(), &serde_json::json!(["Sam"]))
    );
    let (ci, h) = local();
    let v = body(
        close(
            ci,
            h,
            Query(Default::default()),
            Json(CloseBody {
                addr: console.into(),
                confirm_friends: true,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(v["left"], serde_json::json!(["Sam"]));
    assert_eq!(v["open"], false);
    set_live(console, 0);
    fresh_settings();
}

/// Knocks from the home network: the password (wrong, right, lockout), the password approval,
/// a Rooms knock on a room that isn't Public, remove + block and unblock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn knocks_passwords_blocks() {
    let _turn = PARTY_TESTS.lock().await;
    fresh_settings();
    let console = "10.77.0.42";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    let url = serve().await;
    set_live(console, 1);
    open_room(console).await;
    let knock = |pw: Option<&str>, via: &str, device: &str| {
        let mut k = serde_json::json!({"via": via});
        if let Some(p) = pw {
            k["password"] = p.into();
        }
        serde_json::json!({"hello": {"name": "Jo", "proto": 2, "device": device, "knock": k}})
    };
    // Private: a knock from the public list is not for this room.
    let mut g = guest(&url, knock(None, "rooms", "devJo-0000000001")).await;
    assert_eq!(next(&mut g).await["why"], "not_listed");
    let (code, v) = set_room(
        console,
        serde_json::json!({"visibility": "public", "password": "couch42"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert_eq!(v["room"]["password"], true);
    // No password said: a challenge, answered sealed to the room key (a wrong one: tries left);
    // then the right one in the clear (the home network), and the host decides.
    let mut g = guest(&url, knock(None, "rooms", "devJo-0000000001")).await;
    let c = next(&mut g).await;
    assert_eq!(c["knock"], "challenge", "{c}");
    let blob = crate::room_seal::seal(
        c["pk"].as_str().unwrap(),
        "",
        c["c"].as_str().unwrap(),
        "nope",
    )
    .unwrap();
    g.send(TMsg::Text(
        serde_json::json!({"knock": "sealed", "blob": blob}).to_string().into(),
    ))
    .await
    .unwrap();
    let v = next(&mut g).await;
    assert_eq!((&v["why"], &v["tries_left"]), (&"password".into(), &4.into()), "{v}");
    let mut g = guest(&url, knock(Some("nope"), "rooms", "devJo-0000000001")).await;
    let v = next(&mut g).await;
    assert_eq!((&v["why"], &v["tries_left"]), (&"password".into(), &3.into()), "{v}");
    // Sealed right: asking (then given up, so the plain one below asks again).
    let mut g = guest(&url, knock(None, "rooms", "devJo-0000000001")).await;
    let c = next(&mut g).await;
    let blob = crate::room_seal::seal(
        c["pk"].as_str().unwrap(),
        "",
        c["c"].as_str().unwrap(),
        "couch42",
    )
    .unwrap();
    g.send(TMsg::Text(
        serde_json::json!({"knock": "sealed", "blob": blob}).to_string().into(),
    ))
    .await
    .unwrap();
    assert_eq!(next(&mut g).await["stage"], "asking");
    drop(g);
    until("the sealed ask given up", || {
        with_party(&host_key(console), |p| p.pending.is_empty()).unwrap_or(false)
    })
    .await;
    let mut g = guest(&url, knock(Some("couch42"), "rooms", "devJo-0000000001")).await;
    assert_eq!(next(&mut g).await["stage"], "asking");
    let s = host_state(console).await;
    assert_eq!(
        (&s["pending"][0]["came"], &s["pending"][0]["password_ok"], &s["pending"][0]["from_rooms"]),
        (&"rooms".into(), &true.into(), &true.into()),
        "{s}"
    );
    assert_eq!(s["pending"][0]["tag"].as_str().map(str::len), Some(4));
    // One ask per device at a time.
    let mut again = guest(&url, knock(Some("couch42"), "rooms", "devJo-0000000001")).await;
    assert_eq!(next(&mut again).await["why"], "already_asking");
    allow_next(console, true).await;
    let j = next(&mut g).await;
    assert_eq!(j["stage"], "joined", "{j}");
    assert_eq!(j["proto"], jr::PROTO);
    let player = j["player"].as_u64().unwrap() as u8;
    assert_ne!(player, 1, "a stranger never gets Player 1");
    let s = host_state(console).await;
    assert_eq!(s["guests"][0]["from_rooms"], true);

    // Remove and "Don't let them back in this room": the same device hears a plain no.
    let (ci, h) = local();
    let r = body(
        kick(
            ci,
            h,
            Json(KickBody {
                addr: console.into(),
                player,
                block: Some("room".into()),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(r["ok"], true);
    let v = next_with(&mut g, "bye").await;
    assert_eq!(v["bye"], "removed", "a rooms-aware page hears it its way");
    let mut back = guest(&url, knock(Some("couch42"), "rooms", "devJo-0000000001")).await;
    assert_eq!(next(&mut back).await["why"], "denied");
    let s = host_state(console).await;
    let tag = s["room"]["blocked"][0]["tag"].as_str().unwrap().to_string();
    // Unblock: they may ask again.
    let (ci, h) = local();
    let r = body(
        unblock(
            ci,
            h,
            Json(serde_json::from_value(serde_json::json!({"addr": console, "tag": tag})).unwrap()),
        )
        .await,
    )
    .await;
    assert_eq!(r["ok"], true);
    let mut back = guest(&url, knock(Some("couch42"), "rooms", "devJo-0000000001")).await;
    assert_eq!(next(&mut back).await["stage"], "asking");
    allow_next(console, false).await;

    // "The password lets people in without asking".
    let (code, _) = set_room(
        console,
        serde_json::json!({"approval": {"friends": true, "password": true}}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    let mut auto = guest(&url, knock(Some("couch42"), "rooms", "devMia-000000002")).await;
    assert_eq!(next(&mut auto).await["stage"], "joined");
    // Removing the password turns that approval off by itself.
    let (_, v) = set_room(console, serde_json::json!({"password": ""})).await;
    assert_eq!(
        (&v["room"]["password"], &v["room"]["approval"]["password"]),
        (&false.into(), &false.into())
    );
    test_close_party(console);
    set_live(console, 0);
    fresh_settings();
}

/// Friend passes (Play again) go only to people the host let in who are not strangers from the
/// public Rooms list, and a remove takes the pass back (13.0.0 audit).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn friend_passes_skip_strangers_and_end_with_a_remove() {
    let _turn = PARTY_TESTS.lock().await;
    fresh_settings();
    let console = "10.77.0.45";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    let url = serve().await;
    set_live(console, 1);
    let s = open_room(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    let (code, v) = set_room(console, serde_json::json!({"visibility": "public"})).await;
    assert_eq!(code, StatusCode::OK, "{v}");

    // A stranger from the public list asks for a pass: let in, but given none.
    let passes_before = crate::join_friends::count();
    let mut jo = guest(
        &url,
        serde_json::json!({"hello": {"name": "Jo", "proto": 2, "device": "devJo-0000000045",
            "remember": true, "knock": {"via": "rooms"}}}),
    )
    .await;
    assert_eq!(next(&mut jo).await["stage"], "asking");
    allow_next(console, true).await;
    let j = next(&mut jo).await;
    assert_eq!(j["stage"], "joined", "{j}");
    assert!(j["friend"].is_null(), "a stranger from Rooms gets no friend pass: {j}");
    assert_eq!(crate::join_friends::count(), passes_before);
    jo.send(TMsg::Text(r#"{"leave":1}"#.into())).await.unwrap();
    drop(jo);

    // Two people with the invite get one each.
    let mut passes = Vec::new();
    for (name, dev) in [("Sam", "devSam-000000045"), ("Kim", "devKim-000000045")] {
        let mut g = guest(
            &url,
            serde_json::json!({"hello": {"invite": key, "name": name, "proto": 2, "device": dev,
                "remember": true}}),
        )
        .await;
        assert_eq!(next(&mut g).await["stage"], "asking");
        allow_next(console, true).await;
        let j = next(&mut g).await;
        assert_eq!(j["stage"], "joined", "{j}");
        let pass = j["friend"].as_str().expect("a pass").to_string();
        passes.push((g, j["player"].as_u64().unwrap() as u8, pass));
    }
    // The host removes Sam: Sam's pass is taken back; Kim's stays.
    let (mut sam, sam_player, sam_pass) = passes.remove(0);
    let (mut kim, _, kim_pass) = passes.remove(0);
    assert!(room_api::kick_and_block(console, sam_player, None));
    assert_eq!(next_with(&mut sam, "bye").await["bye"], "removed");
    let mut back = guest(&url, serde_json::json!({"hello": {"again": sam_pass}})).await;
    assert_eq!(
        next(&mut back).await["why"],
        "friend",
        "a removed guest's pass asks no more"
    );
    kim.send(TMsg::Text(r#"{"leave":1}"#.into())).await.unwrap();
    drop(kim);
    let mut kim = guest(&url, serde_json::json!({"hello": {"again": kim_pass}})).await;
    let v = next(&mut kim).await;
    assert!(
        v["stage"] == "joined" || v["stage"] == "asking",
        "Kim's pass still works: {v}"
    );
    drop(kim);
    test_close_party(console);
    set_live(console, 0);
    fresh_settings();
}

/// Many controllers: after the first player of a PS5 leaves (`release`), their socket still
/// carries that PS5's chat and voice. It speaks as the player still in, so the host's per-player
/// mute reaches it (13.0.0 audit: it kept the old player number, which nobody could mute).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_released_first_players_talk_speaks_as_the_player_still_in() {
    let _turn = PARTY_TESTS.lock().await;
    fresh_settings();
    let console = "10.77.0.46";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    let url = serve().await;
    set_live(console, 1);
    let s = open_room(console).await;
    let key = s["invite"]["key"].as_str().unwrap().to_string();
    let dev = "devSam-000000046";
    let mut sam = guest(
        &url,
        serde_json::json!({"hello": {"invite": key, "name": "Sam", "proto": 2, "device": dev,
            "caps": ["chat", "voice"]}}),
    )
    .await;
    assert_eq!(next(&mut sam).await["stage"], "asking");
    allow_next(console, true).await;
    let j = next_with(&mut sam, "stage").await;
    assert_eq!(j["stage"], "joined", "{j}");
    let sam_token = j["token"].as_str().unwrap().to_string();
    let sam_player = j["player"].as_u64().unwrap();
    sam.send(TMsg::Text(r#"{"notice":1}"#.into())).await.unwrap();
    // Mia, a second controller on the same PS5 (no picture of her own).
    let mut mia = guest(
        &url,
        serde_json::json!({"hello": {"group": sam_token, "name": "Mia", "media": false,
            "proto": 2, "device": dev}}),
    )
    .await;
    assert_eq!(next_with(&mut mia, "stage").await["stage"], "asking");
    allow_next(console, true).await;
    let j = next_with(&mut mia, "stage").await;
    assert_eq!(j["stage"], "joined", "{j}");
    let mia_player = j["player"].as_u64().unwrap();
    assert_ne!(mia_player, sam_player);

    // Sam leaves; the PS5 talks on through Sam's socket.
    sam.send(TMsg::Text(r#"{"release":1}"#.into()))
        .await
        .unwrap();
    until("Sam's place freed", || {
        with_party(&host_key(console), |p| {
            p.guests.iter().filter(|g| g.active()).count() == 1
        })
        .unwrap_or(false)
    })
    .await;
    let line = |text: &str| {
        TMsg::Text(serde_json::json!({"chat": {"text": text}}).to_string().into())
    };
    let last_line = || async {
        let s = host_state(console).await;
        s["room"]["chat_log"]
            .as_array()
            .and_then(|a| a.last().cloned())
            .unwrap_or_default()
    };
    sam.send(line("still here")).await.unwrap();
    let mut got = serde_json::Value::Null;
    for _ in 0..200 {
        got = last_line().await;
        if got["text"] == "still here" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(got["text"], "still here", "{got}");
    assert_eq!(
        got["player"], mia_player,
        "the PS5's talk speaks as the player still in: {got}"
    );
    // The host mutes that player's chat: the socket's next line is refused.
    let n = crate::join_comms::mute(
        console,
        crate::join_comms::Target::Player(mia_player as u8),
        None,
        Some(true),
    )
    .unwrap();
    assert!(n >= 1);
    sam.send(line("can you hear me")).await.unwrap();
    let r = next_with(&mut sam, "comms_refused").await;
    assert_eq!(r["comms_refused"]["what"], "chat", "{r}");
    assert_eq!(last_line().await["text"], "still here");
    drop(mia);
    drop(sam);
    test_close_party(console);
    set_live(console, 0);
    fresh_settings();
}

/// The preview: what the host chose to show, never an address, and one per address per 10 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_preview_shows_the_room_and_no_address() {
    let _turn = PARTY_TESTS.lock().await;
    fresh_settings();
    let console = "10.77.0.43";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    set_live(console, 1);
    open_room(console).await;
    let peer = ConnectInfo("192.168.7.9:5000".parse::<std::net::SocketAddr>().unwrap());
    let r = preview(peer, Query(room_api::PreviewQuery::default())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let v = body(r).await;
    assert_eq!(v["proto"], jr::PROTO);
    assert_eq!(v["players"][0]["kind"], "host");
    assert!(
        !ps5upload_hub::rooms::has_address(&v.to_string()),
        "{v}"
    );
    let r = preview(peer, Query(room_api::PreviewQuery::default())).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key("retry-after"));
    test_close_party(console);
    set_live(console, 0);
    // No room: 404.
    let other = ConnectInfo("192.168.7.10:5000".parse::<std::net::SocketAddr>().unwrap());
    let r = preview(other, Query(room_api::PreviewQuery::default())).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

/// People from the public list always have PS & Create blocked once playing, whatever the switch.
#[test]
fn strangers_have_ps_and_create_blocked() {
    let mut p = Party::new(
        "10.77.0.44",
        1,
        "Desk".into(),
        Settings {
            block_system_buttons: false,
            ..Settings::default()
        },
    );
    let now = Instant::now();
    let id = p.admit("Jo", 2, now).unwrap().id;
    let g = p.guest_mut(id).unwrap();
    g.meta.via = Some(jr::Via::Rooms);
    g.playing = true;
    let (_, strip) = p.strip_mask(id).unwrap();
    assert_eq!(strip & vpad::button::SYSTEM, vpad::button::SYSTEM);
    let id2 = p.admit("Mia", 3, now).unwrap().id;
    p.guest_mut(id2).unwrap().playing = true;
    let (_, strip) = p.strip_mask(id2).unwrap();
    assert_eq!(strip & vpad::button::SYSTEM, 0, "an invited friend follows the switch");
}

/// A 13.0 helper's owner reading (`vpad.hb` `has_user`) marks a guest who picked a user.
#[tokio::test]
async fn the_helper_s_owner_reading_marks_a_pick() {
    let _turn = PARTY_TESTS.lock().await;
    let console = "10.77.0.45";
    let mut p = Party::new(console, 1, "Desk".into(), Settings::default());
    let id = p.admit("Jo", 2, Instant::now()).unwrap().id;
    with_parties(|m| m.insert(host_key(console), p));
    on_link_event(
        console,
        link::Event::Owner {
            slot: 2,
            user: Some(0x1234),
        },
    );
    let playing = with_party(&host_key(console), |p| {
        p.guests.iter().find(|g| g.id == id).map(|g| g.playing)
    })
    .flatten();
    assert_eq!(playing, Some(true));
    test_close_party(console);
}

/// The lobby on a real hub: the card (proof of work), a knock that needs the password sealed to
/// the room key, the host letting them in, the token, and the preview relayed by the hub.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lobby_lists_the_room_and_runs_a_sealed_knock() {
    use futures_util::{SinkExt, StreamExt};
    let _turn = PARTY_TESTS.lock().await;
    let mut cfg = ps5upload_hub::Config {
        http_listen: "127.0.0.1:0".parse().unwrap(),
        stun_listen: Vec::new(),
        ..ps5upload_hub::Config::default()
    };
    cfg.rooms.pow_bits = 8;
    let hub = ps5upload_hub::Hub::start(cfg, std::sync::Arc::new(|_: &str| {}))
        .await
        .unwrap();
    let hub_ws = format!("ws://{}/ws", hub.http_addr);
    let hub_http = format!("http://{}", hub.http_addr);
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub_ws.clone(),
        stun: Vec::new(),
        upnp: false,
        ..Settings::default()
    });
    let _ = jr::set_password(Some("couch42"));
    jr::test_clear_blocks();
    crate::join_net::lobby::TEST_LOBBY.store(true, Ordering::Relaxed);
    let console = "10.77.0.46";
    let f = Arc::new(FakeHelper::default());
    set_helper(console, f.clone());
    set_live(console, 1);
    open_room(console).await;
    let (code, v) = set_room(
        console,
        serde_json::json!({"visibility": "public", "name": "Couch chaos"}),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{v}");
    let lobby = crate::join_net::lobby::lobby_id().expect("the lobby runs");

    // The card shows in the hub's list.
    let get = |path: String| async move {
        tokio::task::spawn_blocking(move || {
            let cfg = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .proxy(None)
                .build();
            let mut r = ureq::Agent::new_with_config(cfg).get(&path).call().unwrap();
            let code = r.status().as_u16();
            let body = r.body_mut().read_to_string().unwrap();
            (code, body)
        })
        .await
        .unwrap()
    };
    let mut listed = false;
    for _ in 0..100 {
        let (_, b) = get(format!("{hub_http}/rooms")).await;
        if b.contains("Couch chaos") {
            listed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(listed, "the card reached the hub");
    until("listed", || {
        with_party(&host_key(console), |p| {
            matches!(p.room.listed, jr::Listed::Until(_))
        })
        .unwrap_or(false)
    })
    .await;
    // The hub relays a preview from the host engine.
    let (code, b) = get(format!("{hub_http}/rooms/{lobby}/preview")).await;
    assert_eq!(code, 200, "{b}");
    assert!(b.contains("Couch chaos"), "{b}");

    // A guest knocks: challenge, sealed password, the host's answer, the token.
    let (mut ws, _) = tokio_tungstenite::connect_async(&hub_ws).await.unwrap();
    let send = |v: serde_json::Value| TMsg::Text(v.to_string().into());
    ws.send(send(serde_json::json!({"v": 1, "t": "join", "room": lobby})))
        .await
        .unwrap();
    let mut gid = String::new();
    let read = async |ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >| loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("the hub answers")
            .unwrap()
            .unwrap();
        if let TMsg::Text(t) = m {
            return serde_json::from_str::<serde_json::Value>(t.as_str()).unwrap();
        }
    };
    let j = read(&mut ws).await;
    assert_eq!(j["t"], "joined", "{j}");
    gid.push_str(j["gid"].as_str().unwrap());
    ws.send(send(serde_json::json!({"v": 1, "t": "signal", "to": "host",
        "data": {"knock": {"via": "rooms", "name": "Jo", "proto": 2, "device": "devJo-0000000009"}}})))
        .await
        .unwrap();
    let c = read(&mut ws).await;
    assert_eq!(c["data"]["knock"], "challenge", "{c}");
    let blob = crate::room_seal::seal(
        c["data"]["pk"].as_str().unwrap(),
        &gid,
        c["data"]["c"].as_str().unwrap(),
        "couch42",
    )
    .unwrap();
    ws.send(send(serde_json::json!({"v": 1, "t": "signal", "to": "host",
        "data": {"knock": "sealed", "blob": blob}})))
        .await
        .unwrap();
    let w = read(&mut ws).await;
    assert_eq!(w["data"]["knock"], "wait", "{w}");
    let s = host_state(console).await;
    assert_eq!(s["pending"][0]["password_ok"], true, "{s}");
    allow_next(console, true).await;
    let ok = read(&mut ws).await;
    assert_eq!(ok["data"]["knock"], "ok", "{ok}");
    assert!(ok["data"]["token"].as_str().is_some_and(|t| !t.is_empty()), "{ok}");
    assert!(crate::join_net::lobby::may_offer(&lobby, &gid));

    // Private again: the card goes.
    set_room(console, serde_json::json!({"visibility": "private"})).await;
    let mut gone = false;
    for _ in 0..100 {
        let (_, b) = get(format!("{hub_http}/rooms")).await;
        if !b.contains("Couch chaos") {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "the card left the list");
    test_close_party(console);
    crate::join_net::lobby::changed();
    crate::join_net::lobby::TEST_LOBBY.store(false, Ordering::Relaxed);
    set_live(console, 0);
    fresh_settings();
}
