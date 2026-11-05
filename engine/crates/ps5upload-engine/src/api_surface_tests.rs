//! The HTTP surface as a client meets it, through the real router (`build_app`) and all its
//! layers. Nothing here reaches a console or the network beyond this machine's loopback.

use super::*;
use axum::{
    body::{to_bytes, Body},
    http::{header, Method, Request as HttpRequest, Response as HttpResponse},
};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;
use tower::ServiceExt;

fn state() -> AppState {
    let (events_tx, _) = broadcast::channel(2048);
    AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        // TEST-NET: never reached; these tests name no console for a console call.
        default_ps5_addr: "192.0.2.1".to_string(),
        events_tx,
    }
}

fn app_with(state: AppState, installs: pkg_install::PkgInstallStateHandle, base: &str) -> Router {
    build_app(
        state,
        installs,
        LoopbackGuardConfig {
            allowed_ips: Vec::new().into(),
        },
        base,
    )
}

fn app() -> Router {
    app_with(state(), Default::default(), "")
}

const LOOPBACK: &str = "127.0.0.1:50000";
const LAN: &str = "192.168.1.30:50000";

fn request(method: Method, path: &str, peer: &str) -> axum::http::request::Builder {
    let peer: SocketAddr = peer.parse().unwrap();
    let mut b = HttpRequest::builder().method(method).uri(path);
    b.extensions_mut().unwrap().insert(ConnectInfo(peer));
    b.header(header::HOST, "127.0.0.1:19113")
}

async fn call(app: &Router, req: HttpRequest<Body>) -> HttpResponse<Body> {
    app.clone().oneshot(req).await.unwrap()
}

async fn get(app: &Router, path: &str) -> HttpResponse<Body> {
    call(
        app,
        request(Method::GET, path, LOOPBACK)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post_json(app: &Router, path: &str, body: &str) -> HttpResponse<Body> {
    call(
        app,
        request(Method::POST, path, LOOPBACK)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn post_async(app: &Router, path: &str, body: &str) -> HttpResponse<Body> {
    call(
        app,
        request(Method::POST, path, LOOPBACK)
            .header(header::CONTENT_TYPE, "application/json")
            .header("prefer", "respond-async")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

async fn json_of(resp: HttpResponse<Body>) -> Value {
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("json"), "content type {ct:?}");
    let b = to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    serde_json::from_slice(&b).unwrap()
}

/// `{error, code}`, with the code a status names.
async fn assert_shaped(resp: HttpResponse<Body>, status: StatusCode, code: &str) -> String {
    assert_eq!(resp.status(), status);
    let v = json_of(resp).await;
    assert_eq!(v["code"], code, "{v}");
    v["error"].as_str().expect("error text").to_string()
}

// ── Errors ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn extractor_refusals_are_json_with_their_status() {
    let app = app();
    let r = post_json(&app, "/api/ps5/fs/delete", "{not json").await;
    let text = assert_shaped(r, StatusCode::BAD_REQUEST, "bad_request").await;
    assert!(!text.is_empty());

    let r = post_json(&app, "/api/ps5/fs/delete", r#"{"addr":"192.0.2.1"}"#).await;
    let text = assert_shaped(r, StatusCode::UNPROCESSABLE_ENTITY, "unprocessable_entity").await;
    assert!(text.contains("path"), "names the missing field: {text}");

    let r = call(
        &app,
        request(Method::POST, "/api/ps5/fs/delete", LOOPBACK)
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_shaped(
        r,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;

    let r = get(&app, "/api/local/path-kind").await;
    assert_shaped(r, StatusCode::BAD_REQUEST, "bad_request").await;
}

#[tokio::test]
async fn a_handler_json_error_keeps_its_own_shape() {
    let app = app();
    let r = post_json(&app, "/api/ps5/fs/delete", r#"{"path":"/data/x"}"#).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let v = json_of(r).await;
    assert!(v["error"].as_str().unwrap().contains("addr is required"));
    assert!(v.get("code").is_none(), "untouched: {v}");
}

#[tokio::test]
async fn unknown_routes_and_wrong_methods_answer_json() {
    let app = app();
    let text = assert_shaped(
        get(&app, "/api/no/such/route").await,
        StatusCode::NOT_FOUND,
        "not_found",
    )
    .await;
    assert!(text.contains("/api/no/such/route"), "{text}");

    let r = post_json(&app, "/api/jobs", "{}").await;
    assert_eq!(r.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(r.headers().get(header::ALLOW).is_some(), "Allow is kept");
    assert_shaped(r, StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed").await;
}

#[cfg(not(feature = "webui"))]
#[tokio::test]
async fn outside_the_api_a_404_stays_a_plain_404() {
    let r = get(&app(), "/not-api").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert!(r.headers().get(header::CONTENT_TYPE).is_none());
}

#[tokio::test]
async fn the_loopback_guard_refuses_in_json_and_says_what_to_set() {
    let app = app();
    let r = call(
        &app,
        request(Method::GET, "/api/jobs", LAN)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let text = assert_shaped(r, StatusCode::FORBIDDEN, "forbidden").await;
    assert!(text.contains("PS5UPLOAD_ALLOW_IP=192.168.1.30"), "{text}");

    // Not the API: a person's browser reads this one.
    let r = call(
        &app,
        request(Method::GET, "/", LAN).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert!(r.headers()[header::CONTENT_TYPE]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
}

#[tokio::test]
async fn a_cross_site_refusal_names_both_sides_of_the_comparison() {
    let r = call(
        &app(),
        request(Method::POST, "/api/ps5/fs/delete", LOOPBACK)
            .header("origin", "http://ps5upload.home")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    let text = assert_shaped(r, StatusCode::FORBIDDEN, "forbidden").await;
    assert!(text.contains("http://ps5upload.home"), "{text}");
    assert!(text.contains("127.0.0.1:19113"), "{text}");
    assert!(text.contains("Host"), "{text}");
}

// ── PS5UPLOAD_BASE_URL ──────────────────────────────────────────────────────

#[tokio::test]
async fn the_prefix_is_seen_through_by_the_guards() {
    let app = app_with(state(), Default::default(), "/ps5");
    let lan = |path: &str| request(Method::GET, path, LAN).body(Body::empty()).unwrap();
    // What a phone or a friend opens, from the LAN.
    assert_eq!(call(&app, lan("/ps5/pad")).await.status(), StatusCode::OK);
    assert_eq!(call(&app, lan("/ps5/join")).await.status(), StatusCode::OK);
    // Their sockets get past the guard (and then want an upgrade, which this is not).
    for ws in ["/ps5/api/pad/ws", "/ps5/api/join/ws"] {
        assert_ne!(call(&app, lan(ws)).await.status(), StatusCode::FORBIDDEN);
    }
    // The rest of the API stays closed to the LAN, prefix or not.
    let r = call(&app, lan("/ps5/api/jobs")).await;
    assert_shaped(r, StatusCode::FORBIDDEN, "forbidden").await;
    let r = call(&app, lan("/ps5/x/../pad")).await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // From this computer: the API under the prefix, its 404 in shape.
    assert_eq!(get(&app, "/ps5/api/jobs").await.status(), StatusCode::OK);
    let r = get(&app, "/ps5/api/nope").await;
    assert_shaped(r, StatusCode::NOT_FOUND, "not_found").await;
    // Outside the prefix, an /api path is still an API path.
    let r = get(&app, "/api/jobs").await;
    assert_shaped(r, StatusCode::NOT_FOUND, "not_found").await;
}

#[test]
fn the_prefix_is_normalized_however_it_was_written() {
    use crate::host_guard::normalize_base;
    for raw in ["/ps5/", "ps5", "/ps5", " ps5/ "] {
        assert_eq!(normalize_base(raw).as_deref(), Some("/ps5"), "{raw:?}");
    }
    assert_eq!(normalize_base("").as_deref(), Some(""));
    assert_eq!(normalize_base("/").as_deref(), Some(""));
}

// ── Console naming ──────────────────────────────────────────────────────────

#[test]
fn every_console_name_reaches_the_same_field() {
    for key in ["addr", "console", "host"] {
        let body = json!({ key: "192.0.2.7", "path": "/data/x" });
        let req: FsPathReq = serde_json::from_value(body).unwrap();
        assert_eq!(req.addr.as_deref(), Some("192.0.2.7"), "{key}");

        let body = json!({ key: "192.0.2.7", "credential": "c" });
        let req: PowerWakeReq = serde_json::from_value(body).unwrap();
        assert_eq!(req.host, "192.0.2.7", "{key}");

        let body = json!({ key: "192.0.2.7", "source": { "host_file": "/x.pkg" } });
        let req: install::InstallRequest = serde_json::from_value(body).unwrap();
        assert_eq!(req.ps5_addr, "192.0.2.7", "{key}");
    }
    let q: install::AddrQuery = serde_json::from_value(json!({ "console": "192.0.2.7" })).unwrap();
    assert_eq!(q.ps5_addr, "192.0.2.7");
}

#[test]
fn a_struct_with_two_addresses_keeps_them_apart() {
    let req: PowerPairReq =
        serde_json::from_value(json!({ "console": "192.0.2.7", "host": "192.0.2.8" })).unwrap();
    assert_eq!(req.addr.as_deref(), Some("192.0.2.7"));
    assert_eq!(req.host, "192.0.2.8");
}

#[tokio::test]
async fn a_query_takes_the_other_names_too() {
    let app = app();
    // A closed loopback port: the route's answer, with `ip` given as `addr`.
    let r = get(
        &app,
        "/api/ps5/port-check?addr=127.0.0.1&port=1&timeout_ms=100",
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(json_of(r).await["open"], false);
}

// ── Prefer: respond-async ───────────────────────────────────────────────────

async fn wait_done(app: &Router, id: &str) -> Value {
    for _ in 0..200 {
        let v = json_of(get(app, &format!("/api/ops/{id}")).await).await;
        if v["status"] == "done" {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("operation {id} never finished");
}

async fn op_id_of(resp: HttpResponse<Body>) -> String {
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert_eq!(resp.headers()["preference-applied"], "respond-async");
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string();
    let id = json_of(resp).await["op_id"].as_str().unwrap().to_string();
    assert_eq!(location, format!("/api/ops/{id}"));
    id
}

#[tokio::test]
async fn a_route_runs_in_the_background_and_its_answer_is_kept() {
    let app = app();
    let id = op_id_of(post_async(&app, "/api/ps5/fs/delete", r#"{"path":"/data/x"}"#).await).await;
    let v = wait_done(&app, &id).await;
    assert_eq!(v["http_status"], 400);
    assert!(v["body"]["error"].as_str().unwrap().contains("addr"));
    assert_eq!(v["path"], "/api/ps5/fs/delete");
    assert!(v["fs_op_id"].as_u64().unwrap() != 0, "it was given an id");

    let list = json_of(get(&app, "/api/ops").await).await;
    let row = list["ops"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == id.as_str())
        .expect("listed");
    assert_eq!(row["status"], "done");
    assert!(row.get("body").is_none(), "the list stays small");
}

#[tokio::test]
async fn the_status_route_is_named_under_the_prefix() {
    let app = app_with(state(), Default::default(), "/ps5");
    let r = post_async(&app, "/ps5/api/ps5/fs/delete", r#"{"path":"/x"}"#).await;
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let location = r.headers()[header::LOCATION].to_str().unwrap().to_string();
    assert!(location.starts_with("/ps5/api/ops/"), "{location}");
    assert_eq!(get(&app, &location).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_background_extractor_refusal_has_the_same_shape() {
    let app = app();
    let id = op_id_of(post_async(&app, "/api/ps5/fs/delete", "{nope").await).await;
    let v = wait_done(&app, &id).await;
    assert_eq!(v["http_status"], 400);
    assert_eq!(v["body"]["code"], "bad_request");
}

#[tokio::test]
async fn no_prefer_header_no_change() {
    let app = app();
    let r = post_json(&app, "/api/ps5/fs/delete", r#"{"path":"/data/x"}"#).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn only_a_post_to_a_real_route_goes_to_the_background() {
    let app = app();
    let r = post_async(&app, "/api/no/such/route", "{}").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "the 404 is this answer");

    let r = call(
        &app,
        request(Method::GET, "/api/jobs", LOOPBACK)
            .header("prefer", "respond-async")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);

    // The operations routes themselves answer at once.
    let r = post_async(&app, "/api/ops/op-nothing/cancel", "{}").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_refused_caller_starts_nothing() {
    let app = app();
    let r = call(
        &app,
        request(Method::POST, "/api/ps5/fs/delete", LAN)
            .header("prefer", "respond-async")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_answer_to_prefer_carries_cors_for_an_allowed_origin() {
    let r = call(
        &app(),
        request(Method::POST, "/api/ps5/fs/delete", LOOPBACK)
            .header("origin", "http://localhost:1420")
            .header("prefer", "respond-async")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"path":"/x"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    assert!(r
        .headers()
        .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
}

#[tokio::test]
async fn an_unknown_operation_is_a_json_404() {
    let app = app();
    for id in ["op-nope", "job:not-a-uuid", "install:nope"] {
        let r = get(&app, &format!("/api/ops/{id}")).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{id}");
        assert!(json_of(r).await["error"].is_string());
    }
    let r = post_json(&app, "/api/ops/install:nope/cancel", "{}").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

fn finished_job(started_at_ms: u64) -> JobState {
    JobState::Failed {
        started_at_ms,
        completed_at_ms: started_at_ms + 1,
        elapsed_ms: 1,
        error: "boom".to_string(),
        error_reason: None,
        error_detail: None,
        error_console: None,
        error_space: None,
    }
}

#[tokio::test]
async fn one_poller_reads_jobs_and_installs_too() {
    let st = state();
    let installs: pkg_install::PkgInstallStateHandle = Default::default();
    let app = app_with(st.clone(), installs.clone(), "");

    let job = Uuid::new_v4();
    set_job(&st.jobs, &st.events_tx, job, finished_job(5));
    let install = installs.jobs.begin("192.0.2.9").unwrap();

    let list = json_of(get(&app, "/api/ops").await).await;
    let ids: Vec<&str> = list["ops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&format!("job:{job}").as_str()), "{ids:?}");
    assert!(
        ids.contains(&format!("install:{install}").as_str()),
        "{ids:?}"
    );

    let v = json_of(get(&app, &format!("/api/ops/job:{job}")).await).await;
    assert_eq!(v["kind"], "job");
    assert_eq!(v["status"], "done");
    assert_eq!(v["http_status"], 500);
    assert_eq!(v["body"]["error"], "boom");

    let v = json_of(get(&app, &format!("/api/ops/install:{install}")).await).await;
    assert_eq!(v["kind"], "install");
    assert_eq!(v["status"], "running");
    assert!(v["http_status"].is_null());
    assert_eq!(v["body"]["job"], install.as_str());

    // Each one's own cancel.
    let v =
        json_of(post_json(&app, &format!("/api/ops/install:{install}/cancel"), "{}").await).await;
    assert_eq!(v["cancelled"], true);
    assert!(installs.jobs.is_cancelled(&install));
    let v = json_of(post_json(&app, &format!("/api/ops/job:{job}/cancel"), "{}").await).await;
    assert_eq!(v["cancelled"], false, "it already ended");

    let limited = json_of(get(&app, "/api/ops?limit=1").await).await;
    assert_eq!(limited["ops"].as_array().unwrap().len(), 1);
    let r = get(&app, "/api/ops?limit=lots").await;
    assert_shaped(r, StatusCode::BAD_REQUEST, "bad_request").await;
}

/// A route that only ends when it is told, in the same layers as the real ones.
fn slow_app() -> (Router, Arc<tokio::sync::Notify>) {
    let release = Arc::new(tokio::sync::Notify::new());
    let held = release.clone();
    let reg = Arc::new(ops::OpRegistry::default());
    let app = Router::new()
        .route(
            "/api/slow",
            post(move || {
                let held = held.clone();
                async move {
                    held.notified().await;
                    Json(json!({ "finished": true }))
                }
            }),
        )
        .route("/api/ps5/fs/move", post(|body: String| async move { body }))
        .merge(ops::router(reg.clone(), state(), Default::default()))
        .layer(middleware::from_fn_with_state(reg, ops::respond_async));
    (app, release)
}

#[tokio::test]
async fn running_then_done_with_the_routes_own_answer() {
    let (app, release) = slow_app();
    let id = op_id_of(post_async(&app, "/api/slow", "{}").await).await;
    let v = json_of(get(&app, &format!("/api/ops/{id}")).await).await;
    assert_eq!(v["status"], "running");
    assert!(v["http_status"].is_null());
    // `notify_one` keeps a permit if the route has not reached its wait yet.
    release.notify_one();
    let v = wait_done(&app, &id).await;
    assert_eq!(v["http_status"], 200);
    assert_eq!(v["body"]["finished"], true);
}

#[tokio::test]
async fn cancel_without_a_route_cancel_stops_waiting_and_says_so() {
    let (app, _release) = slow_app();
    let id = op_id_of(post_async(&app, "/api/slow", "{}").await).await;
    let v = json_of(post_json(&app, &format!("/api/ops/{id}/cancel"), "{}").await).await;
    assert_eq!(v["abandoned"], true);
    assert_eq!(v["cancelled"], false);
    assert!(v["message"].as_str().unwrap().contains("may still finish"));
    let v = wait_done(&app, &id).await;
    assert_eq!(v["abandoned"], true);
    assert_eq!(v["http_status"], 499);
    assert_eq!(v["body"]["code"], "abandoned");
    // A second cancel finds it done.
    let v = json_of(post_json(&app, &format!("/api/ops/{id}/cancel"), "{}").await).await;
    assert_eq!(v["status"], "done");
}

#[tokio::test]
async fn the_route_receives_the_injected_op_id() {
    let (app, _release) = slow_app();
    let id = op_id_of(
        post_async(
            &app,
            "/api/ps5/fs/move",
            r#"{"addr":"192.0.2.1","from":"/a","to":"/b"}"#,
        )
        .await,
    )
    .await;
    let v = wait_done(&app, &id).await;
    // The route echoes what it received; a JSON answer is kept as JSON.
    let sent = &v["body"];
    assert_eq!(sent["op_id"], v["fs_op_id"]);
    assert_eq!(sent["from"], "/a");
}

// ── Events ──────────────────────────────────────────────────────────────────

/// The stream's text until `done` says it has what it wanted.
async fn read_sse(resp: HttpResponse<Body>, done: impl Fn(&str) -> bool) -> String {
    use tokio_stream::StreamExt as _;
    assert_eq!(resp.status(), StatusCode::OK);
    let mut stream = resp.into_body().into_data_stream();
    let mut text = String::new();
    while !done(&text) {
        let chunk = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap_or_else(|_| panic!("no event in time; so far: {text}"))
            .expect("the stream stays open")
            .unwrap();
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}

/// The SSE frame holding `needle`.
fn frame_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.split("\n\n")
        .find(|f| f.contains(needle))
        .unwrap_or_else(|| panic!("no frame with {needle} in {text}"))
}

fn frame_id(frame: &str) -> u64 {
    frame
        .lines()
        .find_map(|l| l.strip_prefix("id: "))
        .unwrap_or_else(|| panic!("no id in {frame}"))
        .parse()
        .unwrap()
}

#[tokio::test]
async fn job_messages_stay_the_default_event_and_gain_an_id() {
    let st = state();
    let app = app_with(st.clone(), Default::default(), "");
    let resp = get(&app, "/api/events").await;
    let job = Uuid::new_v4();
    set_job(&st.jobs, &st.events_tx, job, finished_job(5));
    let needle = job.to_string();
    let text = read_sse(resp, |t| t.contains(&needle)).await;
    let frame = frame_with(&text, &needle);
    assert!(!frame.contains("event:"), "default event: {frame}");
    assert!(frame_id(frame) > 0);
    // The body is the shape existing consumers parse.
    let data = frame
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .unwrap();
    let v: Value = serde_json::from_str(data).unwrap();
    assert_eq!(v["job_id"], needle);
    assert_eq!(v["job"]["status"], "failed");
}

#[tokio::test]
async fn last_event_id_replays_with_the_event_name() {
    let app = app();
    let needle = format!("marker-{}", Uuid::new_v4());
    let seq = event_bus::bus().publish_json("op", &json!({ "id": needle }));
    let resp = call(
        &app,
        request(Method::GET, "/api/events", LOOPBACK)
            .header("last-event-id", (seq - 1).to_string())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let text = read_sse(resp, |t| t.contains(&needle)).await;
    let frame = frame_with(&text, &needle);
    assert!(frame.contains("event: op"), "{frame}");
    assert_eq!(frame_id(frame), seq);
}

#[tokio::test]
async fn an_id_older_than_the_ring_gets_a_resync_first() {
    let app = app();
    event_bus::bus().publish_json("op", &json!({ "id": "x" }));
    let resp = call(
        &app,
        request(Method::GET, "/api/events", LOOPBACK)
            .header("last-event-id", "1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let text = read_sse(resp, |t| t.contains("event: resync")).await;
    assert!(text.starts_with("event: resync"), "first: {text}");
    assert!(text.contains(r#""resync":true"#), "{text}");
}

#[tokio::test]
async fn an_install_change_is_an_install_event() {
    let installs: pkg_install::PkgInstallStateHandle = Default::default();
    let app = app_with(state(), installs.clone(), "");
    let resp = get(&app, "/api/events").await;
    let job = installs.jobs.begin("192.0.2.10").unwrap();
    let text = read_sse(resp, |t| t.contains(&job)).await;
    assert!(frame_with(&text, &job).contains("event: install"), "{text}");
}

#[tokio::test]
async fn an_operation_is_an_op_event() {
    let app = app();
    let resp = get(&app, "/api/events").await;
    let id = op_id_of(post_async(&app, "/api/ps5/fs/delete", r#"{"path":"/x"}"#).await).await;
    let text = read_sse(resp, |t| t.contains(&id)).await;
    assert!(frame_with(&text, &id).contains("event: op"), "{text}");
}

// ── Lists ───────────────────────────────────────────────────────────────────

async fn job_rows(app: &Router, query: &str) -> Vec<Value> {
    json_of(get(app, &format!("/api/jobs{query}")).await)
        .await
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn jobs_take_limit_and_since_seq() {
    let st = state();
    let app = app_with(st.clone(), Default::default(), "");
    let ids: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
    for (i, id) in ids.iter().enumerate() {
        set_job(&st.jobs, &st.events_tx, *id, finished_job(100 + i as u64));
    }
    // The numbers are given as the feed carries them.
    for _ in 0..200 {
        if ids
            .iter()
            .all(|id| event_bus::bus().job_seq(&id.to_string()) > 0)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(job_rows(&app, "").await.len(), 3);

    let newest = job_rows(&app, "?limit=2").await;
    assert_eq!(newest.len(), 2);
    assert_eq!(newest[0]["job_id"], ids[2].to_string(), "newest first");

    let first = event_bus::bus().job_seq(&ids[0].to_string());
    let after = job_rows(&app, &format!("?since_seq={first}")).await;
    let got: Vec<&str> = after
        .iter()
        .map(|r| r["job_id"].as_str().unwrap())
        .collect();
    assert_eq!(got, vec![ids[1].to_string(), ids[2].to_string()]);

    // A limited page's cursor does not skip what the limit held back.
    let page = get(&app, &format!("/api/jobs?since_seq={}&limit=1", first - 1)).await;
    let cursor: u64 = page.headers()["x-ps5upload-last-seq"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(cursor, first);

    let r = get(&app, "/api/jobs?limit=-1").await;
    assert_shaped(r, StatusCode::BAD_REQUEST, "bad_request").await;
}

#[tokio::test]
async fn the_event_journal_takes_limit_and_since_seq_and_needs_no_since() {
    let app = app();
    assert_eq!(
        get(&app, "/api/event-journal?limit=5").await.status(),
        StatusCode::OK
    );
    let v = json_of(get(&app, "/api/event-journal?since=0&since_seq=7").await).await;
    assert!(v["last_seq"].as_u64().unwrap() >= 7);
    let r = get(&app, "/api/event-journal?since_seq=soon").await;
    assert_shaped(r, StatusCode::BAD_REQUEST, "bad_request").await;
}

// ── Request log ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_failure_is_attributed_to_the_console_its_handler_named() {
    let console = "192.0.2.77";
    let app = Router::new()
        .route(
            "/api/failing",
            post(|| async {
                let _ = required_console_addr(Some("192.0.2.77:9021".to_string()));
                StatusCode::BAD_GATEWAY
            }),
        )
        .layer(middleware::from_fn(log_requests));
    let r = post_json(&app, "/api/failing", "{}").await;
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    let key = failure_key("POST", "/api/failing", Some(console));
    let action =
        log_dedup::failure_log()
            .lock()
            .unwrap()
            .observe(&key, false, std::time::Instant::now());
    assert!(
        matches!(action, log_dedup::LogAction::Recovered { failures: 1 }),
        "the failure was tracked under its console: {action:?}"
    );
}

#[test]
fn the_console_in_a_query_is_decoded_under_any_of_its_names() {
    let of = |q: &str| console_from_query(&q.parse::<axum::http::Uri>().unwrap());
    assert_eq!(
        of("/x?addr=192.0.2.1%3A9021").as_deref(),
        Some("192.0.2.1:9021")
    );
    assert_eq!(
        of("/x?console=%5Bfe80%3A%3A1%5D").as_deref(),
        Some("[fe80::1]")
    );
    assert_eq!(of("/x?a=1&host=ps5.lan").as_deref(), Some("ps5.lan"));
    assert_eq!(of("/x?addr=").as_deref(), None);
    assert_eq!(of("/x").as_deref(), None);
}

// ── Route table ─────────────────────────────────────────────────────────────

/// The source with `#[cfg(test)]` items blanked out.
fn without_test_items(src: &str) -> String {
    let mut out = String::new();
    let mut lines = src.lines();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            // Up to the end of the next item: its braces balancing, or a `;` first.
            let mut depth = 0i32;
            let mut opened = false;
            for l in lines.by_ref() {
                depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
                opened |= l.contains('{');
                if (opened && depth <= 0) || (!opened && l.trim_end().ends_with(';')) {
                    break;
                }
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Every path literal on a `.route(` call in the crate's non-test code.
fn routes_in_source() -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs")
                // Test routers and the MCP catalogue's own scanner, whose samples are not routes.
                && !p.to_string_lossy().ends_with("_tests.rs")
                && !p.file_name().is_some_and(|n| n == "tests.rs" || n == "catalog_routes.rs")
            {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut routes = Vec::new();
    for f in files {
        let src = without_test_items(&std::fs::read_to_string(&f).unwrap());
        for (i, _) in src.match_indices(".route(") {
            let rest = src[i + ".route(".len()..].trim_start();
            if let Some(lit) = rest.strip_prefix('"') {
                let path = &lit[..lit.find('"').unwrap()];
                // Not a path: the text `".route("` itself, in a scanner like this one.
                if path.starts_with('/') {
                    routes.push(path.to_string());
                }
            }
        }
    }
    routes.sort();
    routes.dedup();
    routes
}

/// `/api/x/{id}` with something in the `{id}`.
fn concrete(route: &str) -> String {
    route
        .split('/')
        .map(|seg| if seg.starts_with('{') { "x" } else { seg })
        .collect::<Vec<_>>()
        .join("/")
}

/// A route in the source that is not mounted answers 404. TRACE runs no handler: a route
/// that exists answers 405. Building the router is itself a check: axum panics on a path
/// registered twice.
#[tokio::test]
async fn every_route_in_the_source_is_mounted() {
    let routes = routes_in_source();
    assert!(routes.len() > 300, "the scan found only {}", routes.len());
    let app = app();
    let mut unmounted = Vec::new();
    for route in &routes {
        let r = call(
            &app,
            request(Method::TRACE, &concrete(route), LOOPBACK)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        if r.status() != StatusCode::METHOD_NOT_ALLOWED {
            unmounted.push(format!("{route} -> {}", r.status()));
        }
    }
    assert!(unmounted.is_empty(), "not mounted: {unmounted:#?}");
}

#[tokio::test]
async fn the_openapi_document_is_served_and_behind_the_guards() {
    let app = app();
    let resp = get(&app, "/api/openapi.json").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let doc = json_of(resp).await;
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["info"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(doc["paths"]["/api/jobs"]["get"].is_object());
    assert!(doc["paths"].get("/api/consoles/document").is_none());

    let refused = call(
        &app,
        request(Method::GET, "/api/openapi.json", LAN)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn every_name_for_the_console_works_on_the_routes_that_lacked_them() {
    // Two names for one field is the serde error, which only an alias can cause.
    let app = app();
    let resp = get(
        &app,
        "/api/ps5/console-tasks?addr=192.0.2.1&console=192.0.2.1",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let text = json_of(resp).await["error"].as_str().unwrap().to_string();
    assert!(text.contains("duplicate field"), "{text}");
}
