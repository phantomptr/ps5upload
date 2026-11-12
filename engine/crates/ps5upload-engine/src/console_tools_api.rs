//! Named routes for what the desktop app could only reach through the generic
//! `POST /api/mgmt/call`: a console shell, the app database, a process's modules, file hashes,
//! appmeta healing, which third-party tools answer, and finding consoles on the network. Same
//! `ps5upload_core` calls and the same JSON as the app's commands, so the route table says what
//! the engine can do to a console.
//!
//! A console refusal inside a reply (`err`) stays in the 200 body, as the app's screens read it;
//! 502 means the console could not be asked. The console must be named: these never fall back
//! to a default one.

use axum::{
    body::Bytes,
    extract::{Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ps5upload_core::discover::{self, DiscoverOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

/// The console shell's own wait when the caller names none.
const SHELL_DEFAULT_TIMEOUT_SECS: u32 = 30;

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/api/discover", post(discover_handler))
        .route("/api/ps5/shell/run", post(shell_run))
        .route("/api/ps5/appdb", get(appdb))
        .route("/api/ps5/process/{pid}/modules", get(process_modules))
        .route("/api/ps5/fs/hash", post(fs_hash))
        .route("/api/ps5/image/packed", post(image_packed))
        .route("/api/ps5/appmeta/heal", post(appmeta_heal))
        .route("/api/ps5/companions", get(companions))
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

/// The console a request names, without a port. `None` is answered with [`no_addr`].
fn required_addr(addr: &str) -> Option<String> {
    let a = crate::console_addr(addr);
    (!a.is_empty()).then_some(a)
}

fn no_addr() -> Response {
    bad_request("addr is required")
}

/// Run a blocking console call and answer with its JSON, or 502 with the reason.
async fn console_call<T, E>(f: impl FnOnce() -> Result<T, E> + Send + 'static) -> Response
where
    T: Serialize + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let run = move || {
        #[cfg(test)]
        let _console = tests::scoped_console();
        f()
    };
    match tokio::task::spawn_blocking(run).await {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(e)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("{e:#}") })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("task: {e}") })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct AddrOnly {
    #[serde(default, alias = "console", alias = "host")]
    addr: String,
}

#[derive(Deserialize)]
struct ShellRunRequest {
    #[serde(default, alias = "console", alias = "host")]
    addr: String,
    cmd: String,
    #[serde(default)]
    cwd: Option<String>,
    /// Keeps `cd` and the environment across calls.
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default, alias = "timeout_secs")]
    timeout_s: Option<u32>,
}

/// POST /api/ps5/shell/run {addr, cmd, cwd?, session_id?, timeout_s?}
async fn shell_run(Json(req): Json<ShellRunRequest>) -> Response {
    let Some(addr) = required_addr(&req.addr) else {
        return no_addr();
    };
    if req.cmd.trim().is_empty() {
        return bad_request("cmd is required");
    }
    let timeout = req.timeout_s.unwrap_or(SHELL_DEFAULT_TIMEOUT_SECS);
    console_call(move || {
        ps5upload_core::diagnostics::shell_run(
            &addr,
            &req.cmd,
            req.session_id.as_deref(),
            req.cwd.as_deref(),
            timeout,
        )
    })
    .await
}

/// GET /api/ps5/appdb?addr=
async fn appdb(Query(q): Query<AddrOnly>) -> Response {
    let Some(addr) = required_addr(&q.addr) else {
        return no_addr();
    };
    console_call(move || ps5upload_core::diagnostics::appdb_query(&addr)).await
}

/// GET /api/ps5/process/{pid}/modules?addr=
async fn process_modules(Path(pid): Path<i32>, Query(q): Query<AddrOnly>) -> Response {
    let Some(addr) = required_addr(&q.addr) else {
        return no_addr();
    };
    console_call(move || ps5upload_core::diagnostics::proc_modules(&addr, pid)).await
}

#[derive(Deserialize)]
struct FsHashRequest {
    #[serde(default, alias = "console", alias = "host")]
    addr: String,
    path: String,
    /// `blake3` (the default) or `crc32`.
    #[serde(default)]
    algo: Option<String>,
}

/// POST /api/ps5/fs/hash {addr, path, algo: blake3|crc32}: a file on the console, hashed there.
/// Same reply as the app's commands: `{path, size, hash}` for BLAKE3, `{crc32, size, err}` for
/// CRC32. A big file takes as long as it takes.
async fn fs_hash(Json(req): Json<FsHashRequest>) -> Response {
    let Some(addr) = required_addr(&req.addr) else {
        return no_addr();
    };
    if !req.path.starts_with('/') {
        return bad_request("path must be absolute");
    }
    match req.algo.as_deref().unwrap_or("blake3") {
        "blake3" => {
            console_call(move || {
                ps5upload_core::fs_ops::fs_hash(&addr, &req.path)
                    .map(|h| json!({ "path": h.path, "size": h.size, "hash": h.hash }))
            })
            .await
        }
        "crc32" => {
            console_call(move || ps5upload_core::diagnostics::crc32_file(&addr, &req.path)).await
        }
        other => bad_request(format!("unknown algo {other:?}: use blake3 or crc32")),
    }
}

#[derive(Deserialize)]
struct ImagePackedRequest {
    #[serde(default, alias = "console", alias = "host")]
    addr: String,
    path: String,
}

/// POST /api/ps5/image/packed {addr, path}: whether a game image on the console keeps its files
/// in LZ4 or zstd packs. Reply `{path, packed}` with `packed` `"lz4"`, `"zstd"` or null. A packed
/// game only starts from a ShadowMount+ mount, so the app asks before it mounts one itself. The
/// image is read in place (its file list and eight bytes of the pack index), not copied.
async fn image_packed(Json(req): Json<ImagePackedRequest>) -> Response {
    let Some(addr) = required_addr(&req.addr) else {
        return no_addr();
    };
    if !req.path.starts_with('/') {
        return bad_request("path must be absolute");
    }
    console_call(move || {
        let url = format!("ps5://{addr}{}", req.path);
        let mut tree = ps5upload_fpkg::source::open(std::path::Path::new(&url))?;
        let packed = ps5upload_fpkg::source::packed_kind(tree.as_mut())?;
        Ok::<_, ps5upload_fpkg::Error>(json!({ "path": req.path, "packed": packed }))
    })
    .await
}

#[derive(Deserialize)]
struct HealRequest {
    #[serde(default, alias = "console", alias = "host")]
    addr: String,
    title_id: String,
    #[serde(alias = "src")]
    source_path: String,
}

/// POST /api/ps5/appmeta/heal {addr, title_id, source_path}: copy a title's missing
/// `/user/appmeta/<id>/` files from its game folder. What is already there is left alone.
async fn appmeta_heal(Json(req): Json<HealRequest>) -> Response {
    let Some(addr) = required_addr(&req.addr) else {
        return no_addr();
    };
    if !ps5upload_core::appmeta_heal::is_valid_title_id(&req.title_id) {
        return bad_request("title_id must be 4-16 chars of [A-Z0-9_] (e.g. CUSA00001)");
    }
    if !req.source_path.starts_with('/') {
        return bad_request("source_path must be absolute");
    }
    console_call(move || ps5upload_core::appmeta_heal::heal(&addr, &req.title_id, &req.source_path))
        .await
}

/// GET /api/ps5/companions?addr=: which third-party tools answer on their usual ports. A TCP
/// connect per port, so it needs no helper on the console.
async fn companions(Query(q): Query<AddrOnly>) -> Response {
    let Some(addr) = required_addr(&q.addr) else {
        return no_addr();
    };
    console_call(move || Ok::<_, String>(ps5upload_core::companions::probe_companions(&addr))).await
}

#[derive(Deserialize, Default)]
struct DiscoverRequest {
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// More /24 networks to sweep: `192.168.1.0/24`. For an engine that isn't on the consoles'
    /// LAN itself, such as Docker without host networking.
    #[serde(default)]
    subnets: Vec<String>,
}

/// Said in the reply from a container, where neither mDNS nor the interfaces see the LAN unless
/// the container uses host networking.
const DOCKER_NOTE: &str = "This engine runs in Docker. mDNS and the sweep of its own networks \
    only reach your LAN with host networking; otherwise name your LAN in \"subnets\".";

/// POST /api/discover {timeout_ms?, subnets?}: consoles on the network, by mDNS and by sweeping
/// each /24 for the helper's port. The body is optional.
async fn discover_handler(body: Bytes) -> Response {
    let req: DiscoverRequest = if body.iter().all(u8::is_ascii_whitespace) {
        DiscoverRequest::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => return bad_request(format!("bad request: {e}")),
        }
    };
    let mut extra_subnets = Vec::with_capacity(req.subnets.len());
    for s in &req.subnets {
        match discover::parse_subnet(s) {
            Ok(p) => extra_subnets.push(p),
            Err(e) => return bad_request(e),
        }
    }
    let opts = DiscoverOptions {
        timeout: req.timeout_ms.map(Duration::from_millis),
        extra_subnets,
    };
    console_call(move || {
        let result = discover::discover(&opts);
        let mut v = serde_json::to_value(&result).map_err(|e| e.to_string())?;
        if crate::mesh_api::kind() == "docker" {
            v["note"] = json!(DOCKER_NOTE);
        }
        Ok::<_, String>(v)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ps5upload_core::mgmt::{self, Method, MgmtTransport};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};

    /// A console that answers each method with a canned body and records what it was asked.
    struct Console {
        seen: Mutex<Vec<(u16, Value)>>,
        reply: Vec<u8>,
    }

    impl MgmtTransport for Console {
        fn call(
            &self,
            _addr: &str,
            method: Method,
            _label: &str,
            body: &[u8],
            _timeout: Duration,
        ) -> anyhow::Result<Option<Vec<u8>>> {
            let v = serde_json::from_slice(body).unwrap_or(Value::Null);
            self.seen.lock().unwrap().push((method.id, v));
            Ok(Some(self.reply.clone()))
        }
    }

    static TEST_CONSOLE: Mutex<Option<Arc<Console>>> = Mutex::new(None);

    /// Routes the blocking call's thread to the test's console, while one is set.
    pub(super) fn scoped_console() -> Option<mgmt::ScopedTransport> {
        let c = TEST_CONSOLE.lock().unwrap().clone()?;
        Some(mgmt::scoped_transport(c))
    }

    async fn body_json(r: Response) -> Value {
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        serde_json::from_slice(&b).unwrap()
    }

    #[tokio::test]
    async fn every_console_route_wants_a_console() {
        let r = shell_run(Json(ShellRunRequest {
            addr: " ".into(),
            cmd: "ls".into(),
            cwd: None,
            session_id: None,
            timeout_s: None,
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = appdb(Query(AddrOnly { addr: "".into() })).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = image_packed(Json(ImagePackedRequest {
            addr: "".into(),
            path: "/data/homebrew/G.exfat".into(),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = image_packed(Json(ImagePackedRequest {
            addr: "192.0.2.9".into(),
            path: "homebrew/G.exfat".into(),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "a relative path");
        let r = process_modules(Path(1), Query(AddrOnly { addr: "".into() })).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = companions(Query(AddrOnly { addr: "".into() })).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_empty_command_and_a_bad_algo_are_the_callers_mistake() {
        let r = shell_run(Json(ShellRunRequest {
            addr: "10.0.0.2".into(),
            cmd: "  ".into(),
            cwd: None,
            session_id: None,
            timeout_s: None,
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = fs_hash(Json(FsHashRequest {
            addr: "10.0.0.2".into(),
            path: "/data/x".into(),
            algo: Some("md5".into()),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let r = fs_hash(Json(FsHashRequest {
            addr: "10.0.0.2".into(),
            path: "relative".into(),
            algo: None,
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_bad_title_id_never_reaches_the_console() {
        let r = appmeta_heal(Json(HealRequest {
            addr: "10.0.0.2".into(),
            title_id: "../downloads".into(),
            source_path: "/data/game".into(),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn requests_accept_the_other_names_for_the_console() {
        for key in ["addr", "console", "host"] {
            let v: AddrOnly = serde_json::from_value(json!({ key: "10.0.0.2" })).unwrap();
            assert_eq!(v.addr, "10.0.0.2");
        }
        let v: HealRequest = serde_json::from_value(
            json!({"console": "c", "title_id": "CUSA00001", "src": "/data/g"}),
        )
        .unwrap();
        assert_eq!(v.source_path, "/data/g");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_routes_reach_the_named_console_and_pass_its_reply_through() {
        // Set once here: the only test that gets past validation to the console.
        let c = Arc::new(Console {
            seen: Default::default(),
            reply: br#"{"exit_code":0,"timed_out":false,"stdout":"hi\n","modules":[],"apps":[]}"#
                .to_vec(),
        });
        *TEST_CONSOLE.lock().unwrap() = Some(c.clone());

        let r = shell_run(Json(ShellRunRequest {
            addr: "10.0.0.2:9120".into(),
            cmd: "echo hi".into(),
            cwd: Some("/data".into()),
            session_id: None,
            timeout_s: Some(5),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let v = body_json(r).await;
        assert_eq!(v["stdout"], "hi\n");
        assert_eq!(v["exit_code"], 0);
        {
            let seen = c.seen.lock().unwrap();
            let (_, body) = seen.last().unwrap();
            assert_eq!(body["cmd"], "echo hi");
            assert_eq!(body["cwd"], "/data");
        }

        let r = process_modules(
            Path(77),
            Query(AddrOnly {
                addr: "10.0.0.2".into(),
            }),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(c.seen.lock().unwrap().last().unwrap().1["pid"], 77);

        let r = appdb(Query(AddrOnly {
            addr: "10.0.0.2".into(),
        }))
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(body_json(r).await["apps"], json!([]));

        *TEST_CONSOLE.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn a_public_network_is_refused_before_any_sweep() {
        let body = Bytes::from(json!({"subnets": ["8.8.8.0/24"]}).to_string());
        let r = discover_handler(body).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let v = body_json(r).await;
        assert!(v["error"].as_str().unwrap().contains("private"), "{v}");
        let r = discover_handler(Bytes::from_static(b"{not json")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
