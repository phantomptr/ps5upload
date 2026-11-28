//! join_friends.rs: passes, their limits, the file, the host routes and the hand-off over a real
//! hub. The whole "play again" ask runs in join.rs's tests (`friends_play_again`).

use super::*;
use std::net::SocketAddr;

fn temp_dir(tag: &str) -> PathBuf {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).unwrap();
    let d = std::env::temp_dir().join(format!(
        "ps5u-friends-{tag}-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn room(c: char) -> String {
    let mut r: String = std::iter::repeat_n(c, 21).collect();
    r.push('h');
    r
}

#[test]
fn a_pass_asks_a_few_times_per_gap_and_unknown_ones_are_refused() {
    let mut s = Store::open(None);
    let t0 = Instant::now();
    s.issue("a1", "pass-one", "192.168.1.50", "Sam", None, 1_000);
    let k = s.again("pass-one", None, t0, 1_100).unwrap();
    assert_eq!(k.console, host_key("192.168.1.50"));
    assert_eq!(k.name, "Sam");
    assert_eq!(s.friends[0].last_unix, 1_100);
    // A retry at once (Join as "Sam (2)", a double press) is a person: it goes through...
    for _ in 1..AGAIN_BURST {
        assert!(s.again("pass-one", None, t0, 1_101).is_ok());
    }
    // ...more within the gap is not.
    assert_eq!(
        s.again("pass-one", None, t0, 1_101),
        Err(AgainErr::SlowDown)
    );
    // After the gap: fine.
    assert!(s.again("pass-one", None, t0 + AGAIN_GAP, 1_102).is_ok());
    assert_eq!(s.again("pass-two", None, t0, 1_100), Err(AgainErr::Unknown));
    assert_eq!(s.again("", None, t0, 1_100), Err(AgainErr::Unknown));
    assert_eq!(
        s.again(&"x".repeat(500), None, t0, 1_100),
        Err(AgainErr::Unknown)
    );
}

#[test]
fn passes_are_kept_as_hashes_in_a_private_file_and_forgotten_when_unused() {
    let d = temp_dir("file");
    let path = d.join(FILE);
    let mut s = Store::open(Some(path.clone()));
    s.issue(
        "a1",
        "secret-pass-value",
        "192.168.1.50",
        "Sam",
        None,
        1_000,
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("secret-pass-value"), "{text}");
    assert!(text.contains(&sha256_hex("secret-pass-value")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // Read back.
    let mut s2 = Store::open(Some(path.clone()));
    assert!(s2.find("secret-pass-value").is_some());
    // Unused for longer than UNUSED_FOR: gone.
    let later = 1_000 + UNUSED_FOR.as_secs() + 1;
    assert_eq!(
        s2.again("secret-pass-value", None, Instant::now(), later),
        Err(AgainErr::Unknown)
    );
    assert!(Store::open(Some(path)).friends.is_empty());
    // A file that doesn't read is set aside, not written over.
    let bad = d.join("bad.json");
    std::fs::write(&bad, "{not json").unwrap();
    assert!(Store::open(Some(bad.clone())).friends.is_empty());
    assert!(bad.with_extension("bad").exists());
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_list_is_capped_and_one_app_room_keeps_one_pass() {
    let mut s = Store::open(None);
    for i in 0..(MAX_FRIENDS + 5) {
        s.issue(
            &format!("{i:08x}"),
            &format!("p{i}"),
            "192.168.1.50",
            "X",
            None,
            1_000 + i as u64,
        );
    }
    assert_eq!(s.friends.len(), MAX_FRIENDS);
    // The oldest went.
    assert!(s.find("p0").is_none());
    assert!(s.find(&format!("p{}", MAX_FRIENDS + 4)).is_some());

    // The same app (its waiting room) joining again replaces its pass.
    let mut s = Store::open(None);
    let inbox = Some((room('A'), "hub.example".to_string()));
    s.issue("a1", "old", "192.168.1.50", "Sam", inbox.clone(), 1_000);
    s.issue("a2", "new", "192.168.1.50", "Sam", inbox.clone(), 1_001);
    assert_eq!(s.friends.len(), 1);
    assert!(s.find("old").is_none());
    assert_eq!(s.friends[0].inbox_room, Some(room('A')));
    // On another console it's another friend.
    s.issue("a3", "other", "192.168.1.51", "Sam", inbox, 1_002);
    assert_eq!(s.friends.len(), 2);
}

#[test]
fn only_a_bound_room_on_our_own_hub_is_kept() {
    let ok = json!({"room": room('A'), "hub": "hub.example"});
    assert_eq!(
        inbox_on(Some(&ok), Some("hub.example")),
        Some((room('A'), "hub.example".to_string()))
    );
    for bad in [
        json!({"room": room('A'), "hub": "evil.example"}),
        json!({"room": "short", "hub": "hub.example"}),
        json!({"room": format!("{}/", &room('A')[..21]), "hub": "hub.example"}),
        json!({"room": room('A')}),
        json!("x"),
    ] {
        assert_eq!(inbox_on(Some(&bad), Some("hub.example")), None, "{bad}");
    }
    assert_eq!(inbox_on(None, Some("hub.example")), None);
    assert_eq!(inbox_on(Some(&ok), None), None);
}

fn peer(ip: &str) -> ConnectInfo<SocketAddr> {
    ConnectInfo(format!("{ip}:5555").parse().unwrap())
}

/// What this computer's own page sends.
fn here() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("host", "127.0.0.1:19113".parse().unwrap());
    h
}

async fn body(r: Response) -> (StatusCode, Value) {
    let st = r.status();
    let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_routes_are_the_hosts_only_and_never_show_a_pass() {
    let pass = issue("192.168.1.77", "Robin", None).unwrap();
    assert_eq!(pass.len(), 43);
    // A peer on the LAN is refused.
    let r = list(peer("192.168.1.30"), HeaderMap::new()).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = forget_route(
        peer("192.168.1.30"),
        HeaderMap::new(),
        Json(IdBody { id: "x".into() }),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let (st, v) = body(list(peer("127.0.0.1"), here()).await).await;
    assert_eq!(st, StatusCode::OK);
    let text = v.to_string();
    assert!(!text.contains(&pass));
    assert!(!text.contains(&sha256_hex(&pass)));
    let me = v["friends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Robin")
        .unwrap()
        .clone();
    assert_eq!(me["can_invite"], false);
    // No waiting room: nothing to send to.
    let (st, v) = body(
        send(
            peer("127.0.0.1"),
            here(),
            Json(SendBody {
                id: me["id"].as_str().unwrap().into(),
                addr: None,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(v["error"], "no_inbox");
    let (_, v) = body(
        forget_route(
            peer("127.0.0.1"),
            here(),
            Json(IdBody {
                id: me["id"].as_str().unwrap().into(),
            }),
        )
        .await,
    )
    .await;
    assert_eq!(v["ok"], true);
    assert_eq!(
        again(&pass, None, Instant::now()),
        Err(AgainErr::Unknown),
        "a forgotten pass stops working"
    );
}

/// The app's side of the hand-off: hosts `room` on the hub at `url` and answers one invite.
async fn fake_app(url: String, room: String, secret: String, answer: Value) -> Value {
    let (mut ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap();
    ws.send(TMsg::Text(
        json!({"v": 1, "t": "host", "room": room, "secret": secret})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    loop {
        let Some(Ok(TMsg::Text(t))) = ws.next().await else {
            panic!("hub closed")
        };
        let m: Value = serde_json::from_str(t.as_str()).unwrap();
        if m["t"] == "signal" {
            let gid = m["from"].as_str().unwrap().to_string();
            ws.send(TMsg::Text(
                json!({"v": 1, "t": "signal", "to": gid, "data": answer})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            return m["data"].clone();
        }
    }
}

#[tokio::test]
async fn an_invite_is_handed_to_an_open_app_through_a_real_hub() {
    let config = ps5upload_hub::Config {
        http_listen: "127.0.0.1:0".parse().unwrap(),
        ..Default::default()
    };
    let hub = ps5upload_hub::Hub::start(config, std::sync::Arc::new(|_: &str| {}))
        .await
        .unwrap();
    let url = format!("ws://{}/ws", hub.http_addr);
    let secret = crate::join_net::random_b64(32).unwrap();
    let r = ps5upload_hub::hub::room_for_secret(&secret);

    // Nobody hosts the room: the app is closed.
    assert_eq!(
        hand_off(
            &url,
            &r,
            json!({"link": "http://192.168.1.2:19113/join#i=abcdefgh"})
        )
        .await,
        Err(SendErr::NotOpen)
    );

    let app = tokio::spawn(fake_app(
        url.clone(),
        r.clone(),
        secret.clone(),
        json!({"invite_ok": true}),
    ));
    // Give it a moment to host.
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if hand_off_probe(&url, &r).await {
            break;
        }
    }
    let invite = json!({"link": "http://192.168.1.2:19113/join#i=abcdefgh", "name": "Desk"});
    assert_eq!(hand_off(&url, &r, invite.clone()).await, Ok(()));
    assert_eq!(app.await.unwrap(), json!({"invite": invite}));

    // An app that says no.
    let app = tokio::spawn(fake_app(
        url.clone(),
        r.clone(),
        secret,
        json!({"invite_ok": false, "why": "busy"}),
    ));
    let mut got = Err(SendErr::NotOpen);
    for _ in 0..100 {
        got = hand_off(&url, &r, invite.clone()).await;
        if got != Err(SendErr::NotOpen) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(got, Err(SendErr::Refused("busy".into())));
    let _ = app.await;
    hub.stop(Duration::from_secs(1)).await;
}

/// Whether `room` is hosted (a join that isn't refused with no_room).
async fn hand_off_probe(url: &str, room: &str) -> bool {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws.send(TMsg::Text(
        json!({"v": 1, "t": "join", "room": room})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let ok = matches!(ws.next().await, Some(Ok(TMsg::Text(t))) if t.as_str().contains("joined"));
    let _ = ws
        .send(TMsg::Text(json!({"t": "leave"}).to_string().into()))
        .await;
    ok
}
