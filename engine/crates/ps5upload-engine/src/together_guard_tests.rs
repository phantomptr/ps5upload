//! The "friends playing" guard (together_guard.rs) on every route it covers, through the real
//! router (`build_app`): 409 `friends_playing` while a friend is in, and on with each of the three
//! confirms (header, query, body). Consoles are TEST-NET addresses: a request that gets past the
//! guard fails at the console call (no transport in tests) or at a later refusal held on purpose
//! (a job running on the console), never reaching a console.

use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request as HttpRequest},
};
use serde_json::{json, Value};
use std::net::SocketAddr;
use tower::ServiceExt;

fn app() -> Router {
    let (events_tx, _) = broadcast::channel(64);
    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        default_ps5_addr: "192.0.2.1".to_string(),
        events_tx,
    };
    build_app(
        state,
        Default::default(),
        LoopbackGuardConfig {
            allowed_ips: Vec::new().into(),
        },
        "",
    )
}

/// How a request confirms.
#[derive(Clone, Copy, Debug)]
enum Confirm {
    No,
    Header,
    Query,
    Body,
}

async fn post(
    app: &Router,
    path: &str,
    body: Value,
    content: &str,
    confirm: Confirm,
) -> (StatusCode, Value) {
    let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    let mut body = body;
    let mut uri = path.to_string();
    if matches!(confirm, Confirm::Query) {
        uri.push(if uri.contains('?') { '&' } else { '?' });
        uri.push_str("confirm_friends=1");
    }
    if matches!(confirm, Confirm::Body) {
        body["confirm_friends"] = json!(true);
    }
    let mut b = HttpRequest::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:19113")
        .header(header::CONTENT_TYPE, content);
    if matches!(confirm, Confirm::Header) {
        b = b.header("X-Confirm-Friends", "1");
    }
    b.extensions_mut().unwrap().insert(ConnectInfo(peer));
    let bytes = match body {
        Value::String(s) => s.into_bytes(),
        other => other.to_string().into_bytes(),
    };
    let resp = app
        .clone()
        .oneshot(b.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let raw = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&raw).unwrap_or(Value::Null))
}

fn asked(r: &(StatusCode, Value)) -> bool {
    r.0 == StatusCode::CONFLICT && r.1["error"] == "friends_playing"
}

/// `path` with `body` asks first while a friend is in on `console`, and goes on with each confirm
/// in `forms`.
async fn guarded(app: &Router, console: &str, path: &str, body: Value, forms: &[Confirm]) {
    let r = post(app, path, body.clone(), "application/json", Confirm::No).await;
    assert!(asked(&r), "{path} {body}: {r:?}");
    assert_eq!(r.1["friends"], json!(["Mia"]), "{path}");
    assert_eq!(r.1["message"], together_guard::MESSAGE, "{path}");
    assert!(r.1["console"].as_str().is_some_and(|c| c.contains(console)));
    for form in forms {
        let r = post(app, path, body.clone(), "application/json", *form).await;
        assert!(!asked(&r), "{path} {body} with {form:?}: {r:?}");
        // Through the guard to the console call or a later refusal, not refused as unreadable.
        assert!(
            !matches!(
                r.0,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "{path} {body} with {form:?}: {r:?}"
        );
    }
}

/// Not asked at all (nobody would be cut off by it).
async fn not_guarded(app: &Router, path: &str, body: Value) {
    let r = post(app, path, body.clone(), "application/json", Confirm::No).await;
    assert!(!asked(&r), "{path} {body}: {r:?}");
}

const ALL: &[Confirm] = &[Confirm::Header, Confirm::Query, Confirm::Body];

#[tokio::test]
async fn power_kill_close_and_launch_ask_while_friends_play() {
    let c = "192.0.2.61";
    let app = app();
    // Nobody in: no question.
    not_guarded(
        &app,
        "/api/ps5/power/control",
        json!({"addr": c, "action": "standby"}),
    )
    .await;
    together_guard_party(c);
    for action in ["standby", "reboot", "shutdown"] {
        let body = json!({"addr": c, "action": action});
        guarded(&app, c, "/api/ps5/power/control", body, ALL).await;
    }
    not_guarded(
        &app,
        "/api/ps5/power/control",
        json!({"addr": c, "action": "tick"}),
    )
    .await;
    let kill = json!({"addr": c, "pid": 1234});
    guarded(&app, c, "/api/ps5/process/kill", kill, ALL).await;
    for action in ["kill", "suspend"] {
        let body = json!({"addr": c, "action": action, "app_id": 7});
        guarded(&app, c, "/api/ps5/app/lifecycle", body, ALL).await;
    }
    for action in ["resume", "list"] {
        let body = json!({"addr": c, "action": action, "app_id": 7});
        not_guarded(&app, "/api/ps5/app/lifecycle", body).await;
    }
    let launch = json!({"addr": c, "title_id": "PPSA00001"});
    guarded(&app, c, "/api/ps5/app/launch", launch, ALL).await;
    guarded(&app, c, "/api/play/app/open", json!({"addr": c}), ALL).await;
    // Another console: nobody there.
    let other = json!({"addr": "192.0.2.62", "pid": 1});
    not_guarded(&app, "/api/ps5/process/kill", other).await;
    crate::join::test_close_party(c);
    not_guarded(&app, "/api/ps5/process/kill", json!({"addr": c, "pid": 1})).await;
}

fn together_guard_party(console: &str) {
    crate::join::test_party_with_guest(console, "Mia");
}

/// A helper image as the send recognises one.
fn helper_elf() -> Vec<u8> {
    let mut b = b"\x7fELF".to_vec();
    b.resize(256, 0);
    b.extend_from_slice(b"ps5upload helper");
    b.resize(1024, 0);
    b
}

#[tokio::test]
async fn helper_stop_remove_and_sends_ask_while_friends_play() {
    let c = "192.0.2.63";
    let app = app();
    together_guard_party(c);
    // A job on the console: what gets past the guard is refused by the helper's own rules, with
    // nothing sent.
    let _busy = helper_stop::try_enter(c).expect("busy");
    guarded(&app, c, "/api/ps5/helper/stop", json!({"addr": c}), ALL).await;
    guarded(
        &app,
        c,
        "/api/ps5/helper/uninstall",
        json!({"addr": c}),
        ALL,
    )
    .await;
    let check = json!({"addr": c, "check": true});
    not_guarded(&app, "/api/ps5/helper/uninstall", check).await;
    let send_check = json!({"host": c, "helper": true});
    guarded(&app, c, "/api/payload/send-check", send_check, ALL).await;
    not_guarded(
        &app,
        "/api/payload/send-check",
        json!({"host": c, "helper": false}),
    )
    .await;
    let restore = json!({"ps5_addr": c});
    guarded(&app, c, "/api/pkg/payload-restore", restore, ALL).await;

    // A helper sent as a file on this computer (JSON form).
    let dir = std::env::temp_dir().join(format!("ps5u-friends-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ps5upload.elf");
    std::fs::write(&file, helper_elf()).unwrap();
    let by_path = json!({"host": c, "path": file.to_string_lossy()});
    guarded(&app, c, "/api/payload/send", by_path, ALL).await;

    // A helper sent as the body itself: header or query (there is no JSON body to carry it).
    let raw = |confirm| {
        let app = app.clone();
        async move {
            post(
                &app,
                &format!("/api/payload/send?host={c}&port=9021"),
                Value::String(String::from_utf8_lossy(&helper_elf()).into_owned()),
                "application/octet-stream",
                confirm,
            )
            .await
        }
    };
    assert!(asked(&raw(Confirm::No).await));
    assert!(!asked(&raw(Confirm::Header).await));
    assert!(!asked(&raw(Confirm::Query).await));
    crate::join::test_close_party(c);
    let _ = std::fs::remove_dir_all(&dir);
}
