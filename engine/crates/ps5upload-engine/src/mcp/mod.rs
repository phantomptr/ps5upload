//! The engine's MCP (Model Context Protocol) server: agents drive PS5Upload through it, with
//! nothing extra to install, wherever the engine runs (the desktop app's sidecar, the Docker web
//! UI).
//!
//! * `http.rs`: `POST /mcp`, behind the same loopback and browser-origin guards as the API.
//! * `stdio.rs`: `ps5upload-engine mcp`, a pure bridge from a client's stdio to that endpoint.
//!   It never runs an engine of its own: the running engine is the one that owns the consoles.
//! * `dispatch.rs`: the protocol's methods, for both eras of the protocol (see below).
//! * `tools/`, `resources.rs`, `prompts.rs`: what the methods offer.
//!
//! The server is stateless. Two eras of the protocol are answered side by side, per request: the
//! modern revision (2026-07-28: every request carries its version and capabilities in
//! `params._meta`, and its method and target in HTTP headers) and the initialize-based legacy
//! revisions (2024-11-05 to 2025-11-25).
//!
//! Tools call the engine's HTTP API in-process ([`McpState::api_call`]): a clone of the router,
//! so a tool and a script see the same behaviour and the same errors, and nothing is written
//! twice. A call carries the peer and `Host` of the request that asked for it, so the routes that
//! answer to this computer alone (`AUTOMATION.md`) judge the real caller.

pub(crate) mod access;
mod catalog_routes;
mod dispatch;
mod http;
mod jsonrpc;
mod prompts;
mod resources;
pub(crate) mod stdio;
mod tools;

#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::Duration;

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tokio::sync::{broadcast, watch};
use tower::ServiceExt as _;

pub(crate) use http::router;

/// Largest answer of the API a tool takes in. Above this it says so: the route is for a script.
const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;
/// How long reading an answer's body may take once the route has answered. The handlers answer
/// when their work is done, so this only ever waits on a body that is sent slowly.
const REPLY_BODY_WAIT: Duration = Duration::from_secs(300);

/// Everything the MCP routes share. Cheap to clone.
#[derive(Clone)]
pub(crate) struct McpState(Arc<Inner>);

struct Inner {
    /// The engine's API, with its state: what tools call.
    api: Router,
    events_tx: broadcast::Sender<String>,
    /// The console the API falls back to when a read names none.
    default_console: String,
    /// Whether agents may change things (`access.rs`): read when each call runs.
    access: access::AgentAccess,
    ring: resources::EventRing,
    ring_started: Once,
    ops: tools::files::Ops,
    shutdown: watch::Sender<bool>,
}

impl McpState {
    /// `api` is the engine's API router, already holding its state; `default_console` the
    /// console it falls back to.
    pub(crate) fn new(
        api: Router,
        events_tx: broadcast::Sender<String>,
        default_console: String,
    ) -> Self {
        Self::build(
            api,
            events_tx,
            default_console,
            access::AgentAccess::from_environment(),
        )
    }

    fn build(
        api: Router,
        events_tx: broadcast::Sender<String>,
        default_console: String,
        access: access::AgentAccess,
    ) -> Self {
        Self(Arc::new(Inner {
            api,
            events_tx,
            default_console,
            access,
            ring: resources::EventRing::default(),
            ring_started: Once::new(),
            ops: tools::files::Ops::default(),
            shutdown: watch::channel(false).0,
        }))
    }

    /// Agents may only look: anything that can change something is refused. Read at call time:
    /// the owner can flip it in the app while the engine runs.
    pub(crate) fn read_only(&self) -> bool {
        self.0.access.read_only()
    }

    pub(crate) fn access(&self) -> &access::AgentAccess {
        &self.0.access
    }

    pub(crate) fn default_console(&self) -> &str {
        &self.0.default_console
    }

    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.0.events_tx.subscribe()
    }

    pub(crate) fn ring(&self) -> &resources::EventRing {
        &self.0.ring
    }

    pub(crate) fn ops(&self) -> &tools::files::Ops {
        &self.0.ops
    }

    /// Starts keeping recent events, on the first request this server gets. Not when the engine
    /// starts: the transfer ticker skips building an event when nobody listens, and an engine
    /// nobody drives through MCP should keep that.
    pub(crate) fn start_event_ring(&self) {
        self.0.ring_started.call_once(|| {
            let state = self.clone();
            let events = self.subscribe_events();
            tokio::spawn(resources::keep_events(state, events));
        });
    }

    /// Tells open `subscriptions/listen` streams to end: each would otherwise hold the engine's
    /// graceful shutdown up for as long as its client stays.
    pub(crate) fn begin_shutdown(&self) {
        self.0.shutdown.send_replace(true);
    }

    pub(crate) fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.0.shutdown.subscribe()
    }

    /// One call of the engine's API, in-process. `Err` is a sentence for the user: the call could
    /// not be made, or its answer cannot be carried by a tool. A route's own failure (any status)
    /// is an `Ok` reply.
    pub(crate) async fn api_call(
        &self,
        caller: &Caller,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<ApiReply, String> {
        let label = format!("{method} {path}");
        let method_name = method.as_str().to_string();
        // Every in-process call passes here (curated tools, resources, api_request): none may reach
        // the switch that decides what agents may do, however the path is written. The route is
        // also mounted outside the router this calls, so it would not be found anyway.
        if catalog_routes::reaches_agent_access(path) {
            return Err(format!(
                "{label} is refused: whether agents can change things is the owner's to decide, \
                 in the app under MCP & API › Connect an agent."
            ));
        }
        if catalog_routes::reaches_a_key_document(path, query, body) {
            return Err(format!(
                "{label} is refused: that console document holds the keys that sign in to \
                 consoles, which stay with the app."
            ));
        }
        let mut builder = Request::builder()
            .method(method)
            .uri(with_query(path, query))
            .header(header::HOST, caller.host.clone());
        let request_body = match body {
            Some(json) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let mut request = builder
            .body(request_body)
            .map_err(|e| format!("{label} cannot be asked for: {e}"))?;
        request.extensions_mut().insert(ConnectInfo(caller.peer));

        let response = self
            .0
            .api
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|never| match never {});

        let (parts, body) = response.into_parts();
        let content_type = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if answers_a_stream(&content_type) {
            return Err(format!(
                "{label} answers a live stream ({content_type}), which a tool call cannot carry: \
                 read that URL directly from the engine."
            ));
        }
        let body =
            match tokio::time::timeout(REPLY_BODY_WAIT, to_bytes(body, MAX_REPLY_BYTES)).await {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(_)) => {
                    return Err(format!(
                    "The answer to {label} is too large for a tool call (over {} MiB): read that \
                     URL directly from the engine.",
                    MAX_REPLY_BYTES / (1024 * 1024)
                ))
                }
                Err(_) => {
                    return Err(format!(
                        "The answer to {label} did not finish within {} s.",
                        REPLY_BODY_WAIT.as_secs()
                    ))
                }
            };
        // What agents may not read, taken out here where every in-process call passes (curated
        // tools, resources, api_request): a tailnet sign-in link, and the live invites in the passive
        // reads (catalog_routes::redact_for_agents says why). The app's own HTTP calls never come
        // this way and get the whole answer.
        let body = if catalog_routes::may_hold_agent_secrets(&method_name, path) {
            match serde_json::from_slice::<Value>(&body) {
                Ok(mut json) => {
                    if catalog_routes::redact_for_agents(&method_name, path, &mut json) {
                        Bytes::from(json.to_string())
                    } else {
                        body
                    }
                }
                Err(_) => body,
            }
        } else {
            body
        };
        Ok(ApiReply {
            status: parts.status,
            content_type,
            headers: parts.headers,
            body,
        })
    }
}

/// An answer that never ends, or that only makes sense as one.
fn answers_a_stream(content_type: &str) -> bool {
    let kind = content_type.trim_start().to_ascii_lowercase();
    kind.starts_with("text/event-stream") || kind.starts_with("video/")
}

/// Who a call of the API is made for. Handlers that answer to this computer alone read these.
#[derive(Clone)]
pub(crate) struct Caller {
    peer: SocketAddr,
    host: HeaderValue,
}

impl Caller {
    /// This computer, as a local script addresses it. For a call no connection stands behind.
    pub(crate) fn local() -> Self {
        Self {
            peer: SocketAddr::from(([127, 0, 0, 1], 1)),
            host: HeaderValue::from_static("127.0.0.1"),
        }
    }

    /// The caller behind an HTTP request. Its real peer and `Host` go along, not a local
    /// stand-in: a peer let in by `PS5UPLOAD_ALLOW_IP`, or a page that re-pointed its own name at
    /// 127.0.0.1, must not reach the routes that answer to this computer alone through a tool.
    pub(crate) fn of(parts: &Parts) -> Self {
        let local = Self::local();
        Self {
            peer: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map_or(local.peer, |info| info.0),
            host: parts
                .headers
                .get(header::HOST)
                .cloned()
                .unwrap_or(local.host),
        }
    }
}

/// A route's answer, read in full.
#[derive(Debug)]
pub(crate) struct ApiReply {
    pub status: StatusCode,
    pub content_type: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl ApiReply {
    pub(crate) fn is_success(&self) -> bool {
        self.status.is_success()
    }

    pub(crate) fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `path?a=1&b=2`, values percent-encoded.
fn with_query(path: &str, query: &[(&str, String)]) -> String {
    let mut uri = path.to_string();
    for (i, (key, value)) in query.iter().enumerate() {
        uri.push(if i == 0 { '?' } else { '&' });
        uri.push_str(&encode_component(key));
        uri.push('=');
        uri.push_str(&encode_component(value));
    }
    uri
}

/// A value's percent-encoded form, for a query or one path segment: nothing in it can end the
/// segment or start another.
pub(crate) fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn query_values_cannot_change_the_request() {
        assert_eq!(
            with_query(
                "/api/ps5/list-dir",
                &[("path", "/data/a b&c=d".into()), ("addr", "[::1]".into())]
            ),
            "/api/ps5/list-dir?path=%2Fdata%2Fa%20b%26c%3Dd&addr=%5B%3A%3A1%5D"
        );
        assert_eq!(with_query("/api/jobs", &[]), "/api/jobs");
        assert_eq!(encode_component("a/../b?c"), "a%2F..%2Fb%3Fc");
        assert_eq!(encode_component("ゲ"), "%E3%82%B2");
    }

    #[test]
    fn a_live_stream_is_recognised_by_its_type() {
        assert!(answers_a_stream("text/event-stream"));
        assert!(answers_a_stream("video/mp2t"));
        assert!(!answers_a_stream("application/json"));
        assert!(!answers_a_stream("image/png"));
        assert!(!answers_a_stream(""));
    }
}
