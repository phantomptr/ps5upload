//! End-to-end tests of the MCP endpoint, through its real router with `oneshot`. Hermetic: the API
//! behind it is a small fake (plus a few real handlers that need no console), so nothing here
//! touches a console, the network or a sound device.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{ConnectInfo, Path, Query};
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt as _;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Notify};
use tower::ServiceExt as _;

use super::access::AgentAccess;
use super::jsonrpc::{
    HEADER_MISMATCH, INVALID_PARAMS, INVALID_REQUEST, META_CLIENT_CAPABILITIES,
    META_PROTOCOL_VERSION, META_SERVER_INFO, METHOD_NOT_FOUND, MODERN_VERSION, PARSE_ERROR,
    UNSUPPORTED_VERSION,
};
use super::{router, McpState};

const JOB: &str = "00000000-0000-4000-8000-000000000001";
const RUNNING_THEN_DONE: &str = "00000000-0000-4000-8000-000000000002";

/// Says back what a route was asked and who by.
async fn echo(
    method: Method,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: String,
) -> Json<Value> {
    Json(json!({
        "method": method.as_str(),
        "peer": peer.to_string(),
        "host": headers.get(header::HOST).and_then(|v| v.to_str().ok()),
        "query": query,
        "body": serde_json::from_str::<Value>(&body).ok(),
    }))
}

static POLLS: AtomicUsize = AtomicUsize::new(0);

async fn job(Path(id): Path<String>) -> Response {
    match id.as_str() {
        JOB => Json(json!({"status": "failed", "error": "disk full"})).into_response(),
        RUNNING_THEN_DONE => {
            if POLLS.fetch_add(1, Ordering::SeqCst) < 2 {
                Json(json!({"status": "running", "bytes_sent": 5, "total_bytes": 10}))
                    .into_response()
            } else {
                Json(json!({"status": "done", "bytes_sent": 10})).into_response()
            }
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "job not found"})),
        )
            .into_response(),
    }
}

/// The API, as far as these tests need it.
fn fake_api() -> Router {
    Router::new()
        .route(
            "/api/version",
            get(|| async { Json(json!({"version": "1.2.3", "caps": {"rar": true}})) }),
        )
        .route(
            "/api/jobs",
            get(|| async {
                Json(json!([
                    {"job_id": JOB, "status": "running", "job": {"status": "running"}},
                    {"job_id": RUNNING_THEN_DONE, "status": "done", "job": {"status": "done"}},
                ]))
            }),
        )
        .route("/api/jobs/{id}", get(job))
        .route("/api/echo", get(echo).post(echo).delete(echo).put(echo))
        .route(
            "/api/ps5/fs/mkdir",
            post(|Json(body): Json<Value>| async move { Json(json!({"ok": true, "got": body})) }),
        )
        .route(
            "/api/ps5/fs/stat",
            post(|Json(body): Json<Value>| async move {
                Json(json!({"kind": "dir", "size": 4096, "got": body}))
            }),
        )
        .route(
            "/api/ps5/fs/read-preview",
            post(|Json(body): Json<Value>| async move {
                let base64 = if body["path"] == "/bin" {
                    "/4AB" // Bytes that are not text.
                } else {
                    "aGVsbG8=" // "hello"
                };
                Json(json!({"size": 5, "base64": base64}))
            }),
        )
        .route(
            "/api/ps5/status",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                match q.get("addr").map(String::as_str) {
                    Some("9.9.9.9") => (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({
                            "error": "NODE_STATUS: asleep",
                            "reason": "console_rest_mode",
                            "power": "rest",
                        })),
                    )
                        .into_response(),
                    addr => Json(json!({
                        "version": "8.0.0",
                        "ps5_kernel": "FreeBSD",
                        "asked": addr,
                    }))
                    .into_response(),
                }
            }),
        )
        .route(
            "/api/ps5/power/pair",
            post(|| async {
                Json(json!({
                    "credential": "SECRET-CREDENTIAL",
                    "regist_key": "SECRET-REGIST",
                    "rp_key": "SECRET-RP",
                    "account_id": "42",
                }))
            }),
        )
        .route(
            "/api/shared/consoles",
            get(|| async {
                Json(json!({"rev": 1, "value": {"profiles": [{
                    "id": "1",
                    "name": "Living room",
                    "host": "192.168.1.100",
                    "wake_credential": "SECRET-CREDENTIAL",
                    "wake_regist_key": "SECRET-REGIST",
                    "wake_rp_key": "SECRET-RP",
                }]}}))
            }),
        )
        .route(
            "/api/ps5/power/wake",
            post(|Json(body): Json<Value>| async move {
                Json(json!({"ok": true, "sent": true, "credential_was": body["credential"]}))
            }),
        )
        .route(
            "/api/stream",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(futures_util::stream::pending::<
                        Result<axum::body::Bytes, std::io::Error>,
                    >()),
                )
            }),
        )
        .route(
            "/api/picture",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/png")],
                    vec![0x89u8, 0x50, 0xff],
                )
            }),
        )
        .route(
            "/api/plain-failure",
            get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "it broke") }),
        )
        .route(
            "/api/says-no",
            post(|| async { Json(json!({"ok": false, "error": "refused"})) }),
        )
        // Real handlers: they need no console.
        .route("/api/ps5/rp/session", get(crate::rp_stream_api::current))
        .route(
            "/api/ps5/rp/frame",
            get(crate::rp_stream_api::automation::frame),
        )
}

struct Server {
    state: McpState,
    router: Router,
    events: broadcast::Sender<String>,
}

fn server_with(api: Router, read_only: bool) -> Server {
    server_with_access(api, AgentAccess::in_memory(false, read_only))
}

fn server_with_access(api: Router, access: AgentAccess) -> Server {
    let (events, _) = broadcast::channel(64);
    let state = McpState::build(api, events.clone(), "10.0.0.1".to_string(), access);
    Server {
        router: router(state.clone()),
        state,
        events,
    }
}

fn server() -> Server {
    server_with(fake_api(), false)
}

/// The metadata a modern request carries.
fn modern_params(extra: Value) -> Value {
    let mut params = extra;
    params["_meta"] = json!({
        META_PROTOCOL_VERSION: MODERN_VERSION,
        META_CLIENT_CAPABILITIES: {},
    });
    params
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
    raw: Vec<u8>,
}

impl Server {
    /// Sends a request as a client would: a raw POST with these headers.
    async fn post_raw(&self, headers: &[(&str, &str)], body: Vec<u8>) -> Answer {
        let mut request = Request::builder().method(Method::POST).uri("/mcp");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let raw = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        Answer {
            status,
            headers,
            body: serde_json::from_slice(&raw).unwrap_or(Value::Null),
            raw,
        }
    }

    async fn post(&self, headers: &[(&str, &str)], body: Value) -> Answer {
        self.post_raw(headers, body.to_string().into_bytes()).await
    }

    /// A well-formed modern request.
    async fn modern(&self, method: &str, extra: Value) -> Answer {
        let params = modern_params(extra);
        let name = params
            .get("name")
            .or_else(|| params.get("uri"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut headers = vec![
            ("MCP-Protocol-Version", MODERN_VERSION),
            ("Mcp-Method", method),
        ];
        if let Some(name) = &name {
            headers.push(("Mcp-Name", name.as_str()));
        }
        self.post(
            &headers,
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
        )
        .await
    }

    /// A legacy request: no modern metadata, no version header.
    async fn legacy(&self, method: &str, params: Value) -> Answer {
        self.post(
            &[],
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
        )
        .await
    }

    /// A tool call's result, on the legacy path (the envelope is tested elsewhere).
    async fn tool(&self, name: &str, arguments: Value) -> Value {
        let answer = self
            .legacy("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        assert_eq!(answer.status, StatusCode::OK);
        assert!(
            answer.body.get("error").is_none(),
            "{name} gave a JSON-RPC error: {}",
            answer.body
        );
        answer.body["result"].clone()
    }

    async fn read_resource(&self, uri: &str) -> Value {
        let answer = self.legacy("resources/read", json!({"uri": uri})).await;
        assert_eq!(answer.status, StatusCode::OK, "{uri}");
        answer.body["result"]["contents"][0].clone()
    }
}

fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

// ── The modern era ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn modern_discover_says_what_this_server_is() {
    let answer = server().modern("server/discover", json!({})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body["id"], 1);
    let result = &answer.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"][0], MODERN_VERSION);
    assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
    assert_eq!(result["capabilities"]["resources"]["subscribe"], false);
    assert_eq!(result["capabilities"]["prompts"]["listChanged"], false);
    assert_eq!(result["_meta"][META_SERVER_INFO]["name"], "ps5upload");
    assert_eq!(
        result["_meta"][META_SERVER_INFO]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(result["instructions"].as_str().unwrap().contains("console"));
    // #772: 2026-07-28 caching hints, or a modern client refuses the handshake.
    assert_eq!(result["ttlMs"], 0);
    assert_eq!(result["cacheScope"], "private");
    // A modern answer carries no session.
    assert!(answer.headers.get("mcp-session-id").is_none());
}

#[tokio::test]
async fn modern_ping_is_complete() {
    let answer = server().modern("ping", json!({})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body["result"]["resultType"], "complete");
    // Not one of the cacheable results.
    assert!(answer.body["result"].get("ttlMs").is_none());
}

#[tokio::test]
async fn modern_tools_list_describes_every_tool() {
    let answer = server().modern("tools/list", json!({})).await;
    assert_eq!(answer.status, StatusCode::OK);
    let result = &answer.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["ttlMs"], 0);
    assert_eq!(result["cacheScope"], "private");
    let tools = result["tools"].as_array().unwrap();
    assert!(tools.len() >= 30);
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(tool["description"].is_string(), "{name}");
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(tool["annotations"][hint].is_boolean(), "{name}: {hint}");
        }
    }
    // The same order every time: clients and caches rely on it.
    let again = server().modern("tools/list", json!({})).await;
    assert_eq!(again.body["result"]["tools"], result["tools"]);
}

#[tokio::test]
async fn modern_tools_call_answers_with_data_and_text() {
    let answer = server()
        .modern(
            "tools/call",
            json!({"name": "engine_version", "arguments": {}}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    let result = &answer.body["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["structuredContent"]["version"], "1.2.3");
    assert!(result.get("isError").is_none());
    let blocks = result["content"].as_array().unwrap();
    assert!(blocks[0]["text"].as_str().unwrap().contains("1.2.3"));
    // The data again as text, for clients that read only text.
    let as_text: Value = serde_json::from_str(blocks[1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(as_text, result["structuredContent"]);
}

#[tokio::test]
async fn modern_unknown_method_is_404() {
    let answer = server().modern("nope/nope", json!({})).await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.body["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(answer.body["id"], 1);
}

#[tokio::test]
async fn the_legacy_handshake_is_not_a_modern_method() {
    let answer = server()
        .modern(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::NOT_FOUND);
    assert_eq!(answer.body["error"]["code"], METHOD_NOT_FOUND);
}

#[tokio::test]
async fn modern_unknown_tool_is_invalid_params() {
    let answer = server()
        .modern("tools/call", json!({"name": "nope", "arguments": {}}))
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);
    assert_eq!(answer.body["error"]["message"], "Unknown tool: nope");
}

// ── Header validation ───────────────────────────────────────────────────────────────────────────

fn tool_call_body() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": modern_params(json!({"name": "engine_version", "arguments": {}})),
    })
}

#[tokio::test]
async fn a_header_that_disagrees_with_the_body_is_refused() {
    let server = server();
    for headers in [
        // Wrong version.
        [
            ("MCP-Protocol-Version", "2025-06-18"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "engine_version"),
        ],
        // Wrong method.
        [
            ("MCP-Protocol-Version", MODERN_VERSION),
            ("Mcp-Method", "tools/list"),
            ("Mcp-Name", "engine_version"),
        ],
        // Wrong name.
        [
            ("MCP-Protocol-Version", MODERN_VERSION),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "files_list"),
        ],
    ] {
        let answer = server.post(&headers, tool_call_body()).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{headers:?}");
        assert_eq!(answer.body["error"]["code"], HEADER_MISMATCH, "{headers:?}");
        assert_eq!(answer.body["id"], 1);
    }
}

#[tokio::test]
async fn a_missing_header_is_refused() {
    let server = server();
    for headers in [
        vec![("Mcp-Method", "tools/call"), ("Mcp-Name", "engine_version")],
        vec![
            ("MCP-Protocol-Version", MODERN_VERSION),
            ("Mcp-Name", "engine_version"),
        ],
        vec![
            ("MCP-Protocol-Version", MODERN_VERSION),
            ("Mcp-Method", "tools/call"),
        ],
    ] {
        let answer = server.post(&headers, tool_call_body()).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{headers:?}");
        assert_eq!(answer.body["error"]["code"], HEADER_MISMATCH, "{headers:?}");
        let message = answer.body["error"]["message"].as_str().unwrap();
        assert!(message.contains("missing"), "{message}");
    }
}

#[tokio::test]
async fn mcp_name_may_be_base64_between_sentinels() {
    let server = server();
    // "engine_version" in base64.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", MODERN_VERSION),
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "=?base64?ZW5naW5lX3ZlcnNpb24=?="),
            ],
            tool_call_body(),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert_eq!(
        answer.body["result"]["structuredContent"]["version"],
        "1.2.3"
    );

    // Decodes, to something else.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", MODERN_VERSION),
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "=?base64?ZmlsZXNfbGlzdA==?="),
            ],
            tool_call_body(),
        )
        .await;
    assert_eq!(answer.body["error"]["code"], HEADER_MISMATCH);

    // Does not decode.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", MODERN_VERSION),
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", "=?base64?***?="),
            ],
            tool_call_body(),
        )
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], HEADER_MISMATCH);
}

#[tokio::test]
async fn a_name_a_header_cannot_carry_works_through_the_sentinels() {
    // "ゲーム": an unknown tool, but it gets past the headers to say so.
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": modern_params(json!({"name": "ゲーム"})),
    });
    let name = super::jsonrpc::encode_header_name("ゲーム");
    let answer = server()
        .post(
            &[
                ("MCP-Protocol-Version", MODERN_VERSION),
                ("Mcp-Method", "tools/call"),
                ("Mcp-Name", name.as_str()),
            ],
            body,
        )
        .await;
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);
    assert_eq!(answer.body["error"]["message"], "Unknown tool: ゲーム");
}

#[tokio::test]
async fn an_unsupported_version_says_what_is_supported() {
    let answer = server()
        .post(
            &[
                ("MCP-Protocol-Version", "1999-01-01"),
                ("Mcp-Method", "tools/list"),
            ],
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {"_meta": {
                    META_PROTOCOL_VERSION: "1999-01-01",
                    META_CLIENT_CAPABILITIES: {},
                }},
            }),
        )
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], UNSUPPORTED_VERSION);
    assert_eq!(answer.body["error"]["data"]["requested"], "1999-01-01");
    assert_eq!(answer.body["error"]["data"]["supported"][0], MODERN_VERSION);
}

#[tokio::test]
async fn an_unsupported_version_in_the_header_alone_is_refused_too() {
    let answer = server()
        .post(
            &[("MCP-Protocol-Version", "1999-01-01")],
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], UNSUPPORTED_VERSION);
}

#[tokio::test]
async fn a_modern_request_without_its_metadata_is_invalid_params() {
    let server = server();
    // The header says modern, the body has no `_meta`.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", MODERN_VERSION),
                ("Mcp-Method", "tools/list"),
            ],
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);

    // A modern-only method with none at all.
    for method in ["server/discover", "subscriptions/listen"] {
        let answer = server.legacy(method, json!({})).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{method}");
        assert_eq!(answer.body["error"]["code"], INVALID_PARAMS, "{method}");
    }

    // The version, but no capabilities.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", MODERN_VERSION),
                ("Mcp-Method", "tools/list"),
            ],
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {"_meta": {META_PROTOCOL_VERSION: MODERN_VERSION}},
            }),
        )
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);

    // A version that is not a string.
    let answer = server
        .post(
            &[],
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {"_meta": {META_PROTOCOL_VERSION: 5}},
            }),
        )
        .await;
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);
}

// ── The legacy era ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_legacy_flow_works_start_to_finish() {
    let server = server();

    let answer = server
        .legacy(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "1"},
            }),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    let result = &answer.body["result"];
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "ps5upload");
    assert_eq!(result["serverInfo"]["title"], "PS5Upload");
    assert_eq!(result["capabilities"]["resources"]["subscribe"], false);
    assert!(result["instructions"].is_string());
    assert!(result.get("resultType").is_none());
    assert!(result.get("_meta").is_none());
    assert!(result.get("ttlMs").is_none());
    // A session id is minted, a real one.
    let session = answer.headers["mcp-session-id"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(uuid::Uuid::parse_str(&session).is_ok(), "{session}");

    // Notified: nothing to say back.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", "2025-06-18"),
                ("Mcp-Session-Id", session.as_str()),
            ],
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::ACCEPTED);
    assert!(answer.raw.is_empty());

    // With the session and the version header.
    let answer = server
        .post(
            &[
                ("MCP-Protocol-Version", "2025-06-18"),
                ("Mcp-Session-Id", session.as_str()),
            ],
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(answer.body["result"]["tools"].is_array());
    assert!(answer.body["result"].get("resultType").is_none());
    assert!(answer.body["result"].get("_meta").is_none());

    // With neither: the session is never required, and no version means legacy.
    let answer = server.legacy("tools/list", json!({})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(answer.body["result"]["tools"].is_array());
    assert!(answer.body["result"].get("resultType").is_none());

    let answer = server.legacy("ping", json!({})).await;
    assert_eq!(answer.body["result"], json!({}));
}

#[tokio::test]
async fn initialize_answers_a_version_it_speaks() {
    let server = server();
    for (asked, answered) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("1999-01-01", "2025-11-25"),
        // Not an initialize-based revision.
        (MODERN_VERSION, "2025-11-25"),
    ] {
        let answer = server
            .legacy("initialize", json!({"protocolVersion": asked}))
            .await;
        assert_eq!(
            answer.body["result"]["protocolVersion"], answered,
            "{asked}"
        );
    }
    // A modern client trying the legacy handshake with its version in the header.
    let answer = server
        .post(
            &[("MCP-Protocol-Version", MODERN_VERSION)],
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body["result"]["protocolVersion"], "2025-11-25");
}

#[tokio::test]
async fn legacy_errors_answer_with_a_200_and_keep_their_codes() {
    let answer = server().legacy("nope/nope", json!({})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(answer.body["id"], 1);

    let answer = server().legacy("tools/call", json!({"name": "nope"})).await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);
    assert_eq!(answer.body["error"]["message"], "Unknown tool: nope");
}

// ── HTTP shape ──────────────────────────────────────────────────────────────────────────────────

async fn plain(server: &Server, method: Method) -> Response {
    server
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri("/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn get_is_405_and_delete_is_204() {
    let server = server();
    let response = plain(&server, Method::GET).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[header::ALLOW], "POST, DELETE");
    let response = plain(&server, Method::DELETE).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn a_notification_or_a_clients_answer_is_202_with_nothing() {
    let server = server();
    for message in [
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}),
        json!({"jsonrpc": "2.0", "method": "anything/at/all"}),
        json!({"jsonrpc": "2.0", "id": 9, "result": {}}),
        json!({"jsonrpc": "2.0", "id": 9, "error": {"code": 1, "message": "x"}}),
    ] {
        let answer = server.post(&[], message.clone()).await;
        assert_eq!(answer.status, StatusCode::ACCEPTED, "{message}");
        assert!(answer.raw.is_empty(), "{message}");
    }
}

#[tokio::test]
async fn what_is_not_a_request_is_refused_plainly() {
    let server = server();
    let answer = server.post_raw(&[], b"{not json".to_vec()).await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], PARSE_ERROR);
    assert!(answer.body.get("id").is_none());

    let answer = server.post_raw(&[], Vec::new()).await;
    assert_eq!(answer.body["error"]["code"], PARSE_ERROR);

    for message in [
        json!({"id": 1, "method": "ping"}),
        json!({"jsonrpc": "1.0", "id": 1, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": {}, "method": "ping"}),
        json!({"jsonrpc": "2.0", "id": 1, "method": 5}),
        json!({"jsonrpc": "2.0", "id": 1}),
        json!("ping"),
        json!(5),
    ] {
        let answer = server.post(&[], message.clone()).await;
        assert_eq!(answer.status, StatusCode::BAD_REQUEST, "{message}");
        assert_eq!(answer.body["error"]["code"], INVALID_REQUEST, "{message}");
    }
}

#[tokio::test]
async fn a_batch_answers_its_requests_in_order() {
    let server = server();
    let answer = server
        .post(
            &[],
            json!([
                {"jsonrpc": "2.0", "id": 1, "method": "ping"},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": 2, "method": "nope"},
                {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "engine_version"}},
                {"id": 4, "method": "ping"},
            ]),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    let replies = answer.body.as_array().unwrap();
    assert_eq!(replies.len(), 4);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["error"]["code"], METHOD_NOT_FOUND);
    assert_eq!(
        replies[2]["result"]["structuredContent"]["version"],
        "1.2.3"
    );
    assert_eq!(replies[3]["error"]["code"], INVALID_REQUEST);

    // Only notifications: nothing to answer.
    let answer = server
        .post(
            &[],
            json!([{"jsonrpc": "2.0", "method": "notifications/initialized"}]),
        )
        .await;
    assert_eq!(answer.status, StatusCode::ACCEPTED);

    let answer = server.post(&[], json!([])).await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], INVALID_REQUEST);

    // The modern protocol has none.
    let answer = server
        .post(
            &[],
            json!([{"jsonrpc": "2.0", "id": 1, "method": "ping", "params": modern_params(json!({}))}]),
        )
        .await;
    assert_eq!(answer.body[0]["error"]["code"], INVALID_REQUEST);
}

// ── Origin ──────────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_foreign_page_is_refused_and_a_native_client_is_not() {
    let server = server();
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});

    let answer = server
        .post(
            &[
                ("host", "127.0.0.1:19113"),
                ("origin", "https://evil.example"),
            ],
            ping.clone(),
        )
        .await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);
    let message = answer.body["error"]["message"].as_str().unwrap();
    assert!(message.contains("evil.example"), "{message}");

    // DNS rebinding: same origin to the browser, with this computer's address behind its name.
    let answer = server
        .post(
            &[
                ("host", "evil.example:19113"),
                ("origin", "http://evil.example:19113"),
            ],
            ping.clone(),
        )
        .await;
    assert_eq!(answer.status, StatusCode::FORBIDDEN);

    let answer = server
        .post(
            &[
                ("host", "127.0.0.1:19113"),
                ("origin", "http://localhost:6274"),
            ],
            ping.clone(),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);

    // A native client behind a reverse proxy: no Origin, any name.
    let answer = server.post(&[("host", "nas.local")], ping).await;
    assert_eq!(answer.status, StatusCode::OK);
}

#[tokio::test]
async fn the_guard_covers_get_and_delete_too() {
    let response = server()
        .router
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/mcp")
                .header("host", "127.0.0.1:19113")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// ── Calling the API ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_call_with_no_connection_behind_it_presents_as_a_local_script() {
    let result = server()
        .tool("api_request", json!({"path": "/api/echo"}))
        .await;
    let body = &result["structuredContent"]["body"];
    assert_eq!(body["peer"], "127.0.0.1:1");
    assert_eq!(body["host"], "127.0.0.1");
}

/// A request with what a real connection would have.
async fn tool_from(server: &Server, peer: &str, host: &str, name: &str, arguments: Value) -> Value {
    let peer: SocketAddr = peer.parse().unwrap();
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("host", host)
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            })
            .to_string(),
        ))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = server.router.clone().oneshot(request).await.unwrap();
    let raw = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let answer: Value = serde_json::from_slice(&raw).unwrap();
    answer["result"].clone()
}

#[tokio::test]
async fn a_call_carries_its_callers_peer_and_name() {
    let result = tool_from(
        &server(),
        "192.168.1.50:5555",
        "nas.local:19113",
        "api_request",
        json!({"path": "/api/echo"}),
    )
    .await;
    let body = &result["structuredContent"]["body"];
    assert_eq!(body["peer"], "192.168.1.50:5555");
    assert_eq!(body["host"], "nas.local:19113");
}

#[tokio::test]
async fn the_routes_for_this_computer_alone_accept_a_local_caller_and_no_one_else() {
    let server = server();
    let frame = || json!({"console": "192.168.1.100", "session": 3});

    // Local, with no connection behind it: past the guards, to "nothing streams".
    let result = server.tool("rp_frame", frame()).await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"], "no_session");
    assert!(text_of(&result).contains("Play › Start"));

    // Local, as a real client on this computer.
    let result = tool_from(
        &server,
        "127.0.0.1:50000",
        "localhost:19113",
        "rp_frame",
        frame(),
    )
    .await;
    assert_eq!(result["structuredContent"]["error"], "no_session");

    // A peer PS5UPLOAD_ALLOW_IP let in does not get through a tool what it cannot get directly,
    // however it names this computer.
    let result = tool_from(
        &server,
        "192.168.1.50:5555",
        "192.168.1.5:19113",
        "rp_frame",
        frame(),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"], "host_not_local");
    let result = tool_from(
        &server,
        "192.168.1.50:5555",
        "127.0.0.1:19113",
        "rp_frame",
        frame(),
    )
    .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"], "local_only");

    // Neither does a page or proxy that reaches this computer under another name.
    let result = tool_from(
        &server,
        "127.0.0.1:50000",
        "evil.example:19113",
        "rp_frame",
        frame(),
    )
    .await;
    assert_eq!(result["structuredContent"]["error"], "host_not_local");
}

#[tokio::test]
async fn nothing_streaming_is_reported_not_guessed() {
    // Other tests put a session in place while they hold this lock: "nothing streaming" is only
    // true outside them.
    let _live = crate::rp_stream_api::LIVE_TEST_LOCK.lock().await;
    let result = server().tool("rp_session", json!({})).await;
    assert_eq!(result["structuredContent"]["host"], Value::Null);
    assert!(text_of(&result).contains("Nothing is streaming"));

    let result = server().tool("rp_frame", json!({})).await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("Play › Start"));

    let result = server().tool("rp_input_release", json!({})).await;
    assert_eq!(result["isError"], true);
}

// ── api_request ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn api_request_passes_method_query_and_body() {
    let result = server()
        .tool(
            "api_request",
            json!({
                "method": "POST",
                "path": "/api/echo",
                "query": {"addr": "1.2.3.4", "n": 5, "yes": true, "odd": "a b&c"},
                "body": {"x": [1, 2]},
            }),
        )
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["status"], 200);
    let body = &structured["body"];
    assert_eq!(body["method"], "POST");
    assert_eq!(body["query"]["addr"], "1.2.3.4");
    assert_eq!(body["query"]["n"], "5");
    assert_eq!(body["query"]["yes"], "true");
    assert_eq!(body["query"]["odd"], "a b&c");
    assert_eq!(body["body"]["x"][1], 2);
}

#[tokio::test]
async fn the_console_keys_stay_out_of_api_request_and_the_catalog() {
    let server = server();
    for (method, path) in [
        ("GET", "/api/consoles/document"),
        ("PUT", "/api/consoles/document"),
        ("GET", "/api/consoles/%64ocument"),
        ("GET", "/api/consoles/document/"),
        ("GET", "/api/settings-backup/get"),
    ] {
        let result = server
            .tool("api_request", json!({"method": method, "path": path}))
            .await;
        assert_eq!(result["isError"], true, "{method} {path}: {result}");
        assert!(result.to_string().contains("is refused"), "{result}");
    }
    let found = server
        .tool("api_catalog", json!({"search": "consoles"}))
        .await;
    let text = found.to_string();
    assert!(text.contains("/api/consoles"), "{text}");
    assert!(!text.contains("/api/consoles/document"), "{text}");
    let everything = server.tool("api_catalog", json!({})).await.to_string();
    assert!(!everything.contains("consoles/document"));
    assert!(!everything.contains("settings-backup/get"));
}

#[tokio::test]
async fn pairing_works_and_its_keys_stay_out_of_the_result() {
    let server = server();
    let result = server
        .tool(
            "api_request",
            json!({"method": "POST", "path": "/api/ps5/power/pair", "body": {"host": "192.0.2.1"}}),
        )
        .await;
    assert_ne!(result["isError"], true, "{result}");
    let text = result.to_string();
    assert!(!text.contains("SECRET"), "{text}");
    let body = &result["structuredContent"]["body"];
    assert_eq!(body["account_id"], "42");
}

#[tokio::test]
async fn a_console_backup_slot_stays_out_of_every_tool() {
    let server = server();
    for query in [
        json!({"addr": "192.0.2.1", "name": "backup-01"}),
        json!({"addr": "192.0.2.1", "name": "BACKUP-01"}),
        json!({"addr": "192.0.2.1", "name": "%62ackup-01"}),
    ] {
        for path in ["/api/mesh/doc", "/api/mesh/%64oc", "/api//mesh/doc/"] {
            let result = server
                .tool("api_request", json!({"path": path, "query": query}))
                .await;
            assert_eq!(result["isError"], true, "{path} {query}");
            assert!(result.to_string().contains("is refused"), "{result}");
        }
    }
    let read = server
        .tool(
            "api_request",
            json!({
                "method": "POST",
                "path": "/api/ps5/fs/read-preview",
                "body": {"addr": "192.0.2.1", "path": "/data/ps5upload/mesh/v1/doc-backup-01"},
            }),
        )
        .await;
    assert!(read.to_string().contains("is refused"), "{read}");
    for name in ["state", "consoles", "drives"] {
        let keys = server
            .tool(
                "api_request",
                json!({"path": "/api/mesh/doc", "query": {"addr": "192.0.2.1", "name": name}}),
            )
            .await;
        assert!(keys.to_string().contains("is refused"), "{keys}");
    }
    for path in ["/api/mgmt/call", "/api/ava1/identity"] {
        let refused = server
            .tool(
                "api_request",
                json!({"path": path, "method": "POST", "body": {}}),
            )
            .await;
        assert!(refused.to_string().contains("is refused"), "{refused}");
    }
    // The index and the other documents are not refused (this test's router has no such route).
    for name in ["backups", "prefs"] {
        let other = server
            .tool(
                "api_request",
                json!({"path": "/api/mesh/doc", "query": {"addr": "192.0.2.1", "name": name}}),
            )
            .await;
        assert!(!other.to_string().contains("is refused"), "{other}");
    }
}

#[tokio::test]
async fn api_request_stays_inside_the_api() {
    let server = server();
    for path in [
        "/mcp",
        "/pkg-host/a/b",
        "/api/../mcp",
        "version",
        "/api/echo?x=1",
    ] {
        let result = server.tool("api_request", json!({"path": path})).await;
        assert_eq!(result["isError"], true, "{path}");
    }
    // Its required argument, and only its own arguments.
    assert_eq!(server.tool("api_request", json!({})).await["isError"], true);
    let result = server
        .tool(
            "api_request",
            json!({"path": "/api/echo", "method": "TRACE"}),
        )
        .await;
    assert_eq!(result["isError"], true);
}

#[tokio::test]
async fn a_routes_failure_is_a_tool_error_in_its_own_words() {
    let server = server();
    let result = server
        .tool(
            "api_request",
            json!({"path": "/api/ps5/status", "query": {"addr": "9.9.9.9"}}),
        )
        .await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(
        text.contains("HTTP 502") && text.contains("asleep"),
        "{text}"
    );
    assert!(text.contains("rest mode"), "{text}");
    assert_eq!(result["structuredContent"]["status"], 502);
    assert_eq!(result["structuredContent"]["body"]["power"], "rest");

    // Plain text.
    let result = server
        .tool("api_request", json!({"path": "/api/plain-failure"}))
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("it broke"));

    // No such route, with where to look.
    let result = server
        .tool("api_request", json!({"path": "/api/no-such-route"}))
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("ps5upload://catalog"));

    // A route that takes another method.
    let result = server
        .tool("api_request", json!({"path": "/api/says-no"}))
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["status"], 405);

    // A 200 that says it failed.
    let result = server
        .tool(
            "api_request",
            json!({"method": "POST", "path": "/api/says-no", "body": {}}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("refused"));
}

#[tokio::test]
async fn a_live_stream_is_refused_rather_than_waited_on_forever() {
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        server().tool("api_request", json!({"path": "/api/stream"})),
    )
    .await
    .expect("the tool must not wait for the stream to end");
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("live stream"));
}

#[tokio::test]
async fn bytes_come_back_as_base64_and_a_picture_as_a_picture() {
    let result = server()
        .tool("api_request", json!({"path": "/api/picture"}))
        .await;
    assert_eq!(result["content"][0]["type"], "image");
    assert_eq!(result["content"][0]["mimeType"], "image/png");
    assert_eq!(result["structuredContent"]["base64"], "iVD/");
    assert_eq!(result["structuredContent"]["size"], 3);
    assert_eq!(result["structuredContent"]["content_type"], "image/png");
}

// ── Read-only mode ──────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn read_only_refuses_what_can_change_something() {
    let server = server_with(fake_api(), true);
    let result = server
        .tool(
            "api_request",
            json!({"method": "POST", "path": "/api/echo", "body": {}}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("agents can only look"));

    for (name, arguments) in [
        (
            "files_mkdir",
            json!({"console": "1.2.3.4", "path": "/data/x"}),
        ),
        (
            "files_delete",
            json!({"console": "1.2.3.4", "path": "/data/x"}),
        ),
        (
            "power_control",
            json!({"console": "1.2.3.4", "action": "reboot"}),
        ),
        (
            "games_stop",
            json!({"console": "1.2.3.4", "title_id": "PPSA00001"}),
        ),
        ("jobs_cancel", json!({"job_id": JOB})),
    ] {
        let result = server.tool(name, arguments).await;
        assert_eq!(result["isError"], true, "{name}");
        assert!(text_of(&result).contains("agents can only look"), "{name}");
    }

    // And says so up front.
    let answer = server.modern("server/discover", json!({})).await;
    assert!(answer.body["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("read-only"));
}

#[tokio::test]
async fn read_only_keeps_every_read_even_those_that_are_post_routes() {
    let server = server_with(fake_api(), true);
    let result = server
        .tool("api_request", json!({"path": "/api/echo"}))
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    let result = server.tool("engine_version", json!({})).await;
    assert!(result.get("isError").is_none(), "{result}");
    // These two reach the engine by POST.
    let result = server.tool("files_stat", json!({"path": "/data"})).await;
    assert!(result.get("isError").is_none(), "{result}");
    let result = server.tool("files_read", json!({"path": "/x"})).await;
    assert!(result.get("isError").is_none(), "{result}");
}

// ── The owner's switch (access.rs) ──────────────────────────────────────────────────────────────

impl Server {
    /// `/api/mcp/access`, as the app calls it.
    async fn access(&self, method: Method, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri("/api/mcp/access");
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let raw = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&raw).unwrap_or(Value::Null))
    }

    async fn tool_names(&self) -> Vec<String> {
        let answer = self.legacy("tools/list", json!({})).await;
        answer.body["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }
}

#[tokio::test]
async fn the_owners_switch_changes_the_tool_list_and_the_calls_live() {
    let server = server();
    let (status, state) = server.access(Method::GET, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state,
        json!({"read_only": false, "setting": false, "locked": false})
    );
    let names = server.tool_names().await;
    for name in ["files_delete", "files_mkdir", "settings_set", "api_request"] {
        assert!(names.iter().any(|n| n == name), "{name}");
    }
    let mkdir = json!({"console": "1.2.3.4", "path": "/data/x"});
    let result = server.tool("files_mkdir", mkdir.clone()).await;
    assert!(result.get("isError").is_none(), "{result}");

    // Look only: no restart, the next list and call already know.
    let (status, state) = server
        .access(Method::PUT, Some(json!({"read_only": true})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state,
        json!({"read_only": true, "setting": true, "locked": false})
    );
    let names = server.tool_names().await;
    for gone in [
        "files_delete",
        "files_mkdir",
        "settings_set",
        "power_control",
    ] {
        assert!(!names.iter().any(|n| n == gone), "{gone}");
    }
    for kept in ["files_list", "files_stat", "engine_status", "api_request"] {
        assert!(names.iter().any(|n| n == kept), "{kept}");
    }
    // A client holding the old list is still refused, and told where to change it.
    let result = server.tool("files_mkdir", mkdir.clone()).await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(
        text.contains("this PS5Upload is set so agents can only look"),
        "{text}"
    );
    assert!(text.contains("MCP & API"), "{text}");
    assert!(!text.contains("PS5UPLOAD_MCP_READ_ONLY"), "{text}");
    let result = server
        .tool(
            "api_request",
            json!({"method": "POST", "path": "/api/echo", "body": {}}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("agents can only look"));
    // Reads go on.
    let result = server
        .tool("api_request", json!({"path": "/api/echo"}))
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    let result = server.tool("files_stat", json!({"path": "/data"})).await;
    assert!(result.get("isError").is_none(), "{result}");
    let result = server.tool("engine_status", json!({})).await;
    assert_eq!(result["structuredContent"]["read_only"], true);
    let answer = server.modern("server/discover", json!({})).await;
    assert!(answer.body["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("read-only"));

    // And back.
    let (_, state) = server
        .access(Method::PUT, Some(json!({"read_only": false})))
        .await;
    assert_eq!(state["read_only"], false);
    assert!(server.tool_names().await.iter().any(|n| n == "files_mkdir"));
    let result = server.tool("files_mkdir", mkdir).await;
    assert!(result.get("isError").is_none(), "{result}");

    // A body that is not the switch changes nothing.
    let (status, _) = server
        .access(Method::PUT, Some(json!({"read_only": "no"})))
        .await;
    assert!(status.is_client_error(), "{status}");
    assert_eq!(server.access(Method::GET, None).await.1["read_only"], false);
}

#[tokio::test]
async fn the_environment_wins_and_the_app_is_told_it_is_locked() {
    let server = server_with_access(fake_api(), AgentAccess::in_memory(true, false));
    let (_, state) = server.access(Method::GET, None).await;
    assert_eq!(
        state,
        json!({"read_only": true, "setting": false, "locked": true})
    );
    // The switch is kept, but agents still only look.
    let (status, state) = server
        .access(Method::PUT, Some(json!({"read_only": false})))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["read_only"], true);
    assert_eq!(state["locked"], true);
    assert!(!server
        .tool_names()
        .await
        .iter()
        .any(|n| n == "files_delete"));
    let result = server
        .tool(
            "files_delete",
            json!({"console": "1.2.3.4", "path": "/data/x"}),
        )
        .await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(text.contains("agents can only look"), "{text}");
    assert!(text.contains("PS5UPLOAD_MCP_READ_ONLY"), "{text}");
    let result = server.tool("engine_version", json!({})).await;
    assert!(result.get("isError").is_none(), "{result}");
}

#[tokio::test]
async fn an_agent_cannot_flip_the_switch_however_it_asks() {
    // Everything in the API behind the tools answers: only the guard can say no.
    let server = server_with(Router::new().fallback(echo_any), false);
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
        for path in [
            "/api/mcp/access",
            "/api/mcp/access/",
            "/api//mcp/access",
            "/api/mcp//access//",
            "/API/MCP/Access",
            "/Api/Mcp/ACCESS/",
            "/api/mcp/%61ccess",
            "/api/%6D%63%70/access",
            "/api/mcp%2Faccess",
            "/api/mcp/access/x",
            "/api/mcp/./access",
            "/api/x/../mcp/access",
            " /api/mcp/access ",
        ] {
            let result = server
                .tool(
                    "api_request",
                    json!({"method": method, "path": path, "body": {"read_only": false}}),
                )
                .await;
            assert_eq!(result["isError"], true, "{method} {path}");
            assert!(
                text_of(&result).contains("owner's to decide"),
                "{method} {path}: {}",
                text_of(&result)
            );
        }
    }
    for name in [
        "mcp_read_only",
        "MCP_READ_ONLY",
        "mcp-read-only",
        "mcpReadOnly",
    ] {
        let result = server
            .tool("settings_set", json!({"settings": {name: false}}))
            .await;
        assert_eq!(result["isError"], true, "{name}");
        assert!(text_of(&result).contains("owner's to decide"), "{name}");
    }
    // One refused name refuses the lot: nothing reached the route.
    let result = server
        .tool(
            "settings_set",
            json!({"settings": {"auto_resume": true, "mcp_read_only": false}}),
        )
        .await;
    assert_eq!(result["isError"], true);
    // Other settings still change.
    let asked = route_of(
        &server,
        "settings_set",
        json!({"settings": {"auto_resume": true}}),
    )
    .await;
    assert_eq!(asked["method"], "PATCH");
    assert_eq!(asked["path"], "/api/settings");
    assert_eq!(asked["body"]["settings"]["auto_resume"], true);
    // Even past the guard, the switch is not in the API the tools call: it is mounted beside it.
    let caller = super::Caller::local();
    let refused = server
        .state
        .api_call(&caller, Method::PUT, "/api/mcp/access", &[], None)
        .await
        .unwrap_err();
    assert!(refused.contains("owner's to decide"), "{refused}");
}

#[tokio::test]
async fn a_listening_client_hears_the_tool_list_change() {
    let server = server();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "subscriptions/listen",
        "params": modern_params(json!({"notifications": {"toolsListChanged": true}})),
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("MCP-Protocol-Version", MODERN_VERSION)
        .header("Mcp-Method", "subscriptions/listen")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = server.router.clone().oneshot(request).await.unwrap();
    let mut stream = response.into_body().into_data_stream();
    let acknowledged = next_message(&mut stream).await;
    assert_eq!(
        acknowledged["params"]["notifications"]["toolsListChanged"],
        true
    );

    server
        .access(Method::PUT, Some(json!({"read_only": true})))
        .await;
    let changed = next_message(&mut stream).await;
    assert_eq!(changed["method"], "notifications/tools/list_changed");
    assert_eq!(
        changed["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        8
    );
    server
        .access(Method::PUT, Some(json!({"read_only": false})))
        .await;
    let changed = next_message(&mut stream).await;
    assert_eq!(changed["method"], "notifications/tools/list_changed");

    server.state.begin_shutdown();
    let last = next_message(&mut stream).await;
    assert_eq!(last["id"], 8);
}

// ── Curated tools ───────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn console_is_passed_as_the_routes_addr() {
    let server = server();
    let result = server
        .tool("consoles_status", json!({"console": "1.2.3.4"}))
        .await;
    assert_eq!(result["structuredContent"]["asked"], "1.2.3.4");
    assert!(text_of(&result).contains("8.0.0"));

    // Left out on a read: the route falls back to its default.
    let result = server.tool("consoles_status", json!({})).await;
    assert_eq!(result["structuredContent"]["asked"], Value::Null);

    let result = server
        .tool(
            "files_mkdir",
            json!({"console": "1.2.3.4", "path": "/data/x"}),
        )
        .await;
    assert_eq!(result["structuredContent"]["got"]["addr"], "1.2.3.4");
    assert_eq!(result["structuredContent"]["got"]["path"], "/data/x");
}

#[tokio::test]
async fn a_tool_that_changes_something_will_not_guess_the_console() {
    let result = server()
        .tool("files_mkdir", json!({"path": "/data/x"}))
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("`console`"));
}

#[tokio::test]
async fn a_misspelt_argument_is_refused_not_sent_to_the_default_console() {
    let result = server()
        .tool("consoles_status", json!({"addr": "1.2.3.4"}))
        .await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(
        text.contains("`addr`") && text.contains("console"),
        "{text}"
    );
}

#[tokio::test]
async fn an_engine_error_becomes_a_tool_error_with_a_hint() {
    let result = server()
        .tool("consoles_status", json!({"console": "9.9.9.9"}))
        .await;
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["reason"], "console_rest_mode");
    assert!(result["structuredContent"]["hint"]
        .as_str()
        .unwrap()
        .contains("rest mode"));
    // Tool errors are not protocol errors: the answer is still a 200 result.
    let answer = server()
        .modern(
            "tools/call",
            json!({"name": "consoles_status", "arguments": {"console": "9.9.9.9"}}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK);
    assert_eq!(answer.body["result"]["isError"], true);
    assert_eq!(answer.body["result"]["resultType"], "complete");
}

#[tokio::test]
async fn a_list_answer_is_wrapped_so_clients_accept_it() {
    let result = server().tool("jobs_list", json!({})).await;
    let jobs = result["structuredContent"]["items"].as_array().unwrap();
    assert_eq!(jobs.len(), 2);
    let result = server().tool("jobs_list", json!({"status": "done"})).await;
    let jobs = result["structuredContent"]["items"].as_array().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["job_id"], RUNNING_THEN_DONE);
}

#[tokio::test(start_paused = true)]
async fn jobs_wait_follows_a_job_to_its_end() {
    POLLS.store(0, Ordering::SeqCst);
    let result = server()
        .tool(
            "jobs_wait",
            json!({"job_id": RUNNING_THEN_DONE, "timeout_s": 60}),
        )
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["finished"], true);
    assert_eq!(structured["job"]["status"], "done");
    assert_eq!(structured["waited_s"], 2);
    assert!(text_of(&result).contains("is done"));
}

#[tokio::test(start_paused = true)]
async fn jobs_wait_gives_up_when_told_and_says_it_still_runs() {
    async fn always_running() -> Json<Value> {
        Json(json!({"status": "running"}))
    }
    let api = Router::new().route("/api/jobs/{id}", get(always_running));
    let result = server_with(api, false)
        .tool("jobs_wait", json!({"job_id": "a", "timeout_s": 3}))
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(result["structuredContent"]["finished"], false);
    assert_eq!(result["structuredContent"]["waited_s"], 3);
    assert!(text_of(&result).contains("still running"));
}

#[tokio::test]
async fn jobs_wait_reports_a_failed_job_as_finished_with_its_reason() {
    let result = server().tool("jobs_wait", json!({"job_id": JOB})).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(result["structuredContent"]["finished"], true);
    assert!(text_of(&result).contains("disk full"));
}

#[tokio::test]
async fn jobs_wait_keeps_its_limit_and_reports_an_unknown_job() {
    let result = server()
        .tool("jobs_wait", json!({"job_id": JOB, "timeout_s": 601}))
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("at most 600"));

    let result = server().tool("jobs_wait", json!({"job_id": "nope"})).await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("job not found"));
}

#[tokio::test]
async fn files_read_gives_text_as_text_and_the_rest_as_base64() {
    let result = server()
        .tool("files_read", json!({"path": "/log.txt"}))
        .await;
    assert_eq!(result["structuredContent"]["encoding"], "utf-8");
    assert_eq!(result["structuredContent"]["text"], "hello");
    assert_eq!(result["structuredContent"]["size"], 5);

    let result = server().tool("files_read", json!({"path": "/bin"})).await;
    assert_eq!(result["structuredContent"]["encoding"], "base64");
    assert_eq!(result["structuredContent"]["base64"], "/4AB");
    assert_eq!(result["structuredContent"]["size"], 3);
}

/// A copy route that finishes when told to.
fn slow_copy_api(finish: Arc<Notify>) -> Router {
    Router::new().route(
        "/api/ps5/fs/copy",
        post(move |Json(body): Json<Value>| {
            let finish = finish.clone();
            async move {
                finish.notified().await;
                Json(json!({"ok": true, "got": body}))
            }
        }),
    )
}

#[tokio::test]
async fn a_quick_copy_answers_with_its_result() {
    let finish = Arc::new(Notify::new());
    finish.notify_one();
    let server = server_with(slow_copy_api(finish), false);
    let result = server
        .tool(
            "files_copy",
            json!({"console": "1.2.3.4", "from": "/a", "to": "/b", "overwrite": true}),
        )
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    assert!(text_of(&result).contains("Copied /a to /b"));
    let structured = &result["structuredContent"];
    assert_eq!(structured["got"]["addr"], "1.2.3.4");
    assert_eq!(structured["got"]["overwrite"], true);
    // The route got the id it reports.
    assert_eq!(structured["got"]["op_id"], structured["op_id"]);
    assert!(structured["op_id"].as_u64().unwrap() < (1u64 << 53));
}

#[tokio::test]
async fn a_long_copy_answers_with_an_op_id_and_is_followed_to_its_end() {
    let finish = Arc::new(Notify::new());
    let server = server_with(slow_copy_api(finish.clone()), false);
    let result = server
        .tool(
            "files_copy",
            json!({"console": "1.2.3.4", "from": "/a", "to": "/b", "wait_s": 0, "op_id": 77}),
        )
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(result["structuredContent"]["op_id"], 77);
    assert_eq!(result["structuredContent"]["finished"], false);
    assert!(text_of(&result).contains("do not start it again"));

    // Still running: the route has nothing to say, and that is not a failure.
    let result = server.tool("files_op_status", json!({"op_id": 77})).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(result["structuredContent"]["finished"], false);

    finish.notify_one();
    let mut status = Value::Null;
    for _ in 0..500 {
        status = server.tool("files_op_status", json!({"op_id": 77})).await;
        if status["structuredContent"]["finished"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(status["structuredContent"]["finished"], true, "{status}");
    assert_eq!(status["structuredContent"]["ok"], true);
    assert_eq!(status["structuredContent"]["result"]["got"]["op_id"], 77);
}

#[tokio::test]
async fn an_operation_nobody_started_is_said_to_be_not_running() {
    let result = server()
        .tool("files_op_status", json!({"op_id": 12345}))
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("not running"));
}

#[tokio::test]
async fn a_copy_refused_right_away_is_a_tool_error() {
    let api = Router::new().route(
        "/api/ps5/fs/copy",
        post(|| async {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "fs_copy: destination exists"})),
            )
        }),
    );
    let result = server_with(api, false)
        .tool(
            "files_copy",
            json!({"console": "1.2.3.4", "from": "/a", "to": "/b"}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("destination exists"));
}

#[tokio::test]
async fn games_stop_closes_cleanly_then_by_force() {
    fn processes() -> Router {
        Router::new().route(
            "/api/ps5/process/list",
            get(|| async {
                Json(json!({"processes": [
                    {"pid": 5, "kind": "app", "title_id": "PPSA00001", "app_id": 0},
                    {"pid": 6, "kind": "app", "title_id": "PPSA00001", "app_id": 42},
                    {"pid": 7, "kind": "system", "title_id": ""},
                ], "truncated": false}))
            }),
        )
    }
    let stop = json!({"console": "1.2.3.4", "title_id": "PPSA00001"});

    // A clean close, with the process that carries the app id.
    let api = processes().route(
        "/api/ps5/app/lifecycle",
        post(|Json(body): Json<Value>| async move {
            assert_eq!(body["app_id"], 42);
            assert_eq!(body["action"], "kill");
            Json(json!({"ok": true}))
        }),
    );
    let result = server_with(api, false)
        .tool("games_stop", stop.clone())
        .await;
    assert_eq!(result["structuredContent"]["how"], "app_kill");

    // Refused, as some firmwares do: the SIGKILL fallback.
    let api = processes()
        .route(
            "/api/ps5/app/lifecycle",
            post(|| async {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"error": "APP_LIFECYCLE failed"})),
                )
            }),
        )
        .route(
            "/api/ps5/process/kill",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(body["pid"], 6);
                Json(json!({"ok": true, "pid": 6}))
            }),
        );
    let result = server_with(api, false).tool("games_stop", stop).await;
    assert_eq!(result["structuredContent"]["how"], "sigkill");
    assert_eq!(result["structuredContent"]["pid"], 6);

    // Not running: nothing to do, and nothing was sent.
    let result = server_with(processes(), false)
        .tool(
            "games_stop",
            json!({"console": "1.2.3.4", "title_id": "PPSA99999"}),
        )
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(result["structuredContent"]["running"], false);
}

#[tokio::test]
async fn power_wake_finds_a_saved_credential_and_keeps_it_out_of_its_words() {
    let server = server();
    let result = server
        .tool("power_wake", json!({"console": "192.168.1.100"}))
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    // The fake route says what it was sent; the real one answers `{ok, sent}`.
    assert_eq!(
        result["structuredContent"]["credential_was"],
        "SECRET-CREDENTIAL"
    );
    assert!(!result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("SECRET"));
    let result = server
        .tool(
            "power_wake",
            json!({"console": "192.168.1.100", "credential": "mine"}),
        )
        .await;
    assert_eq!(result["structuredContent"]["credential_was"], "mine");
}

#[tokio::test]
async fn the_console_list_says_where_each_comes_from_and_never_a_key() {
    let server = server();
    let result = server.tool("consoles_list", json!({})).await;
    let structured = &result["structuredContent"];
    let everything = structured.to_string();
    assert!(!everything.contains("SECRET"), "{everything}");
    let consoles = structured["consoles"].as_array().unwrap();
    let saved = consoles
        .iter()
        .find(|console| console["address"] == "192.168.1.100")
        .expect("the saved console is listed");
    assert_eq!(saved["name"], "Living room");
    assert_eq!(saved["can_wake"], true);
    assert!(saved["sources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|source| source == "saved_list"));
    assert_eq!(structured["default_console"], "10.0.0.1");
    assert!(structured["default_source"].is_string());
    assert!(structured["sources_explained"]["saved_list"].is_string());
}

#[tokio::test]
async fn an_engine_with_no_saved_list_still_gives_its_default() {
    let result = server_with(Router::new(), false)
        .tool("consoles_list", json!({}))
        .await;
    assert_eq!(result["structuredContent"]["default_console"], "10.0.0.1");
    assert!(result.get("isError").is_none(), "{result}");
}

#[tokio::test]
async fn the_engine_status_gives_the_overview() {
    let result = server().tool("engine_status", json!({})).await;
    assert!(result.get("isError").is_none(), "{result}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["engine"]["version"], "1.2.3");
    assert_eq!(structured["read_only"], false);
    assert_eq!(structured["jobs"]["total"], 2);
    assert_eq!(structured["jobs"]["running"], 1);
    assert_eq!(structured["jobs"]["done"], 1);
    assert_eq!(structured["consoles"]["default_console"], "10.0.0.1");
    assert!(!structured.to_string().contains("SECRET"));
}

#[tokio::test]
async fn a_frame_is_an_image_with_its_details_beside_it() {
    let api = Router::new()
        .route(
            "/api/ps5/rp/session",
            get(|| async {
                Json(json!({"host": "192.168.1.100", "session": 3, "attached": true}))
            }),
        )
        .route(
            "/api/ps5/rp/frame",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                // Filled in from the running session, with the defaults a model reads well.
                assert_eq!(q["host"], "192.168.1.100");
                assert_eq!(q["session"], "3");
                assert_eq!(q["json"], "1");
                assert_eq!(q["format"], "jpeg");
                assert_eq!(q["max_width"], "1280");
                assert!(!q.contains_key("after"));
                Json(json!({
                    "host": "192.168.1.100",
                    "session": 3,
                    "format": "jpeg",
                    "width": 1280,
                    "height": 720,
                    "frame_age_ms": 16,
                    "frame_id": "2.4711",
                    "image_base64": "AAAA",
                }))
            }),
        );
    let result = server_with(api, false).tool("rp_frame", json!({})).await;
    assert!(result.get("isError").is_none(), "{result}");
    assert_eq!(result["content"][0]["type"], "image");
    assert_eq!(result["content"][0]["data"], "AAAA");
    assert_eq!(result["content"][0]["mimeType"], "image/jpeg");
    // The picture once, not again in the data.
    assert!(result["structuredContent"].get("image_base64").is_none());
    assert_eq!(result["structuredContent"]["frame_id"], "2.4711");
    let text = text_of(&result);
    assert!(text.contains("2.4711") && text.contains("16 ms"), "{text}");
    assert!(!text.contains("AAAA"), "{text}");
}

#[tokio::test]
async fn input_names_its_session_and_goes_as_the_route_wants_it() {
    let api = Router::new().route(
        "/api/ps5/rp/input",
        post(|Json(body): Json<Value>| async move {
            assert_eq!(body["host"], "192.168.1.100");
            assert_eq!(body["session"], 3);
            assert_eq!(body["buttons"], json!(["cross", "up"]));
            assert_eq!(body["left"], json!([0.5, -1]));
            assert_eq!(body["duration_ms"], 150);
            assert!(body.get("r2").is_none());
            Json(json!({"ok": true, "hold": 1}))
        }),
    );
    let server = server_with(api, false);
    let result = server
        .tool(
            "rp_input",
            json!({
                "console": "192.168.1.100",
                "session": 3,
                "buttons": ["cross", "up"],
                "left": [0.5, -1],
                "duration_ms": 150,
            }),
        )
        .await;
    assert!(result.get("isError").is_none(), "{result}");
    assert!(text_of(&result).contains("not that the game reacted"));

    // Input is never sent to a session nobody named, nor with a button that does not exist.
    let result = server
        .tool(
            "rp_input",
            json!({"console": "192.168.1.100", "buttons": ["up"]}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("`session`"));
    let result = server
        .tool(
            "rp_input",
            json!({"console": "c", "session": 3, "buttons": ["jump"]}),
        )
        .await;
    assert_eq!(result["isError"], true);
    let result = server
        .tool(
            "rp_input",
            json!({"console": "c", "session": 3, "left": [2, 0]}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("at most 1"));
}

// ── Resources and prompts ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_listed_resources_can_be_read() {
    let server = server();
    let answer = server.modern("resources/list", json!({})).await;
    assert_eq!(answer.body["result"]["resultType"], "complete");
    let resources = answer.body["result"]["resources"].as_array().unwrap();
    assert!(resources.len() >= 5);
    for resource in resources {
        let uri = resource["uri"].as_str().unwrap();
        let contents = server.read_resource(uri).await;
        assert_eq!(contents["uri"], uri);
        assert_eq!(contents["mimeType"], resource["mimeType"], "{uri}");
        let text = contents["text"].as_str().unwrap();
        assert!(!text.is_empty(), "{uri}");
        if contents["mimeType"] == "application/json" {
            assert!(serde_json::from_str::<Value>(text).is_ok(), "{uri}");
        }
    }
}

#[tokio::test]
async fn the_catalog_resource_holds_every_route() {
    let contents = server().read_resource("ps5upload://catalog").await;
    let catalog: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        catalog["routes"].as_array().unwrap().len(),
        super::catalog_routes::ROUTES.len()
    );
}

#[tokio::test]
async fn the_automation_guide_is_served_as_it_is_written() {
    let contents = server().read_resource("ps5upload://docs/automation").await;
    assert_eq!(contents["mimeType"], "text/markdown");
    assert!(contents["text"]
        .as_str()
        .unwrap()
        .starts_with("# Remote Play automation"));
}

#[tokio::test]
async fn the_status_template_reads_a_console() {
    let server = server();
    let answer = server.modern("resources/templates/list", json!({})).await;
    let templates = answer.body["result"]["resourceTemplates"]
        .as_array()
        .unwrap();
    assert_eq!(
        templates[0]["uriTemplate"],
        "ps5upload://consoles/{addr}/status"
    );

    let contents = server
        .read_resource("ps5upload://consoles/1.2.3.4/status")
        .await;
    let status: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["asked"], "1.2.3.4");

    // A console that does not answer is what its status says, not a failed read.
    let contents = server
        .read_resource("ps5upload://consoles/9.9.9.9/status")
        .await;
    let status: Value = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
    assert_eq!(status["status"], 502);
    assert_eq!(status["body"]["reason"], "console_rest_mode");
}

#[tokio::test]
async fn an_unknown_resource_is_invalid_params() {
    let server = server();
    let answer = server
        .modern("resources/read", json!({"uri": "ps5upload://nope"}))
        .await;
    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);

    let answer = server
        .legacy("resources/read", json!({"uri": "https://example.com/"}))
        .await;
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);

    let answer = server.legacy("resources/read", json!({})).await;
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);
}

#[tokio::test]
async fn the_events_resource_keeps_what_the_bus_said_once_someone_asked() {
    let server = server();
    // The first request starts the keeping, and not before: an engine nobody drives through MCP
    // has no extra listener on its bus.
    assert_eq!(server.events.receiver_count(), 0);
    server.read_resource("ps5upload://events").await;
    tokio::task::yield_now().await;
    assert_eq!(server.events.receiver_count(), 1);

    server
        .events
        .send(json!({"job_id": JOB, "job": {"status": "done"}}).to_string())
        .unwrap();
    let mut events = Value::Null;
    for _ in 0..500 {
        let contents = server.read_resource("ps5upload://events").await;
        events = serde_json::from_str(contents["text"].as_str().unwrap()).unwrap();
        if !events["events"].as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(events["events"][0]["event"]["job_id"], JOB);
    assert!(events["events"][0]["at_ms"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn prompts_are_listed_and_given() {
    let server = server();
    let answer = server.modern("prompts/list", json!({})).await;
    assert_eq!(answer.body["result"]["resultType"], "complete");
    let names: Vec<&str> = answer.body["result"]["prompts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|prompt| prompt["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "play_session",
            "install_game",
            "diagnose_connection",
            "free_up_space"
        ]
    );

    let answer = server
        .modern(
            "prompts/get",
            json!({"name": "diagnose_connection", "arguments": {"console": "1.2.3.4"}}),
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    let text = answer.body["result"]["messages"][0]["content"]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("1.2.3.4"));

    let answer = server.legacy("prompts/get", json!({"name": "nope"})).await;
    assert_eq!(answer.body["error"]["code"], INVALID_PARAMS);
}

// ── subscriptions/listen ────────────────────────────────────────────────────────────────────────

fn listen_request(uris: Value) -> Request<Body> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "subscriptions/listen",
        "params": modern_params(json!({"notifications": {"resourceSubscriptions": uris}})),
    });
    Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("MCP-Protocol-Version", MODERN_VERSION)
        .header("Mcp-Method", "subscriptions/listen")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// The next message on a stream, skipping nothing: a frame is `data: <json>\n\n`.
async fn next_message(
    stream: &mut (impl futures_util::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin),
) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .expect("a message in time")
        .expect("the stream goes on")
        .unwrap();
    let text = String::from_utf8(frame.to_vec()).unwrap();
    let data = text
        .strip_prefix("data: ")
        .unwrap_or_else(|| panic!("{text:?}"));
    serde_json::from_str(data.trim_end()).unwrap()
}

#[tokio::test]
async fn listen_acknowledges_says_when_things_change_and_ends_with_the_engine() {
    let server = server();
    let response = server
        .router
        .clone()
        .oneshot(listen_request(json!([
            "ps5upload://jobs",
            "ps5upload://events",
            "ps5upload://catalog",
        ])))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    let mut stream = response.into_body().into_data_stream();

    // Only what it honours is acknowledged.
    let acknowledged = next_message(&mut stream).await;
    assert_eq!(
        acknowledged["method"],
        "notifications/subscriptions/acknowledged"
    );
    assert_eq!(
        acknowledged["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        7
    );
    assert_eq!(
        acknowledged["params"]["notifications"]["resourceSubscriptions"],
        json!(["ps5upload://events", "ps5upload://jobs"])
    );

    server.events.send("{}".to_string()).unwrap();
    let first = next_message(&mut stream).await;
    let second = next_message(&mut stream).await;
    assert_eq!(first["method"], "notifications/resources/updated");
    assert_eq!(first["params"]["uri"], "ps5upload://events");
    assert_eq!(
        first["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        7
    );
    assert_eq!(second["params"]["uri"], "ps5upload://jobs");

    // The engine stops: the final response, then the end, so its drain is not held up.
    server.state.begin_shutdown();
    let last = next_message(&mut stream).await;
    assert_eq!(last["id"], 7);
    assert_eq!(last["result"]["resultType"], "complete");
    assert_eq!(
        last["result"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        7
    );
    assert!(stream.next().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn listen_keeps_an_idle_stream_open_and_coalesces_a_burst() {
    let server = server();
    let response = server
        .router
        .clone()
        .oneshot(listen_request(json!(["ps5upload://jobs"])))
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    next_message(&mut stream).await;

    // Idle: a comment every 15 s, with time standing still until then.
    let frame = stream.next().await.unwrap().unwrap();
    assert_eq!(&frame[..], b":\n\n");

    // A burst: ten ticks over two seconds, read as they come, is at most one update per 500 ms,
    // and the last change is not lost to the coalescing.
    let events = server.events.clone();
    let started = tokio::time::Instant::now();
    tokio::spawn(async move {
        for _ in 0..10 {
            let _ = events.send("{}".to_string());
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });
    let mut updates = Vec::new();
    while started.elapsed() < Duration::from_secs(4) {
        match tokio::time::timeout(Duration::from_secs(1), stream.next()).await {
            Ok(Some(Ok(frame))) if frame.starts_with(b"data:") => updates.push(started.elapsed()),
            Ok(Some(Ok(_))) | Err(_) => {}
            Ok(_) => panic!("the stream ended"),
        }
    }
    assert!((3..=5).contains(&updates.len()), "{updates:?}");
    for pair in updates.windows(2) {
        assert!(
            pair[1] - pair[0] >= Duration::from_millis(500),
            "{updates:?}"
        );
    }
    let last = *updates.last().unwrap();
    assert!(last >= Duration::from_millis(1800), "{updates:?}");
}

#[tokio::test]
async fn dropping_the_stream_stops_listening() {
    let server = server();
    let before = server.events.receiver_count();
    let response = server
        .router
        .clone()
        .oneshot(listen_request(json!(["ps5upload://jobs"])))
        .await
        .unwrap();
    // `before` plus the stream's, plus the ring's on this server's first request.
    assert!(server.events.receiver_count() > before);
    let while_open = server.events.receiver_count();
    drop(response);
    assert_eq!(server.events.receiver_count(), while_open - 1);
}

#[tokio::test]
async fn nothing_to_honour_still_opens_a_stream_with_no_listener() {
    let server = server();
    let response = server
        .router
        .clone()
        .oneshot(listen_request(json!(["ps5upload://catalog"])))
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    let acknowledged = next_message(&mut stream).await;
    assert_eq!(
        acknowledged["params"]["notifications"]["resourceSubscriptions"],
        json!([])
    );
    server.state.begin_shutdown();
    assert_eq!(next_message(&mut stream).await["id"], 7);
}

// ── Tools that are one route ────────────────────────────────────────────────────────────────────

/// Says back the route that was called, whatever it is.
async fn echo_any(
    method: Method,
    uri: axum::http::Uri,
    Query(query): Query<HashMap<String, String>>,
    body: String,
) -> Json<Value> {
    Json(json!({
        "method": method.as_str(),
        "path": uri.path(),
        "query": query,
        "body": serde_json::from_str::<Value>(&body).ok(),
    }))
}

fn echo_server() -> Server {
    server_with(Router::new().fallback(echo_any), false)
}

/// A body the route's own request type must accept: what keeps a tool's field names true to the
/// handler it calls.
fn reads_as<T: serde::de::DeserializeOwned>(body: &Value) {
    if let Err(e) = serde_json::from_value::<T>(body.clone()) {
        panic!("the route cannot read {body}: {e}");
    }
}

/// Calls a tool and gives what the route was asked.
async fn route_of(server: &Server, name: &str, arguments: Value) -> Value {
    let result = server.tool(name, arguments).await;
    assert_ne!(result["isError"], true, "{name}: {}", text_of(&result));
    let asked = result["structuredContent"].clone();
    if asked.get("path").is_some() {
        return asked;
    }
    // A long operation's answer carries the same.
    asked["result"].clone()
}

#[tokio::test]
async fn each_post_tool_sends_what_its_route_reads() {
    let server = echo_server();
    let c = "1.2.3.4";

    let asked = route_of(
        &server,
        "upload_file",
        json!({"console": c, "src": "/tmp/a", "dest": "/data/a", "bandwidth_cap_mbps": 5.5}),
    )
    .await;
    assert_eq!(asked["path"], "/api/transfer/file");
    assert_eq!(asked["body"]["addr"], c);
    reads_as::<crate::TransferFileReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "upload_dir",
        json!({"console": c, "src_dir": "/tmp/a", "dest_root": "/data/a",
               "excludes": ["*.tmp"], "skip_existing": "fast"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/transfer/dir");
    reads_as::<crate::TransferDirReq>(&asked["body"]);

    for (format, path) in [
        ("zip", "/api/transfer/zip"),
        ("7z", "/api/transfer/7z"),
        ("rar", "/api/transfer/rar"),
    ] {
        let asked = server
            .tool(
                "upload_archive",
                json!({"console": c, "archive": "/tmp/a", "format": format, "dest_root": "/d"}),
            )
            .await;
        let asked = &asked["structuredContent"];
        assert_eq!(asked["path"], path);
        let body = &asked["body"];
        match format {
            "zip" => reads_as::<crate::TransferZipReq>(body),
            "7z" => reads_as::<crate::Transfer7zReq>(body),
            _ => reads_as::<crate::TransferRarReq>(body),
        }
    }

    let asked = route_of(
        &server,
        "files_download",
        json!({"console": c, "src_path": "/data/a", "dest_dir": "/tmp", "kind": "file"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/transfer/download");
    reads_as::<crate::TransferDownloadReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "files_chmod",
        json!({"console": c, "path": "/data/a", "mode": "755", "recursive": true}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/fs/chmod");
    reads_as::<crate::FsChmodReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "files_mount",
        json!({"console": c, "image_path": "/data/a.exfat", "read_only": true}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/fs/mount");
    reads_as::<crate::FsMountReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "files_unmount",
        json!({"console": c, "mount_point": "/mnt/ps5upload/a"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/fs/unmount");
    reads_as::<crate::FsUnmountReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "files_eject",
        json!({"console": c, "mount_point": "/mnt/usb0", "force": true}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/usb/eject");
    reads_as::<crate::UsbEjectReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "games_register",
        json!({"console": c, "src_path": "/data/g"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/app/register");
    reads_as::<crate::AppRegisterReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "games_unregister",
        json!({"console": c, "title_id": "PPSA00001"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/app/unregister");
    reads_as::<crate::AppUnregisterReq>(&asked["body"]);

    let asked = route_of(&server, "processes_kill", json!({"console": c, "pid": 77})).await;
    assert_eq!(asked["path"], "/api/ps5/process/kill");
    reads_as::<crate::ProcessKillReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "cheats_toggle",
        json!({"console": c, "title_id": "PPSA00001", "index": 2, "on": false}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/cheats/toggle");
    reads_as::<crate::CheatsToggleReq>(&asked["body"]);

    let asked = route_of(&server, "cheats_reload", json!({"console": c})).await;
    assert_eq!(asked["path"], "/api/ps5/cheats/reload");
    reads_as::<crate::CheatsAddrQuery>(&asked["body"]);

    let asked = route_of(
        &server,
        "hw_fan_threshold",
        json!({"console": c, "threshold_c": 70}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/hw/fan-threshold");
    reads_as::<crate::FanThresholdReq>(&asked["body"]);

    for (action, path) in [
        ("start", "/api/ps5/hw/fps/capture/start"),
        ("stop", "/api/ps5/hw/fps/capture/stop"),
    ] {
        let asked = route_of(
            &server,
            "hw_fps_capture",
            json!({"console": c, "action": action}),
        )
        .await;
        assert_eq!(asked["path"], path);
        assert_eq!(asked["body"]["addr"], c);
        reads_as::<crate::game_fps_api::AddrBody>(&asked["body"]);
    }

    let asked = route_of(
        &server,
        "saves_backup_now",
        json!({"console": c, "title_id": "PPSA00001", "user_id": 1, "wait_s": 5}),
    )
    .await;
    assert_eq!(asked["path"], "/api/save-backup/now");
    reads_as::<crate::save_backup::BackUpNowBody>(&asked["body"]);
    assert!(asked["body"].get("wait_s").is_none());

    let asked = route_of(
        &server,
        "saves_restore",
        json!({"console": c, "title_id": "PPSA00001", "file": "a.zip",
               "save_path": "/user/x", "user_id": 1}),
    )
    .await;
    assert_eq!(asked["path"], "/api/save-backup/restore");
    reads_as::<crate::save_backup::RestoreBody>(&asked["body"]);

    let asked = route_of(
        &server,
        "files_extract",
        json!({"console": c, "path": "/data/a.zip", "dest": "/data/a", "merge": true}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/fs/extract");
    reads_as::<crate::FsExtractReq>(&asked["body"]);

    let asked = route_of(
        &server,
        "sources_browse",
        json!({"path": "remote://nas-1/games"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/remote/list");
    reads_as::<crate::remote::api::ListBody>(&asked["body"]);

    let asked = route_of(
        &server,
        "sources_fetch",
        json!({"path": "remote://nas-1/a.pkg", "dest_dir": "/tmp"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/remote/fetch");
    reads_as::<crate::remote::fetch::FetchBody>(&asked["body"]);
}

#[tokio::test]
async fn the_routes_that_say_host_get_the_console_as_host() {
    let server = echo_server();
    let asked = route_of(&server, "payloads_list", json!({"console": "1.2.3.4"})).await;
    assert_eq!(asked["path"], "/api/ps5/pldmgr");
    assert_eq!(asked["query"]["host"], "1.2.3.4");
    assert!(asked["query"].get("addr").is_none());
    for (tool, path, extra) in [
        (
            "payloads_launch",
            "/api/ps5/pldmgr/launch",
            json!({"path": "/x"}),
        ),
        (
            "payloads_delete",
            "/api/ps5/pldmgr/delete",
            json!({"filename": "x"}),
        ),
        (
            "payloads_store",
            "/api/ps5/pldmgr/store",
            json!({"filename": "x", "local_path": "/y"}),
        ),
        (
            "payloads_autoload",
            "/api/ps5/pldmgr/autoload",
            json!({"enabled": true, "list": ["x"]}),
        ),
    ] {
        let mut arguments = extra;
        arguments["console"] = json!("1.2.3.4");
        let asked = route_of(&server, tool, arguments).await;
        assert_eq!(asked["path"], path);
        assert_eq!(asked["body"]["host"], "1.2.3.4", "{tool}");
        assert!(asked["body"].get("console").is_none(), "{tool}");
    }
}

#[tokio::test]
async fn the_payload_update_tools_reach_their_routes() {
    let server = echo_server();
    let asked = route_of(&server, "payloads_updates", json!({})).await;
    assert_eq!(asked["path"], "/api/payloads/updates");
    let asked = route_of(&server, "payloads_updates_check", json!({"force": true})).await;
    assert_eq!(asked["path"], "/api/payloads/updates/check");
    assert_eq!(asked["body"]["force"], true);
    let asked = route_of(
        &server,
        "payloads_update",
        json!({"ids": ["kstuff-echostretch"], "sync_console": "1.2.3.4"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/payloads/updates/apply");
    assert_eq!(asked["body"]["sync_console"], "1.2.3.4");
    assert_eq!(asked["body"]["ids"], json!(["kstuff-echostretch"]));
    let asked = route_of(&server, "payloads_rollback", json!({"id": "hush"})).await;
    assert_eq!(asked["path"], "/api/payloads/hush/rollback");
    let asked = route_of(
        &server,
        "payloads_update_settings",
        json!({"auto_update": true}),
    )
    .await;
    assert_eq!(asked["path"], "/api/payloads/updates/settings");
    assert_eq!(asked["body"]["auto_update"], true);
    let asked = route_of(
        &server,
        "payloads_pm_choice",
        json!({"console": "1.2.3.4", "ids": ["hush"], "autoload_ids": []}),
    )
    .await;
    assert_eq!(asked["path"], "/api/payloads/pm-sync");
    assert_eq!(asked["body"]["host"], "1.2.3.4");
    let asked = route_of(&server, "payloads_pm_sync", json!({"console": "1.2.3.4"})).await;
    assert_eq!(asked["path"], "/api/payloads/pm-sync/run");
    assert_eq!(asked["body"]["host"], "1.2.3.4");
}

#[tokio::test]
async fn each_read_tool_asks_its_route_with_the_console_as_addr() {
    let server = echo_server();
    for (tool, path, extra) in [
        ("files_volumes", "/api/ps5/volumes", json!({})),
        ("saves_list", "/api/ps5/saves/list", json!({"user_id": 3})),
        ("cheats_status", "/api/ps5/cheats/status", json!({})),
        ("cheats_list", "/api/ps5/cheats/list", json!({})),
        (
            "cheats_get",
            "/api/ps5/cheats/get",
            json!({"title_id": "PPSA00001"}),
        ),
        ("hw_power", "/api/ps5/hw/power", json!({})),
        ("hw_fps", "/api/ps5/hw/fps", json!({})),
        ("power_telemetry", "/api/ps5/power/telemetry", json!({})),
        ("devices_list", "/api/mesh/state", json!({})),
        ("drive_list", "/api/drive", json!({})),
        (
            "install_preflight",
            "/api/pkg/install/preflight",
            json!({"content_id": "UP0000-PPSA00001_00-0000000000000000"}),
        ),
    ] {
        let mut arguments = extra;
        arguments["console"] = json!("1.2.3.4");
        let asked = route_of(&server, tool, arguments).await;
        assert_eq!(asked["method"], "GET", "{tool}");
        assert_eq!(asked["path"], path, "{tool}");
        assert_eq!(asked["query"]["addr"], "1.2.3.4", "{tool}");
    }
    // Its route has its own name for the console.
    let asked = route_of(&server, "install_history", json!({"console": "1.2.3.4"})).await;
    assert_eq!(asked["query"]["ps5_addr"], "1.2.3.4");
    // No console at all.
    let asked = route_of(
        &server,
        "saves_backups_list",
        json!({"title_id": "PPSA00001"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/save-backup/list");
    assert_eq!(asked["query"]["title_id"], "PPSA00001");
}

#[tokio::test]
async fn an_id_goes_in_the_path_and_nowhere_else() {
    let server = echo_server();
    let asked = route_of(&server, "disk_usage_status", json!({"id": "a/b"})).await;
    assert_eq!(asked["path"], "/api/ps5/disk-usage/a%2Fb");
    assert_eq!(asked["query"], json!({}));

    let asked = route_of(
        &server,
        "cleanup_scan_status",
        json!({"id": 7, "console": "1.2.3.4"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/cleanup/scan/7");
    assert_eq!(asked["query"]["addr"], "1.2.3.4");

    let asked = route_of(
        &server,
        "cleanup_apply",
        json!({"id": 7, "console": "1.2.3.4", "item_ids": [1, 2]}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/cleanup/scan/7/apply");
    assert_eq!(
        asked["body"],
        json!({"addr": "1.2.3.4", "item_ids": [1, 2]})
    );

    let asked = route_of(&server, "sources_test", json!({"id": "nas-1"})).await;
    assert_eq!(asked["path"], "/api/remote/connections/nas-1/test");
}

#[tokio::test]
async fn a_post_that_reads_its_query_gets_the_console_there() {
    let server = echo_server();
    let asked = route_of(
        &server,
        "notifications_clear",
        json!({"console": "1.2.3.4"}),
    )
    .await;
    assert_eq!(asked["method"], "POST");
    assert_eq!(asked["path"], "/api/ps5/notif/clear");
    assert_eq!(asked["query"]["addr"], "1.2.3.4");
    assert_eq!(asked["body"], json!({}));
}

#[tokio::test]
async fn a_started_job_says_how_to_follow_it() {
    let api = Router::new().route(
        "/api/transfer/file",
        post(|| async { Json(json!({"job_id": "j-1"})) }),
    );
    let result = server_with(api, false)
        .tool(
            "upload_file",
            json!({"console": "c", "src": "/a", "dest": "/b"}),
        )
        .await;
    let text = text_of(&result);
    assert!(text.contains("j-1") && text.contains("jobs_wait"), "{text}");
}

#[tokio::test]
async fn files_write_sends_base64_and_refuses_too_much() {
    let server = echo_server();
    let result = server
        .tool(
            "files_write",
            json!({"console": "c", "path": "/data/a.txt", "text": "hello", "create_only": true}),
        )
        .await;
    let asked = &result["structuredContent"];
    assert_eq!(asked["path"], "/api/ps5/fs/write-bytes");
    assert_eq!(asked["body"]["bytes_b64"], "aGVsbG8=");
    reads_as::<crate::FsWriteBytesReq>(&asked["body"]);

    let result = server
        .tool(
            "files_write",
            json!({"console": "c", "path": "/data/a.txt"}),
        )
        .await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("`text` or `base64`"));
}

/// An install API: status moves on as it is polled, and one console is busy.
fn install_api() -> Router {
    static INSTALL_POLLS: AtomicUsize = AtomicUsize::new(0);
    Router::new()
        .route(
            "/api/pkg/install",
            post(|Json(body): Json<Value>| async move {
                if body["ps5_addr"] == "busy.console" {
                    return Json(json!({"ok": false, "error": "busy", "job": "running-1"}));
                }
                Json(json!({"ok": true, "job": "new-1", "got": body}))
            }),
        )
        .route(
            "/api/pkg/install/status",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                if q.get("job").map(String::as_str) == Some("nope") {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(json!({"ok": false, "error": "no such job"})),
                    )
                        .into_response();
                }
                let phase = if INSTALL_POLLS.fetch_add(1, Ordering::SeqCst) < 1 {
                    "deliver"
                } else {
                    "failed"
                };
                Json(
                    json!({"job": q.get("job"), "phase": phase, "verdict": "failed",
                            "hint": "The console refused it."}),
                )
                .into_response()
            }),
        )
}

#[tokio::test]
async fn install_pkg_sends_what_the_route_reads() {
    let server = server_with(install_api(), false);
    let result = server
        .tool(
            "install_pkg",
            json!({
                "console": "1.2.3.4",
                "source_type": "remote",
                "source": "/a.pkg",
                "connection": "nas-1",
                "title_id": "PPSA00001",
                "proxy_link": true,
            }),
        )
        .await;
    assert_ne!(result["isError"], true, "{}", text_of(&result));
    assert!(text_of(&result).contains("new-1"));
    let body = &result["structuredContent"]["got"];
    let request: crate::install::InstallRequest = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(request.ps5_addr, "1.2.3.4");
    assert_eq!(request.title_id.as_deref(), Some("PPSA00001"));
    assert!(request.options.proxy_link);
    assert!(!request.options.allow_destructive_reinstall);
    assert_eq!(request.source.kind(), "remote");
}

#[tokio::test]
async fn a_second_install_names_the_one_running() {
    let server = server_with(install_api(), false);
    let result = server
        .tool(
            "install_pkg",
            json!({"console": "busy.console", "source_type": "url", "source": "http://x/a.pkg"}),
        )
        .await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(
        text.contains("running-1") && text.contains("install_wait"),
        "{text}"
    );
    // Not the hint for a busy Remote Play frame.
    assert!(!text.contains("frame"), "{text}");
    assert_eq!(result["structuredContent"]["job"], "running-1");
}

#[tokio::test]
async fn install_wait_follows_it_and_a_failure_is_a_finished_install() {
    let server = server_with(install_api(), false);
    let result = server
        .tool("install_wait", json!({"job": "new-1", "timeout_s": 30}))
        .await;
    assert_ne!(result["isError"], true);
    let answer = &result["structuredContent"];
    assert_eq!(answer["finished"], true);
    assert_eq!(answer["install"]["phase"], "failed");
    assert!(text_of(&result).contains("The console refused it."));

    let result = server.tool("install_wait", json!({"job": "nope"})).await;
    assert_eq!(result["isError"], true);
    assert!(text_of(&result).contains("no such job"));
}

#[tokio::test]
async fn read_only_refuses_the_new_tools_that_change_something() {
    let server = server_with(Router::new().fallback(echo_any), true);
    for (tool, arguments) in [
        (
            "upload_file",
            json!({"console": "c", "src": "/a", "dest": "/b"}),
        ),
        (
            "install_pkg",
            json!({"console": "c", "source_type": "url", "source": "http://x"}),
        ),
        ("files_write", json!({"console": "c", "path": "/a"})),
        ("processes_kill", json!({"console": "c", "pid": 1})),
        (
            "cleanup_apply",
            json!({"console": "c", "id": 1, "item_ids": [1]}),
        ),
    ] {
        let result = server.tool(tool, arguments).await;
        assert_eq!(result["isError"], true, "{tool}");
        assert!(text_of(&result).contains("agents can only look"), "{tool}");
    }
    // A scan only looks, even though its route is a POST.
    let asked = route_of(&server, "cleanup_scan", json!({"console": "c"})).await;
    assert_eq!(asked["path"], "/api/ps5/cleanup/scan");
}

#[tokio::test]
async fn the_recent_events_are_the_newest_within_the_limit() {
    let api = Router::new().route(
        "/api/event-journal",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            Json(json!({"events": [1, 2, 3], "dropped": 0, "since": q.get("since")}))
        }),
    );
    let result = server_with(api, false)
        .tool("engine_events_recent", json!({"minutes": 5, "limit": 2}))
        .await;
    let answer = &result["structuredContent"];
    assert_eq!(answer["events"], json!([2, 3]));
    let since: u64 = answer["since"].as_str().unwrap().parse().unwrap();
    assert!(crate::now_ms() - since >= 5 * 60_000);
}

#[tokio::test]
async fn the_log_tools_reach_their_routes() {
    let set = route_of(
        &echo_server(),
        "logs_level_set",
        json!({"categories": {"rp": "trace"}, "minutes": 30, "merge": true}),
    )
    .await;
    assert_eq!(set["method"], "PUT");
    assert_eq!(set["path"], "/api/logs/level");
    let read = route_of(&echo_server(), "logs_level_get", json!({})).await;
    assert_eq!(read["path"], "/api/logs/level");

    let api = Router::new().route(
        "/api/logs/recent",
        get(|Query(q): Query<HashMap<String, String>>| async move {
            Json(json!({"lines": [], "count": 0, "next_seq": 0, "asked": q}))
        }),
    );
    let result = server_with(api, false)
        .tool(
            "logs_recent",
            json!({"level": "debug", "category": "rp", "minutes": 10}),
        )
        .await;
    let asked = &result["structuredContent"]["asked"];
    assert_eq!(asked["level"], "debug");
    assert_eq!(asked["category"], "rp");
    let since: u64 = asked["since_ms"].as_str().unwrap().parse().unwrap();
    assert!(crate::now_ms() - since >= 10 * 60_000);
    // A level outside the five is refused before any call.
    let bad = server().tool("logs_recent", json!({"level": "loud"})).await;
    assert_eq!(bad["isError"], true);
}

#[tokio::test]
async fn the_route_search_is_a_tool_too() {
    let result = server()
        .tool("api_catalog", json!({"search": "pkg install status"}))
        .await;
    assert_eq!(
        result["structuredContent"]["routes"],
        json!([{"method": "GET", "path": "/api/pkg/install/status", "summary": "How an install job stands"}])
    );
}

// ── Tools for the routes that start a stream, find consoles and send payloads ───────────────────

#[tokio::test]
async fn rp_start_names_the_console_as_the_route_does() {
    let server = echo_server();
    let asked = route_of(
        &server,
        "rp_start",
        json!({"console": "Living room", "fps": 60, "wait_s": 10}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/rp/start");
    assert_eq!(
        asked["body"],
        json!({"host": "Living room", "fps": 60, "wait_s": 10})
    );
}

#[tokio::test]
async fn the_stream_tools_name_the_session_they_mean() {
    let server = echo_server();
    for (tool, path, extra, field) in [
        ("rp_stop", "/api/ps5/rp/stop", json!({}), None),
        (
            "rp_settings",
            "/api/ps5/rp/settings",
            json!({"keyframe": true}),
            Some("keyframe"),
        ),
        (
            "rp_passcode",
            "/api/ps5/rp/passcode",
            json!({"digits": "1234"}),
            Some("digits"),
        ),
        (
            "rp_keyboard",
            "/api/ps5/rp/keyboard",
            json!({"text": "hi"}),
            Some("text"),
        ),
    ] {
        let mut arguments = extra.clone();
        arguments["console"] = json!("1.2.3.4");
        arguments["session"] = json!(9);
        let result = server.tool(tool, arguments).await;
        assert_ne!(result["isError"], true, "{tool}: {}", text_of(&result));
        let asked = &result["structuredContent"];
        assert_eq!(asked["path"], path, "{tool}");
        assert_eq!(asked["body"]["host"], "1.2.3.4", "{tool}");
        assert_eq!(asked["body"]["session"], 9, "{tool}");
        if let Some(field) = field {
            assert_eq!(asked["body"][field], extra[field], "{tool}");
        }
    }
}

#[tokio::test]
async fn a_passcode_stays_out_of_the_words_of_the_answer() {
    let result = echo_server()
        .tool(
            "rp_passcode",
            json!({"console": "c", "session": 1, "digits": "1234"}),
        )
        .await;
    assert!(!result["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("1234"));
}

#[tokio::test]
async fn the_payload_and_console_tools_reach_their_routes() {
    let server = echo_server();
    let asked = route_of(&server, "consoles_discover", json!({"timeout_ms": 500})).await;
    assert_eq!(asked["path"], "/api/discover");
    assert_eq!(asked["method"], "POST");
    let asked = route_of(
        &server,
        "tailnet_consoles",
        json!({"addr": "den-ps5u", "refresh": true}),
    )
    .await;
    assert_eq!(asked["path"], "/api/tailnet/consoles");
    assert_eq!(asked["method"], "GET");
    assert_eq!(asked["query"]["addr"], "den-ps5u");
    let asked = route_of(&server, "payloads_catalog", json!({})).await;
    assert_eq!(asked["path"], "/api/payloads");
    let asked = route_of(&server, "payloads_download", json!({"id": "kstuff"})).await;
    assert_eq!(asked["path"], "/api/payloads/kstuff/download");
    let asked = route_of(
        &server,
        "payloads_send",
        json!({"console": "c", "payload_id": "kstuff"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/payload/send");
    assert_eq!(asked["body"], json!({"host": "c", "payload_id": "kstuff"}));
    let asked = route_of(
        &server,
        "files_hash",
        json!({"console": "c", "path": "/data/a", "algo": "crc32"}),
    )
    .await;
    assert_eq!(asked["path"], "/api/ps5/fs/hash");
    assert_eq!(asked["body"]["addr"], "c");
    let asked = route_of(&server, "shell_run", json!({"console": "c", "cmd": "ls"})).await;
    assert_eq!(asked["path"], "/api/ps5/shell/run");
    assert_eq!(asked["body"], json!({"addr": "c", "cmd": "ls"}));
    let asked = route_of(&server, "settings_get", json!({})).await;
    assert_eq!(asked["path"], "/api/settings");
    let asked = route_of(&server, "settings_set", json!({"settings": {"a": 1}})).await;
    assert_eq!(asked["method"], "PATCH");
    assert_eq!(asked["body"], json!({"settings": {"a": 1}}));
}

#[tokio::test]
async fn the_console_list_prefers_the_apps_list_and_never_shows_a_key() {
    let api = Router::new().route(
        "/api/consoles",
        get(|| async {
            Json(json!({
                "rev": 3,
                "consoles": [{
                    "id": "c1",
                    "name": "Bedroom",
                    "host": "192.168.1.50",
                    "has_rp_credentials": true,
                    "has_wake_credential": true,
                }],
                "paired_hosts_not_listed": ["192.168.1.51"],
            }))
        }),
    );
    let result = server_with(api, false)
        .tool("consoles_list", json!({}))
        .await;
    let consoles = result["structuredContent"]["consoles"]
        .as_array()
        .unwrap()
        .clone();
    let find = |address: &str| {
        consoles
            .iter()
            .find(|console| console["address"] == address)
            .unwrap_or_else(|| panic!("{address} is not listed"))
            .clone()
    };
    let saved = find("192.168.1.50");
    assert_eq!(saved["name"], "Bedroom");
    assert_eq!(saved["can_wake"], true);
    assert_eq!(saved["can_stream"], true);
    let paired = find("192.168.1.51");
    assert_eq!(paired["can_stream"], true);
    assert_eq!(paired["sources"], json!(["paired"]));
}

async fn call_tool(server: &Server, name: &str, arguments: Value) -> Value {
    server
        .modern("tools/call", json!({"name": name, "arguments": arguments}))
        .await
        .body["result"]
        .clone()
}

#[tokio::test]
async fn api_describe_gives_a_route_with_its_schemas() {
    let server = server();
    let result = call_tool(
        &server,
        "api_describe",
        json!({"method": "POST", "path": "/api/ps5/fs/stat"}),
    )
    .await;
    assert!(result.get("isError").is_none(), "{result}");
    let data = &result["structuredContent"];
    assert_eq!(data["path"], "/api/ps5/fs/stat");
    let body = &data["operation"]["requestBody"]["content"]["application/json"]["schema"]["$ref"];
    let name = body.as_str().unwrap().rsplit('/').next().unwrap();
    assert!(data["schemas"][name]["properties"].get("path").is_some());

    let missing = call_tool(&server, "api_describe", json!({"path": "/api/nope"})).await;
    assert_eq!(missing["isError"], true);
    let secret = call_tool(
        &server,
        "api_describe",
        json!({"path": "/api/consoles/document"}),
    )
    .await;
    assert_eq!(secret["isError"], true);
}

#[tokio::test]
async fn api_catalog_says_what_a_route_does() {
    let result = call_tool(&server(), "api_catalog", json!({"search": "api/version"})).await;
    let routes = result["structuredContent"]["routes"].as_array().unwrap();
    assert!(routes[0]["summary"].is_string(), "{routes:?}");
}

#[tokio::test]
async fn helper_ensure_sends_nothing_to_a_helper_that_answers() {
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = sent.clone();
    let api = Router::new()
        .route(
            "/api/ps5/status",
            get(|| async { Json(json!({"version": "x"})) }),
        )
        .route(
            "/api/pkg/payload-restore",
            post(move || async move {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({"ok": true, "bytes": 1}))
            }),
        );
    let server = server_with(api, false);
    let result = call_tool(
        &server,
        "consoles_helper_ensure",
        json!({"console": "9.9.9.9"}),
    )
    .await;
    assert_eq!(result["structuredContent"]["running"], true, "{result}");
    assert_eq!(result["structuredContent"]["sent"], false);
    assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn helper_ensure_sends_when_nothing_answers_and_says_when_the_send_fails() {
    let api = |ok: bool| {
        Router::new()
            .route(
                "/api/ps5/status",
                get(|| async { (StatusCode::BAD_GATEWAY, Json(json!({"error": "no"}))) }),
            )
            .route(
                "/api/pkg/payload-restore",
                post(move |Json(body): Json<Value>| async move {
                    assert_eq!(body["ps5_addr"], "9.9.9.9");
                    Json(json!({"ok": ok, "error": if ok { "" } else { "no loader" }}))
                }),
            )
    };
    let args = json!({"console": "9.9.9.9"});
    let sent = call_tool(
        &server_with(api(true), false),
        "consoles_helper_ensure",
        args.clone(),
    )
    .await;
    assert_eq!(sent["structuredContent"]["sent"], true, "{sent}");
    // The route answers 200 with ok: false, which must not read as a success.
    let failed = call_tool(
        &server_with(api(false), false),
        "consoles_helper_ensure",
        args,
    )
    .await;
    assert_eq!(failed["isError"], true, "{failed}");
}

#[tokio::test]
async fn golive_start_turns_ids_into_the_routes_shape() {
    let api = Router::new().route(
        "/api/stream/start",
        post(|Json(body): Json<Value>| async move { Json(json!({"ok": true, "got": body})) }),
    );
    let server = server_with(api, false);
    let result = call_tool(
        &server,
        "golive_start",
        json!({"destinations": ["a", "b"], "audio_kbps": 128}),
    )
    .await;
    let got = &result["structuredContent"]["got"];
    assert_eq!(
        got["destinations"],
        json!([{"key_id": "a"}, {"key_id": "b"}]),
        "{result}"
    );
    assert_eq!(got["audio_kbps"], 128);
    let none = call_tool(&server, "golive_start", json!({"destinations": []})).await;
    assert_eq!(none["isError"], true);
}

#[tokio::test]
async fn read_only_refuses_the_friends_and_viewers_tools() {
    let server = server_with(fake_api(), true);
    for (tool, args) in [
        ("together_open", json!({"console": "9.9.9.9"})),
        ("together_close", json!({"console": "9.9.9.9"})),
        ("golive_start", json!({"destinations": ["a"]})),
        (
            "health_fix",
            json!({"console": "9.9.9.9", "action": "clean_junk"}),
        ),
        ("consoles_helper_ensure", json!({"console": "9.9.9.9"})),
    ] {
        let result = call_tool(&server, tool, args).await;
        assert_eq!(result["isError"], true, "{tool}");
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("agents can only look"));
    }
}

#[tokio::test]
async fn the_guides_and_the_document_are_resources() {
    let server = server();
    for uri in [
        "ps5upload://docs/api",
        "ps5upload://docs/mcp",
        "ps5upload://openapi",
    ] {
        let answer = server.modern("resources/read", json!({"uri": uri})).await;
        let text = answer.body["result"]["contents"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.len() > 1000, "{uri}");
    }
}
