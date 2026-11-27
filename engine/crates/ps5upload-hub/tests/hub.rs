//! The hub over real sockets on ephemeral ports: protocol flows, every limit, bad input, STUN,
//! shutdown, and that logs never carry an address, a room id or a payload.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ps5upload_hub::config::Cidr;
use ps5upload_hub::stun;
use ps5upload_hub::{Config, Hub, LogSink};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WAIT: Duration = Duration::from_secs(5);

struct TestHub {
    hub: Hub,
    logs: Arc<Mutex<Vec<String>>>,
}

fn cfg() -> Config {
    Config {
        http_listen: "127.0.0.1:0".parse().unwrap(),
        stun_listen: Vec::new(),
        ..Config::default()
    }
}

async fn start(c: Config) -> TestHub {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let l = logs.clone();
    let sink: LogSink = Arc::new(move |line: &str| l.lock().unwrap().push(line.to_string()));
    let hub = Hub::start(c, sink).await.expect("hub starts");
    TestHub { hub, logs }
}

fn room(n: u32) -> String {
    format!("room{n:0>18}")
}

fn secret(n: u32) -> String {
    format!("secret{n:0>37}")
}

async fn connect_with(t: &TestHub, origin: Option<&str>, xff: Option<&str>) -> Ws {
    try_connect(t, origin, xff).await.expect("websocket opens")
}

async fn try_connect(
    t: &TestHub,
    origin: Option<&str>,
    xff: Option<&str>,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://{}/ws", t.hub.http_addr)
        .into_client_request()
        .unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("origin", o.parse().unwrap());
    }
    if let Some(x) = xff {
        req.headers_mut()
            .insert("x-forwarded-for", x.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

async fn connect(t: &TestHub) -> Ws {
    connect_with(t, None, None).await
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

/// The next text message (pings are answered by the client library while reading).
async fn recv(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("a message in time")
            .expect("socket open")
            .expect("no socket error");
        match m {
            Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("expected text, got {other:?}"),
        }
    }
}

/// Reads to the end; the close code, if the hub sent a close frame.
async fn recv_close(ws: &mut Ws) -> Option<u16> {
    loop {
        match tokio::time::timeout(WAIT, ws.next())
            .await
            .expect("closes in time")
        {
            Some(Ok(Message::Close(f))) => return f.map(|f| u16::from(f.code)),
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => return None,
        }
    }
}

/// Reads a WebSocket's bytes straight off its TCP socket to the end, never writing, and returns the
/// close frame's code among them. A peer read through the client library is not silent: it answers
/// every ping it reads, and a pong that reaches a socket the hub has already closed draws a reset,
/// after which Linux drops whatever was still unread, the close frame included.
async fn raw_close_code(mut ws: Ws) -> Option<u16> {
    let MaybeTlsStream::Plain(tcp) = ws.get_mut() else {
        unreachable!("ws:// is plain TCP")
    };
    let mut b = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        match tokio::time::timeout_at(end, tcp.read(&mut chunk))
            .await
            .expect("closes in time")
        {
            Ok(0) | Err(_) => break,
            Ok(n) => b.extend_from_slice(&chunk[..n]),
        }
    }
    // The hub's frames: unmasked, short (7-bit lengths). Pings until the close.
    let mut i = 0;
    while i + 2 <= b.len() {
        let (op, len) = (b[i] & 0x0f, usize::from(b[i + 1] & 0x7f));
        assert!(b[i + 1] & 0x80 == 0 && len < 126, "a short unmasked frame");
        let p = b.get(i + 2..i + 2 + len)?;
        if op == 0x8 {
            return (len >= 2).then(|| u16::from_be_bytes([p[0], p[1]]));
        }
        i += 2 + len;
    }
    None
}

/// Nothing arrives within `d` (other than pings).
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

async fn expect_error(ws: &mut Ws, why: &str) {
    assert_eq!(recv(ws).await, json!({"t": "error", "why": why}));
}

async fn expect_fatal(ws: &mut Ws, why: &str) {
    expect_error(ws, why).await;
    assert_eq!(recv_close(ws).await, Some(4000), "closes after {why}");
}

async fn host(t: &TestHub, r: &str, s: &str) -> Ws {
    let mut ws = connect(t).await;
    send(&mut ws, json!({"v":1,"t":"host","room":r,"secret":s})).await;
    assert_eq!(recv(&mut ws).await, json!({"t":"hosted","room":r}));
    ws
}

/// Joins; returns the guest socket and its gid (and checks the host heard about it).
async fn join(t: &TestHub, host_ws: &mut Ws, r: &str) -> (Ws, String) {
    join_from(t, host_ws, r, None).await
}

/// [`join`] as the client a trusted proxy names (`xff`).
async fn join_from(t: &TestHub, host_ws: &mut Ws, r: &str, xff: Option<&str>) -> (Ws, String) {
    let mut ws = connect_with(t, None, xff).await;
    send(&mut ws, json!({"v":1,"t":"join","room":r})).await;
    let j = recv(&mut ws).await;
    assert_eq!(j["t"], "joined", "{j}");
    let gid = j["gid"].as_str().unwrap().to_string();
    assert_eq!(gid.len(), 12);
    assert_eq!(recv(host_ws).await, json!({"t":"guest","gid":gid}));
    (ws, gid)
}

async fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    tokio::time::timeout(WAIT, s.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    out
}

fn metric(text: &str, name: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or_else(|| panic!("{name} in metrics"))
}

#[tokio::test]
async fn full_flow() {
    let t = start(cfg()).await;
    let r = room(1);
    let mut h = host(&t, &r, &secret(1)).await;
    let (mut g1, gid1) = join(&t, &mut h, &r).await;
    let (mut g2, gid2) = join(&t, &mut h, &r).await;
    assert_ne!(gid1, gid2);

    // Guest → host and host → guest, data untouched.
    let offer = json!({"type":"offer","sdp":"v=0\r\no=- 1 2 IN IP4 0.0.0.0\r\n"});
    send(&mut g1, json!({"t":"signal","to":"host","data":offer})).await;
    assert_eq!(
        recv(&mut h).await,
        json!({"t":"signal","from":gid1,"data":offer})
    );
    let answer = json!({"type":"answer","sdp":"x","nested":{"a":[1,2,3]}});
    send(&mut h, json!({"t":"signal","to":gid1,"data":answer})).await;
    assert_eq!(
        recv(&mut g1).await,
        json!({"t":"signal","from":"host","data":answer})
    );
    // g2 heard none of it.
    quiet(&mut g2, Duration::from_millis(100)).await;

    // A guest may only address the host; an unknown gid is a soft error.
    send(&mut h, json!({"t":"signal","to":"zzzzzzzzzzzz","data":{}})).await;
    expect_error(&mut h, "no_peer").await;

    // Guest disconnects: the host is told.
    drop(g1);
    assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid1}));
    // Guest leaves: the host is told and the guest's socket closes.
    send(&mut g2, json!({"t":"leave"})).await;
    assert_eq!(recv_close(&mut g2).await, Some(1000));
    assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid2}));

    // The host kicks a guest.
    let (mut g3, gid3) = join(&t, &mut h, &r).await;
    send(&mut h, json!({"t":"leave","gid":gid3})).await;
    assert_eq!(recv(&mut g3).await, json!({"t":"closed"}));
    assert_eq!(recv_close(&mut g3).await, Some(1000));

    // Host disconnects: every guest gets closed.
    let (mut g4, _) = join(&t, &mut h, &r).await;
    let (mut g5, _) = join(&t, &mut h, &r).await;
    drop(h);
    for g in [&mut g4, &mut g5] {
        assert_eq!(recv(g).await, json!({"t":"closed"}));
        assert_eq!(recv_close(g).await, Some(1000));
    }
    // The room waits for its host: joins get no_room meanwhile.
    let mut g6 = connect(&t).await;
    send(&mut g6, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut g6, "no_room").await;

    // Health and metrics.
    let health = http_get(t.hub.http_addr, "/health").await;
    assert!(health.starts_with("HTTP/1.1 200") && health.ends_with("ok\n"));
    let m = http_get(t.hub.http_addr, "/metrics").await;
    assert_eq!(metric(&m, "hub_rooms"), 1);
    assert_eq!(metric(&m, "hub_rooms_hosted"), 0);
    assert_eq!(metric(&m, "hub_guests"), 0);
    assert_eq!(metric(&m, "hub_hosts_total"), 1);
    assert_eq!(metric(&m, "hub_joins_total"), 5);
    assert_eq!(metric(&m, "hub_signals_total"), 2);
    assert_eq!(metric(&m, "hub_errors_total{kind=\"no_room\"}"), 1);
    assert_eq!(metric(&m, "hub_errors_total{kind=\"no_peer\"}"), 1);
    assert!(!m.contains("127.0.0.1") && !m.contains(&r));
}

#[tokio::test]
async fn host_leave_closes_room() {
    let t = start(cfg()).await;
    let r = room(2);
    let mut h = host(&t, &r, &secret(2)).await;
    let (mut g, _) = join(&t, &mut h, &r).await;
    send(&mut h, json!({"t":"leave"})).await;
    assert_eq!(recv_close(&mut h).await, Some(1000));
    assert_eq!(recv(&mut g).await, json!({"t":"closed"}));
    // Gone for good: anyone may open it again, with any secret.
    host(&t, &r, &secret(99)).await;
}

#[tokio::test]
async fn reclaim_and_room_taken() {
    let c = Config {
        room_idle_ttl: Duration::from_millis(300),
        sweep_interval: Duration::from_millis(50),
        ..cfg()
    };
    let t = start(c).await;
    let r = room(3);
    let mut a = host(&t, &r, &secret(3)).await;
    let (mut g, gid) = join(&t, &mut a, &r).await;

    // Wrong secret: refused, the room is untouched.
    let mut x = connect(&t).await;
    send(
        &mut x,
        json!({"v":1,"t":"host","room":r,"secret":secret(4)}),
    )
    .await;
    expect_fatal(&mut x, "room_taken").await;

    // Right secret from a new socket: the old host is closed, the new one learns the guest.
    let mut b = connect(&t).await;
    send(
        &mut b,
        json!({"v":1,"t":"host","room":r,"secret":secret(3)}),
    )
    .await;
    assert_eq!(recv(&mut b).await, json!({"t":"hosted","room":r}));
    assert_eq!(recv(&mut b).await, json!({"t":"guest","gid":gid}));
    assert_eq!(recv_close(&mut a).await, Some(4001));
    // The old socket's disconnect must not have closed the room or the guest.
    send(&mut g, json!({"t":"signal","to":"host","data":{"c":1}})).await;
    assert_eq!(
        recv(&mut b).await,
        json!({"t":"signal","from":gid,"data":{"c":1}})
    );

    // The host drops and comes back within the idle limit: same room.
    drop(b);
    assert_eq!(recv(&mut g).await, json!({"t":"closed"}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let c2 = host(&t, &r, &secret(3)).await;
    drop(c2);

    // Away past the limit: the room is removed and a different secret may take the id.
    tokio::time::sleep(Duration::from_millis(600)).await;
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_rooms"), 0);
    assert_eq!(metric(&m, "hub_rooms_expired_total"), 1);
    assert_eq!(metric(&m, "hub_reclaims_total"), 2);
    host(&t, &r, &secret(5)).await;
}

/// A room id made from its host's secret opens for that secret only: on the hub as it runs and on
/// a hub that has just restarted with no rooms at all (before, the first `host` after a restart
/// took any id with any secret, so a link holder could open the room as theirs).
#[tokio::test]
async fn a_bound_room_opens_for_its_own_secret_only_also_after_a_restart() {
    let s = secret(40);
    let r = ps5upload_hub::hub::room_for_secret(&s);
    assert!(ps5upload_hub::hub::is_bound_room(&r), "{r}");
    let t = start(cfg()).await;
    let mut h = host(&t, &r, &s).await;
    let (_g, _) = join(&t, &mut h, &r).await;
    t.hub.stop(Duration::from_secs(3)).await;
    assert_eq!(recv_close(&mut h).await, Some(1001));

    // The hub is back with nothing in memory. Someone with the link comes first: refused.
    let t = start(cfg()).await;
    let mut x = connect(&t).await;
    send(
        &mut x,
        json!({"v":1,"t":"host","room":r,"secret":secret(41)}),
    )
    .await;
    expect_fatal(&mut x, "room_taken").await;
    // The host comes back with its secret: the room is its again, and guests reach it.
    let mut h = host(&t, &r, &s).await;
    let (mut g, gid) = join(&t, &mut h, &r).await;
    send(&mut g, json!({"t":"signal","to":"host","data":{"o":1}})).await;
    assert_eq!(
        recv(&mut h).await,
        json!({"t":"signal","from":gid,"data":{"o":1}})
    );
    // Any id of the bound form needs the secret it was made from, room or no room.
    let other = ps5upload_hub::hub::room_for_secret(&secret(42));
    let mut y = connect(&t).await;
    send(
        &mut y,
        json!({"v":1,"t":"host","room":other,"secret":secret(43)}),
    )
    .await;
    expect_fatal(&mut y, "room_taken").await;
}

/// An older app's room id (16 random bytes, so it ends in A, Q, g or w) opens as before: with any
/// secret while the id is free, for that secret only while it is open.
#[tokio::test]
async fn an_older_apps_room_id_still_opens() {
    use base64::Engine as _;
    let t = start(cfg()).await;
    let mut open = Vec::new();
    for (n, last) in [(0u8, 'A'), (1, 'Q'), (2, 'g'), (3, 'w')] {
        let mut bytes = [7u8; 16];
        // The last character is the last byte's low 2 bits, then 4 zero bits.
        bytes[15] = 0x1c | n;
        let r = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        assert_eq!((r.len(), r.chars().last()), (22, Some(last)), "{r}");
        assert!(!ps5upload_hub::hub::is_bound_room(&r));
        let mut h = host(&t, &r, &secret(50 + u32::from(n))).await;
        let (_g, _) = join(&t, &mut h, &r).await;
        let mut x = connect(&t).await;
        send(
            &mut x,
            json!({"v":1,"t":"host","room":r,"secret":secret(99)}),
        )
        .await;
        expect_fatal(&mut x, "room_taken").await;
        open.push(h);
    }
}

/// A guest that joined and then sat in the room (it never left, as every client does once its
/// connection is up) is removed after the longest a join may take, and its seat is free again.
#[tokio::test]
async fn an_idle_guest_is_removed_after_its_time() {
    let t = start(Config {
        max_guests_per_room: 2,
        guest_max_age: Duration::from_millis(400),
        sweep_interval: Duration::from_millis(50),
        ..cfg()
    })
    .await;
    let r = room(45);
    let mut h = host(&t, &r, &secret(45)).await;
    let (mut g1, gid1) = join(&t, &mut h, &r).await;
    let (mut g2, gid2) = join(&t, &mut h, &r).await;
    let mut full = connect(&t).await;
    send(&mut full, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut full, "room_full").await;
    // Both idle guests go: each is told, and so is the host.
    for g in [&mut g1, &mut g2] {
        expect_fatal(g, "timeout").await;
    }
    let mut left = [recv(&mut h).await, recv(&mut h).await];
    left.sort_by_key(|v| v["gid"].as_str().unwrap_or("").to_string());
    let mut want = [
        json!({"t":"left","gid":gid1}),
        json!({"t":"left","gid":gid2}),
    ];
    want.sort_by_key(|v| v["gid"].as_str().unwrap_or("").to_string());
    assert_eq!(left, want);
    // The seats are free, and the host is still there.
    let (_g3, _) = join(&t, &mut h, &r).await;
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_guests_expired_total"), 2);
    assert_eq!(metric(&m, "hub_rooms_hosted"), 1);
}

#[tokio::test]
async fn limit_guests_per_room() {
    let t = start(Config {
        max_guests_per_room: 2,
        ..cfg()
    })
    .await;
    let r = room(10);
    let mut h = host(&t, &r, &secret(10)).await;
    let (_g1, _) = join(&t, &mut h, &r).await;
    let (g2, gid2) = join(&t, &mut h, &r).await;
    let mut g3 = connect(&t).await;
    send(&mut g3, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut g3, "room_full").await;
    // A seat frees up.
    drop(g2);
    assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid2}));
    join(&t, &mut h, &r).await;
}

#[tokio::test]
async fn limit_signals() {
    let t = start(Config {
        max_signals_per_guest: 3,
        max_signals_per_host: 4,
        ..cfg()
    })
    .await;
    let r = room(11);
    let mut h = host(&t, &r, &secret(11)).await;
    let (mut g, gid) = join(&t, &mut h, &r).await;
    for i in 0..3 {
        send(&mut g, json!({"t":"signal","to":"host","data":{"i":i}})).await;
        assert_eq!(recv(&mut h).await["data"]["i"], i);
    }
    send(&mut g, json!({"t":"signal","to":"host","data":{"i":3}})).await;
    expect_fatal(&mut g, "rate_limited").await;
    assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid}));

    let (mut g2, gid2) = join(&t, &mut h, &r).await;
    for i in 0..4 {
        send(&mut h, json!({"t":"signal","to":gid2,"data":{"i":i}})).await;
        assert_eq!(recv(&mut g2).await["data"]["i"], i);
    }
    // Over the host's limit: dropped, and the host stays with its guests (it was closed, and
    // its guests with it: guests can make a host send, so that was theirs to do).
    send(&mut h, json!({"t":"signal","to":gid2,"data":{"i":4}})).await;
    quiet(&mut g2, Duration::from_millis(200)).await;
    send(
        &mut g2,
        json!({"t":"signal","to":"host","data":{"still":1}}),
    )
    .await;
    assert_eq!(
        recv(&mut h).await,
        json!({"t":"signal","from":gid2,"data":{"still":1}})
    );
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_host_signals_dropped_total"), 1);
    assert_eq!(metric(&m, "hub_rooms_hosted"), 1);
}

/// A guest's budget is its client's in the room, and leaving and joining again does not refill
/// it (it did: each join came with 20 fresh signals, and a client may join 30 times a minute).
/// Another client has its own.
#[tokio::test]
async fn a_clients_signals_in_a_room_last_across_its_joins() {
    let t = start(Config {
        max_signals_per_guest: 3,
        max_signals_per_guest_client: 5,
        trusted_proxies: vec!["127.0.0.1".parse::<Cidr>().unwrap()],
        ..cfg()
    })
    .await;
    let r = room(13);
    let mut h = host(&t, &r, &secret(13)).await;
    let a = Some("203.0.113.5");
    let (mut a1, gid) = join_from(&t, &mut h, &r, a).await;
    for i in 0..3 {
        send(&mut a1, json!({"t":"signal","to":"host","data":{"i":i}})).await;
        assert_eq!(recv(&mut h).await["data"]["i"], i);
    }
    // One guest's own limit, as before.
    send(&mut a1, json!({"t":"signal","to":"host","data":{"i":3}})).await;
    expect_fatal(&mut a1, "rate_limited").await;
    assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid}));
    // The same client again: what is left of its 5, not 3 more.
    let (mut a2, gid) = join_from(&t, &mut h, &r, a).await;
    for i in 0..2 {
        send(&mut a2, json!({"t":"signal","to":"host","data":{"j":i}})).await;
        assert_eq!(recv(&mut h).await["data"]["j"], i);
    }
    send(&mut a2, json!({"t":"signal","to":"host","data":{"j":2}})).await;
    expect_fatal(&mut a2, "rate_limited").await;
    assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid}));
    // Used up: it may not even join until the window runs out.
    let mut a3 = connect_with(&t, None, a).await;
    send(&mut a3, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut a3, "rate_limited").await;
    // Another client is not held to it.
    let (mut b, gid) = join_from(&t, &mut h, &r, Some("198.51.100.7")).await;
    send(&mut b, json!({"t":"signal","to":"host","data":{"b":1}})).await;
    assert_eq!(
        recv(&mut h).await,
        json!({"t":"signal","from":gid,"data":{"b":1}})
    );
}

#[tokio::test]
async fn limit_joins_per_ip() {
    let t = start(Config {
        max_joins_per_ip: 3,
        ..cfg()
    })
    .await;
    let r = room(12);
    let mut h = host(&t, &r, &secret(12)).await;
    let (_a, _) = join(&t, &mut h, &r).await;
    // A miss counts too.
    let mut miss = connect(&t).await;
    send(&mut miss, json!({"v":1,"t":"join","room":room(999)})).await;
    expect_fatal(&mut miss, "no_room").await;
    let (_b, _) = join(&t, &mut h, &r).await;
    let mut c = connect(&t).await;
    send(&mut c, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut c, "rate_limited").await;
}

#[tokio::test]
async fn limit_rooms_total_and_per_ip() {
    let t = start(Config {
        max_rooms_per_ip: 2,
        max_rooms: 3,
        ..cfg()
    })
    .await;
    let _a = host(&t, &room(20), &secret(20)).await;
    let _b = host(&t, &room(21), &secret(21)).await;
    let mut c = connect(&t).await;
    send(
        &mut c,
        json!({"v":1,"t":"host","room":room(22),"secret":secret(22)}),
    )
    .await;
    expect_fatal(&mut c, "too_many_rooms").await;
    // The proxy header is ignored from an untrusted peer: still the same client.
    let mut d = connect_with(&t, None, Some("198.51.100.1")).await;
    send(
        &mut d,
        json!({"v":1,"t":"host","room":room(22),"secret":secret(22)}),
    )
    .await;
    expect_fatal(&mut d, "too_many_rooms").await;

    let t2 = start(Config {
        max_rooms_per_ip: 1,
        max_rooms: 2,
        trusted_proxies: vec!["127.0.0.1".parse::<Cidr>().unwrap()],
        ..cfg()
    })
    .await;
    // Behind a trusted proxy each forwarded client counts on its own (last entry wins).
    let mut p1 = connect_with(&t2, None, Some("203.0.113.9, 198.51.100.1")).await;
    send(
        &mut p1,
        json!({"v":1,"t":"host","room":room(30),"secret":secret(30)}),
    )
    .await;
    assert_eq!(recv(&mut p1).await["t"], "hosted");
    let mut p1b = connect_with(&t2, None, Some("198.51.100.1")).await;
    send(
        &mut p1b,
        json!({"v":1,"t":"host","room":room(31),"secret":secret(31)}),
    )
    .await;
    expect_fatal(&mut p1b, "too_many_rooms").await;
    let mut p2 = connect_with(&t2, None, Some("198.51.100.2")).await;
    send(
        &mut p2,
        json!({"v":1,"t":"host","room":room(31),"secret":secret(31)}),
    )
    .await;
    assert_eq!(recv(&mut p2).await["t"], "hosted");
    // The hub-wide cap.
    let mut p3 = connect_with(&t2, None, Some("2001:db8::1")).await;
    send(
        &mut p3,
        json!({"v":1,"t":"host","room":room(32),"secret":secret(32)}),
    )
    .await;
    expect_fatal(&mut p3, "server_full").await;
}

#[tokio::test]
async fn keepalive_drops_silent_peers() {
    let t = start(Config {
        ping_interval: Duration::from_millis(100),
        peer_timeout: Duration::from_millis(300),
        ..cfg()
    })
    .await;
    let r = room(40);
    let mut h = host(&t, &r, &secret(40)).await;
    // A guest that reads (and so answers pings) stays.
    let (mut g, gid) = join(&t, &mut h, &r).await;
    // A silent one never pongs: its socket is read underneath the client library, which would.
    let (silent, sgid) = join(&t, &mut h, &r).await;
    let mut watch = tokio::spawn(raw_close_code(silent));
    // Keep the host and the reading guest busy reading until the hub says the silent one left
    // (a deadline, not a fixed wait: a loaded machine takes longer than the timeout).
    let mut left = None;
    let end = tokio::time::Instant::now() + WAIT;
    while left.is_none() && tokio::time::Instant::now() < end {
        tokio::select! {
            m = h.next() => {
                if let Some(Ok(Message::Text(s))) = m {
                    left = Some(serde_json::from_str::<Value>(s.as_str()).unwrap());
                }
            }
            _ = g.next() => {}
            _ = tokio::time::sleep_until(end) => {}
        }
    }
    assert_eq!(left, Some(json!({"t":"left","gid":sgid})));
    // Still reading the others while the silent one's close is collected.
    let code = loop {
        tokio::select! {
            c = &mut watch => break c.unwrap(),
            _ = h.next() => {}
            _ = g.next() => {}
        }
    };
    assert_eq!(code, Some(1001));
    // The reading guest is still in.
    send(&mut g, json!({"t":"signal","to":"host","data":{}})).await;
    assert_eq!(
        recv(&mut h).await,
        json!({"t":"signal","from":gid,"data":{}})
    );
    assert_eq!(
        metric(
            &t.hub.shared.metrics_text(),
            "hub_errors_total{kind=\"timeout\"}"
        ),
        1
    );
}

#[tokio::test]
async fn bad_input_is_refused_and_closed() {
    let t = start(cfg()).await;
    let cases: Vec<(Message, &str)> = vec![
        (Message::text("not json"), "bad_json"),
        (Message::text("[1,2]"), "bad_json"),
        (Message::text(r#"{"v":1}"#), "bad_json"),
        (Message::text(r#"{"v":1,"t":7}"#), "bad_json"),
        (
            Message::text(r#"{"v":2,"t":"join","room":"x"}"#),
            "bad_version",
        ),
        (Message::text(r#"{"t":"join","room":"x"}"#), "bad_version"),
        (Message::text(r#"{"v":"1","t":"host"}"#), "bad_version"),
        (Message::text(r#"{"v":1,"t":"dance"}"#), "unknown_type"),
        (Message::binary(vec![1u8, 2, 3]), "bad_frame"),
        (
            Message::text(r#"{"t":"signal","to":"host","data":{}}"#),
            "bad_state",
        ),
        (Message::text(r#"{"t":"leave"}"#), "bad_state"),
        (
            Message::text(r#"{"v":1,"t":"join","room":"short"}"#),
            "bad_room",
        ),
        (
            Message::text(r#"{"v":1,"t":"join","room":"aaaaaaaaaaaaaaaaaaaa+/"}"#),
            "bad_room",
        ),
        (
            Message::text(json!({"v":1,"t":"host","room":room(50),"secret":"s"}).to_string()),
            "bad_secret",
        ),
        (
            Message::text(format!(
                r#"{{"v":1,"t":"host","room":"{}","secret":"{}","pad":"{}"}}"#,
                room(50),
                secret(50),
                "x".repeat(17 * 1024)
            )),
            "too_large",
        ),
    ];
    for (msg, why) in cases {
        let mut ws = connect(&t).await;
        ws.send(msg).await.unwrap();
        expect_fatal(&mut ws, why).await;
    }

    // Past the hard cap the WebSocket layer cuts the connection itself.
    let mut ws = connect(&t).await;
    let _ = ws.send(Message::text("x".repeat(100 * 1024))).await;
    assert!(matches!(recv_close(&mut ws).await, None | Some(1009)));

    // Errors after a role: a second host, bad signal data, a guest addressing a guest.
    let r = room(51);
    let mut h = host(&t, &r, &secret(51)).await;
    send(&mut h, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut h, "bad_state").await;
    let mut h = host(&t, &r, &secret(51)).await;
    let (mut g, _) = join(&t, &mut h, &r).await;
    send(&mut g, json!({"t":"signal","to":"host","data":"a string"})).await;
    expect_fatal(&mut g, "bad_data").await;
    assert_eq!(recv(&mut h).await["t"], "left");
    let (mut g, _) = join(&t, &mut h, &r).await;
    send(
        &mut g,
        json!({"t":"signal","to":"host","data":{"sdp":"x".repeat(13 * 1024)}}),
    )
    .await;
    expect_fatal(&mut g, "bad_data").await;
    assert_eq!(recv(&mut h).await["t"], "left");
    let (mut g, _) = join(&t, &mut h, &r).await;
    send(&mut g, json!({"t":"signal","to":"abcdefghijkl","data":{}})).await;
    expect_fatal(&mut g, "bad_target").await;
    assert_eq!(recv(&mut h).await["t"], "left");
    send(&mut h, json!({"t":"signal","to":"host","data":{}})).await;
    expect_fatal(&mut h, "bad_target").await;

    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_errors_total{kind=\"bad_json\"}"), 4);
    assert_eq!(metric(&m, "hub_errors_total{kind=\"too_large\"}"), 1);
}

#[tokio::test]
async fn origins() {
    let t = start(cfg()).await;
    // The engine sends no Origin; the guest page sends its own.
    connect_with(&t, None, None).await;
    connect_with(&t, Some("https://ps5upload.phantomptr.com"), None).await;
    // The app's own pages (12.3: Join a friend scans an internet invite in the app).
    for app in [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
    ] {
        connect_with(&t, Some(app), None).await;
    }
    let err = try_connect(&t, Some("https://evil.example"), None)
        .await
        .expect_err("refused");
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => assert_eq!(resp.status(), 403),
        other => panic!("expected HTTP 403, got {other:?}"),
    }
    assert_eq!(
        metric(
            &t.hub.shared.metrics_text(),
            "hub_errors_total{kind=\"bad_origin\"}"
        ),
        1
    );
}

fn stun_request(txid: [u8; 12]) -> Vec<u8> {
    let mut p = vec![0x00, 0x01, 0x00, 0x00];
    p.extend_from_slice(&stun::MAGIC_COOKIE.to_be_bytes());
    p.extend_from_slice(&txid);
    p
}

async fn stun_answer(sock: &UdpSocket) -> Option<Vec<u8>> {
    let mut buf = [0u8; 600];
    match tokio::time::timeout(Duration::from_millis(300), sock.recv(&mut buf)).await {
        Ok(Ok(n)) => Some(buf[..n].to_vec()),
        _ => None,
    }
}

#[tokio::test]
async fn stun_is_off_by_default() {
    let t = start(cfg()).await;
    assert!(t.hub.stun_addrs.is_empty());
}

#[tokio::test]
async fn stun_binding() {
    let t = start(Config {
        stun_listen: vec!["127.0.0.1:0".parse().unwrap()],
        max_stun_per_ip: 2,
        ..cfg()
    })
    .await;
    let server = t.hub.stun_addrs[0];
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.connect(server).await.unwrap();
    let me = sock.local_addr().unwrap();

    let txid = *b"0123456789ab";
    sock.send(&stun_request(txid)).await.unwrap();
    let resp = stun_answer(&sock).await.expect("an answer");
    assert_eq!(&resp[0..2], &[0x01, 0x01], "Binding success");
    assert_eq!(&resp[8..20], &txid, "same transaction");
    assert_eq!(stun::decode_xor_mapped_address(&resp), Some(me));
    // The attribute is XORed, not plain: the raw port bytes are not the port.
    assert_ne!(u16::from_be_bytes([resp[26], resp[27]]), me.port());

    // Not a Binding request: no answer.
    let mut alloc = stun_request(txid);
    alloc[1] = 0x03;
    sock.send(&alloc).await.unwrap();
    assert_eq!(stun_answer(&sock).await, None);
    sock.send(b"hello").await.unwrap();
    assert_eq!(stun_answer(&sock).await, None);

    // Second request is within the limit, the third is not.
    sock.send(&stun_request(txid)).await.unwrap();
    assert!(stun_answer(&sock).await.is_some());
    sock.send(&stun_request(txid)).await.unwrap();
    assert_eq!(stun_answer(&sock).await, None);

    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_stun_requests_total"), 2);
    assert_eq!(metric(&m, "hub_stun_ignored_total"), 2);
    assert_eq!(metric(&m, "hub_stun_limited_total"), 1);
}

#[tokio::test]
async fn shutdown_closes_sockets() {
    let t = start(cfg()).await;
    let r = room(60);
    let mut h = host(&t, &r, &secret(60)).await;
    let (mut g, _) = join(&t, &mut h, &r).await;
    let logs = t.logs.clone();
    t.hub.stop(Duration::from_secs(3)).await;
    assert_eq!(recv_close(&mut h).await, Some(1001));
    assert_eq!(recv_close(&mut g).await, Some(1001));
    assert!(logs.lock().unwrap().iter().any(|l| l == "hub: stopped"));
}

#[tokio::test]
async fn logs_hold_no_addresses_ids_or_payloads() {
    let t = start(Config {
        stun_listen: vec!["127.0.0.1:0".parse().unwrap()],
        trusted_proxies: vec!["127.0.0.1".parse::<Cidr>().unwrap()],
        summary_interval: Duration::from_millis(1),
        sweep_interval: Duration::from_millis(20),
        max_joins_per_ip: 1,
        ..cfg()
    })
    .await;
    let r = room(70);
    let s = secret(70);
    let mut h = host(&t, &r, &s).await;
    let (mut g, gid) = join(&t, &mut h, &r).await;
    send(
        &mut g,
        json!({"t":"signal","to":"host","data":{"candidate":"candidate:1 1 udp 1 192.0.2.44 5000 typ host"}}),
    )
    .await;
    recv(&mut h).await;
    // Errors of several kinds, one carrying a forwarded address.
    let mut x = connect_with(&t, None, Some("203.0.113.77")).await;
    send(
        &mut x,
        json!({"v":1,"t":"host","room":r,"secret":secret(71)}),
    )
    .await;
    expect_fatal(&mut x, "room_taken").await;
    let mut y = connect(&t).await;
    y.send(Message::text("garbage 198.51.100.200"))
        .await
        .unwrap();
    expect_fatal(&mut y, "bad_json").await;
    let mut z = connect(&t).await;
    send(&mut z, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut z, "rate_limited").await;
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&stun_request([9; 12]), t.hub.stun_addrs[0])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let logs = t.logs.clone();
    t.hub.stop(Duration::from_secs(3)).await;

    let logs = logs.lock().unwrap().clone();
    assert!(logs.iter().any(|l| l.starts_with("hub: started")));
    assert!(logs.iter().any(|l| l.starts_with("hub: summary")));
    // Bad frames are counted, not logged one by one.
    assert!(!logs.iter().any(|l| l.contains("protocol_error")));
    let summary = logs
        .iter()
        .rfind(|l| l.starts_with("hub: summary"))
        .unwrap();
    assert!(summary.contains(" errors=3"), "{summary}");
    assert!(summary.contains(" refused=1"), "{summary}");
    let forbidden = [
        "127.0.0.1",
        "::1",
        "203.0.113",
        "198.51.100",
        "192.0.2",
        "candidate",
        "garbage",
        r.as_str(),
        s.as_str(),
        gid.as_str(),
    ];
    for line in &logs {
        for f in forbidden {
            assert!(!line.contains(f), "log line {line:?} contains {f:?}");
        }
        // Only `hub: <event> key=value...` with names and numbers.
        assert!(line.starts_with("hub: "));
        for kv in line.split(' ').skip(2) {
            let (k, v) = kv.split_once('=').expect("key=value");
            assert!(k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
            assert!(v
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        }
    }
}

#[tokio::test]
async fn metrics_on_a_separate_listener() {
    let t = start(Config {
        metrics_listen: Some("127.0.0.1:0".parse().unwrap()),
        ..cfg()
    })
    .await;
    let private = t.hub.metrics_addr.expect("metrics listener");
    let public = http_get(t.hub.http_addr, "/metrics").await;
    assert!(public.starts_with("HTTP/1.1 404"), "{public}");
    assert!(http_get(t.hub.http_addr, "/health").await.ends_with("ok\n"));
    let m = http_get(private, "/metrics").await;
    assert!(m.starts_with("HTTP/1.1 200"));
    assert_eq!(metric(&m, "hub_rooms"), 0);
}

#[tokio::test]
async fn limit_connections_per_ip_and_total() {
    let t = start(Config {
        trusted_proxies: vec!["127.0.0.1".parse::<Cidr>().unwrap()],
        max_connections_per_ip: 2,
        max_connections: 3,
        ..cfg()
    })
    .await;
    let status = |e: tokio_tungstenite::tungstenite::Error| match e {
        tokio_tungstenite::tungstenite::Error::Http(resp) => resp.status().as_u16(),
        other => panic!("expected an HTTP refusal, got {other:?}"),
    };
    let a1 = connect_with(&t, None, Some("203.0.113.1")).await;
    let _a2 = connect_with(&t, None, Some("203.0.113.1")).await;
    let e = try_connect(&t, None, Some("203.0.113.1"))
        .await
        .expect_err("per client");
    assert_eq!(status(e), 429);
    // Another client still gets in, up to the hub's total.
    let _b1 = connect_with(&t, None, Some("203.0.113.2")).await;
    let e = try_connect(&t, None, Some("203.0.113.3"))
        .await
        .expect_err("total");
    assert_eq!(status(e), 503);
    // A closed connection gives its place back.
    drop(a1);
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        match try_connect(&t, None, Some("203.0.113.1")).await {
            Ok(_) => break,
            Err(_) if tokio::time::Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => panic!("place not given back: {e:?}"),
        }
    }
    let m = t.hub.shared.metrics_text();
    assert!(metric(&m, "hub_errors_total{kind=\"too_many_connections\"}") >= 1);
    assert!(metric(&m, "hub_errors_total{kind=\"server_full\"}") >= 1);
}

#[tokio::test]
async fn limit_connections_per_v6_site() {
    let t = start(Config {
        trusted_proxies: vec!["127.0.0.1".parse::<Cidr>().unwrap()],
        max_connections_per_ip: 1,
        max_connections_per_v6_site: 2,
        ..cfg()
    })
    .await;
    let status = |e: tokio_tungstenite::tungstenite::Error| match e {
        tokio_tungstenite::tungstenite::Error::Http(resp) => resp.status().as_u16(),
        other => panic!("expected an HTTP refusal, got {other:?}"),
    };
    // Three /64s of one /48: each is its own client, and the third is over the /48's limit.
    let a = connect_with(&t, None, Some("2001:db8:1:1::1")).await;
    let _b = connect_with(&t, None, Some("2001:db8:1:2::1")).await;
    let e = try_connect(&t, None, Some("2001:db8:1:3::1"))
        .await
        .expect_err("per /48");
    assert_eq!(status(e), 429);
    // Another /48 and IPv4 are not counted with it.
    let _c = connect_with(&t, None, Some("2001:db8:2:1::1")).await;
    let _d = connect_with(&t, None, Some("203.0.113.9")).await;
    // A closed connection gives its place in the /48 back, and the refusal took none.
    drop(a);
    let end = tokio::time::Instant::now() + WAIT;
    loop {
        match try_connect(&t, None, Some("2001:db8:1:3::1")).await {
            Ok(_) => break,
            Err(_) if tokio::time::Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => panic!("place not given back: {e:?}"),
        }
    }
}

#[tokio::test]
async fn a_socket_without_a_role_is_closed() {
    let t = start(Config {
        hello_timeout: Duration::from_millis(200),
        ..cfg()
    })
    .await;
    let mut idle = connect(&t).await;
    assert_eq!(recv_close(&mut idle).await, Some(4000));
    // A host past the same time stays.
    let r = room(80);
    let mut h = host(&t, &r, &secret(80)).await;
    quiet(&mut h, Duration::from_millis(400)).await;
    let (_g, _) = join(&t, &mut h, &r).await;
    assert_eq!(metric(&t.hub.shared.metrics_text(), "hub_connections"), 2);
}

#[tokio::test]
async fn an_idle_hub_logs_no_summary() {
    let t = start(Config {
        summary_interval: Duration::from_millis(1),
        sweep_interval: Duration::from_millis(10),
        ..cfg()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = room(81);
    let _h = host(&t, &r, &secret(81)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let logs = t.logs.lock().unwrap().clone();
    let summaries = logs
        .iter()
        .filter(|l| l.starts_with("hub: summary"))
        .count();
    // One for the room that opened; none for the idle time before or after it.
    assert_eq!(summaries, 1, "{logs:?}");
}

/// One plain HTTP request; the whole answer (status line, headers, body).
async fn http_req(addr: SocketAddr, method: &str, path: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(
        format!("{method} {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut out = String::new();
    tokio::time::timeout(WAIT, s.read_to_string(&mut out))
        .await
        .unwrap()
        .unwrap();
    out
}

fn labelled(text: &str, name: &str, label: &str, value: &str) -> u64 {
    metric(text, &format!("{name}{{{label}=\"{value}\"}}"))
}

/// What is not a WebSocket to `/ws` (scanners, crawlers) answers 404 as before, is counted by the
/// class of its path and is never logged; `/robots.txt` keeps crawlers away; `/ws/` is `/ws`.
#[tokio::test]
async fn strays_are_counted_by_class_and_never_logged() {
    let t = start(Config {
        summary_interval: Duration::from_millis(1),
        sweep_interval: Duration::from_millis(10),
        ..cfg()
    })
    .await;
    let a = t.hub.http_addr;
    for (path, class) in [
        ("/", "root"),
        ("/favicon.ico", "favicon"),
        ("/.well-known/security.txt", "well_known"),
        ("/phpinfo?looks-like-a-query=1", "other"),
        ("/wp-login.php", "other"),
        ("/ws/extra", "other"),
    ] {
        let r = http_req(a, "GET", path).await;
        assert!(r.starts_with("HTTP/1.1 404"), "{path}: {r}");
        let m = t.hub.shared.metrics_text();
        assert!(
            labelled(&m, "hub_http_strays_total", "class", class) > 0,
            "{path}"
        );
    }
    let robots = http_req(a, "GET", "/robots.txt").await;
    assert!(robots.starts_with("HTTP/1.1 200"), "{robots}");
    assert!(robots.ends_with("User-agent: *\nDisallow: /\n"), "{robots}");
    // `/ws` without the upgrade, or with another method: the answers it always had.
    for (method, path, status) in [
        ("GET", "/ws", "400"),
        ("GET", "/ws/", "400"),
        ("HEAD", "/ws", "405"),
        ("POST", "/ws", "405"),
    ] {
        let r = http_req(a, method, path).await;
        assert!(
            r.starts_with(&format!("HTTP/1.1 {status}")),
            "{method} {path}: {r}"
        );
    }
    // The health check is not a stray.
    assert!(http_req(a, "GET", "/health").await.ends_with("ok\n"));
    let m = t.hub.shared.metrics_text();
    assert_eq!(labelled(&m, "hub_http_strays_total", "class", "root"), 1);
    assert_eq!(labelled(&m, "hub_http_strays_total", "class", "other"), 3);
    assert_eq!(labelled(&m, "hub_http_strays_total", "class", "robots"), 1);
    assert_eq!(
        labelled(&m, "hub_http_strays_total", "class", "ws_not_websocket"),
        4
    );
    assert_eq!(t.hub.shared.metrics.strays_total(), 11, "{m}");
    // `/ws/` (a hub address typed with a slash at the end) is the hub.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{a}/ws/"))
        .await
        .expect("/ws/ upgrades");
    let r = room(90);
    send(
        &mut ws,
        json!({"v":1,"t":"host","room":r,"secret":secret(90)}),
    )
    .await;
    assert_eq!(recv(&mut ws).await, json!({"t":"hosted","room":r}));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let logs = t.logs.lock().unwrap().join("\n");
    for bad in [
        "phpinfo",
        "looks-like",
        "wp-login",
        "favicon",
        "well-known",
        "robots",
        "/",
    ] {
        assert!(!logs.contains(bad), "{bad} in {logs}");
    }
    assert!(logs.contains(" strays=11"), "{logs}");
}

/// Each limit that fires is counted by name, beside the error the client gets.
#[tokio::test]
async fn the_limit_that_fired_is_counted() {
    let t = start(Config {
        max_signals_per_guest: 2,
        max_joins_per_ip: 2,
        ..cfg()
    })
    .await;
    let r = room(91);
    let mut h = host(&t, &r, &secret(91)).await;
    let (mut g, _) = join(&t, &mut h, &r).await;
    for i in 0..2 {
        send(&mut g, json!({"t":"signal","to":"host","data":{"i":i}})).await;
        recv(&mut h).await;
    }
    send(&mut g, json!({"t":"signal","to":"host","data":{"i":2}})).await;
    expect_fatal(&mut g, "rate_limited").await;
    recv(&mut h).await; // left
    let (_g2, _) = join(&t, &mut h, &r).await;
    let mut third = connect(&t).await;
    send(&mut third, json!({"v":1,"t":"join","room":r})).await;
    expect_fatal(&mut third, "rate_limited").await;
    let m = t.hub.shared.metrics_text();
    assert_eq!(
        labelled(&m, "hub_limited_total", "limit", "guest_signals"),
        1
    );
    assert_eq!(labelled(&m, "hub_limited_total", "limit", "join_rate"), 1);
    assert_eq!(
        labelled(&m, "hub_limited_total", "limit", "client_signals"),
        0
    );
    assert_eq!(
        labelled(&m, "hub_errors_total", "kind", "rate_limited"),
        2,
        "{m}"
    );
    // A room its host closes counts as closed, not expired.
    send(&mut h, json!({"t":"leave"})).await;
    assert_eq!(recv_close(&mut h).await, Some(1000));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let m = t.hub.shared.metrics_text();
    assert_eq!(metric(&m, "hub_rooms_closed_total"), 1);
    assert_eq!(metric(&m, "hub_rooms_expired_total"), 0);
}

/// One home (one IPv4: a family, or a carrier-grade NAT) with three devices joining one Play
/// Together, each trying twice within the minute (the first try dropped and the page made a new
/// one), with the page as it was before it left out the candidates the engine drops: its offer,
/// then 40 candidates (Firefox found 20 on one plain network for the page's data channel, video
/// and audio; IPv6 or a VPN doubles that). The host answers each try with its answer and 10
/// candidates. Returns how many times a limit fired.
async fn family_session(c: Config) -> u64 {
    let t = start(Config {
        trusted_proxies: vec!["127.0.0.1".parse::<Cidr>().unwrap()],
        ..c
    })
    .await;
    let r = room(92);
    let mut h = host(&t, &r, &secret(92)).await;
    let home = Some("203.0.113.5");
    'tries: for _try in 0..2 {
        for _device in 0..3 {
            let mut g = connect_with(&t, None, home).await;
            send(&mut g, json!({"v":1,"t":"join","room":r})).await;
            if recv(&mut g).await["t"] != "joined" {
                break 'tries;
            }
            let gid = recv(&mut h).await["gid"].as_str().unwrap().to_string();
            for i in 0..41 {
                let data = if i == 0 {
                    json!({"type":"offer","sdp":"v=0"})
                } else {
                    json!({"type":"candidate","sdpMid":"0",
                        "candidate":format!("candidate:{i} 1 udp 1 192.0.2.1 {i} typ host")})
                };
                send(&mut g, json!({"t":"signal","to":"host","data":data})).await;
                if recv(&mut h).await["t"] != "signal" {
                    // `left`: the guest was refused and closed.
                    break 'tries;
                }
            }
            for i in 0..11 {
                send(&mut h, json!({"t":"signal","to":gid,"data":{"i":i}})).await;
                assert_eq!(recv(&mut g).await["data"]["i"], i);
            }
            // Connected: the page leaves the hub.
            send(&mut g, json!({"t":"leave"})).await;
            assert_eq!(recv(&mut h).await, json!({"t":"left","gid":gid}));
        }
    }
    let m = &t.hub.shared.metrics;
    ps5upload_hub::metrics::Limit::ALL
        .iter()
        .map(|l| m.limited_by(*l))
        .sum()
}

#[tokio::test]
async fn a_homes_play_together_never_hits_a_limit() {
    assert_eq!(family_session(cfg()).await, 0);
    // The limits before (20 per guest, 60 per client) closed the first try of such a page.
    let before = Config {
        max_signals_per_guest: 20,
        max_signals_per_guest_client: 60,
        ..cfg()
    };
    assert!(family_session(before).await > 0);
}
