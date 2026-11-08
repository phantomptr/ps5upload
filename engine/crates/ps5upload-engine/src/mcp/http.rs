//! The Streamable HTTP transport: `POST /mcp`.
//!
//! Every answer is one JSON object, except `subscriptions/listen`, which is a stream of server-sent
//! events. `GET` answers 405 (no server-initiated stream) and `DELETE` 204 (no session to end): a
//! legacy client tries both.
//!
//! This sits behind the engine's own guards (`lib.rs`): the loopback guard and the browser-origin
//! guard, as for the API. [`origin_guard`] adds what the protocol asks for on top: a browser may
//! only reach this endpoint with the engine addressed as localhost, which is what stops a page
//! that re-pointed its own name at 127.0.0.1 (DNS rebinding). Native clients send no `Origin`.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

use super::dispatch::{self, Era};
use super::jsonrpc::{
    self, Incoming, RpcError, INTERNAL_ERROR, META_SUBSCRIPTION_ID, METHOD_NOT_FOUND,
    MODERN_VERSION, PARSE_ERROR, SUPPORTED_VERSIONS,
};
use super::resources::{URI_EVENTS, URI_JOBS};
use super::{access, Caller, McpState};

/// The same cap the rest of the API has.
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
/// A comment line on an idle stream, so proxies and clients keep it open.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// A subscribed resource says it changed at most twice a second: a transfer ticks five times.
const UPDATE_EVERY: Duration = Duration::from_millis(500);

const HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";
const HEADER_METHOD: &str = "mcp-method";
const HEADER_NAME: &str = "mcp-name";
const HEADER_SESSION: &str = "mcp-session-id";

/// The MCP routes, and the owner's switch for what agents may do. That switch is mounted here,
/// after the engine took the clone of its API that tools call, so no tool can reach it (`access.rs`
/// says what that does and does not stop). It is not behind [`origin_guard`]: the app's web UI may
/// be opened by another name than localhost, as the rest of the API may.
pub(crate) fn router(state: McpState) -> Router {
    Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp).delete(delete_mcp))
        .route_layer(middleware::from_fn(origin_guard))
        .route("/api/mcp/access", get(access::get).put(access::put))
        .with_state(state)
}

/// Refuses a browser the protocol says to refuse (see the module comment).
async fn origin_guard(req: Request, next: Next) -> Response {
    if browser_allowed(req.headers()) {
        return next.run(req).await;
    }
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("(unreadable)");
    crate::log_warn!("mcp: refusing a browser request from origin {origin}");
    let error = RpcError::invalid_request(format!(
        "This MCP server does not answer pages from {origin}: browsers may reach it only from \
         this computer, with the engine addressed as localhost."
    ));
    rpc_reply(StatusCode::FORBIDDEN, None, &error)
}

fn browser_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    crate::browser_origin_allows(origin, host) && crate::vinput_api::host_is_local(headers)
}

async fn get_mcp() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST, DELETE")],
        "This MCP server has no server-initiated stream: POST JSON-RPC messages to this URL.",
    )
        .into_response()
}

async fn delete_mcp() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn post_mcp(State(state): State<McpState>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let caller = Caller::of(&parts);
    let Ok(bytes) = to_bytes(body, MAX_REQUEST_BYTES).await else {
        let error = RpcError::invalid_request(format!(
            "That request could not be read, or is over {} MiB.",
            MAX_REQUEST_BYTES / (1024 * 1024)
        ));
        return rpc_reply(StatusCode::BAD_REQUEST, None, &error);
    };
    state.start_event_ring();
    let message: Value = match serde_json::from_slice(&bytes) {
        Ok(message) => message,
        Err(e) => {
            let error = RpcError::new(PARSE_ERROR, format!("That is not valid JSON: {e}"));
            return rpc_reply(StatusCode::BAD_REQUEST, None, &error);
        }
    };
    match &message {
        Value::Array(items) => batch(&state, &caller, items).await,
        _ => single(&state, &caller, &parts.headers, &message).await,
    }
}

/// One JSON-RPC message.
async fn single(
    state: &McpState,
    caller: &Caller,
    headers: &HeaderMap,
    message: &Value,
) -> Response {
    let (id, method, params) = match jsonrpc::classify(message) {
        Ok(Incoming::Request { id, method, params }) => (id, method, params),
        Ok(Incoming::Notification | Incoming::Response) => {
            return StatusCode::ACCEPTED.into_response()
        }
        Err((id, error)) => return rpc_reply(StatusCode::BAD_REQUEST, id, &error),
    };
    let era = match admit(headers, method, params) {
        Ok(era) => era,
        Err(error) => return rpc_reply(StatusCode::BAD_REQUEST, Some(id), &error),
    };
    if era == Era::Modern && method == "subscriptions/listen" {
        return listen(state, id, params);
    }
    match dispatch::call(state, caller, era, method, params).await {
        Ok(result) => {
            let mut response = Json(jsonrpc::success(id, result)).into_response();
            if method == "initialize" {
                // Clients that expect a session get one; nothing is kept for it, and later
                // requests are answered with or without it.
                let session = uuid::Uuid::new_v4().to_string();
                if let Ok(value) = HeaderValue::from_str(&session) {
                    response.headers_mut().insert(HEADER_SESSION, value);
                }
            }
            response
        }
        Err(error) => rpc_reply(status_of(era, &error), Some(id), &error),
    }
}

/// A batch of messages (the 2025-03-26 revision has them; no later one does). Answered in order,
/// one after the other. Notifications and answers among them get no entry.
async fn batch(state: &McpState, caller: &Caller, items: &[Value]) -> Response {
    if items.is_empty() {
        let error = RpcError::invalid_request("An empty batch is not a request.");
        return rpc_reply(StatusCode::BAD_REQUEST, None, &error);
    }
    let mut replies = Vec::new();
    for item in items {
        match jsonrpc::classify(item) {
            Ok(Incoming::Request { id, method, params }) => {
                let answer = if jsonrpc::meta_protocol_version(params).is_some() {
                    Err(RpcError::invalid_request(
                        "Batches are not part of the modern protocol: send one request at a time.",
                    ))
                } else {
                    dispatch::call(state, caller, Era::Legacy, method, params).await
                };
                replies.push(match answer {
                    Ok(result) => jsonrpc::success(id, result),
                    Err(error) => jsonrpc::failure(Some(id), &error),
                });
            }
            Ok(Incoming::Notification | Incoming::Response) => {}
            Err((id, error)) => replies.push(jsonrpc::failure(id, &error)),
        }
    }
    if replies.is_empty() {
        return StatusCode::ACCEPTED.into_response();
    }
    Json(Value::Array(replies)).into_response()
}

/// Reads a request's era and checks what that era requires of it on HTTP.
fn admit(headers: &HeaderMap, method: &str, params: &Value) -> Result<Era, RpcError> {
    let header_version = text_header(headers, HEADER_PROTOCOL_VERSION);
    let Some(version) = jsonrpc::meta_protocol_version(params) else {
        return admit_legacy(header_version, method);
    };
    let Some(version) = version.as_str() else {
        return Err(RpcError::invalid_params(
            "The protocol version in params._meta must be a string.",
        ));
    };
    if header_version != Some(version) {
        return Err(mismatch("MCP-Protocol-Version", header_version, version));
    }
    let header_method = text_header(headers, HEADER_METHOD);
    if header_method != Some(method) {
        return Err(mismatch("Mcp-Method", header_method, method));
    }
    if let Some(subject) = jsonrpc::named_subject(method, params) {
        let header_name = text_header(headers, HEADER_NAME);
        let decoded = header_name.and_then(jsonrpc::decode_header_name);
        if decoded.as_deref() != Some(subject) {
            return Err(mismatch("Mcp-Name", header_name, subject));
        }
    }
    dispatch::check_modern_meta(params)?;
    Ok(Era::Modern)
}

fn admit_legacy(header_version: Option<&str>, method: &str) -> Result<Era, RpcError> {
    if let Some(version) = header_version {
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(RpcError::unsupported_version(version));
        }
        if version == MODERN_VERSION && method != "initialize" {
            return Err(RpcError::invalid_params(format!(
                "A {MODERN_VERSION} request needs params._meta with its protocol version and \
                 client capabilities."
            )));
        }
    }
    if matches!(method, "server/discover" | "subscriptions/listen") {
        return Err(RpcError::invalid_params(format!(
            "{method} needs params._meta with the protocol version and client capabilities."
        )));
    }
    Ok(Era::Legacy)
}

fn text_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn mismatch(header: &str, got: Option<&str>, want: &str) -> RpcError {
    RpcError::header_mismatch(match got {
        Some(got) => format!("The {header} header says {got:?}, but the request says {want:?}."),
        None => format!("The {header} header is missing; this request needs it to say {want:?}."),
    })
}

/// A JSON-RPC error's HTTP status. A modern client reads them from the status too. A legacy one
/// expects a 200 for an error that answers a request it sent.
fn status_of(era: Era, error: &RpcError) -> StatusCode {
    if era == Era::Legacy {
        return StatusCode::OK;
    }
    match error.code {
        METHOD_NOT_FOUND => StatusCode::NOT_FOUND,
        INTERNAL_ERROR => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    }
}

fn rpc_reply(status: StatusCode, id: Option<&Value>, error: &RpcError) -> Response {
    (status, Json(jsonrpc::failure(id, error))).into_response()
}

/// `subscriptions/listen`: a stream that says when a subscribed resource changed, until the client
/// goes away (the stream is then dropped, and everything in it with it) or the engine stops.
fn listen(state: &McpState, id: &Value, params: &Value) -> Response {
    let asked = params
        .get("notifications")
        .and_then(|n| n.get("resourceSubscriptions"))
        .and_then(Value::as_array);
    let uris: Vec<&'static str> = [URI_EVENTS, URI_JOBS]
        .into_iter()
        .filter(|uri| asked.is_some_and(|list| list.iter().any(|u| u.as_str() == Some(*uri))))
        .collect();
    // The tool list changes when the owner switches agents between looking and changing things.
    let tools_list = params
        .get("notifications")
        .and_then(|n| n.get("toolsListChanged"))
        .and_then(Value::as_bool)
        == Some(true);
    let mut honoured = json!({ "resourceSubscriptions": uris });
    if tools_list {
        honoured["toolsListChanged"] = json!(true);
    }
    let acknowledged = json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": {
            "_meta": { META_SUBSCRIPTION_ID: id },
            "notifications": honoured,
        },
    });
    let access = tools_list.then(|| {
        let mut changes = state.access().subscribe();
        changes.mark_unchanged();
        changes
    });
    let stream_state = Listen {
        id: id.clone(),
        frames: VecDeque::from([acknowledged]),
        // No subscription this server can honour: the stream only stays open.
        events: (!uris.is_empty()).then(|| state.subscribe_events()),
        uris,
        access,
        shutdown: state.subscribe_shutdown(),
        changed: false,
        last_update: None,
        ended: false,
    };
    let stream = futures_util::stream::unfold(stream_state, |mut listen| async move {
        let frame = listen.next_frame().await?;
        let event = Event::default().data(frame.to_string());
        Some((Ok::<_, Infallible>(event), listen))
    });
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

/// One open `subscriptions/listen` stream.
struct Listen {
    id: Value,
    /// Ready to send, in order.
    frames: VecDeque<Value>,
    uris: Vec<&'static str>,
    events: Option<broadcast::Receiver<String>>,
    /// Whether agents may only look, when the client asked to hear the tool list change.
    access: Option<watch::Receiver<bool>>,
    shutdown: watch::Receiver<bool>,
    /// Something changed since the last update went out.
    changed: bool,
    last_update: Option<Instant>,
    ended: bool,
}

impl Listen {
    /// The next message of the stream. `None` ends it.
    async fn next_frame(&mut self) -> Option<Value> {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                return Some(frame);
            }
            if self.ended {
                return None;
            }
            if *self.shutdown.borrow() {
                self.end();
                continue;
            }
            // Immediately for the first change after a quiet spell; a burst then waits its turn.
            let update_at = self
                .last_update
                .map_or_else(Instant::now, |last| last + UPDATE_EVERY);
            tokio::select! {
                // Ending when the sender is gone too: nothing can tell this stream to stop then.
                changed = self.shutdown.changed() => {
                    if changed.is_err() {
                        self.end();
                    }
                }
                changed = next_access_change(&mut self.access) => {
                    if changed {
                        self.frames.push_back(json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/tools/list_changed",
                            "params": { "_meta": { META_SUBSCRIPTION_ID: self.id } },
                        }));
                    } else {
                        self.access = None;
                    }
                }
                changed = next_event(&mut self.events) => {
                    if changed {
                        self.changed = true;
                    } else {
                        // The bus is gone: nothing more will change. The stream stays until the
                        // client leaves or the engine stops, as with nothing subscribed.
                        self.events = None;
                    }
                }
                _ = tokio::time::sleep_until(update_at), if self.changed => {
                    self.changed = false;
                    self.last_update = Some(Instant::now());
                    for uri in &self.uris {
                        self.frames.push_back(json!({
                            "jsonrpc": "2.0",
                            "method": "notifications/resources/updated",
                            "params": {
                                "_meta": { META_SUBSCRIPTION_ID: self.id },
                                "uri": uri,
                            },
                        }));
                    }
                }
            }
        }
    }
}

impl Listen {
    /// The engine stops: the final response, then the end of the stream.
    fn end(&mut self) {
        self.ended = true;
        self.frames.push_back(jsonrpc::success(
            &self.id,
            json!({
                "resultType": "complete",
                "_meta": { META_SUBSCRIPTION_ID: self.id },
            }),
        ));
    }
}

/// Waits for the owner to switch what agents may do: `true`, it changed; `false`, nothing can
/// change it any more. With nothing asked for, this waits forever.
async fn next_access_change(access: &mut Option<watch::Receiver<bool>>) -> bool {
    let Some(access) = access else {
        return std::future::pending().await;
    };
    let changed = access.changed().await.is_ok();
    if changed {
        access.mark_unchanged();
    }
    changed
}

/// Waits for something on the event bus: `true`, something changed (falling behind the bus counts);
/// `false`, the bus closed. With no bus, this waits forever.
async fn next_event(events: &mut Option<broadcast::Receiver<String>>) -> bool {
    let Some(events) = events else {
        return std::future::pending().await;
    };
    !matches!(
        events.recv().await,
        Err(broadcast::error::RecvError::Closed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut out = HeaderMap::new();
        for &(name, value) in pairs {
            out.insert(name, HeaderValue::from_static(value));
        }
        out
    }

    #[test]
    fn native_clients_send_no_origin_and_pass() {
        assert!(browser_allowed(&headers(&[("host", "nas.local:19113")])));
    }

    #[test]
    fn a_local_page_may_reach_a_local_engine() {
        for origin in [
            "http://localhost:6274",
            "http://127.0.0.1:5173",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(
                browser_allowed(&headers(&[("host", "127.0.0.1:19113"), ("origin", origin)])),
                "{origin}"
            );
        }
    }

    #[test]
    fn a_foreign_page_is_refused() {
        for origin in ["https://evil.example", "null", "http://192.168.1.5:19113"] {
            assert!(
                !browser_allowed(&headers(&[("host", "127.0.0.1:19113"), ("origin", origin)])),
                "{origin}"
            );
        }
    }

    #[test]
    fn a_page_that_re_pointed_its_own_name_at_this_computer_is_refused() {
        // Same origin to the browser, and the engine's general rule allows same-origin: only its
        // own name gives it away (see `vinput_api::host_is_local`).
        assert!(crate::browser_origin_allows(
            "http://evil.example:19113",
            "evil.example:19113"
        ));
        assert!(!browser_allowed(&headers(&[
            ("host", "evil.example:19113"),
            ("origin", "http://evil.example:19113"),
        ])));
    }

    #[test]
    fn a_legacy_request_may_name_a_legacy_version_or_none() {
        assert_eq!(admit_legacy(None, "tools/list"), Ok(Era::Legacy));
        assert_eq!(
            admit_legacy(Some("2025-06-18"), "tools/list"),
            Ok(Era::Legacy)
        );
        assert_eq!(
            admit_legacy(Some("1999-01-01"), "tools/list")
                .unwrap_err()
                .code,
            jsonrpc::UNSUPPORTED_VERSION
        );
    }

    #[test]
    fn a_modern_header_needs_modern_metadata_except_on_the_legacy_handshake() {
        assert_eq!(
            admit_legacy(Some(MODERN_VERSION), "tools/list")
                .unwrap_err()
                .code,
            jsonrpc::INVALID_PARAMS
        );
        assert_eq!(
            admit_legacy(Some(MODERN_VERSION), "initialize"),
            Ok(Era::Legacy)
        );
        assert_eq!(
            admit_legacy(None, "server/discover").unwrap_err().code,
            jsonrpc::INVALID_PARAMS
        );
    }

    #[test]
    fn statuses_differ_only_for_the_modern_era() {
        let not_found = RpcError::method_not_found("x");
        assert_eq!(status_of(Era::Modern, &not_found), StatusCode::NOT_FOUND);
        assert_eq!(status_of(Era::Legacy, &not_found), StatusCode::OK);
        let params = RpcError::invalid_params("x");
        assert_eq!(status_of(Era::Modern, &params), StatusCode::BAD_REQUEST);
        assert_eq!(status_of(Era::Legacy, &params), StatusCode::OK);
        let internal = RpcError::new(INTERNAL_ERROR, "x");
        assert_eq!(
            status_of(Era::Modern, &internal),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
