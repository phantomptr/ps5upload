//! Payload updates and the Payload Manager sync over HTTP (`ps5upload_core::payload_library`'s
//! `updates` and `pm_sync`), and the "Keep payloads up to date" schedule.
//!
//! - `GET  /api/payloads/updates`                 the last check, what was applied, the settings
//! - `POST /api/payloads/updates/check`           check every payload for a newer release
//! - `POST /api/payloads/updates/apply`           update (all, or `ids`), then sync `console`
//! - `GET|PUT /api/payloads/updates/settings`     `{auto_update}`
//! - `POST /api/payloads/{id}/rollback`           put back the copy the last update replaced
//! - `GET|PUT /api/payloads/pm-sync`              what a console's Payload Manager keeps / loads
//! - `POST /api/payloads/pm-sync/run`             bring Payload Manager in step

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ps5upload_core::payload_library::{
    HttpPm, Library, LibraryError, PmConsole, PmSyncReport, UpdateOutcome, UpdateSettings,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::payloads_api::{library_error, no_library};

/// How a route reaches a console's Payload Manager: over HTTP, or a fake in tests.
pub(crate) type PmFactory = Arc<dyn Fn(&str) -> Box<dyn PmConsole> + Send + Sync>;

fn http_pm() -> PmFactory {
    Arc::new(|host: &str| Box::new(HttpPm(host.to_string())) as Box<dyn PmConsole>)
}

#[derive(Clone)]
struct Api {
    lib: Option<Library>,
    pm: PmFactory,
}

impl Api {
    async fn run<T: Serialize + Send + 'static>(
        &self,
        f: impl FnOnce(Library, PmFactory) -> Result<T, LibraryError> + Send + 'static,
    ) -> Response {
        let Some(lib) = self.lib.clone() else {
            return no_library();
        };
        let pm = self.pm.clone();
        match tokio::task::spawn_blocking(move || f(lib, pm)).await {
            Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
            Ok(Err(e)) => library_error(&e),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        }
    }
}

pub(crate) fn routes<S: Clone + Send + Sync + 'static>(lib: Option<Library>) -> Router<S> {
    Router::new().merge(routes_with(lib, http_pm()))
}

pub(crate) fn routes_with<S: Clone + Send + Sync + 'static>(
    lib: Option<Library>,
    pm: PmFactory,
) -> Router<S> {
    Router::new()
        .route("/api/payloads/updates", get(overview))
        .route("/api/payloads/updates/check", post(check))
        .route("/api/payloads/updates/apply", post(apply))
        .route(
            "/api/payloads/updates/settings",
            get(get_settings).put(put_settings),
        )
        .route("/api/payloads/{id}/rollback", post(rollback))
        .route("/api/payloads/pm-sync", get(get_choice).put(put_choice))
        .route("/api/payloads/pm-sync/run", post(run_sync))
        .with_state(Api { lib, pm })
}

/// GET /api/payloads/updates → `{settings, state}`: the last check, what was applied and the
/// last Payload Manager syncs. Never asks the network or a console.
async fn overview(State(api): State<Api>) -> Response {
    api.run(|lib, _| {
        Ok(json!({
            "settings": lib.update_settings(),
            "state": lib.update_state(),
        }))
    })
    .await
}

#[derive(Deserialize, Default)]
struct CheckReq {
    /// Ask each source even when the hour-long release cache is fresh.
    #[serde(default)]
    force: bool,
}

/// POST /api/payloads/updates/check `{force?}` → the update state with one row per payload.
async fn check(State(api): State<Api>, body: axum::body::Bytes) -> Response {
    let req: CheckReq = match parse_or_default(&body) {
        Ok(r) => r,
        Err(r) => return *r,
    };
    api.run(move |lib, _| lib.check_updates(req.force, &lib.pm_sync_ids()))
        .await
}

fn parse_or_default<T: serde::de::DeserializeOwned + Default>(
    body: &[u8],
) -> Result<T, Box<Response>> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("bad request: {e}") })),
            )
                .into_response(),
        )
    })
}

#[derive(Deserialize, Default)]
struct ApplyReq {
    /// The payloads to update. Absent: every one the last check found an update for.
    #[serde(default)]
    ids: Option<Vec<String>>,
    /// A console whose Payload Manager is synced afterwards.
    #[serde(default, alias = "host", alias = "addr", alias = "sync_console")]
    console: Option<String>,
}

/// A payload that could not be updated.
#[derive(Serialize)]
struct ApplyError {
    payload_id: String,
    code: &'static str,
    error: String,
}

#[derive(Serialize)]
struct ApplyResult {
    updated: Vec<UpdateOutcome>,
    errors: Vec<ApplyError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pm_sync: Option<PmSyncReport>,
}

/// Update `ids` (or everything the last check found), then sync `console` when given.
fn apply_updates(
    lib: &Library,
    ids: Option<Vec<String>>,
    console: Option<&str>,
    pm: &PmFactory,
) -> ApplyResult {
    let ids = ids.unwrap_or_else(|| {
        let mut state = lib.update_state();
        if state.checked_at == 0 {
            if let Ok(s) = lib.check_updates(false, &lib.pm_sync_ids()) {
                state = s;
            }
        }
        state.available().map(|r| r.payload_id.clone()).collect()
    });
    let mut out = ApplyResult {
        updated: Vec::new(),
        errors: Vec::new(),
        pm_sync: None,
    };
    for id in ids {
        match lib.update_payload(&id) {
            Ok(o) => {
                if o.updated {
                    crate::log_info!("payload update: {id} {} -> {}", o.from, o.to);
                }
                out.updated.push(o);
            }
            Err(e) => {
                crate::log_warn!("payload update: {id} failed: {}", e.message);
                out.errors.push(ApplyError {
                    payload_id: id,
                    code: e.code,
                    error: e.message,
                });
            }
        }
    }
    if let Some(host) = console.map(str::trim).filter(|h| !h.is_empty()) {
        out.pm_sync = Some(lib.sync_payload_manager(host, pm(host).as_ref()));
    }
    out
}

/// POST /api/payloads/updates/apply `{ids?, console?}` → `{updated, errors, pm_sync?}`.
/// Each update keeps the copy it replaces for a rollback; one failure does not stop the rest.
async fn apply(State(api): State<Api>, body: axum::body::Bytes) -> Response {
    let req: ApplyReq = match parse_or_default(&body) {
        Ok(r) => r,
        Err(r) => return *r,
    };
    api.run(move |lib, pm| Ok(apply_updates(&lib, req.ids, req.console.as_deref(), &pm)))
        .await
}

/// POST /api/payloads/{id}/rollback → what was put back. 404 `no_previous` when no earlier copy
/// is kept.
async fn rollback(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run(move |lib, _| lib.rollback_payload(&id)).await
}

/// GET /api/payloads/updates/settings → `{auto_update}`.
async fn get_settings(State(api): State<Api>) -> Response {
    api.run(|lib, _| Ok(lib.update_settings())).await
}

/// PUT /api/payloads/updates/settings `{auto_update}`: "Keep payloads up to date" checks at
/// start and daily, and applies what is newer.
async fn put_settings(State(api): State<Api>, Json(s): Json<UpdateSettings>) -> Response {
    api.run(move |lib, _| lib.set_update_settings(&s)).await
}

#[derive(Deserialize)]
struct HostQuery {
    #[serde(alias = "console", alias = "addr")]
    host: String,
}

/// GET /api/payloads/pm-sync?host= → what this console's Payload Manager keeps and loads.
async fn get_choice(State(api): State<Api>, Query(q): Query<HostQuery>) -> Response {
    api.run(move |lib, _| Ok(lib.pm_sync_choice(&q.host))).await
}

#[derive(Deserialize)]
struct ChoiceReq {
    #[serde(alias = "console", alias = "addr")]
    host: String,
    #[serde(default)]
    ids: Vec<String>,
    #[serde(default)]
    autoload_ids: Vec<String>,
}

/// PUT /api/payloads/pm-sync `{host, ids, autoload_ids}`: the payloads Payload Manager keeps,
/// and those it loads at boot. Takes effect on the next sync.
async fn put_choice(State(api): State<Api>, Json(req): Json<ChoiceReq>) -> Response {
    api.run(move |lib, _| lib.set_pm_sync_choice(&req.host, &req.ids, &req.autoload_ids))
        .await
}

#[derive(Deserialize)]
struct RunReq {
    #[serde(alias = "console", alias = "addr")]
    host: String,
}

/// POST /api/payloads/pm-sync/run `{host}` → what changed. `present: false` when Payload
/// Manager doesn't answer; nothing else is touched then.
async fn run_sync(State(api): State<Api>, Json(req): Json<RunReq>) -> Response {
    api.run(move |lib, pm| {
        let r = lib.sync_payload_manager(&req.host, pm(&req.host).as_ref());
        crate::log_info!(
            "pldmgr sync: host={} present={} changes={}",
            r.host,
            r.present,
            r.changes.len()
        );
        Ok(r)
    })
    .await
}

/// How often the schedule looks at whether a run is due.
const TICK: Duration = Duration::from_secs(30 * 60);
/// A run a day.
const DAY_SECS: i64 = 24 * 60 * 60;

/// One automatic run, when "Keep payloads up to date" is on and it is due (at start, then
/// daily): check, update what is newer, then sync every console with chosen payloads.
fn auto_run(lib: &Library, at_start: bool, pm: &PmFactory) -> bool {
    if !lib.update_settings().auto_update {
        return false;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let last = lib.update_state().last_auto_run_at;
    if !at_start && now - last < DAY_SECS {
        return false;
    }
    let _ = lib.edit_update_state(|s| s.last_auto_run_at = now);
    let state = match lib.check_updates(false, &lib.pm_sync_ids()) {
        Ok(s) => s,
        Err(e) => {
            crate::log_warn!("payload auto-update: check failed: {}", e.message);
            return true;
        }
    };
    let ids: Vec<String> = state.available().map(|r| r.payload_id.clone()).collect();
    if !ids.is_empty() {
        apply_updates(lib, Some(ids), None, pm);
    }
    for host in lib.pm_sync_hosts() {
        lib.sync_payload_manager(&host, pm(&host).as_ref());
    }
    true
}

/// Start the "Keep payloads up to date" schedule. Off unless the user turned it on.
pub(crate) fn spawn_scheduler() {
    let Some(lib) = crate::payloads_api::library() else {
        return;
    };
    tokio::spawn(async move {
        // Let the app come up first.
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut at_start = true;
        loop {
            let (l, pm) = (lib.clone(), http_pm());
            let _ = tokio::task::spawn_blocking(move || auto_run(&l, at_start, &pm)).await;
            at_start = false;
            tokio::time::sleep(TICK).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_core::payload_library::{FetchKind, HttpClient, HttpResponse};
    use ps5upload_core::payload_manager::{Autoload, StoreCheck, StoredPayload};
    use serde_json::Value;

    /// No network: every URL answers 404.
    struct Offline;
    impl HttpClient for Offline {
        fn get(&self, _: &str, _: Option<&str>, _: FetchKind) -> Result<HttpResponse, String> {
            Ok(HttpResponse {
                status: 404,
                location: None,
                content_length: Some(0),
                body: Box::new(std::io::Cursor::new(Vec::new())),
            })
        }
    }

    /// A console without Payload Manager.
    struct NoPm;
    impl PmConsole for NoPm {
        fn version(&self) -> Result<String, String> {
            Err("no Payload Manager on :8084 (connection refused)".into())
        }
        fn list(&self) -> Result<Vec<StoredPayload>, String> {
            unreachable!("nothing is asked once version fails")
        }
        fn check(&self, _: &str) -> Result<StoreCheck, String> {
            unreachable!()
        }
        fn store(&self, _: &str, _: &[u8]) -> Result<(), String> {
            unreachable!()
        }
        fn delete(&self, _: &str) -> Result<(), String> {
            unreachable!()
        }
        fn autoload_get(&self) -> Result<Autoload, String> {
            unreachable!()
        }
        fn autoload_set(&self, _: bool, _: &[String]) -> Result<(), String> {
            unreachable!()
        }
    }

    struct Harness {
        base: String,
        dir: std::path::PathBuf,
        lib: Library,
        agent: ureq::Agent,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn serve(tag: &str) -> Harness {
        let dir = std::env::temp_dir().join(format!(
            "ps5upload-payload-updates-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = Library::with_http(dir.join("payloads"), Arc::new(Offline));
        let pm: PmFactory = Arc::new(|_: &str| Box::new(NoPm) as Box<dyn PmConsole>);
        let app: Router = routes_with(Some(lib.clone()), pm);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let cfg = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .proxy(None)
            .build();
        Harness {
            base,
            dir,
            lib,
            agent: ureq::Agent::new_with_config(cfg),
        }
    }

    impl Harness {
        async fn call(
            &self,
            method: &'static str,
            path: &str,
            body: Option<Value>,
        ) -> (u16, Value) {
            let (agent, url) = (self.agent.clone(), format!("{}{path}", self.base));
            tokio::task::spawn_blocking(move || {
                let mut r = match (method, body) {
                    ("GET", _) => agent.get(&url).call(),
                    ("PUT", Some(b)) => agent
                        .put(&url)
                        .header("content-type", "application/json")
                        .send(b.to_string()),
                    (_, Some(b)) => agent
                        .post(&url)
                        .header("content-type", "application/json")
                        .send(b.to_string()),
                    (_, None) => agent.post(&url).send_empty(),
                }
                .unwrap();
                let text = r.body_mut().read_to_string().unwrap_or_default();
                (
                    r.status().as_u16(),
                    serde_json::from_str(&text).unwrap_or(Value::Null),
                )
            })
            .await
            .unwrap()
        }
    }

    fn cache(lib: &Library, id: &str, version: &str) {
        let dir = lib.dir().join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kstuff.elf"), b"\x7FELF....").unwrap();
        std::fs::write(dir.join("VERSION"), version).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_are_off_by_default_and_round_trip() {
        let h = serve("settings").await;
        let (s, v) = h.call("GET", "/api/payloads/updates/settings", None).await;
        assert_eq!((s, v), (200, json!({"auto_update": false})));
        let (s, v) = h
            .call(
                "PUT",
                "/api/payloads/updates/settings",
                Some(json!({"auto_update": true})),
            )
            .await;
        assert_eq!((s, v["auto_update"].clone()), (200, json!(true)));
        let (_, v) = h.call("GET", "/api/payloads/updates", None).await;
        assert_eq!(v["settings"]["auto_update"], true);
        assert_eq!(v["state"]["checked_at"], 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_check_offline_says_why_per_payload() {
        let h = serve("check").await;
        cache(&h.lib, "kstuff-echostretch", "v1");
        let (s, v) = h.call("POST", "/api/payloads/updates/check", None).await;
        assert_eq!(s, 200, "{v}");
        let row = &v["rows"][0];
        assert_eq!(row["payload_id"], "kstuff-echostretch");
        assert_eq!(row["installed_version"], "v1");
        assert_eq!(row["update_available"], false);
        assert!(row["error"].as_str().unwrap().contains("404"));
        assert!(v["checked_at"].as_i64().unwrap() > 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rollback_without_a_kept_copy_is_404_with_its_code() {
        let h = serve("rollback").await;
        cache(&h.lib, "kstuff-echostretch", "v1");
        let (s, v) = h
            .call("POST", "/api/payloads/kstuff-echostretch/rollback", None)
            .await;
        assert_eq!((s, v["code"].clone()), (404, json!("no_previous")));
        let (s, v) = h.call("POST", "/api/payloads/nope/rollback", None).await;
        assert_eq!((s, v["code"].clone()), (404, json!("unknown_payload")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn without_payload_manager_the_sync_says_so_and_the_choice_stays() {
        let h = serve("pm-absent").await;
        cache(&h.lib, "kstuff-echostretch", "v1");
        let (s, c) = h
            .call(
                "PUT",
                "/api/payloads/pm-sync",
                Some(
                    json!({"host": "192.0.2.9", "ids": [], "autoload_ids": ["kstuff-echostretch"]}),
                ),
            )
            .await;
        assert_eq!(s, 200, "{c}");
        assert_eq!(c["ids"], json!(["kstuff-echostretch"]));
        let (s, r) = h
            .call(
                "POST",
                "/api/payloads/pm-sync/run",
                Some(json!({"host": "192.0.2.9"})),
            )
            .await;
        assert_eq!(s, 200);
        assert_eq!(r["present"], false);
        assert!(r["message"].as_str().unwrap().contains("isn't running"));
        let (_, c) = h
            .call("GET", "/api/payloads/pm-sync?host=192.0.2.9", None)
            .await;
        assert_eq!(c["autoload_ids"], json!(["kstuff-echostretch"]));
        // Applying with nothing available still reports the console's sync.
        let (s, a) = h
            .call(
                "POST",
                "/api/payloads/updates/apply",
                Some(json!({"console": "192.0.2.9"})),
            )
            .await;
        assert_eq!(s, 200, "{a}");
        assert_eq!(a["updated"], json!([]));
        assert_eq!(a["pm_sync"]["present"], false);
        let (_, o) = h.call("GET", "/api/payloads/updates", None).await;
        assert_eq!(o["state"]["pm_syncs"]["192.0.2.9"]["present"], false);
    }

    #[test]
    fn the_schedule_does_nothing_until_turned_on_then_runs_daily() {
        let dir = std::env::temp_dir().join(format!(
            "ps5upload-payload-updates-auto-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let lib = Library::with_http(dir.join("payloads"), Arc::new(Offline));
        let pm: PmFactory = Arc::new(|_: &str| Box::new(NoPm) as Box<dyn PmConsole>);
        assert!(!auto_run(&lib, true, &pm), "off by default");
        lib.set_update_settings(&UpdateSettings { auto_update: true })
            .unwrap();
        assert!(auto_run(&lib, true, &pm), "at start");
        assert!(lib.update_state().last_auto_run_at > 0);
        assert!(!auto_run(&lib, false, &pm), "not again the same day");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
