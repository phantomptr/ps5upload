//! PS5Upload Play's channel: pairing, the token, what an app without one sees, and its intents.

use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TMsg;

type App =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The tests share the engine's one hub: they take turns, each from a clean one.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A fresh hub kept in `dir` (None: in memory).
fn reset(dir: Option<PathBuf>) {
    *TEST_DIR.lock().unwrap() = dir;
    *HUB.lock().unwrap() = None;
}

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "ps5upload-play-app-{tag}-{}-{}",
        std::process::id(),
        random_hex(4).unwrap()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn local() -> (ConnectInfo<std::net::SocketAddr>, HeaderMap) {
    let mut h = HeaderMap::new();
    h.insert("host", "127.0.0.1:3000".parse().unwrap());
    (ConnectInfo("127.0.0.1:5555".parse().unwrap()), h)
}

async fn body(r: Response) -> (StatusCode, Value) {
    let status = r.status();
    let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&b).unwrap_or(Value::Null))
}

async fn type_code(code: &str) -> (StatusCode, Value) {
    let (ci, h) = local();
    body(
        pair(
            ci,
            h,
            Json(PairBody {
                code: code.to_string(),
            }),
        )
        .await,
    )
    .await
}

async fn serve() -> String {
    let app = router::<()>();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            l,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("ws://{addr}/api/play/app")
}

async fn connect(url: &str, token: Option<&str>) -> App {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let hello = match token {
        Some(t) => json!({"hello": {"token": t, "v": "13.0.0"}}),
        None => json!({"hello": {"v": "13.0.0"}}),
    };
    ws.send(TMsg::Text(hello.to_string().into())).await.unwrap();
    ws
}

/// The next text message with `key`, within 5 s.
async fn next_with(ws: &mut App, key: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let m = tokio::time::timeout_at(deadline, ws.next())
            .await
            .unwrap_or_else(|_| panic!("no message with {key} in time"));
        match m {
            Some(Ok(TMsg::Text(t))) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                if v.get(key).is_some() {
                    return v;
                }
            }
            Some(Ok(_)) => {}
            other => panic!("closed waiting for {key}: {other:?}"),
        }
    }
}

async fn send(ws: &mut App, v: Value) {
    ws.send(TMsg::Text(v.to_string().into())).await.unwrap();
}

/// Pairs a new app from this computer: its socket and token.
async fn paired_app(url: &str) -> (App, String) {
    let mut app = connect(url, None).await;
    assert_eq!(next_with(&mut app, "doc").await["doc"]["paired"], false);
    send(&mut app, json!({"pair": {}})).await;
    let p = next_with(&mut app, "pairing").await;
    let code = p["pairing"]["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), CODE_LEN);
    assert_eq!(p["pairing"]["expires_in_s"], PAIR_CODE_TTL.as_secs());
    // Typed as people read it, with the dash and in small letters.
    let typed = crate::join::code_display(&code).to_lowercase();
    let (st, v) = type_code(&typed).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["console"], "127.0.0.1");
    let got = next_with(&mut app, "paired").await;
    assert_eq!(got["paired"]["id"], v["id"]);
    let token = got["paired"]["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    (app, token)
}

#[test]
fn a_store_keeps_only_the_tokens_hash_and_reads_it_back_after_a_restart() {
    let dir = temp_dir("store");
    let path = dir.join(PAIRED_FILE);
    let mut s = Store::open(Some(path.clone()));
    let (id, token) = s
        .add("192.168.1.50", Some("Living room".into()), 7)
        .unwrap();
    assert_eq!(s.check(&token).map(|a| a.id.clone()), Some(id.clone()));
    assert!(s.check("0".repeat(64).as_str()).is_none());
    assert!(s.check("").is_none());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains(&token), "the token itself is never kept");
    assert!(text.contains(&sha256_hex(&token)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A restart: the same token still works, for the same console.
    let s = Store::open(Some(path.clone()));
    let a = s.check(&token).unwrap();
    assert_eq!(
        (a.id.as_str(), a.console.as_str()),
        (id.as_str(), "192.168.1.50")
    );
    assert_eq!(a.name.as_deref(), Some("Living room"));
    // Unpaired: gone, after a restart too.
    let mut s = Store::open(Some(path.clone()));
    assert!(s.remove(&id).unwrap());
    assert!(!s.remove(&id).unwrap());
    assert!(Store::open(Some(path.clone())).check(&token).is_none());
    // A damaged file is set aside, not written over.
    std::fs::write(&path, b"{not json").unwrap();
    assert!(Store::open(Some(path.clone())).apps.is_empty());
    assert!(path.with_extension("bad").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_holds_so_many_apps_and_the_one_seen_longest_ago_makes_room() {
    let mut s = Store::open(None);
    let first = s.add("10.0.0.1", None, 1).unwrap();
    for i in 0..MAX_PAIRED {
        s.add(&format!("10.0.1.{i}"), None, 100 + i as u64).unwrap();
    }
    assert_eq!(s.apps.len(), MAX_PAIRED);
    assert!(s.check(&first.1).is_none());
}

#[test]
fn ten_wrong_codes_lock_pairing_and_void_the_codes_waiting() {
    let mut p = Pairing::default();
    let t = Instant::now();
    let (tx, _rx) = mpsc::channel(4);
    let ip: IpAddr = "192.168.1.50".parse().unwrap();
    let (code, _) = p.request("192.168.1.50", ip, 1, tx, t).unwrap();
    for i in 1..BAD_CODES_BEFORE_LOCK {
        assert_eq!(
            p.confirm("ZZZZZZ", t).err(),
            Some(ConfirmErr::Wrong(BAD_CODES_BEFORE_LOCK - i))
        );
    }
    assert_eq!(
        p.confirm("ZZZZZZ", t).err(),
        Some(ConfirmErr::Locked(PAIR_LOCK.as_secs()))
    );
    // Locked: even the right code does nothing now, and it is void after.
    assert!(matches!(p.confirm(&code, t), Err(ConfirmErr::Locked(_))));
    let later = t + PAIR_LOCK + Duration::from_secs(1);
    assert_eq!(p.confirm(&code, later).err(), Some(ConfirmErr::Wrong(9)));
    assert_eq!(p.locked_s(later), None);
}

#[test]
fn codes_expire_are_one_per_console_and_asking_is_limited_per_address() {
    let mut p = Pairing::default();
    let t = Instant::now();
    let ip: IpAddr = "192.168.1.50".parse().unwrap();
    let (tx, _rx) = mpsc::channel(4);
    let (a, _) = p.request("192.168.1.50", ip, 1, tx.clone(), t).unwrap();
    // The same console asks again: the first code stops working.
    let (b, _) = p.request("192.168.1.50", ip, 2, tx.clone(), t).unwrap();
    assert_eq!(p.pending.len(), 1);
    assert_ne!(a, b);
    // A third ask this minute still gets a code; a fourth is too many.
    assert!(p.request("192.168.1.50", ip, 3, tx.clone(), t).is_ok());
    assert_eq!(
        p.request("192.168.1.50", ip, 4, tx.clone(), t).err(),
        Some("slow_down")
    );
    // An expired code does not pair.
    let mut p = Pairing::default();
    let (c, _) = p.request("192.168.1.51", ip, 1, tx, t).unwrap();
    assert!(p
        .confirm(&c, t + PAIR_CODE_TTL + Duration::from_secs(1))
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_pairs_with_the_code_on_the_tv_and_its_token_works_after_a_restart() {
    let _turn = TURN.lock().await;
    let dir = temp_dir("pair");
    reset(Some(dir.clone()));
    let url = serve().await;
    let (mut app, token) = paired_app(&url).await;
    // Paired: the full status.
    let d = next_with(&mut app, "doc").await;
    assert_eq!(d["doc"]["paired"], true, "{d}");
    assert!(d["doc"]["together"].is_object());
    assert_eq!(d["doc"]["console"]["addr"], "127.0.0.1");
    // Listed for PS5Upload, connected, never with its token.
    let (ci, h) = local();
    let (_, list) = body(paired(ci, h).await).await;
    assert_eq!(list["apps"][0]["connected"], true, "{list}");
    assert!(!list.to_string().contains(&token));
    drop(app);
    // The engine restarts: the token still works.
    reset(Some(dir.clone()));
    let mut again = connect(&url, Some(&token)).await;
    let d = next_with(&mut again, "doc").await;
    assert_eq!(d["doc"]["paired"], true, "{d}");
    // Unpaired from PS5Upload: the app is told at once and sees only the minimal status.
    let id = list["apps"][0]["id"].as_str().unwrap().to_string();
    let (ci, h) = local();
    let (_, r) = body(unpair(ci, h, Json(UnpairBody { id })).await).await;
    assert_eq!(r["ok"], true);
    assert_eq!(next_with(&mut again, "auth").await["auth"], "revoked");
    let d = next_with(&mut again, "doc").await;
    assert_eq!(d["doc"]["paired"], false);
    // …and the token no longer works.
    let mut third = connect(&url, Some(&token)).await;
    assert_eq!(next_with(&mut third, "auth").await["auth"], "invalid");
    reset(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_app_without_a_token_gets_only_that_the_engine_is_there() {
    let _turn = TURN.lock().await;
    reset(None);
    let console = "127.0.0.1";
    let _rx = crate::join::test_party_with_ask(console, 9_001, "Secret Friend");
    let url = serve().await;
    let mut app = connect(&url, None).await;
    let d = next_with(&mut app, "doc").await;
    assert_eq!(
        d,
        json!({"doc": {"engine": true, "version": env!("CARGO_PKG_VERSION"), "paired": false}})
    );
    // Asking for anything else is refused, and nothing about the party leaks.
    send(
        &mut app,
        json!({"decide": {"ask": 9_001, "allow": true}, "req": 3}),
    )
    .await;
    let r = next_with(&mut app, "done").await;
    assert_eq!(
        r,
        json!({"done": "decide", "ok": false, "error": "not_paired", "req": 3})
    );
    let mut wrong = connect(&url, Some(&"ab".repeat(32))).await;
    assert_eq!(next_with(&mut wrong, "auth").await["auth"], "invalid");
    let d = next_with(&mut wrong, "doc").await;
    assert!(!d.to_string().contains("Secret Friend"));
    assert!(d["doc"].get("together").is_none());
    crate::join::test_close_party(console);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ten_wrong_codes_typed_in_ps5upload_lock_pairing() {
    let _turn = TURN.lock().await;
    reset(None);
    let url = serve().await;
    let mut app = connect(&url, None).await;
    next_with(&mut app, "doc").await;
    send(&mut app, json!({"pair": {}})).await;
    let code = next_with(&mut app, "pairing").await["pairing"]["code"]
        .as_str()
        .unwrap()
        .to_string();
    // Not a code at all: refused without counting.
    assert_eq!(type_code("hello!").await.1["error"], "code");
    let wrong = if code == "AAAAAA" { "BBBBBB" } else { "AAAAAA" };
    for left in (1..BAD_CODES_BEFORE_LOCK).rev() {
        let (st, v) = type_code(wrong).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(v["tries_left"], left);
    }
    let (st, v) = type_code(wrong).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"], "locked");
    // The right code no longer pairs either, and the app is never sent a token.
    let (st, _) = type_code(&code).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    let (ci, h) = local();
    let (_, list) = body(paired(ci, h).await).await;
    assert!(list["locked_s"].as_u64().is_some_and(|s| s > 0), "{list}");
    assert_eq!(list["apps"], json!([]));
    // An app asking for a code meanwhile is told.
    send(&mut app, json!({"pair": {}, "req": 1})).await;
    assert_eq!(next_with(&mut app, "done").await["error"], "locked");
    reset(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_answer_to_an_ask_wins() {
    let _turn = TURN.lock().await;
    reset(None);
    let console = "127.0.0.1";
    let rx = crate::join::test_party_with_ask(console, 9_002, "Sam");
    let url = serve().await;
    let (mut app, _token) = paired_app(&url).await;
    let d = next_with(&mut app, "doc").await;
    assert_eq!(d["doc"]["together"]["pending"][0]["name"], "Sam", "{d}");
    assert!(d["doc"]["together"]["invite"]["code"].is_string());
    send(
        &mut app,
        json!({"decide": {"ask": 9_002, "allow": true}, "req": 1}),
    )
    .await;
    let r = next_with(&mut app, "done").await;
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(rx.await.unwrap(), crate::join::Decision::Allow);
    // A second answer (here, or from PS5Upload) is too late.
    send(
        &mut app,
        json!({"decide": {"ask": 9_002, "allow": false}, "req": 2}),
    )
    .await;
    let r = next_with(&mut app, "done").await;
    assert_eq!(r["error"], "already_answered");
    assert_eq!(r["req"], 2);
    assert_eq!(
        crate::join::decide_on(console, 9_002, true),
        Err("already_answered")
    );
    // The app's own state shows in the status.
    send(
        &mut app,
        json!({"app": {"open": true, "foreground": true, "v": "13.0.0"}}),
    )
    .await;
    next_with(&mut app, "done").await;
    let mut seen = false;
    for _ in 0..20 {
        let d = next_with(&mut app, "doc").await;
        if d["doc"]["app"]["foreground"] == true {
            assert_eq!(d["doc"]["app"]["v"], "13.0.0");
            seen = true;
            break;
        }
    }
    assert!(seen);
    crate::join::test_close_party(console);
    reset(None);
}

#[test]
fn the_steady_status_leaves_out_what_changes_all_the_time() {
    let v = json!({"a": 1, "together": {"guests": [{"rtt_ms": 30, "name": "x"}],
        "invite": {"expires_in_s": 100, "code": "C"}}});
    assert_eq!(
        steady(&v),
        json!({"a": 1, "together": {"guests": [{"name": "x"}], "invite": {"code": "C"}}})
    );
}

#[test]
fn an_app_on_a_mapped_address_is_its_ipv4_console() {
    assert_eq!(
        console_of("::ffff:192.168.1.50".parse().unwrap()),
        "192.168.1.50"
    );
    assert_eq!(console_of("192.168.1.50".parse().unwrap()), "192.168.1.50");
}
