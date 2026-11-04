//! `/api/payloads`: the Payload Library over HTTP, for the web UI and any API client. Thin over
//! `ps5upload_core::payload_library`, the same code the desktop app's Payloads screen calls.
//!
//! The library's folder is `PS5UPLOAD_PAYLOADS_DIR` (the desktop app points its sidecar at its
//! own cache), else `payloads` under the engine's data folder.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use ps5upload_core::payload_library::{AddRepo, Library, LibraryError};
use serde::Deserialize;
use serde_json::json;

/// The library this engine serves, or `None` when it has no data folder at all.
pub(crate) fn library() -> Option<Library> {
    Library::from_env(crate::remote::store::data_dir())
}

pub(crate) fn no_library() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "no folder to keep payloads in; set PS5UPLOAD_DATA_DIR",
            "code": "no_data_dir",
        })),
    )
        .into_response()
}

fn status_of(code: &str) -> StatusCode {
    match code {
        "unknown_payload" | "unknown_release" => StatusCode::NOT_FOUND,
        "repo_host_is_ip"
        | "repo_host_gitlab"
        | "repo_host_invalid"
        | "repo_owner_invalid"
        | "repo_name_invalid"
        | "invalid_checksum_file"
        | "asset_url_refused"
        | "no_asset" => StatusCode::BAD_REQUEST,
        "custom_repos_unreadable" | "download_cancelled" => StatusCode::CONFLICT,
        "io_failed" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_GATEWAY,
    }
}

pub(crate) fn library_error(e: &LibraryError) -> Response {
    (
        status_of(e.code),
        Json(json!({ "error": e.message, "code": e.code })),
    )
        .into_response()
}

#[derive(Clone)]
struct Api(Option<Library>);

impl Api {
    /// Run a library call off the async runtime: it blocks on the network and the disk.
    async fn run<T: serde::Serialize + Send + 'static>(
        &self,
        f: impl FnOnce(Library) -> Result<T, LibraryError> + Send + 'static,
    ) -> Response {
        let Some(lib) = self.0.clone() else {
            return no_library();
        };
        match tokio::task::spawn_blocking(move || f(lib)).await {
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

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    router_with(library())
}

fn router_with<S: Clone + Send + Sync + 'static>(library: Option<Library>) -> Router<S> {
    Router::new()
        .route("/api/payloads", get(list))
        .route("/api/payloads/repos", post(add_repo))
        .route("/api/payloads/repos/{id}", delete(remove_repo))
        .route("/api/payloads/{id}/releases", get(releases))
        .route("/api/payloads/{id}/releases/latest", get(latest))
        .route("/api/payloads/{id}/download", post(download))
        .route("/api/payloads/{id}/download/cancel", post(cancel))
        .with_state(Api(library))
}

/// GET /api/payloads: the catalogue with the user's repos, and what is cached.
async fn list(State(api): State<Api>) -> Response {
    api.run(|lib| {
        let mut out = json!({
            "payloads": lib.catalog(),
            "local": lib.local_inventory(),
        });
        // A list that can't be read shows only the curated entries; say why.
        if let Some(why) = lib.custom_repos_problem() {
            out["custom_repos_error"] = json!(why);
        }
        Ok(out)
    })
    .await
}

#[derive(Deserialize, Default)]
struct ReleaseQuery {
    #[serde(default, alias = "force_refresh")]
    force: Option<String>,
}

impl ReleaseQuery {
    fn force(&self) -> bool {
        matches!(self.force.as_deref(), Some("1") | Some("true"))
    }
}

/// GET /api/payloads/{id}/releases?force=1: newest first, cached for an hour.
async fn releases(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(q): Query<ReleaseQuery>,
) -> Response {
    let force = q.force();
    api.run(move |lib| lib.releases(&id, force)).await
}

/// GET /api/payloads/{id}/releases/latest?force=1
async fn latest(
    State(api): State<Api>,
    Path(id): Path<String>,
    Query(q): Query<ReleaseQuery>,
) -> Response {
    let force = q.force();
    api.run(move |lib| lib.release(&id, force)).await
}

#[derive(Deserialize, Default)]
struct DownloadRequest {
    /// A release's `picked_asset_url`, with its `version`, as the screens send it.
    #[serde(default)]
    asset_url: Option<String>,
    #[serde(default)]
    version: Option<String>,
    /// A release tag. Absent with no `asset_url`: the latest.
    #[serde(default)]
    release: Option<String>,
}

/// POST /api/payloads/{id}/download: fetch a release into the cache. Takes as long as the
/// download does; `/download/cancel` stops it.
async fn download(
    State(api): State<Api>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let req: DownloadRequest = if body.is_empty() {
        DownloadRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": format!("bad request: {e}") })),
                )
                    .into_response()
            }
        }
    };
    api.run(move |lib| match req.asset_url {
        Some(url) => lib.download(&id, &url, req.version.as_deref().unwrap_or_default()),
        None => lib.download_release(&id, req.release.as_deref(), false),
    })
    .await
}

/// POST /api/payloads/{id}/download/cancel → {cancelled}. The download's own request answers
/// 409 `download_cancelled`.
async fn cancel(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run(move |lib| Ok(json!({ "cancelled": lib.cancel_download(&id) })))
        .await
}

/// POST /api/payloads/repos {host, owner, name, display_name?, asset_hint?, checksum_file?}
async fn add_repo(State(api): State<Api>, Json(req): Json<AddRepo>) -> Response {
    api.run(move |lib| lib.add_custom_repo(&req)).await
}

/// DELETE /api/payloads/repos/{id} → {removed}
async fn remove_repo(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run(move |lib| Ok(json!({ "removed": lib.remove_custom_repo(&id)? })))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn read_body(r: &mut ureq::http::Response<ureq::Body>) -> Value {
        let text = r.body_mut().read_to_string().unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    struct Harness {
        base: String,
        dir: std::path::PathBuf,
        agent: ureq::Agent,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn serve(tag: &str) -> Harness {
        let dir = std::env::temp_dir().join(format!(
            "ps5upload-payloads-api-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let app: Router = router_with(Some(Library::new(dir.join("payloads"))));
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
            agent: ureq::Agent::new_with_config(cfg),
        }
    }

    impl Harness {
        async fn get(&self, path: &str) -> (u16, Value) {
            let (agent, url) = (self.agent.clone(), format!("{}{path}", self.base));
            tokio::task::spawn_blocking(move || {
                let mut r = agent.get(&url).call().unwrap();
                (r.status().as_u16(), read_body(&mut r))
            })
            .await
            .unwrap()
        }
        async fn post(&self, path: &str, body: Value) -> (u16, Value) {
            let (agent, url) = (self.agent.clone(), format!("{}{path}", self.base));
            tokio::task::spawn_blocking(move || {
                let mut r = agent
                    .post(&url)
                    .header("content-type", "application/json")
                    .send(body.to_string())
                    .unwrap();
                (r.status().as_u16(), read_body(&mut r))
            })
            .await
            .unwrap()
        }
        async fn delete(&self, path: &str) -> (u16, Value) {
            let (agent, url) = (self.agent.clone(), format!("{}{path}", self.base));
            tokio::task::spawn_blocking(move || {
                let mut r = agent.delete(&url).call().unwrap();
                (r.status().as_u16(), read_body(&mut r))
            })
            .await
            .unwrap()
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_catalogue_and_a_custom_repo_round_trip() {
        let h = serve("repos").await;
        let (status, v) = h.get("/api/payloads").await;
        assert_eq!(status, 200);
        let curated = v["payloads"].as_array().unwrap().len();
        assert!(curated > 10);
        assert_eq!(v["local"], json!([]));
        assert!(v.get("custom_repos_error").is_none());

        let (status, added) = h
            .post(
                "/api/payloads/repos",
                json!({"host": "www.github.com", "owner": "Me", "name": "thing.git"}),
            )
            .await;
        assert_eq!(status, 200, "{added}");
        assert_eq!(added["is_custom"], true);
        assert_eq!(added["homepage"], "https://github.com/Me/thing");
        let id = added["id"].as_str().unwrap().to_string();

        let (_, v) = h.get("/api/payloads").await;
        assert_eq!(v["payloads"].as_array().unwrap().len(), curated + 1);

        let (status, gone) = h.delete(&format!("/api/payloads/repos/{id}")).await;
        assert_eq!(status, 200);
        assert_eq!(gone["removed"], true);
        let (_, gone) = h.delete(&format!("/api/payloads/repos/{id}")).await;
        assert_eq!(gone["removed"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn errors_carry_a_code_and_a_fitting_status() {
        let h = serve("errors").await;
        let (status, v) = h
            .post(
                "/api/payloads/repos",
                json!({"host": "192.168.1.5", "owner": "a", "name": "b"}),
            )
            .await;
        assert_eq!((status, v["code"].as_str()), (400, Some("repo_host_is_ip")));

        let (status, v) = h.get("/api/payloads/nope/releases").await;
        assert_eq!((status, v["code"].as_str()), (404, Some("unknown_payload")));

        let (status, v) = h.post("/api/payloads/nope/download", json!({})).await;
        assert_eq!((status, v["code"].as_str()), (404, Some("unknown_payload")));

        // The request is pinned before any network.
        let (status, v) = h
            .post(
                "/api/payloads/kstuff-echostretch/download",
                json!({"asset_url": "http://127.0.0.1/x.elf", "version": "v1"}),
            )
            .await;
        assert_eq!(
            (status, v["code"].as_str()),
            (400, Some("asset_url_refused"))
        );

        let (status, v) = h
            .post(
                "/api/payloads/kstuff-echostretch/download/cancel",
                json!({}),
            )
            .await;
        assert_eq!(status, 200);
        assert_eq!(v["cancelled"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_repo_list_is_reported_and_kept() {
        let h = serve("unreadable").await;
        let dir = h.dir.join("payloads");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("custom-repos.json"), b"{ oops").unwrap();
        let (status, v) = h.get("/api/payloads").await;
        assert_eq!(status, 200);
        assert!(v["custom_repos_error"].is_string(), "{v}");
        let (status, v) = h
            .post(
                "/api/payloads/repos",
                json!({"host": "github.com", "owner": "a", "name": "b"}),
            )
            .await;
        assert_eq!(
            (status, v["code"].as_str()),
            (409, Some("custom_repos_unreadable"))
        );
        assert_eq!(
            std::fs::read(dir.join("custom-repos.json")).unwrap(),
            b"{ oops"
        );
    }

    #[tokio::test]
    async fn no_data_folder_says_what_to_set() {
        let app: Router = router_with(None);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/payloads", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let (status, v) = tokio::task::spawn_blocking(move || {
            let cfg = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .proxy(None)
                .build();
            let mut r = ureq::Agent::new_with_config(cfg).get(&url).call().unwrap();
            (r.status().as_u16(), read_body(&mut r))
        })
        .await
        .unwrap();
        assert_eq!(status, 503);
        assert_eq!(v["code"], "no_data_dir");
    }

    #[test]
    fn force_is_one_or_true() {
        let q = |s: Option<&str>| ReleaseQuery {
            force: s.map(String::from),
        };
        assert!(q(Some("1")).force() && q(Some("true")).force());
        assert!(!q(None).force() && !q(Some("0")).force());
    }
}
