//! Load test for Play Rooms (design: play-rooms 9.4). The hub runs as its own process (so its
//! resident memory is its own) and this test drives it over real sockets:
//!
//! - `rooms` lobby hosts (each its own client address) with a card, renewing it, answering
//!   previews and knocks, plus as many invite-room host sockets (a room has both);
//! - `list_rps` room-list requests a second from many viewers, in mixed query shapes;
//! - friends lookups, previews and knocks (guests join a lobby, knock, get "no", leave);
//! - then a robot flood: a few clients hammering the list and opening sockets as fast as they
//!   can, while the people above go on.
//!
//! It checks the budget: resident memory, list latency for people, no host socket dropped,
//! people never limited, robots slowed down; and prints the numbers (outbound bytes by class,
//! projected per month at the design's rates).
//!
//! Full run (1,000 rooms, 60 s + 40 s flood; needs a few thousand file descriptors):
//!
//! ```text
//! cargo test -p ps5upload-hub --release --test load -- --ignored --nocapture
//! ```
//!
//! A small version (100 rooms, a few seconds) runs with the normal tests.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use ps5upload_hub::hub::room_for_secret;
use ps5upload_hub::rooms::solve_pow;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

struct Plan {
    rooms: u32,
    secs: u64,
    flood_secs: u64,
    list_rps: u32,
    status_rps: u32,
    preview_rps: u32,
    knock_rps: u32,
    /// Connections each of the 5 robots floods the list over.
    bot_conns: u32,
    /// How often each host renews in the test (the design's engines: every 2 minutes).
    renew: Duration,
}

struct HubProc {
    child: Child,
    addr: SocketAddr,
}

impl Drop for HubProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_hub(rooms: u32) -> HubProc {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ps5upload-hub"))
        .env_clear()
        .env("HUB_HTTP_LISTEN", "127.0.0.1:0")
        .env("HUB_TRUSTED_PROXIES", "127.0.0.1")
        .env("HUB_AD_POW_BITS", "10")
        .env("HUB_MAX_ADS", (rooms + 10).to_string())
        .env("HUB_MAX_CONNECTIONS", "3500")
        .env("HUB_SUMMARY_SECS", "0")
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("hub binary runs");
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let port: u16 = loop {
        let l = lines.next().expect("a start line").unwrap();
        if let Some(p) = l.split("http_port=").nth(1) {
            break p.split_whitespace().next().unwrap().parse().unwrap();
        }
    };
    // Keep reading (a full pipe would block the hub); count cut-back lines.
    std::thread::spawn(move || for _ in lines {});
    HubProc {
        child,
        addr: SocketAddr::from(([127, 0, 0, 1], port)),
    }
}

fn rss_bytes(pid: u32) -> u64 {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        * 1024
}

/// CPU time the process used so far, in seconds (`ps -o time`: [[h:]m]m:ss.cc).
fn cpu_secs(pid: u32) -> f64 {
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .split(':')
        .fold(0.0, |acc, p| acc * 60.0 + p.parse::<f64>().unwrap_or(0.0))
}

fn ip(n: u32) -> String {
    format!("10.{}.{}.{}", (n >> 16) & 255, (n >> 8) & 255, n & 255)
}

fn card(i: u32) -> Value {
    let games = [
        ("It Takes Two", "PPSA01234"),
        ("Astro Bot", "PPSA05555"),
        ("Overcooked All You Can Eat", "PPSA00042"),
        ("Rocket League", "PPSA00777"),
    ];
    let (title, id) = games[(i % 4) as usize];
    let regions = ["eu-west", "na-east", "asia-east", "south-am"];
    json!({"listed": !i.is_multiple_of(5), "name": format!("Couch night {i}"), "host": "Player",
           "tag": format!("{:04X}", i & 0xffff), "game": {"title": title, "id": id},
           "proto": 2, "ver": "13.0.0", "caps": ["web", "ps5app", "preview", "chat", "voice"],
           "slots": [1 + i % 3, 4], "pw": i.is_multiple_of(2), "lang": "en",
           "region": regions[(i % 4) as usize], "pk": "B".repeat(44)})
}

#[derive(Default)]
struct Stats {
    ws_in: AtomicU64,
    http_in: AtomicU64,
    lists: AtomicU64,
    human_429: AtomicU64,
    bot_ok: AtomicU64,
    bot_429: AtomicU64,
    bot_ws_refused: AtomicU64,
    bot_ws_ok: AtomicU64,
    previews_ok: AtomicU64,
    knocks_answered: AtomicU64,
    hosts_dropped: AtomicU64,
    renews: AtomicU64,
}

async fn http(addr: SocketAddr, path: &str, xff: &str) -> (u16, usize) {
    let Ok(mut s) = TcpStream::connect(addr).await else {
        return (0, 0);
    };
    let req = format!(
        "GET {path} HTTP/1.1\r\nhost: x\r\nconnection: close\r\nx-forwarded-for: {xff}\r\n\r\n"
    );
    if s.write_all(req.as_bytes()).await.is_err() {
        return (0, 0);
    }
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut out)).await;
    let status = std::str::from_utf8(&out[..out.len().min(16)])
        .ok()
        .and_then(|h| h.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, out.len())
}

/// One request on a kept-alive connection: (status, bytes, the hub closes it).
async fn http_keepalive(s: &mut TcpStream, path: &str, xff: &str) -> Option<(u16, usize, bool)> {
    let req = format!("GET {path} HTTP/1.1\r\nhost: x\r\nx-forwarded-for: {xff}\r\n\r\n");
    s.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    let mut b = [0u8; 4096];
    let head_end = loop {
        let n = tokio::time::timeout(Duration::from_secs(10), s.read(&mut b))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&b[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let len: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + len {
        let n = s.read(&mut b).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&b[..n]);
    }
    Some((status, buf.len(), head.contains("connection: close")))
}

async fn ws(
    addr: SocketAddr,
    xff: &str,
) -> Option<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>> {
    let mut req = format!("ws://{addr}/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("x-forwarded-for", xff.parse().unwrap());
    match tokio_tungstenite::connect_async(req).await {
        Ok((w, _)) => Some(w),
        Err(e) => {
            // Robots (100.x) are refused on purpose. Hosts are 10.x and knocking guests 172.16.x:
            // a knock that cannot connect is the test machine running out of ports (macOS has
            // ~16k and keeps closed ones 30 s), not a host the hub dropped (`hosts_dropped`
            // counts those, and the run fails on any).
            if xff.starts_with("10.") {
                eprintln!("load: a host could not connect: {e}");
            } else if !xff.starts_with("100.") {
                eprintln!("load: a knocking guest could not connect: {e}");
            }
            None
        }
    }
}

fn secret(n: u32) -> String {
    format!("loadsecret{n:0>33}")
}

/// One room's two host sockets: the lobby (card, renewals, previews, knocks) and the invite room.
async fn host(addr: SocketAddr, i: u32, plan: Arc<Plan>, st: Arc<Stats>, stop: Arc<AtomicBool>) {
    let client = ip(i + 1);
    let Some(mut inv) = ws(addr, &client).await else {
        st.hosts_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let s = format!("invite{i:0>37}");
    let r = format!("inviteroom{i:0>12}");
    let _ = inv
        .send(Message::text(
            json!({"v":1,"t":"host","room":r,"secret":s}).to_string(),
        ))
        .await;
    let Some(mut w) = ws(addr, &client).await else {
        st.hosts_dropped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let s = secret(i);
    let room = room_for_secret(&s);
    let _ = w
        .send(Message::text(
            json!({"v":1,"t":"host","room":room,"secret":s,"lobby":true}).to_string(),
        ))
        .await;
    let mut created = false;
    // Spread renewals over the period.
    let first = plan.renew.mul_f64(f64::from(i % 100) / 100.0);
    let mut renew =
        tokio::time::interval_at(tokio::time::Instant::now() + plan.renew + first, plan.renew);
    // The engine pings the hub every 20 s.
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            m = w.next() => {
                let Some(Ok(m)) = m else {
                    if !stop.load(Ordering::Relaxed) {
                        eprintln!("load: lobby host {i} ended: {m:?}");
                        st.hosts_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    return;
                };
                if let Message::Close(f) = &m {
                    eprintln!("load: lobby host {i} closed: {f:?}");
                }
                let Message::Text(t) = m else { continue };
                if t.as_str().contains("error") || t.as_str().contains("refused") {
                    eprintln!("load: lobby host {i} got: {}", t.as_str());
                }
                st.ws_in.fetch_add(t.len() as u64, Ordering::Relaxed);
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                match v["t"].as_str().unwrap_or("") {
                    "hosted" if !created => {
                        created = true;
                        let n = v["ad_pow"]["n"].as_str().unwrap();
                        let pow = solve_pow(n, 10);
                        let _ = w.send(Message::text(json!({"t":"ad","ad":card(i),"pow":pow}).to_string())).await;
                    }
                    "preview" => {
                        let doc = json!({"proto":2,"host":{"name":"Player","ver":"13.0.0"},
                            "players":[{"name":"Player","seat":1,"kind":"host"},{"name":"Mia","seat":2,"kind":"web","tier":"good","rtt_ms":31}],
                            "stream":{"res":"1080p","fps":60,"codec":"hevc","kbps":15000}});
                        let _ = w.send(Message::text(json!({"t":"preview","pid":v["pid"],"data":doc}).to_string())).await;
                    }
                    "signal" => {
                        let _ = w.send(Message::text(json!({"t":"signal","to":v["from"],"data":{"knock":"no"}}).to_string())).await;
                    }
                    _ => {}
                }
            }
            _ = renew.tick() => {
                st.renews.fetch_add(1, Ordering::Relaxed);
                if w.send(Message::text("{\"t\":\"ad\"}")).await.is_err() {
                    return;
                }
            }
            _ = ping.tick() => { let _ = w.send(Message::Ping(Default::default())).await; }
            m = inv.next() => {
                if m.is_none() && !stop.load(Ordering::Relaxed) {
                    st.hosts_dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// A guest knocks on a lobby, waits for the host's answer, leaves.
async fn knock(addr: SocketAddr, n: u32, room: String, st: Arc<Stats>) {
    let Some(mut g) = ws(addr, &format!("172.16.{}.{}", (n >> 8) & 255, n & 255)).await else {
        return;
    };
    let _ = g
        .send(Message::text(
            json!({"v":1,"t":"join","room":room}).to_string(),
        ))
        .await;
    let k = json!({"knock":{"via":"rooms","name":"Guest","device":"AAAAAAAAAAAAAAAAAAAAAA","ver":"13.0.0"}});
    let _ = g
        .send(Message::text(
            json!({"t":"signal","to":"host","data":k}).to_string(),
        ))
        .await;
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(end, g.next()).await {
        if let Message::Text(t) = m {
            st.ws_in.fetch_add(t.len() as u64, Ordering::Relaxed);
            if t.as_str().contains("\"knock\":\"no\"") {
                st.knocks_answered.fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
    }
    let _ = g.send(Message::text("{\"t\":\"leave\"}")).await;
}

fn metric(text: &str, line: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix(line)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or(0)
}

async fn metrics(addr: SocketAddr) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /metrics HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() as f64 - 1.0) * p) as usize]
}

async fn run(plan: Plan) {
    let plan = Arc::new(plan);
    let hub = spawn_hub(plan.rooms);
    let addr = hub.addr;
    let pid = hub.child.id();
    let st = Arc::new(Stats::default());
    let stop = Arc::new(AtomicBool::new(false));
    let idle_rss = rss_bytes(pid);
    let mut tasks = Vec::new();
    for i in 0..plan.rooms {
        tasks.push(tokio::spawn(host(
            addr,
            i,
            plan.clone(),
            st.clone(),
            stop.clone(),
        )));
        if i % 50 == 49 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    // Wait for the cards.
    let t0 = Instant::now();
    loop {
        let m = metrics(addr).await;
        let n = metric(&m, "hub_ads_listed") + metric(&m, "hub_ads_friends");
        if n >= u64::from(plan.rooms) || t0.elapsed() > Duration::from_secs(60) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let setup = t0.elapsed();
    let rooms_rss = rss_bytes(pid);
    let m0 = metrics(addr).await;
    let out0 = |m: &str, c: &str| metric(m, &format!("hub_out_bytes_total{{class=\"{c}\"}}"));
    let peak = Arc::new(AtomicU64::new(rooms_rss));
    {
        let peak = peak.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(rss_bytes(pid), Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
    }
    // Steady load, then the flood on top.
    let lat = Arc::new(std::sync::Mutex::new(Vec::new()));
    let flood_lat = Arc::new(std::sync::Mutex::new(Vec::new()));
    let flooding = Arc::new(AtomicBool::new(false));
    let shapes = [
        "/rooms",
        "/rooms",
        "/rooms",
        "/rooms?sort=latency",
        "/rooms?sort=newest",
        "/rooms?free=1&nopw=1",
        "/rooms?game=PPSA05555",
        "/rooms?game=astro&sort=players",
        "/rooms?proto=2",
        "/rooms?page=12",
    ];
    let total = plan.secs + plan.flood_secs;
    let cpu0 = cpu_secs(pid);
    let mut cpu_flood = None;
    let start = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut n: u64 = 0;
    let mut flood_tasks = Vec::new();
    while start.elapsed() < Duration::from_secs(total) {
        tick.tick().await;
        n += 1;
        let el = start.elapsed().as_secs_f64();
        // Per 20 ms slot: list_rps / 50 lists, and the rest on their rates.
        let every = |rps: u32| rps > 0 && n.is_multiple_of((50 / u64::from(rps).min(50)).max(1));
        for k in 0..(plan.list_rps / 50).max(1) {
            if plan.list_rps < 50 && !every(plan.list_rps) {
                break;
            }
            let viewer = format!("192.168.{}.{}", (n % 200) as u8, k as u8);
            let path = shapes[((n + u64::from(k)) % shapes.len() as u64) as usize];
            let (st, lat, fl, flooding) =
                (st.clone(), lat.clone(), flood_lat.clone(), flooding.clone());
            tokio::spawn(async move {
                let t = Instant::now();
                let (code, len) = http(addr, path, &viewer).await;
                let us = t.elapsed().as_micros() as u64;
                if flooding.load(Ordering::Relaxed) {
                    fl.lock().unwrap().push(us)
                } else {
                    lat.lock().unwrap().push(us)
                }
                st.http_in.fetch_add(len as u64, Ordering::Relaxed);
                st.lists.fetch_add(1, Ordering::Relaxed);
                if code == 429 {
                    st.human_429.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        if every(plan.status_rps) {
            let ids: Vec<String> = (0..16)
                .map(|j| room_for_secret(&secret(((n as u32) * 16 + j) % plan.rooms)))
                .collect();
            let path = format!("/rooms/status?r={}", ids.join(","));
            let viewer = format!("192.169.{}.{}", (n % 250) as u8, (n / 250 % 250) as u8);
            let st = st.clone();
            tokio::spawn(async move {
                let (_, len) = http(addr, &path, &viewer).await;
                st.http_in.fetch_add(len as u64, Ordering::Relaxed);
            });
        }
        if every(plan.preview_rps) {
            let id = room_for_secret(&secret((n as u32 * 7) % plan.rooms));
            let viewer = format!("192.170.{}.{}", (n % 250) as u8, (n / 250 % 250) as u8);
            let st = st.clone();
            tokio::spawn(async move {
                let (code, len) = http(addr, &format!("/rooms/{id}/preview"), &viewer).await;
                st.http_in.fetch_add(len as u64, Ordering::Relaxed);
                if code == 200 {
                    st.previews_ok.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        if every(plan.knock_rps) {
            let room = room_for_secret(&secret((n as u32 * 13) % plan.rooms));
            tokio::spawn(knock(addr, n as u32, room, st.clone()));
        }
        // The flood: 5 robots on the list, 2 opening sockets, as fast as they can.
        if el >= plan.secs as f64 && !flooding.load(Ordering::Relaxed) && plan.flood_secs > 0 {
            cpu_flood = Some((cpu_secs(pid), Instant::now()));
            flooding.store(true, Ordering::Relaxed);
            let until = Instant::now() + Duration::from_secs(plan.flood_secs);
            for b in 0..5u32 {
                for _ in 0..plan.bot_conns {
                    let st = st.clone();
                    flood_tasks.push(tokio::spawn(async move {
                        // Persistent connections (the heavier flood, and no port churn on the
                        // test machine); a new one whenever the hub closes it.
                        let xff = format!("100.64.0.{b}");
                        while Instant::now() < until {
                            let Ok(mut s) = TcpStream::connect(addr).await else {
                                continue;
                            };
                            while Instant::now() < until {
                                match http_keepalive(&mut s, "/rooms", &xff).await {
                                    Some((code, len, close)) => {
                                        st.http_in.fetch_add(len as u64, Ordering::Relaxed);
                                        if code == 429 {
                                            st.bot_429.fetch_add(1, Ordering::Relaxed);
                                        } else if code == 200 {
                                            st.bot_ok.fetch_add(1, Ordering::Relaxed);
                                        }
                                        if close {
                                            break;
                                        }
                                    }
                                    None => break,
                                }
                            }
                        }
                    }));
                }
            }
            for b in 0..2u32 {
                let st = st.clone();
                flood_tasks.push(tokio::spawn(async move {
                    let mut open = Vec::new();
                    while Instant::now() < until {
                        match ws(addr, &format!("100.65.0.{b}")).await {
                            Some(w) => {
                                st.bot_ws_ok.fetch_add(1, Ordering::Relaxed);
                                open.push(w);
                                if open.len() > 40 {
                                    open.clear();
                                }
                            }
                            None => {
                                st.bot_ws_refused.fetch_add(1, Ordering::Relaxed);
                                // Paced a little: each refused try is a new TCP connection, and the
                                // test machine has only ~16k ports to churn through.
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                    }
                }));
            }
        }
    }
    for f in flood_tasks {
        let _ = f.await;
    }
    let cpu_end = cpu_secs(pid);
    let steady_cpu = match cpu_flood {
        Some((c, _)) => (c - cpu0) / plan.secs as f64,
        None => (cpu_end - cpu0) / total as f64,
    };
    let flood_cpu = cpu_flood
        .map(|(c, at)| (cpu_end - c) / at.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let m1 = metrics(addr).await;
    let dropped = st.hosts_dropped.load(Ordering::Relaxed);
    let listed = metric(&m1, "hub_ads_listed") + metric(&m1, "hub_ads_friends");
    stop.store(true, Ordering::Relaxed);
    let peak = peak.load(Ordering::Relaxed);
    let mut lat = lat.lock().unwrap().clone();
    let mut fl = flood_lat.lock().unwrap().clone();
    let secs = total as f64;
    let d = |c: &str| out0(&m1, c).saturating_sub(out0(&m0, c));
    let month = 30.0 * 86_400.0;
    // The design's daily rates at this many rooms (9.4): 20 lists, 5 previews, 2 knocks per room
    // a day; status lookups of apps on the Play screen every 60 s (1 per room around the clock);
    // a renewal per card every 2 minutes; WebSocket keepalive (pongs to the engine's pings every
    // 20 s on both host sockets, about 75 bytes each with TLS and TCP).
    let r = f64::from(plan.rooms);
    let lists_done = st.lists.load(Ordering::Relaxed).max(1) as f64;
    let per_list = d("list") as f64 / (lists_done + st.bot_ok.load(Ordering::Relaxed) as f64);
    let design = [
        ("lists", r * 20.0 * 30.0 * per_list),
        ("status", r * 1440.0 * 30.0 * 900.0 / 16.0),
        ("previews", r * 5.0 * 30.0 * 800.0),
        ("knocks", r * 2.0 * 30.0 * 10.0 * 300.0),
        ("renewals", r * 720.0 * 30.0 * 75.0),
        ("keepalive", r * 2.0 * month / 20.0 * 75.0),
    ];
    let design_total: f64 = design.iter().map(|(_, b)| b).sum();
    println!("\n== Hub load test ==");
    println!(
        "rooms {} (lobby + invite socket each), steady {} s + flood {} s, setup {:.1} s",
        plan.rooms,
        plan.secs,
        plan.flood_secs,
        setup.as_secs_f64()
    );
    println!(
        "resident memory: idle {:.1} MiB, with rooms {:.1} MiB, peak {:.1} MiB (budget 160)",
        idle_rss as f64 / 1048576.0,
        rooms_rss as f64 / 1048576.0,
        peak as f64 / 1048576.0
    );
    println!(
        "hub CPU (one thread): steady {:.0}%, during the flood {:.0}%",
        steady_cpu * 100.0,
        flood_cpu * 100.0
    );
    println!(
        "lists by people: {} ({:.0}/s), p50 {:.1} ms, p99 {:.1} ms; during the flood p99 {:.1} ms; 429s to people: {}",
        st.lists.load(Ordering::Relaxed),
        st.lists.load(Ordering::Relaxed) as f64 / secs,
        pct(&mut lat, 0.5) as f64 / 1000.0,
        pct(&mut lat, 0.99) as f64 / 1000.0,
        pct(&mut fl, 0.99) as f64 / 1000.0,
        st.human_429.load(Ordering::Relaxed)
    );
    println!(
        "robots: list ok {} / slowed (429) {}; sockets opened {} / refused {}; temporary blocks {}",
        st.bot_ok.load(Ordering::Relaxed),
        st.bot_429.load(Ordering::Relaxed),
        st.bot_ws_ok.load(Ordering::Relaxed),
        st.bot_ws_refused.load(Ordering::Relaxed),
        metric(&m1, "hub_temp_blocks_total")
    );
    println!(
        "previews answered {}, knocks answered {}, renewals {}, host sockets dropped {}, cards at end {}",
        st.previews_ok.load(Ordering::Relaxed),
        st.knocks_answered.load(Ordering::Relaxed),
        st.renews.load(Ordering::Relaxed),
        dropped,
        listed
    );
    println!(
        "list cache: hits {} misses {}; cut-back {}",
        metric(&m1, "hub_rooms_list_total{result=\"hit\"}"),
        metric(&m1, "hub_rooms_list_total{result=\"miss\"}"),
        metric(&m1, "hub_cutback")
    );
    for c in ["list", "status", "preview", "ws", "refused"] {
        println!(
            "out bytes {c}: {} ({:.1} KB/s in this test)",
            d(c),
            d(c) as f64 / secs / 1000.0
        );
    }
    println!(
        "bytes per list response (with headers): {:.0}; at this test's rates a month would be {:.0} GB",
        per_list,
        (d("list") + d("status") + d("preview") + d("ws")) as f64 / secs * month / 1e9
    );
    for (k, b) in design {
        println!("design rates, {k}: {:.2} GB/month", b / 1e9);
    }
    println!(
        "design rates, total: {:.1} GB/month (budget 100)",
        design_total / 1e9
    );
    for l in m1.lines().filter(|l| {
        (l.starts_with("hub_ads_refused_total") || l.starts_with("hub_ads_removed_total"))
            && !l.ends_with(" 0")
    }) {
        println!("{l}");
    }
    drop(hub);
    // The budget.
    assert_eq!(dropped, 0, "no host socket dropped");
    assert_eq!(listed, u64::from(plan.rooms), "every card still there");
    assert_eq!(
        st.human_429.load(Ordering::Relaxed),
        0,
        "people never limited"
    );
    assert!(peak <= 160 << 20, "resident {} MiB", peak >> 20);
    assert!(pct(&mut lat, 0.99) <= 50_000, "list p99");
    if plan.flood_secs > 0 {
        assert!(st.bot_429.load(Ordering::Relaxed) > st.bot_ok.load(Ordering::Relaxed));
        assert!(st.bot_ws_refused.load(Ordering::Relaxed) > 0);
    }
    assert!(design_total < 100e9);
}

/// 100 rooms for a few seconds: runs with every test run.
#[tokio::test]
async fn load_small() {
    run(Plan {
        rooms: 100,
        secs: 3,
        flood_secs: 2,
        list_rps: 50,
        status_rps: 5,
        preview_rps: 5,
        knock_rps: 5,
        bot_conns: 1,
        renew: Duration::from_secs(1),
    })
    .await;
}

/// The design's load test: 1,000 rooms renewing (every 10 s here, 12x the engines' rate), 50
/// list requests a second, friends lookups, previews and knocks for a minute, then a 40 s flood (past the 30 s a flood must last to be blocked).
#[tokio::test]
#[ignore = "a measurement: about 110 s; run with --release --ignored --nocapture"]
async fn load_1000_rooms() {
    run(Plan {
        rooms: 1000,
        secs: 60,
        flood_secs: 40,
        list_rps: 50,
        status_rps: 5,
        preview_rps: 5,
        knock_rps: 5,
        bot_conns: 8,
        renew: Duration::from_secs(10),
    })
    .await;
}
