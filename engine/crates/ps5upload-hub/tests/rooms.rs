//! Play Rooms over real sockets: cards (create, update, renew, delist, expiry, limits,
//! validation), the cached list (paging, sorts, filters, CORS), the friends lookup, relayed
//! previews, knocks, kill switches, the run-time blocklist, cut-back mode, limits that leave
//! people alone and slow robots down, and privacy (no address in any response or log).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ps5upload_hub::geo::Geo;
use ps5upload_hub::hub::room_for_secret;
use ps5upload_hub::rooms::solve_pow;
use ps5upload_hub::{Config, Hub, LogSink};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);
const SITE: &str = "https://ps5upload.phantomptr.com";

struct T {
    hub: Hub,
    logs: Arc<Mutex<Vec<String>>>,
}

/// Rooms test settings: a trusted local proxy (so `x-forwarded-for` names the client), a cheap
/// proof of work and a fast tick.
fn cfg() -> Config {
    let mut c = Config {
        http_listen: "127.0.0.1:0".parse().unwrap(),
        stun_listen: Vec::new(),
        trusted_proxies: vec!["127.0.0.1".parse().unwrap()],
        ..Config::default()
    };
    c.rooms.pow_bits = 8;
    c.rooms.tick = Duration::from_millis(50);
    c
}

async fn start(c: Config) -> T {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let l = logs.clone();
    let sink: LogSink = Arc::new(move |line: &str| l.lock().unwrap().push(line.to_string()));
    T {
        hub: Hub::start(c, sink).await.expect("hub starts"),
        logs,
    }
}

fn secret(n: u32) -> String {
    format!("secret{n:0>37}")
}

/// Client `n`'s address (a documentation range).
fn ip(n: u32) -> String {
    format!("203.0.{}.{}", 113 + n / 250, 1 + n % 250)
}

async fn ws(t: &T, xff: &str) -> Ws {
    let mut req = format!("ws://{}/ws", t.hub.http_addr)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("x-forwarded-for", xff.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("a message in time")
            .expect("open")
            .expect("no error");
        match m {
            Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected text, got {other:?}"),
        }
    }
}

async fn recv_close(ws: &mut Ws) -> Option<u16> {
    loop {
        match tokio::time::timeout(WAIT, ws.next()).await.expect("closes") {
            Some(Ok(Message::Close(f))) => return f.map(|f| u16::from(f.code)),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return None,
        }
    }
}

async fn quiet(ws: &mut Ws, d: Duration) {
    let end = tokio::time::Instant::now() + d;
    loop {
        match tokio::time::timeout_at(end, ws.next()).await {
            Err(_) => return,
            Ok(Some(Ok(Message::Ping(_) | Message::Pong(_)))) => continue,
            Ok(other) => panic!("expected nothing, got {other:?}"),
        }
    }
}

/// A lobby host: returns its socket, room id and proof-of-work nonce.
async fn lobby(t: &T, n: u32, xff: &str) -> (Ws, String, String) {
    let s = secret(n);
    let r = room_for_secret(&s);
    let mut h = ws(t, xff).await;
    send(
        &mut h,
        json!({"v":1,"t":"host","room":r,"secret":s,"lobby":true}),
    )
    .await;
    let m = recv(&mut h).await;
    assert_eq!(m["t"], "hosted");
    assert_eq!(m["room"], r);
    assert_eq!(m["lobby"], true);
    assert_eq!(
        m["caps"],
        json!(["ads", "cid", "knock", "preview", "status"])
    );
    let nonce = m["ad_pow"]["n"].as_str().unwrap().to_string();
    assert_eq!(m["ad_pow"]["bits"], 8);
    (h, r, nonce)
}

fn card(name: &str) -> Value {
    json!({"listed": true, "name": name, "host": "Sam", "tag": "4F2A",
           "game": {"title": "It Takes Two", "id": "PPSA01234"},
           "proto": 2, "ver": "13.0.0", "caps": ["web", "ps5app", "preview"],
           "slots": [1, 4], "pw": false, "lang": "en", "region": "eu-west",
           "pk": "B".repeat(44)})
}

/// Creates a card; returns the hub's answer.
async fn create(h: &mut Ws, nonce: &str, c: Value) -> Value {
    send(h, json!({"t":"ad","ad":c,"pow":solve_pow(nonce, 8)})).await;
    recv(h).await
}

/// A listed lobby with a card.
async fn listed(t: &T, n: u32, xff: &str, c: Value) -> (Ws, String) {
    let (mut h, r, nonce) = lobby(t, n, xff).await;
    let a = create(&mut h, &nonce, c).await;
    assert_eq!(a["t"], "ad_ok", "{a}");
    (h, r)
}

struct Http {
    status: u16,
    head: String,
    body: String,
}

impl Http {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|_| panic!("json: {}", self.body))
    }
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().to_string())
        })
    }
}

async fn get_at(addr: SocketAddr, path: &str, extra: &[(&str, &str)]) -> Http {
    request(addr, "GET", path, extra, "").await
}

async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    extra: &[(&str, &str)],
    body: &str,
) -> Http {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n");
    for (k, v) in extra {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("content-length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    tokio::time::timeout(WAIT, s.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    Http {
        status,
        head: head.to_string(),
        body: body.to_string(),
    }
}

async fn get(t: &T, path: &str, xff: &str) -> Http {
    get_at(t.hub.http_addr, path, &[("x-forwarded-for", xff)]).await
}

fn metric(text: &str, line: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix(line)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or_else(|| panic!("{line} in metrics"))
}

async fn eventually(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("never happened");
}

/// No IPv4 or IPv6 address anywhere in `text`.
fn no_address(text: &str) {
    assert!(
        !ps5upload_hub::rooms::has_address(text),
        "an address in: {text}"
    );
    assert!(!text.contains("203.0."), "{text}");
    assert!(!text.contains("127.0.0.1"), "{text}");
}

#[tokio::test]
async fn a_card_is_created_listed_updated_renewed_and_delisted() {
    let mut c = cfg();
    c.rooms.list_cache = Duration::from_millis(200);
    let t = start(c).await;
    let (mut h, r, nonce) = lobby(&t, 1, &ip(1)).await;
    // No or a wrong proof of work.
    send(&mut h, json!({"t":"ad","ad":card("Couch chaos")})).await;
    assert_eq!(recv(&mut h).await, json!({"t":"ad_refused","why":"pow"}));
    send(
        &mut h,
        json!({"t":"ad","ad":card("Couch chaos"),"pow":"nope"}),
    )
    .await;
    assert_eq!(recv(&mut h).await, json!({"t":"ad_refused","why":"pow"}));
    let a = create(&mut h, &nonce, card("Couch chaos")).await;
    assert_eq!(a["t"], "ad_ok");
    let until = a["until"].as_u64().unwrap();
    let list = get(&t, "/rooms", &ip(50)).await;
    assert_eq!(list.status, 200);
    no_address(&list.body);
    let v = list.json();
    assert_eq!(v["proto"], json!({"min": 2, "latest": 2}));
    assert_eq!(v["next"], Value::Null);
    let room = &v["rooms"][0];
    assert_eq!(room["room"], r);
    assert_eq!(room["name"], "Couch chaos");
    assert_eq!(room["host"], "Sam");
    assert_eq!(room["tag"], "4F2A");
    assert_eq!(
        room["game"],
        json!({"title": "It Takes Two", "id": "PPSA01234"})
    );
    assert_eq!(room["slots"], json!([1, 4]));
    assert_eq!(room["region"], "eu-west");
    assert!(
        room.get("pk").is_none(),
        "the key is in the friends lookup, not the list"
    );
    // Update: renews too.
    let mut c2 = card("Couch chaos");
    c2["slots"] = json!([3, 4]);
    send(&mut h, json!({"t":"ad","ad":c2})).await;
    assert_eq!(recv(&mut h).await, json!({"t":"ad_ok","until":until}));
    send(&mut h, json!({"t":"ad"})).await;
    assert_eq!(recv(&mut h).await, json!({"t":"ad_ok","until":until}));
    tokio::time::sleep(Duration::from_millis(250)).await;
    let v = get(&t, "/rooms", &ip(50)).await.json();
    assert_eq!(v["rooms"][0]["slots"], json!([3, 4]));
    // A bad update keeps the card as it was.
    let mut bad = card("x");
    bad["slots"] = json!([1, 4]);
    send(&mut h, json!({"t":"ad","ad":bad})).await;
    assert_eq!(recv(&mut h).await, json!({"t":"ad_refused","why":"name"}));
    // Delist: a new nonce for the next create.
    send(&mut h, json!({"t":"unad"})).await;
    let g = recv(&mut h).await;
    assert_eq!(g["t"], "ad_gone");
    assert_eq!(g["why"], "unad");
    let n2 = g["ad_pow"]["n"].as_str().unwrap().to_string();
    assert_ne!(n2, nonce, "a nonce is spent by its create");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(get(&t, "/rooms", &ip(50)).await.json()["rooms"], json!([]));
    // The spent nonce does not create again; the new one does.
    send(
        &mut h,
        json!({"t":"ad","ad":card("Couch chaos"),"pow":solve_pow(&nonce, 8)}),
    )
    .await;
    let again = recv(&mut h).await;
    assert!(again["t"] == "ad_ok" || again == json!({"t":"ad_refused","why":"pow"}));
    if again["t"] != "ad_ok" {
        assert_eq!(create(&mut h, &n2, card("Couch chaos")).await["t"], "ad_ok");
    }
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_ads_listed"), 1);
    assert_eq!(metric(&m, "hub_ads_total{kind=\"created\"}"), 2);
    assert_eq!(metric(&m, "hub_ads_removed_total{why=\"unad\"}"), 1);
}

#[tokio::test]
async fn a_card_lives_only_while_its_socket_does() {
    let t = start(cfg()).await;
    let (h, r) = listed(&t, 1, &ip(1), card("Couch chaos")).await;
    let st = get(&t, &format!("/rooms/status?r={r}"), &ip(9))
        .await
        .json();
    assert!(st["rooms"][&r].is_object());
    drop(h);
    eventually(|| metric(&t.hub.shared.metrics_text(), "hub_ads_listed") == 0).await;
    let st = get(&t, &format!("/rooms/status?r={r}"), &ip(9))
        .await
        .json();
    assert_eq!(st["rooms"], json!({}));
    assert_eq!(
        metric(
            &t.hub.shared.metrics_text(),
            "hub_ads_removed_total{why=\"closed\"}"
        ),
        1
    );
}

#[tokio::test]
async fn cards_expire_unrenewed_and_after_their_lifetime() {
    let mut c = cfg();
    c.rooms.ad_renew = Duration::from_millis(300);
    let t = start(c).await;
    let (mut h, _) = listed(&t, 1, &ip(1), card("Stale room")).await;
    let g = recv(&mut h).await;
    assert_eq!(g["t"], "ad_gone");
    assert_eq!(g["why"], "stale");
    // A renewed card stays until its lifetime ends.
    let mut c = cfg();
    c.rooms.ad_max_age = Duration::from_millis(600);
    c.rooms.ad_renew = Duration::from_millis(400);
    let t = start(c).await;
    let (mut h, _) = listed(&t, 1, &ip(1), card("Old room")).await;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        send(&mut h, json!({"t":"ad"})).await;
        assert_eq!(recv(&mut h).await["t"], "ad_ok");
    }
    let g = loop {
        let m = recv(&mut h).await;
        if m["t"] == "ad_gone" {
            break m;
        }
    };
    assert_eq!(g["why"], "expired");
    assert!(g["ad_pow"]["n"].is_string());
}

#[tokio::test]
async fn limits_per_socket_ip_hub_and_creates() {
    let mut c = cfg();
    c.rooms.max_ads = 3;
    c.rooms.creates_per_hour = 3;
    let t = start(c).await;
    // Invite rooms carry no cards.
    let s = secret(90);
    let r = room_for_secret(&s);
    let mut plain = ws(&t, &ip(90)).await;
    send(&mut plain, json!({"v":1,"t":"host","room":r,"secret":s})).await;
    assert_eq!(recv(&mut plain).await, json!({"t":"hosted","room":r}));
    send(
        &mut plain,
        json!({"t":"ad","ad":card("Plain room"),"pow":"0"}),
    )
    .await;
    assert_eq!(
        recv(&mut plain).await,
        json!({"t":"ad_refused","why":"no_lobby"})
    );
    // A lobby must be a bound id.
    let mut odd = ws(&t, &ip(91)).await;
    send(
        &mut odd,
        json!({"v":1,"t":"host","room":"A".repeat(22),"secret":secret(91),"lobby":true}),
    )
    .await;
    assert_eq!(recv(&mut odd).await["why"], "bad_room");
    // Two cards per client.
    let _a = listed(&t, 1, &ip(1), card("Room one")).await;
    let _b = listed(&t, 2, &ip(1), card("Room two")).await;
    let (mut h3, _, n3) = lobby(&t, 3, &ip(1)).await;
    assert_eq!(
        create(&mut h3, &n3, card("Room three")).await["why"],
        "limit"
    );
    // The hub's cap refuses, never evicts.
    let _c = listed(&t, 4, &ip(2), card("Room four")).await;
    let (mut h5, _, n5) = lobby(&t, 5, &ip(3)).await;
    assert_eq!(create(&mut h5, &n5, card("Room five")).await["why"], "busy");
    assert_eq!(metric(&t.hub.shared.metrics_text(), "hub_ads_listed"), 3);
    // Creates per client per hour: create, delist, again...
    let mut c = cfg();
    c.rooms.creates_per_hour = 2;
    let t = start(c).await;
    let (mut h, _, mut nonce) = lobby(&t, 1, &ip(1)).await;
    for i in 0..3 {
        let a = create(&mut h, &nonce, card("Again room")).await;
        if i == 2 {
            assert_eq!(a, json!({"t":"ad_refused","why":"limit"}));
            break;
        }
        assert_eq!(a["t"], "ad_ok");
        send(&mut h, json!({"t":"unad"})).await;
        nonce = recv(&mut h).await["ad_pow"]["n"]
            .as_str()
            .unwrap()
            .to_string();
    }
    // Updates per card: a burst, then `rate`, the card stays.
    let t = start(cfg()).await;
    let (mut h, _) = listed(&t, 1, &ip(1), card("Busy room")).await;
    let mut rate = 0;
    for _ in 0..10 {
        send(&mut h, json!({"t":"ad"})).await;
        let a = recv(&mut h).await;
        if a["why"] == "rate" {
            rate += 1;
        }
    }
    assert!(rate > 0);
}

#[tokio::test]
async fn bad_cards_are_refused_with_their_reason() {
    let t = start(cfg()).await;
    let (mut h, _, nonce) = lobby(&t, 1, &ip(1)).await;
    let pow = solve_pow(&nonce, 8);
    let mut cases = Vec::new();
    for (k, v, why) in [
        ("name", json!("Join discord now"), "name"),
        ("name", json!("Admin"), "name"),
        ("host", json!("ps5upload team"), "name"),
        ("game", json!({"title": "x", "id": "bad"}), "game"),
        ("proto", json!(1), "old"),
        ("slots", json!([9, 4]), "bad"),
    ] {
        let mut c = card("Fine room");
        c[k] = v;
        cases.push((c, why));
    }
    let mut huge = card("Fine room");
    huge["caps"] = json!(vec!["x".repeat(16); 16]);
    huge["pad"] = json!("y".repeat(900));
    cases.push((huge, "bad"));
    for (c, why) in cases {
        send(&mut h, json!({"t":"ad","ad":c,"pow":pow})).await;
        assert_eq!(
            recv(&mut h).await,
            json!({"t":"ad_refused","why":why}),
            "{c}"
        );
    }
    // Ads from a guest socket are a protocol error.
    let mut g = ws(&t, &ip(2)).await;
    send(&mut g, json!({"t":"ad","ad":card("x")})).await;
    assert_eq!(recv(&mut g).await["why"], "bad_state");
}

#[tokio::test]
async fn the_list_is_cached_paged_by_bytes_sorted_and_filtered() {
    let mut c = cfg();
    c.rooms.max_ads_per_ip = 100;
    c.rooms.creates_per_hour = 100;
    c.rooms.list_cache = Duration::from_millis(1000);
    let t = start(c).await;
    let mut hosts = Vec::new();
    for i in 0..30u32 {
        let mut c = card(&format!("Room {i}"));
        c["slots"] = json!([1 + i % 4, 4]);
        c["pw"] = json!(i % 3 == 0);
        c["proto"] = json!(if i % 10 == 0 { 5 } else { 2 });
        if i % 5 == 0 {
            c["game"] = json!({"title": "Astro Bot", "id": "PPSA05555"});
        }
        // Long enough names that a page is cut by bytes before items.
        c["name"] = json!(format!("Room number {i:02} long name"));
        hosts.push(listed(&t, i, &ip(i), c).await);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let first = get(&t, "/rooms", &ip(200)).await;
    assert!(first.body.len() <= 2048 + 200, "{} bytes", first.body.len());
    no_address(&first.body);
    let v = first.json();
    let n = v["rooms"].as_array().unwrap().len();
    assert!((1..=12).contains(&n), "{n}");
    // Walk every page: each room once.
    let mut seen = std::collections::HashSet::new();
    let mut page = Value::from(0);
    while let Some(p) = page.as_u64() {
        let v = get(&t, &format!("/rooms?page={p}"), &ip(200)).await.json();
        for r in v["rooms"].as_array().unwrap() {
            assert!(seen.insert(r["room"].as_str().unwrap().to_string()));
        }
        page = v["next"].clone();
    }
    assert_eq!(seen.len(), 30);
    // The same query again is a hit; random extra keys share it.
    let _ = get(&t, "/rooms", &ip(200)).await;
    let m0 = t.hub.shared.metrics_text();
    let _ = get(&t, "/rooms?junk=1", &ip(201)).await;
    let _ = get(&t, "/rooms?x=2&y=3", &ip(202)).await;
    let m1 = t.hub.shared.metrics_text();
    let hits = |m: &str| metric(m, "hub_rooms_list_total{result=\"hit\"}");
    assert_eq!(hits(&m1), hits(&m0) + 2);
    // Filters.
    let rooms = |v: &Value| v["rooms"].as_array().unwrap().clone();
    let all = |q: &'static str| {
        let t = &t;
        async move {
            let mut out = Vec::new();
            let mut page = Value::from(0);
            while let Some(p) = page.as_u64() {
                let v = get(t, &format!("/rooms?{q}&page={p}"), &ip(203))
                    .await
                    .json();
                out.extend(rooms(&v));
                page = v["next"].clone();
            }
            out
        }
    };
    let astro = all("game=PPSA05555").await;
    assert_eq!(astro.len(), 6);
    assert_eq!(all("game=astro").await.len(), 6);
    assert!(all("free=1")
        .await
        .iter()
        .all(|r| r["slots"][0].as_u64() < r["slots"][1].as_u64()));
    assert!(all("nopw=1").await.iter().all(|r| r["pw"] == false));
    // Works with my version: protocol 2 viewers see 2 and (2 +- 2) but not 5.
    let compat = all("proto=2").await;
    assert_eq!(compat.len(), 27);
    assert!(compat.iter().all(|r| r["proto"] == 2));
    assert_eq!(all("lang=en").await.len(), 30);
    assert_eq!(all("lang=de").await.len(), 0);
    // Sorts.
    let players = rooms(&get(&t, "/rooms?sort=players", &ip(204)).await.json());
    let p: Vec<u64> = players
        .iter()
        .map(|r| r["slots"][0].as_u64().unwrap())
        .collect();
    assert!(p.windows(2).all(|w| w[0] >= w[1]), "{p:?}");
    let space = rooms(&get(&t, "/rooms?sort=space", &ip(204)).await.json());
    let s: Vec<u64> = space
        .iter()
        .map(|r| r["slots"][1].as_u64().unwrap() - r["slots"][0].as_u64().unwrap())
        .collect();
    assert!(s.windows(2).all(|w| w[0] >= w[1]), "{s:?}");
    let newest = rooms(&get(&t, "/rooms?sort=newest", &ip(204)).await.json());
    assert_eq!(newest[0]["name"], "Room number 29 long name");
    let games = rooms(&get(&t, "/rooms?sort=game", &ip(204)).await.json());
    assert_eq!(games[0]["game"]["title"], "Astro Bot");
    // Locked and friends-only cards are not listed.
    let (h0, _) = &mut hosts[1];
    let mut c = card("Room number 01 long name");
    c["locked"] = json!(true);
    send(h0, json!({"t":"ad","ad":c})).await;
    assert_eq!(recv(h0).await["t"], "ad_ok");
    let (h2, _) = &mut hosts[2];
    let mut c = card("Room number 02 long name");
    c["listed"] = json!(false);
    send(h2, json!({"t":"ad","ad":c})).await;
    assert_eq!(recv(h2).await["t"], "ad_ok");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(all("sort=newest").await.len(), 28);
}

#[tokio::test]
async fn regions_estimates_and_the_viewer_come_from_countries_never_addresses() {
    let t = start(cfg()).await;
    let (g, _) = Geo::parse("203.0.113.0,203.0.113.255,DE\n198.51.100.0,198.51.100.255,JP\n");
    t.hub.shared.set_geo(g);
    let mut c = card("Berlin room");
    c["region"] = json!("auto");
    let (_h1, _) = listed(&t, 1, "203.0.113.5", c).await;
    let mut c = card("Hidden room");
    c["region"] = json!("hidden");
    let (_h2, _) = listed(&t, 2, "203.0.113.6", c).await;
    let v = get(&t, "/rooms?sort=latency", "198.51.100.7").await;
    no_address(&v.body);
    let v = v.json();
    assert_eq!(v["you"], json!({"country": "JP", "region": "asia-east"}));
    let rooms = v["rooms"].as_array().unwrap();
    let berlin = rooms.iter().find(|r| r["name"] == "Berlin room").unwrap();
    assert_eq!(berlin["region"], "eu-west");
    assert_eq!(berlin["est_ms"], 160);
    let hidden = rooms.iter().find(|r| r["name"] == "Hidden room").unwrap();
    assert!(hidden.get("region").is_none() && hidden.get("est_ms").is_none());
    // Lowest latency first; unknown last.
    assert_eq!(rooms[0]["name"], "Berlin room");
    // A viewer's own region refines its country's, and "near me" filters on it.
    let v = get(&t, "/rooms?me=eu-east&near=1", "198.51.100.7")
        .await
        .json();
    assert_eq!(
        v["you"]["region"], "asia-east",
        "another continent: the country wins"
    );
    assert_eq!(v["rooms"], json!([]));
    let v = get(&t, "/rooms?near=1", "203.0.113.200").await.json();
    assert_eq!(v["rooms"].as_array().unwrap().len(), 1);
    assert_eq!(v["rooms"][0]["est_ms"], 30);
}

#[tokio::test]
async fn friends_lookup_answers_only_for_ids_with_a_card() {
    let t = start(cfg()).await;
    let mut c = card("Friends room");
    c["listed"] = json!(false);
    c["pw"] = json!(true);
    let (_h, r) = listed(&t, 1, &ip(1), c).await;
    // Not in the public list.
    assert_eq!(get(&t, "/rooms", &ip(9)).await.json()["rooms"], json!([]));
    let other = room_for_secret(&secret(77));
    let v = get(&t, &format!("/rooms/status?r={r},{other},nonsense"), &ip(9)).await;
    no_address(&v.body);
    let v = v.json();
    assert_eq!(v["every_s"], 60);
    assert_eq!(v["rooms"].as_object().unwrap().len(), 1);
    let e = &v["rooms"][&r];
    assert_eq!(e["listed"], false);
    assert_eq!(e["pw"], true);
    assert_eq!(e["locked"], false);
    assert_eq!(e["slots"], json!([1, 4]));
    assert_eq!(e["pk"], "B".repeat(44));
    // At most 16 ids are looked at.
    let many: Vec<String> = (0..40).map(|_| r.clone()).collect();
    let v = get(&t, &format!("/rooms/status?r={}", many.join(",")), &ip(9)).await;
    assert_eq!(v.status, 200);
}

#[tokio::test]
async fn previews_are_relayed_once_per_room_and_shared() {
    let mut c = cfg();
    c.rooms.preview_wait = Duration::from_millis(300);
    c.rooms.preview_ttl = Duration::from_millis(600);
    let t = start(c).await;
    let (mut h, r) = listed(&t, 1, &ip(1), card("Preview room")).await;
    let addr = t.hub.http_addr;
    let path = format!("/rooms/{r}/preview");
    let p2 = path.clone();
    let first =
        tokio::spawn(
            async move { get_at(addr, &p2, &[("x-forwarded-for", "198.51.100.1")]).await },
        );
    let ask = recv(&mut h).await;
    assert_eq!(ask["t"], "preview");
    let pid = ask["pid"].as_str().unwrap().to_string();
    let doc =
        json!({"proto": 2, "players": [{"name": "Sam", "seat": 1}], "stream": {"res": "1080p"}});
    send(&mut h, json!({"t":"preview","pid":pid,"data":doc})).await;
    let a = first.await.unwrap();
    assert_eq!(a.status, 200);
    assert_eq!(a.json(), doc);
    // Others within the time get the same answer; the host is not asked again.
    for i in 0..5 {
        let b = get(&t, &path, &format!("198.51.100.{}", 10 + i)).await;
        assert_eq!(b.status, 200);
        assert_eq!(b.json(), doc);
    }
    quiet(&mut h, Duration::from_millis(100)).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // No answer: 504, and that too is shared for the time.
    let late = get(&t, &path, "198.51.100.30").await;
    assert_eq!(late.status, 504);
    assert_eq!(recv(&mut h).await["t"], "preview");
    assert_eq!(get(&t, &path, "198.51.100.31").await.status, 504);
    quiet(&mut h, Duration::from_millis(100)).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    // An answer holding an address is refused (502), never served.
    let addr2 = t.hub.http_addr;
    let p3 = path.clone();
    let asked =
        tokio::spawn(
            async move { get_at(addr2, &p3, &[("x-forwarded-for", "198.51.100.40")]).await },
        );
    let pid = recv(&mut h).await["pid"].as_str().unwrap().to_string();
    send(
        &mut h,
        json!({"t":"preview","pid":pid,"data":{"host":"192.0.2.7"}}),
    )
    .await;
    let refused = asked.await.unwrap();
    assert_eq!(refused.status, 502);
    no_address(&refused.body);
    // Unknown rooms: 404 without bothering anyone.
    let none = get(
        &t,
        &format!("/rooms/{}/preview", room_for_secret(&secret(5))),
        "198.51.100.50",
    )
    .await;
    assert_eq!(none.status, 404);
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_previews_total{result=\"relayed\"}"), 3);
}

/// A guest joins a lobby, knocks (only knocks until let in), gets one pending knock, a few
/// tries, and after the host's "ok" its offer goes through.
#[tokio::test]
async fn knocks_are_gated_counted_and_opaque() {
    let t = start(cfg()).await;
    let (mut h, r) = listed(&t, 1, &ip(1), card("Knock room")).await;
    let mut g = ws(&t, &ip(2)).await;
    send(&mut g, json!({"v":1,"t":"join","room":r})).await;
    let gid = recv(&mut g).await["gid"].as_str().unwrap().to_string();
    let note = recv(&mut h).await;
    assert_eq!(note["gid"], gid);
    let cid = note["cid"].as_str().unwrap().to_string();
    assert_eq!(cid.len(), 8);
    no_address(&note.to_string());
    // An offer before approval is not forwarded.
    send(
        &mut g,
        json!({"t":"signal","to":"host","data":{"sdp":{"type":"offer","sdp":"v=0"}}}),
    )
    .await;
    assert_eq!(
        recv(&mut g).await,
        json!({"t":"error","why":"not_approved"})
    );
    quiet(&mut h, Duration::from_millis(50)).await;
    // A knock goes through as it came (the hub does not read the sealed part).
    let knock = json!({"knock": {"via": "rooms", "name": "Mia", "device": "x", "ver": "13.0.0"}});
    send(&mut g, json!({"t":"signal","to":"host","data":knock})).await;
    let fwd = recv(&mut h).await;
    assert_eq!(fwd, json!({"t":"signal","from":gid,"data":knock}));
    // One pending knock per guest.
    send(&mut g, json!({"t":"signal","to":"host","data":knock})).await;
    assert_eq!(
        recv(&mut g).await,
        json!({"t":"error","why":"knock_pending"})
    );
    // Challenge, sealed answer (opaque, size-capped), then "no".
    send(
        &mut h,
        json!({"t":"signal","to":gid,"data":{"knock":"challenge","c":"abc"}}),
    )
    .await;
    assert_eq!(recv(&mut g).await["data"]["knock"], "challenge");
    let sealed = json!({"knock":"sealed","blob":"Q".repeat(600)});
    send(&mut g, json!({"t":"signal","to":"host","data":sealed})).await;
    assert_eq!(recv(&mut h).await["data"], sealed);
    send(
        &mut h,
        json!({"t":"signal","to":gid,"data":{"knock":"no","why":"password"}}),
    )
    .await;
    assert_eq!(recv(&mut g).await["data"]["knock"], "no");
    // Retrying a password a few times is fine...
    let mut slowed = None;
    for i in 0..6 {
        send(&mut g, json!({"t":"signal","to":"host","data":knock})).await;
        // Forwarded (nothing back to the guest) or slowed down (an answer, socket open).
        if let Ok(Some(Ok(Message::Text(t)))) =
            tokio::time::timeout(Duration::from_millis(200), g.next()).await
        {
            slowed = Some((i, serde_json::from_str::<Value>(t.as_str()).unwrap()));
            break;
        }
        assert_eq!(recv(&mut h).await["from"], gid);
        send(&mut h, json!({"t":"signal","to":gid,"data":{"knock":"no"}})).await;
        assert_eq!(recv(&mut g).await["data"]["knock"], "no");
    }
    // ... and only past five tries in a row is the guest asked to wait (not closed).
    let (i, m) = slowed.expect("slowed down");
    assert_eq!(i, 4, "four more after the first (5 in all)");
    assert_eq!(m["why"], "rate_limited");
    assert!(m["retry_s"].as_u64().unwrap() >= 1);
    // A second guest from elsewhere is let in; its offer then goes through.
    let mut g2 = ws(&t, &ip(3)).await;
    send(&mut g2, json!({"v":1,"t":"join","room":r})).await;
    let gid2 = recv(&mut g2).await["gid"].as_str().unwrap().to_string();
    let _ = recv(&mut h).await;
    send(&mut g2, json!({"t":"signal","to":"host","data":knock})).await;
    let _ = recv(&mut h).await;
    send(
        &mut h,
        json!({"t":"signal","to":gid2,"data":{"knock":"ok"}}),
    )
    .await;
    assert_eq!(recv(&mut g2).await["data"]["knock"], "ok");
    let offer = json!({"sdp":{"type":"offer","sdp":"v=0"}});
    send(&mut g2, json!({"t":"signal","to":"host","data":offer})).await;
    assert_eq!(recv(&mut h).await["data"], offer);
    // A knock too large before approval is a protocol error.
    let mut g3 = ws(&t, &ip(4)).await;
    send(&mut g3, json!({"v":1,"t":"join","room":r})).await;
    let _ = recv(&mut g3).await;
    let _ = recv(&mut h).await;
    let big = json!({"knock": {"pad": "z".repeat(3000)}});
    send(&mut g3, json!({"t":"signal","to":"host","data":big})).await;
    assert_eq!(recv(&mut g3).await["why"], "bad_data");
    assert_eq!(recv_close(&mut g3).await, Some(4000));
    // Lobbies take more guests than invite rooms (16).
    let m = t.hub.shared.metrics_text();
    assert!(metric(&m, "hub_knocks_total{result=\"forwarded\"}") >= 6);
    assert_eq!(metric(&m, "hub_knocks_total{result=\"pending\"}"), 1);
    assert_eq!(metric(&m, "hub_knocks_total{result=\"client\"}"), 1);
}

#[tokio::test]
async fn kill_switches_turn_features_off() {
    let mut c = cfg();
    c.rooms.enabled = false;
    c.rooms.knocks = true;
    let t = start(c).await;
    let v = get(&t, "/rooms", &ip(1)).await.json();
    assert_eq!(v["off"], true);
    assert_eq!(v["rooms"], json!([]));
    assert_eq!(v["proto"]["min"], 2);
    let (mut h, _, n) = lobby(&t, 1, &ip(1)).await;
    assert_eq!(create(&mut h, &n, card("Off room")).await["why"], "off");
    assert_eq!(
        get(&t, "/rooms/status?r=x", &ip(1)).await.json()["off"],
        true
    );
    // Knocks off: no lobbies (invite rooms work).
    let mut c = cfg();
    c.rooms.knocks = false;
    let t = start(c).await;
    let s = secret(1);
    let mut h = ws(&t, &ip(1)).await;
    send(
        &mut h,
        json!({"v":1,"t":"host","room":room_for_secret(&s),"secret":s,"lobby":true}),
    )
    .await;
    assert_eq!(recv(&mut h).await, json!({"t":"error","why":"no_lobby"}));
    assert_eq!(recv_close(&mut h).await, Some(1000));
    // Previews off.
    let mut c = cfg();
    c.rooms.previews = false;
    let t = start(c).await;
    let (_h, r) = listed(&t, 1, &ip(1), card("No preview")).await;
    assert_eq!(
        get(&t, &format!("/rooms/{r}/preview"), &ip(2)).await.status,
        503
    );
}

#[tokio::test]
async fn the_blocklist_and_switches_reload_without_a_restart() {
    let dir = std::env::temp_dir().join(format!("hub-control-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("control");
    std::fs::write(&file, "# nothing yet\n").unwrap();
    let mut c = cfg();
    c.rooms.control_file = Some(file.clone());
    c.rooms.admin_token = Some("t".repeat(20));
    c.metrics_listen = Some("127.0.0.1:0".parse().unwrap());
    let t = start(c).await;
    let mut tagged = card("Tagged room");
    tagged["tag"] = json!("BEEF");
    let (mut h1, _) = listed(&t, 1, &ip(1), tagged).await;
    let (mut h2, _) = listed(&t, 2, "198.51.100.9", card("Other room")).await;
    // File: block a tag and an address block.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    std::fs::write(&file, "tag beef\nip 198.51.100.0/24\nbogus line\n").unwrap();
    let g = recv(&mut h1).await;
    assert_eq!(
        (g["t"].as_str(), g["why"].as_str()),
        (Some("ad_gone"), Some("blocked"))
    );
    let g = recv(&mut h2).await;
    assert_eq!(g["why"], "blocked");
    assert_eq!(get(&t, "/rooms", "198.51.100.20").await.status, 403);
    assert_eq!(get(&t, "/rooms", &ip(5)).await.status, 200);
    // New cards with that tag are refused.
    let mut again = card("Tagged again");
    again["tag"] = json!("beef");
    let (mut h3, _, n3) = lobby(&t, 3, &ip(3)).await;
    assert_eq!(create(&mut h3, &n3, again).await["why"], "blocked");
    assert!(t
        .logs
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("hub: control from=file")
            && l.contains("tags=1")
            && l.contains("bad_lines=1")));
    // Admin: only with the token, only on the private listener.
    let admin = t.hub.metrics_addr.unwrap();
    let r = request(admin, "POST", "/admin/control", &[], "rooms off").await;
    assert_eq!(r.status, 404);
    let pub_r = request(
        t.hub.http_addr,
        "POST",
        "/admin/control",
        &[("authorization", &format!("Bearer {}", "t".repeat(20)))],
        "rooms off",
    )
    .await;
    assert_eq!(pub_r.status, 404, "never on the public port");
    let (mut h4, _) = listed(&t, 4, &ip(4), card("Fourth room")).await;
    let auth = format!("Bearer {}", "t".repeat(20));
    let r = request(
        admin,
        "POST",
        "/admin/control",
        &[("authorization", &auth)],
        "rooms off\nmin_proto 2",
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["rooms"], false);
    assert_eq!(recv(&mut h4).await["why"], "off");
    assert_eq!(get(&t, "/rooms", &ip(5)).await.json()["off"], true);
    // An empty body clears the run-time overlay.
    let r = request(
        admin,
        "POST",
        "/admin/control",
        &[("authorization", &auth)],
        "",
    )
    .await;
    assert_eq!(r.json()["rooms"], true);
    let r = get_at(admin, "/admin/control", &[("authorization", &auth)]).await;
    assert_eq!(r.json()["overlay"], false);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn cut_back_mode_starts_on_memory_or_traffic_and_ends() {
    let mut c = cfg();
    c.rooms.max_ads_per_ip = 20;
    c.rooms.creates_per_hour = 20;
    let t = start(c).await;
    let mut hosts = Vec::new();
    for i in 0..10 {
        hosts.push(listed(&t, i, &ip(i), card(&format!("Room {i:02} here"))).await);
    }
    let (_, r) = &hosts[0];
    let r = r.clone();
    t.hub.shared.rooms.set_resident_for_test(205 << 20);
    eventually(|| metric(&t.hub.shared.metrics_text(), "hub_cutback") == 1).await;
    assert!(t
        .logs
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.starts_with("hub: cutback on=1 why=memory")));
    // Smaller pages, previews off, new cards refused, existing ones kept.
    let v = get(&t, "/rooms", &ip(9)).await.json();
    assert_eq!(v["cutback"], true);
    assert!(v["rooms"].as_array().unwrap().len() <= 6);
    assert_eq!(
        get(&t, &format!("/rooms/{r}/preview"), &ip(9)).await.status,
        503
    );
    assert_eq!(
        get(&t, &format!("/rooms/status?r={r}"), &ip(9))
            .await
            .json()["every_s"],
        300
    );
    let (mut h, _, n) = lobby(&t, 50, &ip(2)).await;
    assert_eq!(create(&mut h, &n, card("Late room")).await["why"], "busy");
    assert_eq!(metric(&t.hub.shared.metrics_text(), "hub_ads_listed"), 10);
    // Past the refuse line new sockets get 503.
    t.hub.shared.rooms.set_resident_for_test(230 << 20);
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut req = format!("ws://{}/ws", t.hub.http_addr)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("x-forwarded-for", ip(3).parse().unwrap());
    assert!(tokio_tungstenite::connect_async(req).await.is_err());
    // Back under: off again.
    t.hub.shared.rooms.set_resident_for_test(50 << 20);
    eventually(|| metric(&t.hub.shared.metrics_text(), "hub_cutback") == 0).await;
    // Traffic: a budget this small is passed by a few lists.
    let mut c = cfg();
    c.rooms.out_budget_month = 1_000_000;
    let t = start(c).await;
    for _ in 0..3 {
        let _ = get(&t, "/rooms", &ip(9)).await;
    }
    eventually(|| metric(&t.hub.shared.metrics_text(), "hub_cutback") == 1).await;
    assert!(t
        .logs
        .lock()
        .unwrap()
        .iter()
        .any(|l| l.contains("why=traffic")));
}

#[tokio::test]
async fn cors_allows_only_our_origins() {
    let t = start(cfg()).await;
    let (_h, r) = listed(&t, 1, &ip(1), card("Cors room")).await;
    for path in ["/rooms".to_string(), format!("/rooms/status?r={r}")] {
        for origin in [SITE, "tauri://localhost", "http://tauri.localhost"] {
            let a = get_at(
                t.hub.http_addr,
                &path,
                &[("origin", origin), ("x-forwarded-for", &ip(5))],
            )
            .await;
            assert_eq!(a.status, 200, "{origin}");
            assert_eq!(
                a.header("access-control-allow-origin").as_deref(),
                Some(origin)
            );
            assert!(a.header("vary").unwrap().contains("origin"));
        }
        for bad in [
            "https://evil.example",
            "null",
            "https://ps5upload.phantomptr.com.evil.example",
        ] {
            let a = get_at(
                t.hub.http_addr,
                &path,
                &[("origin", bad), ("x-forwarded-for", &ip(5))],
            )
            .await;
            assert_eq!(a.status, 403, "{bad}");
            assert!(a.header("access-control-allow-origin").is_none());
        }
        // An app's own client (no Origin): answered, no CORS header.
        let a = get(&t, &path, &ip(5)).await;
        assert_eq!(a.status, 200);
        assert!(a.header("access-control-allow-origin").is_none());
    }
    // Previews too.
    let a = get_at(
        t.hub.http_addr,
        &format!("/rooms/{r}/preview"),
        &[("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(a.status, 403);
}

/// What people do never meets a limit: open the room list, pull to refresh a few times, page
/// through, open ten rooms' details, check friends, knock and retry a password.
#[tokio::test]
async fn a_person_never_meets_a_limit() {
    let mut c = cfg();
    c.rooms.max_ads_per_ip = 20;
    c.rooms.creates_per_hour = 20;
    let t = start(c).await;
    let mut hosts = Vec::new();
    let mut ids = Vec::new();
    for i in 0..10 {
        let (h, r) = listed(&t, i, &ip(i), card(&format!("Room {i:02} here"))).await;
        hosts.push(h);
        ids.push(r);
    }
    let me = "198.51.100.77";
    // Mashing refresh: 20 in a row.
    for _ in 0..20 {
        assert_eq!(get(&t, "/rooms", me).await.status, 200);
    }
    // Ten rooms' details (hosts answer).
    let mut answers = Vec::new();
    for (i, r) in ids.iter().enumerate() {
        let addr = t.hub.http_addr;
        let path = format!("/rooms/{r}/preview");
        let me = me.to_string();
        answers.push(tokio::spawn(async move {
            get_at(addr, &path, &[("x-forwarded-for", &me)]).await
        }));
        let h = &mut hosts[i];
        let pid = recv(h).await["pid"].as_str().unwrap().to_string();
        send(h, json!({"t":"preview","pid":pid,"data":{"ok":1}})).await;
    }
    for a in answers {
        assert_eq!(a.await.unwrap().status, 200);
    }
    for _ in 0..5 {
        assert_eq!(
            get(&t, &format!("/rooms/status?r={}", ids.join(",")), me)
                .await
                .status,
            200
        );
    }
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_throttled_total{class=\"list\"}"), 0);
    assert_eq!(metric(&m, "hub_throttled_total{class=\"preview\"}"), 0);
    assert_eq!(metric(&m, "hub_throttled_total{class=\"status\"}"), 0);
    assert_eq!(metric(&m, "hub_clients_blocked"), 0);
}

/// A script hammering the list is slowed down with Retry-After, everyone else is unaffected, and
/// the flood is served from the cache (no rebuilds).
#[tokio::test]
async fn a_robot_is_slowed_down_and_others_are_not() {
    let t = start(cfg()).await;
    let bot = "198.51.100.66";
    let mut limited = 0;
    let mut retry = None;
    for _ in 0..80 {
        let r = get(&t, "/rooms", bot).await;
        if r.status == 429 {
            limited += 1;
            retry = r.header("retry-after");
            assert_eq!(r.json()["why"], "slow_down");
        }
    }
    assert!(limited >= 40, "{limited}");
    assert!(retry.unwrap().parse::<u64>().unwrap() >= 1);
    assert_eq!(get(&t, "/rooms", "198.51.100.67").await.status, 200);
    let m = t.hub.shared.metrics_text();
    assert!(metric(&m, "hub_rooms_list_total{result=\"miss\"}") <= 2);
    // A socket flooding messages is closed (no app sends 100 at once).
    let s = secret(8);
    let mut w = ws(&t, &ip(8)).await;
    send(
        &mut w,
        json!({"v":1,"t":"host","room":room_for_secret(&s),"secret":s}),
    )
    .await;
    for _ in 0..150 {
        let _ = w.send(Message::text("{\"t\":\"ad\"}")).await;
    }
    let mut closed = false;
    while let Ok(Some(Ok(m))) = tokio::time::timeout(WAIT, w.next()).await {
        if let Message::Close(f) = m {
            closed = f.is_some();
            break;
        }
    }
    assert!(closed);
}

#[tokio::test]
async fn logs_and_metrics_never_hold_addresses_or_names() {
    let t = start(cfg()).await;
    let (mut h, r) = listed(&t, 1, "203.0.113.99", card("Secret name room")).await;
    let _ = get(&t, "/rooms", "198.51.100.5").await;
    let _ = get(&t, &format!("/rooms/status?r={r}"), "198.51.100.5").await;
    send(&mut h, json!({"t":"unad"})).await;
    let _ = recv(&mut h).await;
    let mut all = t.logs.lock().unwrap().join("\n");
    all.push_str(&t.hub.shared.metrics_text());
    for bad in ["203.0.113", "198.51.100", "Secret name", "Sam", &r[..10]] {
        assert!(!all.contains(bad), "{bad} in logs/metrics");
    }
}
