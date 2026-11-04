//! What a client can read without calling a tool: the route list, the automation guide, the known
//! consoles, the jobs, recent events, and one console's status.

use std::collections::VecDeque;
use std::sync::Mutex;

use axum::http::Method;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::jsonrpc::RpcError;
use super::{catalog_routes, tools, Caller, McpState};

const URI_CATALOG: &str = "ps5upload://catalog";
const URI_OPENAPI: &str = "ps5upload://openapi";
const URI_AUTOMATION: &str = "ps5upload://docs/automation";
const URI_API: &str = "ps5upload://docs/api";
const URI_MCP: &str = "ps5upload://docs/mcp";
const URI_CONSOLES: &str = "ps5upload://consoles";
pub(crate) const URI_JOBS: &str = "ps5upload://jobs";
pub(crate) const URI_EVENTS: &str = "ps5upload://events";
const CONSOLE_STATUS_PREFIX: &str = "ps5upload://consoles/";
const CONSOLE_STATUS_SUFFIX: &str = "/status";

// From the repository's root: both Dockerfiles copy these, and `.dockerignore` lets them through.
const AUTOMATION_GUIDE: &str = include_str!("../../../../../AUTOMATION.md");
const API_GUIDE: &str = include_str!("../../../../../API.md");
const MCP_GUIDE: &str = include_str!("../../../../../MCP.md");

/// How many events `ps5upload://events` keeps.
const RING_SIZE: usize = 200;

/// The answer to `resources/list`.
pub(crate) fn list() -> Value {
    json!({
        "resources": [
            {
                "uri": URI_CATALOG,
                "name": "catalog",
                "title": "API route list",
                "description": "Every route of the engine's HTTP API, by method and path: what \
                                api_request can call.",
                "mimeType": "application/json",
            },
            {
                "uri": URI_OPENAPI,
                "name": "openapi",
                "title": "OpenAPI document",
                "description": "The whole API in OpenAPI 3.1, the same as GET /api/openapi.json: \
                                large. The api_describe tool gives one route at a time.",
                "mimeType": "application/json",
            },
            {
                "uri": URI_API,
                "name": "docs_api",
                "title": "API guide",
                "description": "How to reach the API, its guards, errors, long calls, events and \
                                route families.",
                "mimeType": "text/markdown",
            },
            {
                "uri": URI_MCP,
                "name": "docs_mcp",
                "title": "MCP guide",
                "description": "What this server can do, read-only mode, destructive tools and \
                                what needs a page or ffmpeg.",
                "mimeType": "text/markdown",
            },
            {
                "uri": URI_AUTOMATION,
                "name": "docs_automation",
                "title": "Remote Play automation guide",
                "description": "How the Remote Play automation routes work, their errors and \
                                limits, and the live feed.",
                "mimeType": "text/markdown",
            },
            {
                "uri": URI_CONSOLES,
                "name": "consoles",
                "title": "Known consoles",
                "description": "The consoles this engine knows and where it knows them from, and \
                                its default console: the same as the consoles_list tool.",
                "mimeType": "application/json",
            },
            {
                "uri": URI_JOBS,
                "name": "jobs",
                "title": "Jobs",
                "description": "The engine's recent jobs (uploads, downloads, conversions) and \
                                how each stands.",
                "mimeType": "application/json",
            },
            {
                "uri": URI_EVENTS,
                "name": "events",
                "title": "Recent engine events",
                "description": "The last job changes on the engine's event bus, newest last, kept \
                                from this server's first request on. A job's running ticks count \
                                as one entry, which its ending replaces.",
                "mimeType": "application/json",
            },
        ],
    })
}

/// The answer to `resources/templates/list`.
pub(crate) fn templates() -> Value {
    json!({
        "resourceTemplates": [{
            "uriTemplate": "ps5upload://consoles/{addr}/status",
            "name": "console_status",
            "title": "Console status",
            "description": "One console's helper status, by its address: what the consoles_status \
                            tool answers, and why not when it cannot.",
            "mimeType": "application/json",
        }],
    })
}

/// The answer to `resources/read`.
pub(crate) async fn read(
    state: &McpState,
    caller: &Caller,
    params: &Value,
) -> Result<Value, RpcError> {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return Err(RpcError::invalid_params("resources/read needs a \"uri\"."));
    };
    let json_contents = |value: Value| contents(uri, "application/json", value.to_string());
    match uri {
        URI_CATALOG => Ok(json_contents(catalog_routes::as_json())),
        URI_OPENAPI => Ok(contents(
            uri,
            "application/json",
            crate::openapi::document().to_string(),
        )),
        URI_API => Ok(contents(uri, "text/markdown", API_GUIDE.to_string())),
        URI_MCP => Ok(contents(uri, "text/markdown", MCP_GUIDE.to_string())),
        URI_AUTOMATION => Ok(contents(uri, "text/markdown", AUTOMATION_GUIDE.to_string())),
        URI_CONSOLES => Ok(json_contents(tools::consoles::known(state, caller).await)),
        URI_JOBS => {
            let jobs = api_body(state, caller, "/api/jobs", &[]).await;
            Ok(json_contents(jobs))
        }
        URI_EVENTS => Ok(json_contents(state.ring().snapshot())),
        _ => {
            let Some(address) = console_status_address(uri) else {
                return Err(RpcError::invalid_params(format!(
                    "Unknown resource: {uri}. resources/list and resources/templates/list say \
                     what there is."
                )));
            };
            let query = [("addr", address)];
            let status = api_body(state, caller, "/api/ps5/status", &query).await;
            Ok(json_contents(status))
        }
    }
}

fn contents(uri: &str, mime_type: &str, text: String) -> Value {
    json!({ "contents": [{ "uri": uri, "mimeType": mime_type, "text": text }] })
}

/// A route's answer as a resource's content. A console that does not answer is what its status
/// says, not a failure of the read: `{status, body}` says what the route said.
async fn api_body(
    state: &McpState,
    caller: &Caller,
    path: &str,
    query: &[(&str, String)],
) -> Value {
    match state.api_call(caller, Method::GET, path, query, None).await {
        Ok(reply) if reply.is_success() => reply.json().unwrap_or_else(|| json!(reply.text())),
        Ok(reply) => json!({
            "status": reply.status.as_u16(),
            "body": reply.json().unwrap_or_else(|| json!(reply.text())),
        }),
        Err(problem) => json!({ "error": problem }),
    }
}

/// The address in `ps5upload://consoles/{addr}/status`, percent-decoded.
fn console_status_address(uri: &str) -> Option<String> {
    let encoded = uri
        .strip_prefix(CONSOLE_STATUS_PREFIX)?
        .strip_suffix(CONSOLE_STATUS_SUFFIX)?;
    let address = percent_decode(encoded)?;
    let plain = !address.is_empty()
        && address
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'[' | b']'));
    plain.then_some(address)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The last events on the engine's bus.
#[derive(Default)]
pub(crate) struct EventRing {
    inner: Mutex<RingInner>,
}

#[derive(Default)]
struct RingInner {
    entries: VecDeque<Value>,
    next_seq: u64,
    /// Events lost to falling behind the bus.
    missed: u64,
}

impl EventRing {
    fn lock(&self) -> std::sync::MutexGuard<'_, RingInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keeps one event. A running job ticks five times a second, which would push everything else
    /// out within a minute, so a job's latest event replaces its running state before it.
    fn push(&self, raw: &str) {
        let event: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
        let job = event
            .get("job_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut ring = self.lock();
        if let Some(job) = &job {
            ring.entries
                .retain(|entry| running_job(&entry["event"]) != Some(job.as_str()));
        }
        ring.next_seq += 1;
        let entry = json!({ "seq": ring.next_seq, "at_ms": crate::now_ms(), "event": event });
        ring.entries.push_back(entry);
        while ring.entries.len() > RING_SIZE {
            ring.entries.pop_front();
        }
    }

    fn note_missed(&self, count: u64) {
        self.lock().missed += count;
    }

    fn snapshot(&self) -> Value {
        let ring = self.lock();
        json!({
            "events": ring.entries,
            "kept_at_most": RING_SIZE,
            "missed": ring.missed,
        })
    }
}

/// The job an event shows running, if it does.
fn running_job(event: &Value) -> Option<&str> {
    if event.get("job")?.get("status")?.as_str()? != "running" {
        return None;
    }
    event.get("job_id")?.as_str()
}

/// Keeps the ring filled, for as long as the engine's bus lasts.
pub(crate) async fn keep_events(state: McpState, mut events: broadcast::Receiver<String>) {
    loop {
        match events.recv().await {
            Ok(raw) => state.ring().push(&raw),
            Err(broadcast::error::RecvError::Lagged(count)) => state.ring().note_missed(count),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_console_status_uri_names_an_address() {
        let address = |uri| console_status_address(uri);
        assert_eq!(
            address("ps5upload://consoles/192.168.1.100/status").as_deref(),
            Some("192.168.1.100")
        );
        assert_eq!(
            address("ps5upload://consoles/%5B%3A%3A1%5D/status").as_deref(),
            Some("[::1]")
        );
        for bad in [
            "ps5upload://consoles//status",
            "ps5upload://consoles/a/b/status",
            "ps5upload://consoles/a%2Fb/status",
            "ps5upload://consoles/a%zz/status",
            "ps5upload://consoles/a&b/status",
            "ps5upload://consoles/1.2.3.4",
            "ps5upload://consoles",
        ] {
            assert_eq!(address(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_running_jobs_ticks_are_one_entry_and_its_ending_stays() {
        let ring = EventRing::default();
        let tick = |sent: u32| {
            json!({"job_id": "a", "job": {"status": "running", "bytes_sent": sent}}).to_string()
        };
        ring.push(&tick(1));
        ring.push(&json!({"job_id": "b", "job": {"status": "running"}}).to_string());
        ring.push(&tick(2));
        let events = ring.snapshot()["events"].as_array().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"]["job_id"], "b");
        assert_eq!(events[1]["event"]["job"]["bytes_sent"], 2);

        // Its ending replaces its ticks: one entry per job, however long it ran.
        ring.push(&json!({"job_id": "a", "job": {"status": "done"}}).to_string());
        let events = ring.snapshot()["events"].as_array().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1]["event"]["job"]["status"], "done");

        // An ending stays when the same id runs again, and `seq` tells a reader what is new.
        ring.push(&tick(3));
        let events = ring.snapshot()["events"].as_array().unwrap().clone();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1]["event"]["job"]["status"], "done");
        assert!(events[2]["seq"].as_u64() > events[1]["seq"].as_u64());
    }

    #[test]
    fn the_ring_keeps_the_newest_and_counts_what_it_missed() {
        let ring = EventRing::default();
        for i in 0..(RING_SIZE + 5) {
            ring.push(&json!({"job_id": i.to_string(), "job": {"status": "done"}}).to_string());
        }
        ring.note_missed(3);
        let snapshot = ring.snapshot();
        let events = snapshot["events"].as_array().unwrap();
        assert_eq!(events.len(), RING_SIZE);
        assert_eq!(events[0]["event"]["job_id"], "5");
        assert_eq!(snapshot["missed"], 3);
        // Something that is not JSON is kept as text.
        ring.push("plain");
        let snapshot = ring.snapshot();
        assert_eq!(snapshot["events"][RING_SIZE - 1]["event"], "plain");
    }

    #[test]
    fn the_docker_builds_have_the_guides() {
        // `include_str!` above needs these in the image's build context, and only the release's
        // Docker workflow would find out otherwise.
        let ignore = include_str!("../../../../../.dockerignore");
        for file in ["AUTOMATION.md", "API.md", "MCP.md"] {
            assert!(
                ignore.lines().any(|line| line == format!("!{file}")),
                "{file} in .dockerignore"
            );
            for dockerfile in [
                include_str!("../../../../Dockerfile"),
                include_str!("../../../../Dockerfile.webui"),
            ] {
                assert!(
                    dockerfile
                        .lines()
                        .any(|line| line.starts_with("COPY ") && line.contains(file)),
                    "{file} in a Dockerfile"
                );
            }
        }
        assert!(AUTOMATION_GUIDE.contains("Remote Play automation"));
    }

    /// MCP.md lists the tools a client should ask about first. A new one must be added there.
    #[test]
    fn the_guide_names_every_destructive_tool() {
        let listed = tools::list();
        for tool in listed["tools"].as_array().unwrap() {
            if tool["annotations"]["destructiveHint"] == true {
                let name = tool["name"].as_str().unwrap();
                assert!(
                    MCP_GUIDE.contains(&format!("`{name}`")),
                    "{name} is destructive: name it in MCP.md"
                );
            }
        }
    }
}
