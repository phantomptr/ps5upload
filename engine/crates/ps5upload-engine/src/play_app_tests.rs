//! PS5Upload Play's channel: pairing, the token, what an app without one sees, and its intents.

use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TMsg;

type App =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The tests share the engine's one hub: they take turns, each from a clean one.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A fresh hub kept in `dir` (None: in memory), with no AVA1 keys pinned.
fn reset(dir: Option<PathBuf>) {
    *TEST_DIR.lock().unwrap() = dir;
    *HUB.lock().unwrap() = None;
    TEST_KEYS.lock().unwrap().clear();
}

/// The console at `addr` has the AVA1 key `key` (as if this engine pinned it there).
fn pin(addr: &str, key: &str) {
    TEST_KEYS
        .lock()
        .unwrap()
        .push((host_key(addr), key.to_string()));
}

const KEY_A: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const KEY_B: &str = "bb00000000000000000000000000000000000000000000000000000000000000";
const APP_ID: &str = "0123456789abcdef0123456789abcdef";

/// A pairing made earlier at `console` (with its key and the app's install id): the token.
fn paired_earlier(console: &str, key: Option<&str>, app_id: Option<&str>) -> String {
    with_hub(|h| {
        h.store().add(
            console,
            Some("Living room".into()),
            1,
            key.map(str::to_string),
            app_id.map(str::to_string),
        )
    })
    .unwrap()
    .1
}

async fn connect_hello(url: &str, hello: Value) -> App {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws.send(TMsg::Text(json!({ "hello": hello }).to_string().into()))
        .await
        .unwrap();
    ws
}

/// The next `play` event on the engine's feed that has `key`, within 5 s.
async fn next_event(sub: &mut crate::event_bus::Subscription, key: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let ev = tokio::time::timeout_at(deadline, sub.rx.recv())
            .await
            .unwrap_or_else(|_| panic!("no play event with {key} in time"))
            .unwrap();
        if ev.kind == Some("play") {
            let v: Value = serde_json::from_str(&ev.data).unwrap();
            if v.get(key).is_some() {
                return v;
            }
        }
    }
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
        .add("192.168.1.50", Some("Living room".into()), 7, None, None)
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
    let first = s.add("10.0.0.1", None, 1, None, None).unwrap();
    for i in 0..MAX_PAIRED {
        s.add(&format!("10.0.1.{i}"), None, 100 + i as u64, None, None)
            .unwrap();
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

#[test]
fn the_hello_keeps_a_usable_install_id_and_drops_anything_else() {
    let h = |v: Value| parse_hello(&json!({ "hello": v })).unwrap();
    assert_eq!(h(json!({"app_id": APP_ID})).app_id.as_deref(), Some(APP_ID));
    // base64url of 16 bytes (22 characters) is an install id too.
    assert!(h(json!({"app_id": "AbCdEfGhIjKlMnOpQrSt-_"}))
        .app_id
        .is_some());
    for bad in [
        json!("short"),
        json!("x".repeat(65)),
        json!("0123456789abcdef0123456789abcde!"),
        json!(12),
    ] {
        assert_eq!(h(json!({ "app_id": bad })).app_id, None, "{bad}");
    }
    assert_eq!(h(json!({})), Hello::default());
    assert!(parse_hello(&json!({"hi": {}})).is_none());
}

#[test]
fn a_paired_apps_file_from_before_still_reads_and_learns_the_new_fields() {
    let dir = temp_dir("old-file");
    let path = dir.join(PAIRED_FILE);
    let token = "ab".repeat(32);
    // What 13.0.0's first builds wrote: no console key, no install id.
    let old = json!({"version": 1, "apps": [{
        "id": "0a0b0c0d",
        "token_sha256": sha256_hex(&token),
        "console": "192.168.1.50",
        "name": "Living room",
        "paired_unix": 5,
        "last_seen_unix": null,
    }]});
    std::fs::write(&path, old.to_string()).unwrap();
    let mut s = Store::open(Some(path.clone()));
    let a = s.check(&token).unwrap().clone();
    assert_eq!(
        (a.console_key.as_deref(), a.app_id.as_deref()),
        (None, None)
    );
    // Seen again: what it did not have is kept from now on, and survives a restart.
    s.seen(&a.id, 9, Some(KEY_A.into()), Some(APP_ID.into()));
    let a = Store::open(Some(path.clone()))
        .check(&token)
        .unwrap()
        .clone();
    assert_eq!(a.console_key.as_deref(), Some(KEY_A));
    assert_eq!(a.app_id.as_deref(), Some(APP_ID));
    assert_eq!(a.last_seen_unix, Some(9));
    // …but never replaced once known.
    s.seen(&a.id, 10, Some(KEY_B.into()), Some("z".repeat(32)));
    let a = s.check(&token).unwrap();
    assert_eq!(a.console_key.as_deref(), Some(KEY_A));
    assert_eq!(a.app_id.as_deref(), Some(APP_ID));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_same_app_paired_again_replaces_its_old_pairing() {
    let mut s = Store::open(None);
    let (_, first) = s
        .add("10.0.0.1", None, 1, None, Some(APP_ID.into()))
        .unwrap();
    let (_, other) = s.add("10.0.0.2", None, 1, None, None).unwrap();
    let (_, second) = s
        .add("10.0.0.1", None, 2, None, Some(APP_ID.into()))
        .unwrap();
    assert!(s.check(&first).is_none());
    assert!(s.check(&second).is_some());
    assert!(s.check(&other).is_some());
    assert_eq!(s.apps.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_console_that_moved_keeps_its_pairing_when_its_ava1_key_proves_it() {
    let _turn = TURN.lock().await;
    reset(None);
    let url = serve().await;
    // Paired at 192.168.1.50; the same console (the same AVA1 key) now answers at 127.0.0.1.
    let token = paired_earlier("192.168.1.50", Some(KEY_A), None);
    pin("127.0.0.1", KEY_A);
    let mut app = connect(&url, Some(&token)).await;
    let d = next_with(&mut app, "doc").await;
    assert_eq!(d["doc"]["paired"], true, "{d}");
    assert_eq!(d["doc"]["console"]["addr"], "127.0.0.1");
    let a = with_hub(|h| h.store().check(&token).cloned()).unwrap();
    assert_eq!(a.console, "127.0.0.1", "re-bound to the new address");
    // A token whose console has another key than the one at this address: another console, even
    // with the same install id.
    let other = paired_earlier("192.168.1.60", Some(KEY_B), Some(APP_ID));
    let mut app2 = connect_hello(&url, json!({"token": other, "app_id": APP_ID})).await;
    assert_eq!(next_with(&mut app2, "auth").await["auth"], "other_console");
    assert_eq!(next_with(&mut app2, "doc").await["doc"]["paired"], false);
    let b = with_hub(|h| h.store().check(&other).cloned()).unwrap();
    assert_eq!(b.console, "192.168.1.60", "left where it was");
    reset(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_console_with_no_key_is_confirmed_in_ps5upload_with_one_click() {
    let _turn = TURN.lock().await;
    reset(None);
    let url = serve().await;
    let token = paired_earlier("192.168.1.50", None, Some(APP_ID));
    let mut sub = crate::event_bus::bus().subscribe(None);
    let mut app = connect_hello(&url, json!({"token": token, "app_id": APP_ID})).await;
    assert_eq!(next_with(&mut app, "auth").await["auth"], "confirm_move");
    assert_eq!(next_with(&mut app, "doc").await["doc"]["paired"], false);
    // PS5Upload's own feed is told, and lists it.
    let ev = next_event(&mut sub, "moved").await;
    assert_eq!(
        ev["moved"],
        json!({"console": "127.0.0.1", "id": ev["moved"]["id"], "name": "Living room",
            "addr_old": "192.168.1.50", "addr_new": "127.0.0.1"})
    );
    let (ci, hd) = local();
    let (_, list) = body(paired(ci, hd).await).await;
    assert_eq!(list["moves"][0]["addr_new"], "127.0.0.1", "{list}");
    assert_eq!(list["moves"][0]["addr_old"], "192.168.1.50");
    // Not moved until confirmed.
    let a = with_hub(|h| h.store().check(&token).cloned()).unwrap();
    assert_eq!(a.console, "192.168.1.50");
    // From another computer: refused.
    let lan = ConnectInfo("192.168.1.9:5555".parse().unwrap());
    let mut lan_h = HeaderMap::new();
    lan_h.insert("host", "192.168.1.2:3000".parse().unwrap());
    let r = moved(
        lan,
        lan_h,
        Json(MovedBody {
            console_old: None,
            addr: "127.0.0.1".into(),
            ok: true,
        }),
    )
    .await;
    assert!(!r.status().is_success());
    // One click here: moved, and the app is told at once.
    let (ci, hd) = local();
    let (st, v) = body(
        moved(
            ci,
            hd,
            Json(MovedBody {
                console_old: Some("192.168.1.50".into()),
                addr: "127.0.0.1".into(),
                ok: true,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["moved"], true);
    assert_eq!(v["console"], "127.0.0.1");
    assert_eq!(next_with(&mut app, "auth").await["auth"], "moved");
    let d = next_with(&mut app, "doc").await;
    assert_eq!(d["doc"]["paired"], true, "{d}");
    // Nothing waits any more; the token works here from now on.
    let (ci, hd) = local();
    let (st, v) = body(
        moved(
            ci,
            hd,
            Json(MovedBody {
                console_old: None,
                addr: "127.0.0.1".into(),
                ok: true,
            }),
        )
        .await,
    )
    .await;
    assert_eq!(
        (st, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_waiting"))
    );
    let mut again = connect_hello(&url, json!({"token": token, "app_id": APP_ID})).await;
    assert_eq!(next_with(&mut again, "doc").await["doc"]["paired"], true);
    reset(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_moved_console_needs_the_same_install_id_and_can_be_refused() {
    let _turn = TURN.lock().await;
    reset(None);
    let url = serve().await;
    let token = paired_earlier("192.168.1.50", None, Some(APP_ID));
    // No install id, or another one: another console (as before).
    for hello in [
        json!({"token": token}),
        json!({"token": token, "app_id": "f".repeat(32)}),
    ] {
        let mut app = connect_hello(&url, hello).await;
        assert_eq!(next_with(&mut app, "auth").await["auth"], "other_console");
    }
    // A pairing that never had an install id is never offered.
    let plain = paired_earlier("192.168.1.51", None, None);
    let mut app = connect_hello(&url, json!({"token": plain, "app_id": APP_ID})).await;
    assert_eq!(next_with(&mut app, "auth").await["auth"], "other_console");
    // Refused in PS5Upload: the app is told it is another console; nothing moved.
    let mut app = connect_hello(&url, json!({"token": token, "app_id": APP_ID})).await;
    assert_eq!(next_with(&mut app, "auth").await["auth"], "confirm_move");
    let (ci, hd) = local();
    let (st, v) = body(
        moved(
            ci,
            hd,
            Json(MovedBody {
                console_old: None,
                addr: "127.0.0.1".into(),
                ok: false,
            }),
        )
        .await,
    )
    .await;
    assert_eq!((st, v["moved"].as_bool()), (StatusCode::OK, Some(false)));
    assert_eq!(next_with(&mut app, "auth").await["auth"], "other_console");
    let a = with_hub(|h| h.store().check(&token).cloned()).unwrap();
    assert_eq!(a.console, "192.168.1.50");
    reset(None);
}

async fn click_pair(console: &str) -> (StatusCode, Value) {
    let (ci, h) = local();
    body(
        pair_confirm(
            ci,
            h,
            Json(PairConfirmBody {
                console: console.to_string(),
            }),
        )
        .await,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_on_the_tv_is_offered_to_ps5upload_and_pairs_with_one_click() {
    let _turn = TURN.lock().await;
    reset(None);
    let url = serve().await;
    let mut sub = crate::event_bus::bus().subscribe(None);
    let mut feed = HashMap::new();
    // Nothing waits: nothing to click.
    let (st, v) = click_pair("127.0.0.1").await;
    assert_eq!(
        (st, v["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_waiting"))
    );
    let mut app = connect_hello(&url, json!({"app_id": APP_ID})).await;
    next_with(&mut app, "doc").await;
    send(&mut app, json!({"pair": {}})).await;
    let code = next_with(&mut app, "pairing").await["pairing"]["code"]
        .as_str()
        .unwrap()
        .to_string();
    let shown = crate::join::code_display(&code);
    // PS5Upload's own feed and list show the code to compare.
    waiting_round(&mut feed, Instant::now());
    let ev = next_event(&mut sub, "pair_waiting").await;
    assert_eq!(ev["pair_waiting"]["console"], "127.0.0.1");
    assert_eq!(ev["pair_waiting"]["code_display"], shown.as_str());
    // Told once, not every look.
    waiting_round(&mut feed, Instant::now());
    let (ci, h) = local();
    let (_, list) = body(paired(ci, h).await).await;
    assert_eq!(list["waiting"][0]["code_display"], shown.as_str(), "{list}");
    assert!(list["waiting"][0].get("name").is_some());
    // From another computer: refused, and the code still waits.
    let lan = ConnectInfo("192.168.1.9:5555".parse().unwrap());
    let mut lan_h = HeaderMap::new();
    lan_h.insert("host", "192.168.1.2:3000".parse().unwrap());
    let r = pair_confirm(
        lan,
        lan_h,
        Json(PairConfirmBody {
            console: "127.0.0.1".into(),
        }),
    )
    .await;
    assert!(!r.status().is_success());
    // One click: paired exactly as typing the code would, install id kept.
    let (st, v) = click_pair("127.0.0.1").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["console"], "127.0.0.1");
    let got = next_with(&mut app, "paired").await;
    assert_eq!(got["paired"]["id"], v["id"]);
    let token = got["paired"]["token"].as_str().unwrap().to_string();
    let a = with_hub(|h| h.store().check(&token).cloned()).unwrap();
    assert_eq!(a.app_id.as_deref(), Some(APP_ID));
    assert_eq!(next_with(&mut app, "doc").await["doc"]["paired"], true);
    // The code no longer waits: the feed says so, and a second click finds nothing.
    waiting_round(&mut feed, Instant::now());
    let ev = next_event(&mut sub, "pair_ended").await;
    assert_eq!(ev["pair_ended"]["console"], "127.0.0.1");
    let (st, _) = click_pair("127.0.0.1").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // The code typed by hand still works too.
    let (_app2, _token2) = paired_app(&url).await;
    reset(None);
}

#[test]
fn a_click_is_not_a_guess_but_a_lock_still_holds() {
    let mut p = Pairing::default();
    let t = Instant::now();
    let (tx, _rx) = mpsc::channel(4);
    let ip: IpAddr = "192.168.1.50".parse().unwrap();
    p.request("192.168.1.50", ip, 1, tx, t).unwrap();
    // Clicking a console with nothing waiting does not count toward the lock.
    for _ in 0..BAD_CODES_BEFORE_LOCK + 2 {
        assert!(matches!(
            p.take("192.168.1.51", t),
            Err(ConfirmErr::Wrong(_))
        ));
    }
    assert_eq!(p.locked_s(t), None);
    // Locked by wrong codes typed: a click waits too.
    for _ in 0..BAD_CODES_BEFORE_LOCK {
        let _ = p.confirm("ZZZZZZ", t);
    }
    assert!(matches!(
        p.take("192.168.1.50", t),
        Err(ConfirmErr::Locked(_))
    ));
}
