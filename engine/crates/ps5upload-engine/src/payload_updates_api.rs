//! Payload updates and the Payload Manager sync over HTTP (`ps5upload_core::payload_library`'s
//! `updates` and `pm_sync`).
//!
//! - `GET  /api/payloads/updates`                 the last check and what was applied
//! - `POST /api/payloads/updates/check`           check for a newer release (no download)
//! - `POST /api/payloads/updates/apply`           refused: 410 `payload_downloads_removed`
//! - `POST /api/payloads/{id}/rollback`           put back the copy an earlier update replaced
//! - `GET|PUT /api/payloads/pm-sync`              what a console's Payload Manager keeps / loads
//! - `POST /api/payloads/pm-sync/run`             bring Payload Manager in step

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ps5upload_core::payload_library::{HttpPm, Library, LibraryError, PmConsole};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::payloads_api::{downloads_removed, library_error, no_library};

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
        .route("/api/payloads/{id}/rollback", post(rollback))
        .route("/api/payloads/pm-sync", get(get_choice).put(put_choice))
        .route("/api/payloads/pm-sync/run", post(run_sync))
        .with_state(Api { lib, pm })
}

/// GET /api/payloads/updates → `{state}`: the last check, what was applied and the last Payload
/// Manager syncs. Never asks the network or a console.
async fn overview(State(api): State<Api>) -> Response {
    api.run(|lib, _| {
        Ok(json!({
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

/// POST /api/payloads/updates/apply: refused (410 `payload_downloads_removed`). ps5upload no
/// longer downloads payloads; a check says what is newer, and the user gets it from the project
/// page.
async fn apply() -> Response {
    downloads_removed(
        "ps5upload no longer downloads payloads, so updates can't be applied: get the new \
         version from the payload's project page and send the file."
            .to_string(),
    )
}

/// POST /api/payloads/{id}/rollback → what was put back. 404 `no_previous` when no earlier copy
/// is kept (earlier versions of the app kept one when they updated a payload).
async fn rollback(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run(move |lib, _| lib.rollback_payload(&id)).await
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
    async fn the_overview_has_the_state_and_apply_is_refused() {
        let h = serve("overview").await;
        let (s, v) = h.call("GET", "/api/payloads/updates", None).await;
        assert_eq!((s, v["state"]["checked_at"].clone()), (200, json!(0)));
        assert!(v.get("settings").is_none());
        cache(&h.lib, "kstuff-echostretch", "v1");
        let (s, v) = h
            .call(
                "POST",
                "/api/payloads/updates/apply",
                Some(json!({"ids": ["kstuff-echostretch"]})),
            )
            .await;
        assert_eq!(
            (s, v["code"].clone()),
            (410, json!("payload_downloads_removed"))
        );
        assert!(v["error"].as_str().unwrap().contains("project page"));
        let (s, _) = h.call("GET", "/api/payloads/updates/settings", None).await;
        assert_eq!(s, 404, "the auto-update switch is gone");
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
        let (_, o) = h.call("GET", "/api/payloads/updates", None).await;
        assert_eq!(o["state"]["pm_syncs"]["192.0.2.9"]["present"], false);
    }
}
