//! `/api/payloads`: the Payload Library over HTTP, for the web UI and any API client. Thin over
//! `ps5upload_core::payload_library`, the same code the desktop app's Payloads screen calls.
//!
//! The library's folder is `PS5UPLOAD_PAYLOADS_DIR` (the desktop app points its sidecar at its
//! own cache), else `payloads` under the engine's data folder.
//!
//! The built-in catalogue is a list of links: its payloads are never downloaded (their old download
//! routes answer 410 `payload_downloads_removed` with where to get the file). The payload sources a
//! user adds (`/api/payloads/repos`) are: their releases are read when asked, and their file is
//! downloaded when asked, never in the background.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use ps5upload_core::payload_library::{Library, LibraryError, PayloadInfo, SourceError};
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
        "unknown_payload"
        | "unknown_release"
        | "no_previous"
        // `not_downloaded`: the older name of `not_on_this_computer`, still accepted.
        | "not_downloaded"
        | "not_on_this_computer" => StatusCode::NOT_FOUND,
        "repo_host_is_ip" | "repo_host_gitlab" | "repo_host_invalid" | "repo_owner_invalid"
        | "repo_name_invalid" | "no_asset" | "bad_host" => StatusCode::BAD_REQUEST,
        "custom_repos_unreadable" => StatusCode::CONFLICT,
        // A file that is not a payload: what was sent, not a server further on.
        "asset_not_payload" => StatusCode::UNPROCESSABLE_ENTITY,
        "io_failed" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_GATEWAY,
    }
}

/// The status for a payload source's error code.
fn source_status(code: &str) -> StatusCode {
    match code {
        c if c.starts_with("url_") => StatusCode::BAD_REQUEST,
        "invalid_file_name" | "needs_file" => StatusCode::BAD_REQUEST,
        "unknown_source" | "project_not_found" | "no_matching_file" => StatusCode::NOT_FOUND,
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        other => status_of(other),
    }
}

/// A payload source's error: `{error, code, retry_at?}` (`retry_at`: when the host said to ask
/// again, seconds since the Unix epoch).
fn source_error(e: &SourceError) -> Response {
    let mut body = json!({ "error": e.message, "code": e.code });
    if let Some(at) = e.retry_at {
        body["retry_at"] = json!(at);
    }
    (source_status(&e.code), Json(body)).into_response()
}

pub(crate) fn library_error(e: &LibraryError) -> Response {
    (
        status_of(e.code),
        Json(json!({ "error": e.message, "code": e.code })),
    )
        .into_response()
}

/// The code a route that used to download a payload answers with (HTTP 410 Gone).
pub(crate) const DOWNLOADS_REMOVED: &str = "payload_downloads_removed";

/// What to tell a client that asked for a payload download: where the user gets the file.
pub(crate) fn downloads_removed_text(info: Option<&PayloadInfo>) -> String {
    match info {
        Some(p) if !p.homepage.is_empty() => format!(
            "ps5upload no longer downloads payloads: get {} from its project page ({}) and send \
             the file.",
            p.display_name, p.homepage
        ),
        Some(p) => format!(
            "ps5upload no longer downloads payloads: get {} from its project page and send the \
             file.",
            p.display_name
        ),
        None => "ps5upload no longer downloads payloads: get the payload from its project page \
                 and send the file."
            .to_string(),
    }
}

/// 410 Gone with [`DOWNLOADS_REMOVED`].
pub(crate) fn downloads_removed(text: String) -> Response {
    (
        StatusCode::GONE,
        Json(json!({ "error": text, "code": DOWNLOADS_REMOVED })),
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

impl Api {
    /// As [`Api::run`], for the payload sources' calls and their errors.
    async fn run_source<T: serde::Serialize + Send + 'static>(
        &self,
        f: impl FnOnce(Library) -> Result<T, SourceError> + Send + 'static,
    ) -> Response {
        let Some(lib) = self.0.clone() else {
            return no_library();
        };
        match tokio::task::spawn_blocking(move || f(lib)).await {
            Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
            Ok(Err(e)) => source_error(&e),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
                .into_response(),
        }
    }
}

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    // A merge, so the API description's generator follows it to the routes.
    Router::new().merge(router_with(library()))
}

fn router_with<S: Clone + Send + Sync + 'static>(library: Option<Library>) -> Router<S> {
    Router::new()
        .route("/api/payloads", get(list))
        .route("/api/payloads/repos", get(list_repos).post(add_repo))
        .route("/api/payloads/repos/preview", post(preview_repo))
        .route("/api/payloads/repos/{id}", delete(remove_repo))
        .route("/api/payloads/repos/{id}/file", put(set_repo_file))
        .route("/api/payloads/repos/{id}/download", post(download_repo))
        .route("/api/payloads/sources", get(list_sources).post(add_source))
        .route("/api/payloads/sources/refresh", post(refresh_sources))
        .route("/api/payloads/sources/{id}", delete(remove_source))
        .route("/api/payloads/{id}/releases", get(releases))
        .route("/api/payloads/{id}/releases/latest", get(latest))
        .route("/api/payloads/{id}/download", post(download))
        .with_state(Api(library.clone()))
        .merge(crate::payload_updates_api::routes(library))
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

/// POST /api/payloads/{id}/download: refused (410 `payload_downloads_removed`). The app no
/// longer downloads payloads; the answer names the payload's project page.
async fn download(State(api): State<Api>, Path(id): Path<String>) -> Response {
    let info = match &api.0 {
        Some(lib) => {
            let lib = lib.clone();
            tokio::task::spawn_blocking(move || lib.catalog().into_iter().find(|p| p.id == id))
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    downloads_removed(downloads_removed_text(info.as_ref()))
}

#[derive(Deserialize, Default)]
struct SourcesQuery {
    #[serde(default)]
    check: Option<String>,
    #[serde(default)]
    force: Option<String>,
}

fn is_yes(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1") | Some("true"))
}

/// GET /api/payloads/repos?check=1&force=1 → {sources}: the payload sources the user added.
/// Without `check`, nothing asks the network (a release list read in the last ten minutes is
/// shown). With it, each source's releases are read (a list from the last ten minutes is used
/// unless `force`).
async fn list_repos(State(api): State<Api>, Query(q): Query<SourcesQuery>) -> Response {
    let (check, force) = (is_yes(&q.check), is_yes(&q.force));
    api.run_source(move |lib| Ok(json!({ "sources": lib.user_sources(check, force)? })))
        .await
}

#[derive(Deserialize)]
struct PreviewBody {
    url: String,
}

/// POST /api/payloads/repos/preview {url} → the project's newest releases with `.elf` and `.bin`
/// files, for picking the file. Saves nothing.
async fn preview_repo(State(api): State<Api>, Json(b): Json<PreviewBody>) -> Response {
    api.run_source(move |lib| lib.preview_user_source(&b.url))
        .await
}

#[derive(Deserialize)]
struct AddBody {
    url: String,
    file_name: String,
}

/// POST /api/payloads/repos {url, file_name} → the source: adds a project as a payload source
/// with the file picked from its releases (adding one again changes its file). Asks no network.
async fn add_repo(State(api): State<Api>, Json(b): Json<AddBody>) -> Response {
    api.run_source(move |lib| lib.add_user_source(&b.url, &b.file_name))
        .await
}

#[derive(Deserialize)]
struct FileBody {
    file_name: String,
}

/// PUT /api/payloads/repos/{id}/file {file_name} → the source: change the file it downloads, or
/// pick it for a source added before 12.6.0.
async fn set_repo_file(
    State(api): State<Api>,
    Path(id): Path<String>,
    Json(b): Json<FileBody>,
) -> Response {
    api.run_source(move |lib| lib.set_user_source_file(&id, &b.file_name))
        .await
}

/// POST /api/payloads/repos/{id}/download → the source: downloads the newest release's file into
/// the payload folder (through a temporary file, checked against its size and the release's
/// checksum file when there is one). The copy it replaces stays if anything fails.
async fn download_repo(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run_source(move |lib| lib.download_user_source(&id))
        .await
}

/// DELETE /api/payloads/repos/{id} → {removed}: the source and its downloaded file.
async fn remove_repo(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run(move |lib| Ok(json!({ "removed": lib.remove_custom_repo(&id)? })))
        .await
}

/// GET /api/payloads/sources → {sources}: the payload lists added (#829), from the copies on disk.
async fn list_sources(State(api): State<Api>) -> Response {
    api.run(|lib| Ok(json!({ "sources": lib.sources() }))).await
}

/// POST /api/payloads/sources: refused (410 `payload_downloads_removed`). Lists added earlier
/// stay listed and can be removed.
async fn add_source() -> Response {
    downloads_removed(
        "ps5upload no longer downloads payloads, so payload lists can't be added: get the \
         payload from its project page and send the file."
            .to_string(),
    )
}

/// POST /api/payloads/sources/refresh: refused (410 `payload_downloads_removed`).
async fn refresh_sources() -> Response {
    downloads_removed(
        "ps5upload no longer downloads payloads or payload lists: get the payload from its \
         project page and send the file."
            .to_string(),
    )
}

/// DELETE /api/payloads/sources/{id} → {removed}: the list, and the local copies of its payloads.
async fn remove_source(State(api): State<Api>, Path(id): Path<String>) -> Response {
    api.run(move |lib| Ok(json!({ "removed": lib.remove_source(&id)? })))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn a_file_that_is_not_a_payload_is_the_requests_fault_not_a_gateways() {
        assert_eq!(
            status_of("asset_not_payload"),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(status_of("not_on_this_computer"), StatusCode::NOT_FOUND);
        assert_eq!(status_of("not_downloaded"), StatusCode::NOT_FOUND);
    }

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
        serve_lib(tag, Library::new).await
    }

    async fn serve_lib(tag: &str, make: impl FnOnce(std::path::PathBuf) -> Library) -> Harness {
        let dir = crate::test_tmp::temp_path(format!(
            "ps5upload-payloads-api-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let app: Router = router_with(Some(make(dir.join("payloads"))));
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
        async fn put(&self, path: &str, body: Value) -> (u16, Value) {
            let (agent, url) = (self.agent.clone(), format!("{}{path}", self.base));
            tokio::task::spawn_blocking(move || {
                let mut r = agent
                    .put(&url)
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
    async fn the_catalogue_lists_and_a_saved_custom_repo_can_be_removed() {
        let h = serve("repos").await;
        let (status, v) = h.get("/api/payloads").await;
        assert_eq!(status, 200);
        let curated = v["payloads"].as_array().unwrap().len();
        assert!(curated > 10);
        assert_eq!(v["local"], json!([]));
        assert!(v.get("custom_repos_error").is_none());

        // A repo an earlier version added stays listed and can be removed.
        let id = "custom-0a1b2c3d4e5f";
        std::fs::create_dir_all(h.dir.join("payloads")).unwrap();
        std::fs::write(
            h.dir.join("payloads").join("custom-repos.json"),
            json!([{
                "id": id,
                "display_name": "Me/thing",
                "repo_host": "github.com",
                "repo_owner": "Me",
                "repo_name": "thing",
            }])
            .to_string(),
        )
        .unwrap();
        let (_, v) = h.get("/api/payloads").await;
        let payloads = v["payloads"].as_array().unwrap();
        assert_eq!(payloads.len(), curated + 1);
        assert!(payloads.iter().any(|p| p["id"] == id));
        let (status, gone) = h.delete(&format!("/api/payloads/repos/{id}")).await;
        assert_eq!((status, gone["removed"].clone()), (200, json!(true)));
        let (_, v) = h.get("/api/payloads").await;
        assert_eq!(v["payloads"].as_array().unwrap().len(), curated);
        let (_, gone) = h.delete(&format!("/api/payloads/repos/{id}")).await;
        assert_eq!(gone["removed"], false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_download_route_is_refused_with_where_to_get_the_file() {
        let h = serve("refused").await;
        let (status, v) = h
            .post("/api/payloads/kstuff-echostretch/download", json!({}))
            .await;
        assert_eq!((status, v["code"].as_str()), (410, Some(DOWNLOADS_REMOVED)));
        let text = v["error"].as_str().unwrap();
        assert!(text.contains("no longer downloads payloads"), "{text}");
        assert!(
            text.contains("https://github.com/EchoStretch/kstuff-lite"),
            "{text}"
        );
        let (status, v) = h.post("/api/payloads/nope/download", json!({})).await;
        assert_eq!((status, v["code"].as_str()), (410, Some(DOWNLOADS_REMOVED)));
        for path in ["/api/payloads/sources", "/api/payloads/sources/refresh"] {
            let (status, v) = h
                .post(path, json!({"host": "github.com", "owner": "a", "name": "b", "url": "https://x.org/p.json"}))
                .await;
            assert_eq!(
                (status, v["code"].as_str()),
                (410, Some(DOWNLOADS_REMOVED)),
                "{path}"
            );
        }
        assert!(!h.dir.join("payloads").join("custom-repos.json").exists());
        let (status, v) = h.get("/api/payloads/sources").await;
        assert_eq!((status, v["sources"].clone()), (200, json!([])));
        let (status, v) = h.delete("/api/payloads/sources/src-000000000000").await;
        assert_eq!((status, v["removed"].clone()), (200, json!(false)));
    }

    /// One scripted answer: status, body, and when to ask again.
    type Answer = (u16, Vec<u8>, Option<u64>);

    /// A made-up GitHub: url → answer. Unknown urls answer 404.
    #[derive(Default)]
    struct FakeHost(std::sync::Mutex<std::collections::HashMap<String, Answer>>);

    impl FakeHost {
        fn answer(&self, url: &str, status: u16, body: &[u8], retry_at: Option<u64>) {
            self.0
                .lock()
                .unwrap()
                .insert(url.to_string(), (status, body.to_vec(), retry_at));
        }
    }

    impl ps5upload_core::payload_library::HttpClient for FakeHost {
        fn get(
            &self,
            url: &str,
            _: Option<&str>,
            _: ps5upload_core::payload_library::FetchKind,
        ) -> Result<ps5upload_core::payload_library::HttpResponse, String> {
            let (status, body, retry_at) =
                self.0
                    .lock()
                    .unwrap()
                    .get(url)
                    .cloned()
                    .unwrap_or((404, Vec::new(), None));
            Ok(ps5upload_core::payload_library::HttpResponse {
                status,
                location: None,
                content_length: Some(body.len() as u64),
                retry_at,
                body: Box::new(std::io::Cursor::new(body)),
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_source_is_added_checked_downloaded_and_removed() {
        let host = std::sync::Arc::new(FakeHost::default());
        let api_url = "https://api.github.com/repos/me/kstuff/releases?per_page=10";
        let file_url = "https://github.com/me/kstuff/releases/download/v1.6.0/kstuff-v1.6.0.elf";
        let mut elf = b"\x7FELF".to_vec();
        elf.resize(64, 7);
        let list = json!([{
            "tag_name": "v1.6.0", "name": "kstuff 1.6.0", "published_at": "2026-11-01T00:00:00Z",
            "html_url": "https://github.com/me/kstuff/releases/tag/v1.6.0",
            "assets": [{ "name": "kstuff-v1.6.0.elf", "browser_download_url": file_url, "size": 64 }],
        }]);
        host.answer(api_url, 200, list.to_string().as_bytes(), None);
        host.answer(file_url, 200, &elf, None);
        let fake = host.clone();
        let h = serve_lib("sources", move |dir| Library::with_http(dir, fake)).await;

        let (status, v) = h.get("/api/payloads/repos").await;
        assert_eq!((status, v["sources"].clone()), (200, json!([])));

        let url = "https://github.com/me/kstuff";
        let (status, v) = h
            .post("/api/payloads/repos/preview", json!({ "url": url }))
            .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["releases"][0]["files"][0]["pattern"], "kstuff-v*.elf");

        let (status, v) = h
            .post(
                "/api/payloads/repos",
                json!({ "url": url, "file_name": "kstuff-v1.6.0.elf" }),
            )
            .await;
        assert_eq!(status, 200, "{v}");
        let id = v["id"].as_str().unwrap().to_string();
        assert!(v.get("have").is_none());

        let (_, v) = h.get("/api/payloads/repos?check=1").await;
        assert_eq!(v["sources"][0]["latest"]["tag"], "v1.6.0");

        let (status, v) = h
            .post(&format!("/api/payloads/repos/{id}/download"), json!({}))
            .await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["have"]["size"], 64);
        assert_eq!(v["have"]["sha256"].as_str().unwrap().len(), 64);
        let path = v["have"]["path"].as_str().unwrap().to_string();
        assert_eq!(std::fs::read(&path).unwrap(), elf);
        // The library lists it as a local copy too, so a send by id finds it.
        let (_, v) = h.get("/api/payloads").await;
        assert!(v["local"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["payload_id"] == id));

        let (status, v) = h
            .put(
                &format!("/api/payloads/repos/{id}/file"),
                json!({ "file_name": "x.zip" }),
            )
            .await;
        assert_eq!(
            (status, v["code"].as_str()),
            (400, Some("invalid_file_name"))
        );

        let (status, v) = h.delete(&format!("/api/payloads/repos/{id}")).await;
        assert_eq!((status, v["removed"].clone()), (200, json!(true)));
        assert!(!std::path::Path::new(&path).exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn source_errors_say_why_with_a_fitting_status() {
        let host = std::sync::Arc::new(FakeHost::default());
        host.answer(
            "https://api.github.com/repos/me/kstuff/releases?per_page=10",
            403,
            b"{}",
            Some(1_900_000_000),
        );
        let fake = host.clone();
        let h = serve_lib("source-errors", move |dir| Library::with_http(dir, fake)).await;
        let (status, v) = h
            .post(
                "/api/payloads/repos/preview",
                json!({ "url": "https://github.com/me/kstuff" }),
            )
            .await;
        assert_eq!((status, v["code"].as_str()), (429, Some("rate_limited")));
        assert_eq!(v["retry_at"], 1_900_000_000);
        let (status, v) = h
            .post(
                "/api/payloads/repos/preview",
                json!({ "url": "http://github.com/me/kstuff" }),
            )
            .await;
        assert_eq!((status, v["code"].as_str()), (400, Some("url_not_https")));
        let (status, v) = h
            .post(
                "/api/payloads/repos/preview",
                json!({ "url": "https://127.0.0.1:19113/a/b" }),
            )
            .await;
        assert_eq!(
            (status, v["code"].as_str()),
            (400, Some("url_host_not_allowed"))
        );
        let (status, v) = h
            .post("/api/payloads/repos/custom-0a/download", json!({}))
            .await;
        assert_eq!((status, v["code"].as_str()), (404, Some("unknown_source")));
        assert!(!h.dir.join("payloads").exists(), "nothing written");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn errors_carry_a_code_and_a_fitting_status() {
        let h = serve("errors").await;
        let (status, v) = h.get("/api/payloads/nope/releases").await;
        assert_eq!((status, v["code"].as_str()), (404, Some("unknown_payload")));
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
