// join.rs's real-engine e2e with PS5Upload Play's own network code (included into
// `join::tests::play_cli`, for its fakes). The engine's real routes (Play Rooms, the companion
// socket, the proxies, Thanks & Support) on 127.0.0.1, a pretend session on a pretend console (no
// PS5), and the app's guest and companion code as `apps/play`'s `play-cli` runs it on a computer.
//
// Run by `make -C apps/play e2e-engine` (it builds play-cli and sets PS5UPLOAD_PLAY_CLI):
//   PS5UPLOAD_PLAY_CLI=apps/play/build/host-tests/play-cli \
//     cargo test -p ps5upload-engine --lib play_cli -- --ignored --nocapture
//
// What it proves, end to end: a knock with the room password (challenge, sealed blob, the
// host's answer), a wrong password (`tries_left`), a name already in the room (the suggestion),
// chat and voice up and down between two app guests, many controllers on one PS5 (`media:false`,
// `group`, "let all from that PS5 in", counted against the size, `release`), Play again with no
// password, the companion's room intents, `session.user` / `console.owner`, the ready check,
// "It's me" (Player 1), Recent players, the preview's host link, the rooms proxies and
// `GET /api/support`.

use super::*;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};

/// A play-cli run: its output lines as they come.
struct Cli {
    name: &'static str,
    child: tokio::process::Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Cli {
    fn start(bin: &str, name: &'static str, args: &[&str]) -> Cli {
        let mut child = tokio::process::Command::new(bin)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("play-cli starts");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let out = child.stdout.take().unwrap();
        let l = lines.clone();
        tokio::spawn(async move {
            let mut r = BufReader::new(out).lines();
            while let Ok(Some(line)) = r.next_line().await {
                l.lock().unwrap().push(line);
            }
        });
        Cli { name, child, lines }
    }

    fn text(&self) -> String {
        self.lines.lock().unwrap().join("\n")
    }

    /// The first line holding `what`, waiting up to `secs`.
    async fn until(&self, what: &str, secs: u64) -> String {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(l) = self.lines.lock().unwrap().iter().find(|l| l.contains(what)) {
                return l.clone();
            }
            if Instant::now() > end {
                panic!("{}: no \"{what}\" in {secs} s:\n{}", self.name, self.text());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Waits for it to end (at most `secs`): everything it said.
    async fn done(mut self, secs: u64) -> String {
        let _ = tokio::time::timeout(Duration::from_secs(secs), self.child.wait()).await;
        // its last lines are read by the reader task
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.text()
    }
}

/// The value of `key=` in a `result` / `chat` line of play-cli.
fn field(out: &str, line_start: &str, key: &str) -> Option<String> {
    out.lines()
        .rfind(|l| l.starts_with(line_start))?
        .split_whitespace()
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(str::to_string))
}

/// The next ask on `console` (10 s at most): its id and the host's view of it.
async fn next_ask(console: &str) -> (u64, serde_json::Value) {
    for _ in 0..400 {
        let s = host_state(console).await;
        if let Some(a) = s["pending"].as_array().and_then(|p| p.first()) {
            return (a["id"].as_u64().unwrap(), a.clone());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no ask in time: {}", host_state(console).await);
}

async fn decide_http(id: u64, allow: serde_json::Value) -> serde_json::Value {
    let (ci, h) = local();
    body(decide(ci, h, Json(DecideBody { id, allow })).await).await
}

/// A plain HTTP call to the engine on `port` (as the clients make it): status and JSON.
async fn http(
    port: u16,
    method: &str,
    path: &str,
    b: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let url = format!("http://127.0.0.1:{port}{path}");
    let method = method.to_string();
    tokio::task::spawn_blocking(move || {
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build();
        let agent = ureq::Agent::new_with_config(cfg);
        let mut r = match (method.as_str(), b) {
            ("POST", Some(b)) => agent
                .post(&url)
                .header("Content-Type", "application/json")
                .send(b.to_string())
                .unwrap(),
            _ => agent.get(&url).call().unwrap(),
        };
        let code = r.status().as_u16();
        let text = r.body_mut().read_to_string().unwrap_or_default();
        (
            code,
            serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
        )
    })
    .await
    .unwrap()
}

/// A stand-in hub's HTTP side for the engine's rooms proxies: `/rooms`, `/rooms/status`,
/// `/rooms/{id}/preview`, each saying what it was asked.
async fn fake_hub_http() -> String {
    use axum::routing::get;
    let app = axum::Router::new()
        .route(
            "/rooms",
            get(|q: axum::extract::RawQuery| async move {
                Json(serde_json::json!({"rooms": [], "asked": q.0}))
            }),
        )
        .route(
            "/rooms/status",
            get(|q: axum::extract::RawQuery| async move {
                Json(serde_json::json!({"every_s": 60, "rooms": {}, "asked": q.0}))
            }),
        )
        .route(
            "/rooms/{id}/preview",
            get(
                |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    Json(serde_json::json!({"proto": 2, "room": {"name": "Hub copy"}, "asked": id}))
                },
            ),
        );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    format!("ws://127.0.0.1:{port}/ws")
}

const DEV_LEE: &str = "AQECAwQFBgcICQoLDA0ODw";
const DEV_SAM: &str = "AgECAwQFBgcICQoLDA0ODw";
const DEV_WES: &str = "AwECAwQFBgcICQoLDA0ODw";
const DEV_NED: &str = "BAECAwQFBgcICQoLDA0ODw";
const DEV_EVE: &str = "BQECAwQFBgcICQoLDA0ODw";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs apps/play's play-cli: make -C apps/play e2e-engine"]
async fn play_cli_rooms_end_to_end() {
    let Ok(bin) = std::env::var("PS5UPLOAD_PLAY_CLI") else {
        eprintln!("PS5UPLOAD_PLAY_CLI is not set: nothing to run");
        return;
    };
    let bin = bin.as_str();
    let _turn = PARTY_TESTS.lock().await;
    let _live_turn = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let hub = fake_hub_http().await;
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = Some(Settings {
        hub_url: hub,
        stun: Vec::new(),
        upnp: false,
        internet_invites: true,
        ..Settings::default()
    });
    let _ = crate::join_room::set_password(None);
    crate::join_room::test_clear_blocks();
    crate::join_history::test_clear();
    // The companion socket's console is the app's own address: here, this computer.
    let console = "127.0.0.1";
    let f = Arc::new(FakeHelper::default());
    *f.signed_in.lock().unwrap() = vec![0x1000];
    *f.foreground.lock().unwrap() = 0x1000;
    set_helper(console, f.clone());
    set_live(console, 1);
    let live = Arc::new(crate::rp_stream_api::test_live::Session::start(console));
    live.console_rtt(2.4);
    // The console's stream: a codec header now and then, pictures at 30 a second.
    let feed_live = live.clone();
    let feed = tokio::spawn(async move {
        let mut n = 0u32;
        loop {
            if n.is_multiple_of(60) {
                feed_live.send(0, &[0, 0, 0, 1, 0x67, 1, 2, 3], true);
            }
            feed_live.picture(&[0, 0, 0, 1, 0x65, 9, 9, 9], true);
            n += 1;
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    });
    let app = router::<()>().merge(crate::play_app::router::<()>()).route(
        "/api/support",
        axum::routing::get(crate::support_api::support),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(
            l,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });
    let p = port.to_string();
    let p = p.as_str();
    // The room: a password, room for three friends.
    let s = {
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
    };
    assert_eq!(s["open"], true, "{s}");
    let (code, v) = http(
        port,
        "POST",
        "/api/join/room",
        Some(serde_json::json!({"addr": console, "password": "hunter22", "name": "Couch chaos"})),
    )
    .await;
    assert_eq!(code, 200, "{v}");
    let token = crate::play_app::test_pair(console);

    // 1. A knock from a PS5 nearby, with the room password: challenge, sealed blob, the host lets
    //    them in.
    let lee = Cli::start(
        bin,
        "lee",
        &[
            "join",
            "127.0.0.1",
            p,
            "--via",
            "nearby",
            "--password",
            "hunter22",
            "--name",
            "Lee",
            "--device",
            DEV_LEE,
            "--notice",
            "--seconds",
            "45",
        ],
    );
    let (id, ask) = next_ask(console).await;
    assert_eq!(ask["came"], "nearby", "{ask}");
    assert_eq!(
        ask["password_ok"], true,
        "the sealed password was opened: {ask}"
    );
    assert_eq!(
        ask["own"], true,
        "this computer is the host's own device: {ask}"
    );
    assert!(ask["tag"].as_str().is_some_and(|t| t.len() == 4), "{ask}");
    assert_eq!(decide_http(id, true.into()).await["ok"], true);
    lee.until("phase joined", 10).await;
    let lee_tok = lee.until("token ", 10).await;
    assert!(lee_tok.ends_with("player 2"), "{lee_tok}");

    // 1b. A wrong password: refused, and how many tries are left.
    let wes = Cli::start(
        bin,
        "wes",
        &[
            "join",
            "127.0.0.1",
            p,
            "--via",
            "nearby",
            "--password",
            "wrong-one",
            "--name",
            "Wes",
            "--device",
            DEV_WES,
            "--seconds",
            "6",
        ],
    );
    let out = wes.done(20).await;
    assert!(out.contains("phase refused password"), "{out}");
    assert_eq!(
        field(&out, "chat lines", "tries_left").as_deref(),
        Some("4"),
        "{out}"
    );

    // 2. A name already in the room (case and look-alikes don't count): the suggestion.
    let ned = Cli::start(
        bin,
        "ned",
        &[
            "join",
            "127.0.0.1",
            p,
            "--via",
            "nearby",
            "--password",
            "hunter22",
            "--name",
            "LEE",
            "--device",
            DEV_NED,
            "--seconds",
            "6",
        ],
    );
    let out = ned.done(20).await;
    assert!(out.contains("phase name_taken"), "{out}");
    assert!(out.contains("suggest LEE (2)"), "{out}");

    // 3. A second guest who talks: a quick message, a typed line, voice frames with
    //    push-to-talk; and asks for a friend pass (Play again later).
    let sam = Cli::start(
        bin,
        "sam",
        &[
            "join",
            "127.0.0.1",
            p,
            "--via",
            "nearby",
            "--password",
            "hunter22",
            "--name",
            "Sam",
            "--device",
            DEV_SAM,
            "--notice",
            "--remember",
            "--quick",
            "5",
            "--text",
            "gg all",
            "--talk",
            "40",
            "--seconds",
            "40",
            "--release-after",
            "16",
        ],
    );
    let (id, _) = next_ask(console).await;
    assert_eq!(decide_http(id, true.into()).await["ok"], true);
    sam.until("phase joined", 10).await;
    let sam_tok = sam.until("token ", 10).await;
    let sam_tok = sam_tok.split_whitespace().nth(1).unwrap().to_string();
    let pass = sam.until("friend ", 10).await;
    let pass = pass.trim_start_matches("friend ").to_string();
    sam.until("text sent 1", 10).await;

    // 4. Many controllers: another player on Sam's PS5 (no picture or sound of its own). The host
    //    lets every player from that PS5 in, from the TV (the companion).
    let mia = Cli::start(
        bin,
        "mia",
        &[
            "join",
            "127.0.0.1",
            p,
            "--group",
            &sam_tok,
            "--no-media",
            "--via",
            "group",
            "--name",
            "Mia",
            "--device",
            DEV_SAM,
            "--seconds",
            "22",
        ],
    );
    let (_, ask) = next_ask(console).await;
    assert_eq!(ask["came"], "group", "{ask}");
    assert_eq!(ask["players"], serde_json::json!(["Sam", "Mia"]), "{ask}");
    let tv = Cli::start(
        bin,
        "tv",
        &[
            "companion",
            "127.0.0.1",
            p,
            &token,
            "--allow-group-first",
            "--seconds",
            "4",
        ],
    );
    let out = tv.done(15).await;
    assert!(
        out.contains("came=group") && out.contains("group=2 own=1"),
        "the TV sees the PS5's group:\n{out}"
    );
    assert!(
        out.contains("doc_user user=user4096 owner=user4096"),
        "session.user and console.owner:\n{out}"
    );
    mia.until("phase joined", 10).await;
    let s = host_state(console).await;
    let sam_g = s["guests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "Sam")
        .unwrap()
        .clone();
    let mia_g = s["guests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["name"] == "Mia")
        .unwrap()
        .clone();
    assert_eq!(mia_g["group"], sam_g["id"], "{s}");
    assert_eq!(sam_g["group"], sam_g["id"], "{s}");
    // A third from that PS5 gets in without asking: but the room (3 friends) is full now.
    let kai = Cli::start(
        bin,
        "kai",
        &[
            "join",
            "127.0.0.1",
            p,
            "--group",
            &sam_tok,
            "--no-media",
            "--via",
            "group",
            "--name",
            "Kai",
            "--device",
            DEV_SAM,
            "--seconds",
            "6",
        ],
    );
    let out = kai.done(20).await;
    assert!(
        out.contains("phase refused full"),
        "a group counts against the room's size:\n{out}"
    );

    // Chat and voice reached Lee (the engine forwards, never decodes).
    // 5. Sam leaves (`release`), Mia plays on with Sam's socket keeping the picture; once Mia
    //    leaves, Sam's socket ends.
    let out = mia.done(40).await;
    assert!(out.contains("phase ended left"), "{out}");
    assert_eq!(
        field(&out, "result", "pictures").as_deref(),
        Some("0"),
        "no picture on a media:false socket:\n{out}"
    );
    let out = sam.done(40).await;
    assert!(out.contains("release sent 1"), "{out}");
    assert!(
        out.contains("phase ended left"),
        "the media holder ends with the last of its PS5:\n{out}"
    );
    assert!(
        field(&out, "result", "pictures")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
            > 100,
        "{out}"
    );
    assert!(
        field(&out, "chat lines", "voice_out")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
            > 0,
        "{out}"
    );

    // 6. Play again: Sam's pass, no room password (the pass is the proof), in without asking.
    let again = Cli::start(
        bin,
        "again",
        &[
            "join",
            "127.0.0.1",
            p,
            "--again",
            &pass,
            "--via",
            "again",
            "--name",
            "Sam",
            "--device",
            DEV_SAM,
            "--seconds",
            "6",
        ],
    );
    let out = again.done(25).await;
    assert!(
        out.contains("phase joined"),
        "Play again with no password:\n{out}"
    );
    assert!(!out.contains("phase password"), "{out}");

    // 7. The ready check and the TV's room intents (visibility, password, chat, voice, mutes,
    //    remove + block, unblock, a quick message).
    let tv = Cli::start(
        bin,
        "tv",
        &[
            "companion",
            "127.0.0.1",
            p,
            &token,
            "--ready-check",
            "--seconds",
            "3",
        ],
    );
    let out = tv.done(15).await;
    assert!(!out.contains("intent_error"), "{out}");
    let s = host_state(console).await;
    assert!(
        s["room"]["ready"]["asked"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "{s}"
    );
    let (code, v) = http(
        port,
        "POST",
        "/api/join/ready",
        Some(serde_json::json!({"addr": console, "start": true})),
    )
    .await;
    assert_eq!(
        (code, v["error"].as_str()),
        (409, Some("running")),
        "one at a time: {v}"
    );
    let out = lee.done(60).await;
    assert!(out.contains("phase ended"), "{out}");
    let lines: u32 = field(&out, "chat lines", "lines")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    assert!(
        lines >= 2,
        "Sam's quick message and typed line reached Lee:\n{out}"
    );
    assert!(
        field(&out, "chat lines", "voice_in")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
            > 0,
        "Sam's voice reached Lee:\n{out}"
    );

    // 8. "It's me": the host's own device (this computer) asks; the TV answers it, and with no
    //    host page on the stream it takes Player 1.
    live.page_left();
    let eve = Cli::start(
        bin,
        "eve",
        &[
            "join",
            "127.0.0.1",
            p,
            "--via",
            "nearby",
            "--password",
            "hunter22",
            "--name",
            "Eve",
            "--device",
            DEV_EVE,
            "--seconds",
            "8",
        ],
    );
    next_ask(console).await;
    let tv = Cli::start(
        bin,
        "tv",
        &[
            "companion",
            "127.0.0.1",
            p,
            &token,
            "--its-me-first",
            "--seconds",
            "3",
        ],
    );
    tv.done(15).await;
    let out = eve.done(20).await;
    assert_eq!(
        field(&out, "result", "player").as_deref(),
        Some("1"),
        "{out}"
    );

    // 9. The TV's room intents (these remove Player 2 and change the room).
    let lee2 = Cli::start(
        bin,
        "lee2",
        &[
            "join",
            "127.0.0.1",
            p,
            "--via",
            "nearby",
            "--password",
            "hunter22",
            "--name",
            "Lee",
            "--device",
            DEV_LEE,
            "--seconds",
            "10",
        ],
    );
    let (id, _) = next_ask(console).await;
    decide_http(id, true.into()).await;
    lee2.until("phase joined", 10).await;
    let tv = Cli::start(
        bin,
        "tv",
        &[
            "companion",
            "127.0.0.1",
            p,
            &token,
            "--room-intents",
            "--seconds",
            "3",
        ],
    );
    let out = tv.done(15).await;
    // allow_group 7: no such ask (already answered); the rest (and the host's quick message) go through.
    assert!(
        !out.contains("intent_error notice") && !out.contains("intent_error refused"),
        "{out}"
    );
    let out = lee2.done(20).await;
    assert!(out.contains("phase ended removed"), "{out}");
    let s = host_state(console).await;
    assert_eq!(s["room"]["visibility"], "public", "{s}");
    assert_eq!(s["room"]["chat"], "quick", "{s}");
    assert_eq!(s["room"]["voice"], "ptt", "{s}");
    assert!(
        s["room"]["blocked"]
            .as_array()
            .is_some_and(|b| !b.is_empty()),
        "{s}"
    );
    assert_eq!(s["room"]["host_link"]["rtt_ms"], 2, "{s}");

    // 10. The clients' HTTP paths: Recent players, the preview, the rooms proxies, Thanks & Support,
    //     "let all from that PS5 in" by HTTP.
    let (code, v) = http(port, "GET", "/api/join/history", None).await;
    assert_eq!(code, 200);
    let names: Vec<&str> = v["players"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(names.contains(&"Mia") && names.contains(&"Lee"), "{v}");
    let mia_h = v["players"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Mia")
        .unwrap();
    assert_eq!(mia_h["came"], "group", "{v}");
    assert!(
        mia_h["last_unix"].as_u64().is_some() && mia_h["minutes"].as_u64().is_some(),
        "{v}"
    );
    let (code, v) = http(
        port,
        "POST",
        "/api/join/history/forget",
        Some(serde_json::json!({"id": mia_h["id"]})),
    )
    .await;
    assert_eq!((code, v["ok"].as_bool()), (200, Some(true)), "{v}");
    let (code, v) = http(port, "GET", "/api/join/preview", None).await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["host_link"]["rtt_ms"], 2, "{v}");
    let lobby = crate::join_net::lobby::lobby_id().unwrap_or_else(|| "B".repeat(22));
    let (code, v) = http(port, "GET", "/api/join/rooms?sort=players&x=1", None).await;
    assert_eq!(
        (code, v["asked"].as_str()),
        (200, Some("sort=players")),
        "{v}"
    );
    let (code, v) = http(
        port,
        "GET",
        &format!("/api/join/rooms/status?r={lobby}"),
        None,
    )
    .await;
    assert_eq!(code, 200, "{v}");
    assert_eq!(
        v["asked"].as_str(),
        Some(format!("r={lobby}").as_str()),
        "{v}"
    );
    let (code, v) = http(
        port,
        "GET",
        &format!("/api/join/rooms/{lobby}/preview"),
        None,
    )
    .await;
    assert_eq!(
        (code, v["asked"].as_str()),
        (200, Some(lobby.as_str())),
        "{v}"
    );
    let (code, _) = http(port, "GET", "/api/join/rooms/bad!/preview", None).await;
    assert_eq!(code, 400);
    let (code, v) = http(port, "GET", "/api/support", None).await;
    assert_eq!(code, 200);
    assert!(v["rev"].is_string() && v["names"].is_array(), "{v}");
    let (code, v) = http(
        port,
        "POST",
        "/api/join/decide",
        Some(serde_json::json!({"addr": console, "id": 999_999, "allow": "group"})),
    )
    .await;
    assert_eq!((code, v["ok"].as_bool()), (200, Some(false)), "{v}");
    let (code, _) = http(
        port,
        "POST",
        "/api/join/decide",
        Some(serde_json::json!({"id": 1, "allow": "maybe"})),
    )
    .await;
    assert_eq!(code, 400);

    feed.abort();
    {
        let (ci, h) = local();
        let _ = close(
            ci,
            h,
            Query(Default::default()),
            Json(CloseBody {
                addr: console.into(),
                confirm_friends: true,
            }),
        )
        .await;
    }
    set_live(console, 0);
    drop(live);
    *SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let _ = crate::join_room::set_password(None);
    crate::join_room::test_clear_blocks();
}
