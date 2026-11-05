//! The queue as a client meets it, through the real router (`build_app_and_mcp`) and all its
//! layers. Nothing reaches a console: either items go to a scripted API with jobs the test ends
//! by hand, or to the engine's own routes against a loopback address with no helper on it.
//!
//! Each test has its own console address: the busy count is per process.

use super::*;
use crate::queue_api::{QueueState, Timing};
use axum::{
    body::{to_bytes, Body},
    extract::Path as UrlPath,
    http::{header, Method, Request as HttpRequest},
    routing::{get, post},
};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tower::ServiceExt;

const FAST: Timing = Timing {
    tick: Duration::from_millis(10),
    settle: Duration::from_millis(20),
};

/// The API items go to in most tests: each start makes a job the test ends through [`Fake`].
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeJobs>>);

#[derive(Default)]
struct FakeJobs {
    /// job id → its answer on `/api/jobs/{id}`.
    jobs: HashMap<String, Value>,
    /// (route, body) of every start, in order.
    started: Vec<(String, Value)>,
    /// job ids a cancel was asked for.
    cancelled: Vec<String>,
    /// The next install is refused as busy.
    install_busy: bool,
    /// What `/api/ps5/console-tasks` answers; without one it is a helper too old to have it.
    console_tasks: Option<Value>,
    /// Paths `/api/ps5/fs/delete` was asked for.
    deleted: Vec<String>,
}

impl Fake {
    fn lock(&self) -> std::sync::MutexGuard<'_, FakeJobs> {
        self.0.lock().unwrap()
    }

    fn router(&self) -> Router {
        let start = |route: &'static str, field: &'static str| {
            let fake = self.clone();
            post(move |Json(body): Json<Value>| {
                let fake = fake.clone();
                async move {
                    let mut jobs = fake.lock();
                    if route == "/api/pkg/install" && std::mem::take(&mut jobs.install_busy) {
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({"ok": false, "error": "busy", "job": "other"})),
                        );
                    }
                    let id = format!("job-{}", jobs.started.len());
                    jobs.started.push((route.to_string(), body));
                    jobs.jobs.insert(
                        id.clone(),
                        json!({"status": "running", "phase": "deliver", "bytes_sent": 5}),
                    );
                    (StatusCode::ACCEPTED, Json(json!({ field: id })))
                }
            })
        };
        let fake = self.clone();
        let read = get(move |UrlPath(id): UrlPath<String>| {
            let fake = fake.clone();
            async move {
                match fake.lock().jobs.get(&id) {
                    Some(job) => (StatusCode::OK, Json(job.clone())),
                    None => (
                        StatusCode::NOT_FOUND,
                        Json(json!({"error": "job not found"})),
                    ),
                }
            }
        });
        let fake = self.clone();
        let cancel = post(move |UrlPath(id): UrlPath<String>| {
            let fake = fake.clone();
            async move {
                let mut jobs = fake.lock();
                jobs.cancelled.push(id.clone());
                jobs.jobs.insert(
                    id,
                    json!({"status": "failed", "error": "transfer_cancelled"}),
                );
                Json(json!({"cancelled": true}))
            }
        });
        let tasks = {
            let fake = self.clone();
            get(move || {
                let fake = fake.clone();
                async move {
                    match fake.lock().console_tasks.clone() {
                        Some(answer) => (StatusCode::OK, Json(answer)),
                        None => (
                            StatusCode::NOT_IMPLEMENTED,
                            Json(json!({"reason": "unsupported"})),
                        ),
                    }
                }
            })
        };
        let delete = {
            let fake = self.clone();
            post(move |Json(body): Json<Value>| {
                let fake = fake.clone();
                async move {
                    let path = body["path"].as_str().unwrap_or_default().to_string();
                    fake.lock().deleted.push(path);
                    Json(json!({"ok": true}))
                }
            })
        };
        Router::new()
            .route("/api/ps5/console-tasks", tasks)
            .route("/api/ps5/fs/delete", delete)
            .route("/api/transfer/file", start("/api/transfer/file", "job_id"))
            .route("/api/transfer/rar", start("/api/transfer/rar", "job_id"))
            .route("/api/link/download", start("/api/link/download", "job_id"))
            .route("/api/pkg/install", start("/api/pkg/install", "job"))
            .route("/api/jobs/{id}", read)
            .route("/api/jobs/{id}/cancel", cancel)
    }

    fn end(&self, job: &str, state: Value) {
        self.lock().jobs.insert(job.to_string(), state);
    }

    fn started(&self) -> Vec<(String, Value)> {
        self.lock().started.clone()
    }
}

struct Engine {
    app: Router,
    answers: Arc<AtomicBool>,
    installs: pkg_install::PkgInstallStateHandle,
}

fn engine_on(path: Option<PathBuf>, fake: Option<&Fake>) -> Engine {
    let answers = Arc::new(AtomicBool::new(true));
    let queue = QueueState::for_test(path, answers.clone(), FAST);
    if let Some(fake) = fake {
        queue.use_api(fake.router());
    }
    let (events_tx, _) = broadcast::channel(64);
    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        default_ps5_addr: "192.0.2.1".to_string(),
        events_tx,
    };
    let installs = pkg_install::PkgInstallStateHandle::default();
    let (app, _) = build_app_and_mcp(
        state,
        installs.clone(),
        LoopbackGuardConfig {
            allowed_ips: Vec::new().into(),
        },
        "",
        queue.clone(),
    );
    Engine {
        app,
        answers,
        installs,
    }
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ps5u-queue-{name}-{}-{}",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("engine-queue.json")
}

impl Engine {
    async fn send(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let peer: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let mut request = HttpRequest::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:19113");
        request.extensions_mut().unwrap().insert(ConnectInfo(peer));
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1 << 22).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, path, Some(body)).await
    }

    async fn add_upload(&self, console: &str, name: &str) -> String {
        let (status, item) = self
            .post(
                "/api/queue",
                json!({
                    "console": console,
                    "kind": "upload_file",
                    "request": {"src": format!("/tmp/{name}"), "dest": format!("/data/{name}")},
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{item}");
        assert_eq!(item["label"], name);
        item["id"].as_str().unwrap().to_string()
    }

    async fn item(&self, id: &str) -> Value {
        let (status, item) = self
            .send(Method::GET, &format!("/api/queue/{id}"), None)
            .await;
        assert_eq!(status, StatusCode::OK, "{item}");
        item
    }

    async fn wait_status(&self, id: &str, want: &str) -> Value {
        for _ in 0..500 {
            let item = self.item(id).await;
            if item["status"] == want {
                return item;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{id} never became {want}: {}", self.item(id).await);
    }

    async fn console(&self, console: &str) -> Value {
        let (status, list) = self
            .send(Method::GET, &format!("/api/queue?console={console}"), None)
            .await;
        assert_eq!(status, StatusCode::OK);
        list["consoles"][0].clone()
    }

    async fn wait_waiting_for(&self, console: &str, want: &str) {
        for _ in 0..500 {
            if self.console(console).await["waiting_for"] == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "{console} never waited for {want}: {}",
            self.console(console).await
        );
    }
}

async fn wait_started(fake: &Fake, count: usize) {
    for _ in 0..500 {
        if fake.started().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("only {} of {count} started", fake.started().len());
}

fn done() -> Value {
    json!({"status": "done"})
}

#[tokio::test(flavor = "multi_thread")]
async fn items_run_in_order_one_at_a_time_through_their_routes() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.1.1";
    let first = engine.add_upload(console, "one.bin").await;
    let running = engine.wait_status(&first, "running").await;
    let second = engine.add_upload(console, "two.bin").await;
    let (status, third) = engine
        .post(
            "/api/queue",
            json!({
                // Another name for the console, as elsewhere.
                "addr": format!("{console}:9120"),
                "kind": "install",
                "request": {"source": {"console_path": "/data/three.pkg"}},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{third}");
    assert_eq!(third["console"], console);
    assert_eq!(third["position"], 1);
    assert_eq!(third["request"]["ps5_addr"], console);
    let third = third["id"].as_str().unwrap().to_string();

    wait_started(&fake, 1).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(fake.started().len(), 1, "one at a time");
    assert_eq!(fake.started()[0].0, "/api/transfer/file");
    assert_eq!(fake.started()[0].1["addr"], console);
    assert_eq!(fake.started()[0].1["tx_id"].as_str().unwrap().len(), 32);
    assert_eq!(running["runs"], 1);

    let summary = engine.console(console).await;
    assert_eq!(summary["state"], "running");
    assert_eq!(summary["running_item"], first.as_str());
    assert_eq!(summary["queued"], 2);

    fake.end("job-0", done());
    let finished = engine.wait_status(&first, "done").await;
    assert_eq!(finished["job_id"], "job-0");
    assert!(finished["finished_at_ms"].is_u64());
    wait_started(&fake, 2).await;

    // A failure is reported and the line goes on: nothing is retried by itself.
    fake.end(
        "job-1",
        json!({
            "status": "failed",
            "error": "raw text",
            "error_reason": "ava1_no_space",
            "error_detail": "no room left",
        }),
    );
    let failed = engine.wait_status(&second, "failed").await;
    assert_eq!(failed["code"], "ava1_no_space");
    assert_eq!(failed["error"], "no room left");
    wait_started(&fake, 3).await;
    assert_eq!(fake.started()[2].0, "/api/pkg/install");
    assert_eq!(fake.started()[2].1["ps5_addr"], console);
    let job = engine.wait_status(&third, "running").await;
    assert_eq!(job["runs"], 1);
    fake.end(
        "job-2",
        json!({"phase": "done", "verdict": "may_not_launch", "hint": "try it"}),
    );
    let installed = engine.wait_status(&third, "done").await;
    assert_eq!(installed["verdict"], "may_not_launch");
    assert_eq!(installed["hint"], "try it");

    // Asked to run again: the same tx_id, so the upload picks up what landed.
    let (status, resumed) = engine
        .post(&format!("/api/queue/{second}/resume"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{resumed}");
    wait_started(&fake, 4).await;
    assert_eq!(fake.started()[3].1["tx_id"], fake.started()[1].1["tx_id"]);
    assert_eq!(engine.wait_status(&second, "running").await["runs"], 2);
    fake.end("job-3", done());
    engine.wait_status(&second, "done").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn consoles_run_side_by_side() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let a = engine.add_upload("127.0.2.1", "a.bin").await;
    let b = engine.add_upload("127.0.2.2", "b.bin").await;
    engine.wait_status(&a, "running").await;
    engine.wait_status(&b, "running").await;
    let (_, one) = engine
        .send(Method::GET, "/api/queue?host=127.0.2.1", None)
        .await;
    assert_eq!(one["items"].as_array().unwrap().len(), 1);
    assert_eq!(one["items"][0]["id"], a.as_str());
    let (_, all) = engine.send(Method::GET, "/api/queue", None).await;
    assert_eq!(all["consoles"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_resume_remove_and_move() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.3.1";
    let first = engine.add_upload(console, "1.bin").await;
    engine.wait_status(&first, "running").await;
    let second = engine.add_upload(console, "2.bin").await;
    let third = engine.add_upload(console, "3.bin").await;
    let fourth = engine.add_upload(console, "4.bin").await;

    // Move.
    let (status, moved) = engine
        .post(&format!("/api/queue/{fourth}/move"), json!({"position": 0}))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["position"], 0);
    assert_eq!(engine.item(&second).await["position"], 1);
    let (status, refused) = engine
        .post(&format!("/api/queue/{first}/move"), json!({"position": 0}))
        .await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (StatusCode::CONFLICT, Some("not_waiting"))
    );
    let (status, refused) = engine
        .post(&format!("/api/queue/{second}/move"), json!({}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert!(refused["error"].is_string() && refused["code"].is_string());

    // A paused item is skipped and keeps its place.
    let (status, paused) = engine
        .post(&format!("/api/queue/{fourth}/pause"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(paused["status"], "paused");
    assert_eq!(paused["position"], 0);

    // Removing a waiting item.
    let (status, removed) = engine
        .post(&format!("/api/queue/{third}/remove"), json!({}))
        .await;
    assert_eq!(
        (status, &removed["removed"]),
        (StatusCode::OK, &json!(true))
    );
    let (status, gone) = engine
        .send(Method::GET, &format!("/api/queue/{third}"), None)
        .await;
    assert_eq!(
        (status, gone["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );

    // A running upload does not go without being told to stop.
    let (status, refused) = engine
        .post(&format!("/api/queue/{first}/remove"), json!({}))
        .await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (StatusCode::CONFLICT, Some("item_running"))
    );

    // Pausing a running upload cancels its job and keeps the item.
    wait_started(&fake, 1).await;
    let (status, _) = engine
        .post(&format!("/api/queue/{first}/pause"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    engine.wait_status(&first, "paused").await;
    assert_eq!(fake.lock().cancelled, ["job-0"]);

    // `second` was next that is not paused; `fourth` and `first` wait to be resumed.
    wait_started(&fake, 2).await;
    assert_eq!(fake.started()[1].1["dest"], "/data/2.bin");
    engine.wait_status(&second, "running").await;

    // Told to stop, the running one stops and goes.
    let (status, removing) = engine
        .post(
            &format!("/api/queue/{second}/remove"),
            json!({"cancel": true}),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{removing}");
    for _ in 0..500 {
        let (status, _) = engine
            .send(Method::GET, &format!("/api/queue/{second}"), None)
            .await;
        if status == StatusCode::NOT_FOUND {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (status, _) = engine
        .send(Method::GET, &format!("/api/queue/{second}"), None)
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Resumed, `first` picks up with its own tx_id.
    let (status, _) = engine
        .post(&format!("/api/queue/{first}/resume"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    wait_started(&fake, 3).await;
    assert_eq!(fake.started()[2].1["tx_id"], fake.started()[0].1["tx_id"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_install_cannot_pause_or_go() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let (_, item) = engine
        .post(
            "/api/queue",
            json!({
                "console": "127.0.4.1",
                "kind": "install",
                "request": {"source": {"host_file": "/tmp/a.pkg"}},
            }),
        )
        .await;
    let id = item["id"].as_str().unwrap().to_string();
    wait_started(&fake, 1).await;
    engine.wait_status(&id, "running").await;
    let (status, refused) = engine
        .post(&format!("/api/queue/{id}/pause"), json!({}))
        .await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (StatusCode::CONFLICT, Some("install_cannot_pause"))
    );
    let (status, refused) = engine
        .post(&format!("/api/queue/{id}/remove"), json!({"cancel": true}))
        .await;
    assert_eq!(
        (status, refused["code"].as_str()),
        (StatusCode::CONFLICT, Some("install_running"))
    );
    assert!(fake.lock().cancelled.is_empty());
    // Stopped directly, it ends as what it is.
    fake.end("job-0", json!({"phase": "failed", "reason": "cancelled"}));
    engine.wait_status(&id, "cancelled").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_install_slot_means_wait_not_fail() {
    let fake = Fake::default();
    fake.lock().install_busy = true;
    let engine = engine_on(None, Some(&fake));
    let (_, item) = engine
        .post(
            "/api/queue",
            json!({
                "console": "127.0.4.2",
                "kind": "install",
                "request": {"source": {"host_file": "/tmp/a.pkg"}},
            }),
        )
        .await;
    let id = item["id"].as_str().unwrap().to_string();
    wait_started(&fake, 1).await;
    let running = engine.wait_status(&id, "running").await;
    assert_eq!(running["runs"], 1, "the refused start does not count");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_queue_waits_for_other_work_on_the_console_and_for_holds() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.5.1";

    // A job started outside the queue: an upload holds this guard while it runs.
    let job = helper_stop::enter(console);
    let id = engine.add_upload(console, "w.bin").await;
    engine.wait_waiting_for(console, "other_work").await;
    assert!(fake.started().is_empty());
    drop(job);
    wait_started(&fake, 1).await;
    engine.wait_status(&id, "running").await;

    // While it runs a hold is refused, and says what runs.
    let hold = json!({"console": console, "holder": "app-1"});
    let (status, refused) = engine.post("/api/queue/hold", hold.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "queue_running");
    assert_eq!(refused["item"]["id"], id.as_str());

    // Between items a hold is granted, and nothing starts until it ends.
    let next = engine.add_upload(console, "x.bin").await;
    fake.end("job-0", done());
    engine.wait_status(&id, "done").await;
    let mut granted = false;
    for _ in 0..200 {
        if engine.post("/api/queue/hold", hold.clone()).await.0 == StatusCode::OK {
            granted = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The item may have got in first: then the client waits its turn, as the app does.
    if !granted {
        fake.end("job-1", done());
        engine.wait_status(&next, "done").await;
        return;
    }
    engine.wait_waiting_for(console, "hold").await;
    assert_eq!(engine.console(console).await["holders"], json!(["app-1"]));
    assert_eq!(fake.started().len(), 1);
    let (status, released) = engine
        .post(
            "/api/queue/release",
            json!({"host": console, "holder": "app-1"}),
        )
        .await;
    assert_eq!(
        (status, &released["released"]),
        (StatusCode::OK, &json!(true))
    );
    wait_started(&fake, 2).await;

    // An install another client runs holds the line too.
    fake.end("job-1", done());
    engine.wait_status(&next, "done").await;
    let slot = engine.installs.jobs.begin(console).unwrap();
    let last = engine.add_upload(console, "y.bin").await;
    engine.wait_waiting_for(console, "other_work").await;
    assert_eq!(engine.item(&last).await["status"], "queued");
    engine.installs.jobs.finish(&slot);
    // `begin` left a status that is not over: finishing releases the slot all the same.
    wait_started(&fake, 3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hold_lapses_by_itself() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.5.2";
    let (status, held) = engine
        .post(
            "/api/queue/hold",
            json!({"addr": console, "holder": "app-2", "ttl_s": 1}),
        )
        .await;
    assert_eq!((status, &held["held"]), (StatusCode::OK, &json!(true)));
    engine.add_upload(console, "z.bin").await;
    engine.wait_waiting_for(console, "hold").await;
    assert!(fake.started().is_empty());
    for _ in 0..300 {
        if !fake.started().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the item never started after the hold lapsed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_console_that_does_not_answer_holds_the_line_instead_of_failing_it() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.5.3";
    engine.answers.store(false, Ordering::SeqCst);
    let one = engine.add_upload(console, "a.bin").await;
    let two = engine.add_upload(console, "b.bin").await;
    engine.wait_waiting_for(console, "console").await;
    assert!(fake.started().is_empty());
    assert_eq!(engine.item(&one).await["status"], "queued");
    assert_eq!(engine.item(&two).await["status"], "queued");
    engine.answers.store(true, Ordering::SeqCst);
    wait_started(&fake, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_line_and_never_restarts_what_ran() {
    let path = scratch("restart");
    let console = "127.0.6.1";
    let fake = Fake::default();
    let (running, waiting, paused, token, rar) = {
        let engine = engine_on(Some(path.clone()), Some(&fake));
        let running = engine.add_upload(console, "ran.bin").await;
        engine.wait_status(&running, "running").await;
        wait_started(&fake, 1).await;
        let waiting = engine.add_upload(console, "waited.bin").await;
        let paused = engine.add_upload(console, "paused.bin").await;
        engine
            .post(&format!("/api/queue/{paused}/pause"), json!({}))
            .await;
        let (_, token) = engine
            .post(
                "/api/queue",
                json!({
                    "console": console,
                    "kind": "install",
                    "request": {"source": {"url": "https://cdn.example/a.pkg?sig=SECRET1"}},
                }),
            )
            .await;
        assert_eq!(
            token["request"]["source"]["url"],
            "https://cdn.example/a.pkg?..."
        );
        assert_eq!(token["secret_in_memory"], true);
        let (_, rar) = engine
            .post(
                "/api/queue",
                json!({
                    "console": console,
                    "kind": "upload_rar",
                    "request": {
                        "archive_path": "/tmp/a.rar",
                        "dest_root": "/data/a",
                        "password": "SECRET2",
                    },
                }),
            )
            .await;
        assert!(rar["request"].get("password").is_none());
        (
            running,
            waiting,
            paused,
            token["id"].as_str().unwrap().to_string(),
            rar["id"].as_str().unwrap().to_string(),
        )
    };
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(!on_disk.contains("SECRET"), "{on_disk}");
    assert!(on_disk.contains(&waiting));

    // The file as a crash leaves it: `running` stays `running` on disk.
    assert!(on_disk.contains("\"running\""));
    let fake2 = Fake::default();
    let engine = engine_on(Some(path.clone()), Some(&fake2));
    let was_running = engine.item(&running).await;
    assert_eq!(was_running["status"], "interrupted");
    assert_eq!(was_running["code"], "interrupted");
    assert_eq!(was_running["job_id"], "job-0");
    assert_eq!(engine.item(&paused).await["status"], "paused");
    for secret_item in [&token, &rar] {
        let item = engine.item(secret_item).await;
        assert_eq!(item["status"], "interrupted");
        assert_eq!(item["code"], "secret_not_kept");
        let (status, refused) = engine
            .post(&format!("/api/queue/{secret_item}/resume"), json!({}))
            .await;
        assert_eq!(
            (status, refused["code"].as_str()),
            (StatusCode::CONFLICT, Some("secret_not_kept"))
        );
    }
    // What waited runs; what ran does not, until someone asks.
    wait_started(&fake2, 1).await;
    assert_eq!(fake2.started()[0].1["dest"], "/data/waited.bin");
    engine.wait_status(&waiting, "running").await;
    fake2.end("job-0", done());
    engine.wait_status(&waiting, "done").await;
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(fake2.started().len(), 1, "nothing else starts by itself");
    let (status, _) = engine
        .post(&format!("/api/queue/{running}/resume"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK);
    wait_started(&fake2, 2).await;
    assert_eq!(fake2.started()[1].1["dest"], "/data/ran.bin");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreadable_file_is_set_aside_and_the_queue_starts_empty() {
    let path = scratch("corrupt");
    std::fs::write(&path, b"{ not json").unwrap();
    let engine = engine_on(Some(path.clone()), None);
    let (_, list) = engine.send(Method::GET, "/api/queue", None).await;
    assert_eq!(list["items"], json!([]));
    assert!(path.with_extension("json.corrupt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_job_is_interrupted_and_stop_on_failure_holds_the_rest() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.6.2";
    let (_, first) = engine
        .post(
            "/api/queue",
            json!({
                "console": console,
                "kind": "upload_file",
                "stop_on_failure": true,
                "request": {"src": "/tmp/u.pkg", "dest": "/data/u.pkg"},
            }),
        )
        .await;
    let first = first["id"].as_str().unwrap().to_string();
    let second = engine.add_upload(console, "after.bin").await;
    wait_started(&fake, 1).await;
    fake.lock().jobs.clear();
    let lost = engine.wait_status(&first, "interrupted").await;
    assert_eq!(lost["code"], "job_lost");
    let held = engine.wait_status(&second, "paused").await;
    assert_eq!(held["code"], "held_after_failure");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(fake.started().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_that_did_not_apply_failed_holds_the_rest_and_can_be_retried() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.6.3";
    let (_, first) = engine
        .post(
            "/api/queue",
            json!({
                "console": console,
                "kind": "install",
                "stop_on_failure": true,
                "request": {"source": {"host_file": "/tmp/update.pkg"}},
            }),
        )
        .await;
    let first = first["id"].as_str().unwrap().to_string();
    let second = engine.add_upload(console, "dlc.bin").await;
    wait_started(&fake, 1).await;
    // What install/mod.rs ends with for DidNotApply and Regressed.
    fake.end(
        "job-0",
        json!({"phase": "done", "verdict": "failed", "hint": "the update did not apply"}),
    );
    let failed = engine.wait_status(&first, "failed").await;
    assert_eq!(failed["verdict"], "failed");
    assert_eq!(failed["error"], "the update did not apply");
    let held = engine.wait_status(&second, "paused").await;
    assert_eq!(held["code"], "held_after_failure");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(fake.started().len(), 1);
    let (status, retried) = engine
        .post(&format!("/api/queue/{first}/resume"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{retried}");
    wait_started(&fake, 2).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removal_holds_however_the_run_ended() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let id = engine.add_upload("127.0.6.4", "gone.bin").await;
    wait_started(&fake, 1).await;
    let (status, answer) = engine
        .post(&format!("/api/queue/{id}/remove"), json!({"cancel": true}))
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    // The job got to its end before the cancel reached it.
    fake.end("job-0", done());
    for _ in 0..200 {
        let (_, list) = engine
            .send(Method::GET, "/api/queue?console=127.0.6.4", None)
            .await;
        if !list.to_string().contains(id.as_str()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the item stayed listed");
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_events_reach_the_engines_feed() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let mut feed = event_bus::bus().subscribe(None).rx;
    let id = engine.add_upload("127.0.7.1", "e.bin").await;
    let mut actions = Vec::new();
    for _ in 0..200 {
        let event = tokio::time::timeout(Duration::from_secs(5), feed.recv())
            .await
            .unwrap()
            .unwrap();
        if event.kind != Some("queue") {
            continue;
        }
        let body: Value = serde_json::from_str(&event.data).unwrap();
        if body["item"]["id"] == id.as_str() {
            actions.push(body["action"].as_str().unwrap().to_string());
        }
        if actions.iter().any(|a| a == "job_started") {
            break;
        }
    }
    assert_eq!(actions, ["added", "started", "job_started"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_are_json_with_a_code() {
    let engine = engine_on(None, Some(&Fake::default()));
    let add = |body: Value| engine.post("/api/queue", body);

    let (status, r) =
        add(json!({"kind": "upload_file", "request": {"src": "/a", "dest": "/b"}})).await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("missing_console"))
    );
    let (status, r) = add(json!({"console": "127.0.8.1", "kind": "teleport", "request": {}})).await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unknown_kind"))
    );
    assert!(r["error"].as_str().unwrap().contains("upload_file"));
    let (status, r) = add(json!({"console": "127.0.8.1", "kind": "upload_file"})).await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    let (status, r) =
        add(json!({"console": "127.0.8.1", "kind": "upload_file", "request": {"src": "/a"}})).await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    assert!(r["error"].as_str().unwrap().contains("dest"), "{r}");
    let (status, r) = add(json!({
        "console": "127.0.8.1",
        "kind": "upload_file",
        "request": {"src": "/a", "dest": "/b", "tx_id": "short"},
    }))
    .await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    let (status, r) = add(json!({
        "console": "127.0.8.1",
        "kind": "install",
        "request": {"source": {"pigeon": "/a"}},
    }))
    .await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );
    let (status, r) = add(json!({
        "console": "127.0.8.1",
        "kind": "upload_file",
        "request": {"src": "x".repeat(MAX_TEST_REQUEST), "dest": "/b"},
    }))
    .await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("request_too_large"))
    );
    // Malformed: the shared layer's words, the same shape.
    let (status, r) = engine
        .post("/api/queue/hold", json!({"console": "127.0.8.1"}))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r["error"].is_string() && r["code"].is_string());
    let (status, r) = engine
        .post(
            "/api/queue/hold",
            json!({"console": "127.0.8.1", "holder": " "}),
        )
        .await;
    assert_eq!(
        (status, r["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("bad_request"))
    );

    for action in ["pause", "resume", "remove"] {
        let (status, r) = engine
            .post(&format!("/api/queue/q-nope/{action}"), json!({}))
            .await;
        assert_eq!(
            (status, r["code"].as_str()),
            (StatusCode::NOT_FOUND, Some("not_found"))
        );
    }
    let (status, r) = engine.send(Method::DELETE, "/api/queue", None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(r["code"].is_string());
}

const MAX_TEST_REQUEST: usize = 1024 * 1024 + 1;

/// The engine's own routes and job map, with no helper listening on this address: a real job
/// fails with the same token an API client gets, and the line goes on.
#[tokio::test(flavor = "multi_thread")]
async fn real_jobs_and_a_real_install_run_through_the_engines_own_routes() {
    let path = scratch("real");
    let src = path.with_file_name("one.bin");
    std::fs::write(&src, b"hello").unwrap();
    let engine = engine_on(None, None);
    let console = "127.0.0.1";
    let (status, missing) = engine
        .post(
            "/api/queue",
            json!({
                "console": console,
                "kind": "upload_file",
                "request": {"src": "/no/such/file.bin", "dest": "/data/x/file.bin"},
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{missing}");
    let missing = missing["id"].as_str().unwrap().to_string();
    let (_, upload) = engine
        .post(
            "/api/queue",
            json!({
                "console": console,
                "kind": "upload_file",
                "request": {"src": src, "dest": "/data/x/one.bin"},
            }),
        )
        .await;
    let upload = upload["id"].as_str().unwrap().to_string();
    let (_, install) = engine
        .post(
            "/api/queue",
            json!({
                "console": console,
                "kind": "install",
                "request": {"source": {"console_path": "/user/app/PPSA01234/app.pkg"}},
            }),
        )
        .await;
    let install = install["id"].as_str().unwrap().to_string();

    // The route refused to start: its words and a code from its status.
    let refused = engine.wait_status(&missing, "failed").await;
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("cannot read source"),
        "{refused}"
    );
    assert_eq!(refused["code"], "bad_request");
    assert!(refused.get("job_id").is_none());

    // It started and failed: the job's own reason.
    let failed = engine.wait_status(&upload, "failed").await;
    assert_eq!(failed["code"], "helper_not_ava1", "{failed}");
    assert!(failed["job_id"].is_string());

    // The real install route: refused at once for a console's own installed copy.
    let refused = engine.wait_status(&install, "failed").await;
    assert_eq!(refused["code"], "bad_request", "{refused}");
    assert!(
        refused["error"].as_str().unwrap().contains("own copy"),
        "{refused}"
    );
    let _ = std::fs::remove_file(&src);
}

/// The tools, over the engine's own `/mcp`.
#[tokio::test(flavor = "multi_thread")]
async fn the_mcp_tools_drive_the_same_queue() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let tool = |name: &'static str, arguments: Value| {
        let engine = &engine;
        async move {
            let (status, answer) = engine
                .post(
                    "/mcp",
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": name, "arguments": arguments},
                    }),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "{answer}");
            answer["result"].clone()
        }
    };
    let console = "127.0.10.1";
    let added = tool(
        "queue_add",
        json!({
            "console": console,
            "kind": "upload_file",
            "request": {"src": "/tmp/t.bin", "dest": "/data/t.bin"},
        }),
    )
    .await;
    assert_ne!(added["isError"], json!(true), "{added}");
    let id = added["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_started(&fake, 1).await;

    // What a tool added, an HTTP client sees.
    assert_eq!(engine.wait_status(&id, "running").await["console"], console);
    let list = tool("queue_list", json!({"console": console})).await;
    assert_eq!(list["structuredContent"]["items"][0]["id"], id.as_str());

    let second = tool(
        "queue_add",
        json!({
            "console": console,
            "kind": "upload_file",
            "request": {"src": "/tmp/u.bin", "dest": "/data/u.bin"},
        }),
    )
    .await;
    let second = second["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let paused = tool("queue_pause", json!({"id": second})).await;
    assert_eq!(paused["structuredContent"]["status"], "paused");
    let settled = tool("queue_wait", json!({"id": second, "timeout_s": 0})).await;
    assert_eq!(settled["structuredContent"]["finished"], true);
    let moved = tool("queue_move", json!({"id": second, "position": 5})).await;
    assert_ne!(moved["isError"], json!(true), "{moved}");
    let resumed = tool("queue_resume", json!({"id": second})).await;
    assert_eq!(resumed["structuredContent"]["status"], "queued");
    let waited = tool("queue_wait", json!({"id": second, "timeout_s": 0})).await;
    assert_eq!(waited["structuredContent"]["finished"], false);

    // A refusal is a tool error with the engine's words.
    let refused = tool("queue_remove", json!({"id": id})).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(refused.to_string().contains("item_running"), "{refused}");
    let removed = tool("queue_remove", json!({"id": second})).await;
    assert_eq!(removed["structuredContent"]["removed"], true);
    let (_, list) = engine.send(Method::GET, "/api/queue", None).await;
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["id"] != second.as_str()));
}

#[tokio::test]
async fn an_item_waits_for_a_task_running_on_the_console() {
    let fake = Fake::default();
    fake.lock().console_tasks = Some(json!({
        "ok": true, "supported": true,
        "tasks": [{"id": "a", "state": "done"}, {"id": "b", "state": "running"}],
    }));
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.5.40";
    let id = engine.add_upload(console, "t.bin").await;
    engine.wait_waiting_for(console, "console_task").await;
    assert!(fake.started().is_empty());
    fake.lock().console_tasks = Some(json!({
        "ok": true, "supported": true, "tasks": [{"id": "b", "state": "cancelled"}],
    }));
    wait_started(&fake, 1).await;
    engine.wait_status(&id, "running").await;
    fake.end("job-0", done());
    engine.wait_status(&id, "done").await;
}

/// A part is deleted by hand or resumed, never by name from here: a gap between looking and deleting
/// could take another job's files, and a leftover only costs room.
#[tokio::test]
async fn removing_a_running_upload_leaves_its_part() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.5.41";
    let id = engine.add_upload(console, "r.bin").await;
    wait_started(&fake, 1).await;
    engine.wait_status(&id, "running").await;
    let (status, answer) = engine
        .post(&format!("/api/queue/{id}/remove"), json!({"cancel": true}))
        .await;
    assert!(status.is_success(), "{answer}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(fake.lock().cancelled, ["job-0"]);
    assert!(fake.lock().deleted.is_empty());
}

#[tokio::test]
async fn a_done_item_shows_where_its_job_ended() {
    let fake = Fake::default();
    let engine = engine_on(None, Some(&fake));
    let console = "127.0.5.43";
    let id = engine.add_upload(console, "e.bin").await;
    wait_started(&fake, 1).await;
    let mut end = done();
    end["bytes_sent"] = json!(8_388_608_u64);
    end["total_bytes"] = json!(8_388_608_u64);
    fake.end("job-0", end);
    let item = engine.wait_status(&id, "done").await;
    assert_eq!(item["progress"]["bytes_sent"], 8_388_608_u64, "{item}");
}

/// Through the real router: `wake_rp_key` was found in a browser's disk cache.
#[tokio::test]
async fn api_answers_are_not_cached() {
    let engine = engine_on(None, None);
    for path in [
        "/api/consoles/document",
        "/api/consoles",
        "/api/settings-backup",
        "/api/settings-backup/get?id=nothing",
        "/api/mesh/me",
        "/api/queue",
        "/api/version",
        "/api/engine-logs",
        "/api/no/such/route",
    ] {
        let res = engine
            .app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(path)
                    .header(header::HOST, "127.0.0.1")
                    .extension(axum::extract::ConnectInfo(SocketAddr::from((
                        [127, 0, 0, 1],
                        50_000,
                    ))))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{path}: {}",
            res.status()
        );
    }
}

/// A route that wants caching keeps it, and a picture is not told anything new.
#[tokio::test]
async fn a_route_with_its_own_cache_rule_keeps_it() {
    let app = Router::new()
        .route(
            "/api/cover",
            get(|| async { ([(header::CACHE_CONTROL, "private, max-age=300")], "{}") }),
        )
        .route(
            "/api/picture",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], vec![1_u8]) }),
        )
        .route("/page", get(|| async { "words" }))
        .layer(middleware::from_fn(web_pages::api_no_store));
    let cache_of = |path: &'static str| {
        let app = app.clone();
        async move {
            let req = HttpRequest::builder()
                .uri(path)
                .body(Body::empty())
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            res.headers()
                .get(header::CACHE_CONTROL)
                .map(|v| v.to_str().unwrap().to_string())
        }
    };
    assert_eq!(
        cache_of("/api/cover").await.as_deref(),
        Some("private, max-age=300")
    );
    assert_eq!(cache_of("/api/picture").await, None);
    assert_eq!(cache_of("/page").await, None);
}
