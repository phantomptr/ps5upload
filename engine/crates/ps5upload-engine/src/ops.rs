//! One way to run any slow call and watch it: `Prefer: respond-async` on an `/api/*` POST.
//!
//! The layer runs the route on a task of its own and answers `202 {"op_id": "..."}` at once.
//! `GET /api/ops/{id}` then says `{status: running|done, http_status, body}`, the answer the route
//! would have given. So no reverse proxy or client timeout can cut a long call off, and nothing
//! about a route changes unless a caller asks.
//!
//! `POST /api/ops/{id}/cancel` uses the route's own cancel where it has one: console file
//! operations get a server-assigned `op_id` added to their body (unless the caller sent one), so
//! they can be stopped. For any other route it stops waiting and says the work may still finish;
//! a console call cannot be recalled.
//!
//! `GET /api/ops` also lists transfer jobs (`job:<id>`) and installs (`install:<id>`), and the
//! status and cancel routes take those ids, so one poller works for all three.

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{MatchedPath, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::task::AbortHandle;

use crate::api_errors::{error_body, request_path};
use crate::event_bus::bus;
use crate::{json_err, JobState};

/// At most this many operations are remembered; the oldest finished ones go first.
const MAX_OPS: usize = 256;
/// A finished operation is kept this long.
const KEEP_FINISHED: Duration = Duration::from_secs(3600);
/// A request bigger than this is refused with `Prefer: respond-async`: the body has to be held
/// while the task runs. The global request limit is the same; only an upload route goes higher,
/// and those report their own progress.
const MAX_REQUEST_BODY: usize = 64 * 1024 * 1024;
/// One answer kept in full up to this size.
const MAX_RESPONSE_BODY: usize = 4 * 1024 * 1024;
/// All kept answers together; the oldest finished operations go first past this.
const MAX_TOTAL_BODY: usize = 64 * 1024 * 1024;

/// Routes that take `op_id` and whose own cancel is `/api/ps5/fs/op-cancel`.
const FS_OP_ROUTES: &[&str] = &[
    "/api/ps5/fs/delete",
    "/api/ps5/fs/copy",
    "/api/ps5/fs/move",
    "/api/ps5/fs/unpack",
    "/api/ps5/fs/extract",
    "/api/ps5/dump",
];

/// The route's own way to stop.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FsOp {
    addr: Option<String>,
    op_id: u64,
}

#[derive(Debug)]
struct Op {
    id: String,
    method: String,
    path: String,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
    finished: Option<Instant>,
    http_status: Option<u16>,
    body: Value,
    body_truncated: bool,
    /// Bytes `body` holds, for the total limit.
    body_len: usize,
    abandoned: bool,
    fs_op: Option<FsOp>,
    abort: Option<AbortHandle>,
}

impl Op {
    fn status(&self) -> &'static str {
        if self.finished.is_some() {
            "done"
        } else {
            "running"
        }
    }

    fn summary(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "kind": "op",
            "status": self.status(),
            "http_status": self.http_status,
            "method": self.method,
            "path": self.path,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
        });
        if self.abandoned {
            v["abandoned"] = json!(true);
        }
        if let Some(f) = &self.fs_op {
            // The console's own progress: GET /api/ps5/fs/op-status?op_id=&addr=
            v["fs_op_id"] = json!(f.op_id);
        }
        v
    }

    fn full(&self) -> Value {
        let mut v = self.summary();
        v["body"] = self.body.clone();
        if self.body_truncated {
            v["body_truncated"] = json!(true);
        }
        v
    }
}

/// The operations this engine remembers.
#[derive(Default)]
pub(crate) struct OpRegistry {
    ops: Mutex<HashMap<String, Op>>,
}

fn new_op_id() -> String {
    format!("op-{}", uuid::Uuid::new_v4().simple())
}

impl OpRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Op>> {
        self.ops.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Room for one more, or false when every slot is still running.
    fn insert(&self, op: Op) -> bool {
        let mut g = self.lock();
        prune(&mut g, Instant::now());
        if g.len() >= MAX_OPS {
            return false;
        }
        g.insert(op.id.clone(), op);
        true
    }

    fn set_abort(&self, id: &str, abort: AbortHandle) {
        if let Some(op) = self.lock().get_mut(id) {
            op.abort = Some(abort);
        }
    }

    /// Records the answer. An operation already marked done (abandoned) keeps its record.
    fn finish(&self, id: &str, status: StatusCode, body: Value, truncated: bool, len: usize) {
        let mut g = self.lock();
        let Some(op) = g.get_mut(id) else { return };
        if op.finished.is_some() {
            return;
        }
        op.finished = Some(Instant::now());
        op.finished_at_ms = Some(ps5upload_core::events::now_ms());
        op.http_status = Some(status.as_u16());
        op.body = body;
        op.body_truncated = truncated;
        op.body_len = len;
        op.abort = None;
        let event = op.summary();
        prune(&mut g, Instant::now());
        drop(g);
        bus().publish_json("op", &event);
    }

    fn get(&self, id: &str) -> Option<Value> {
        self.lock().get(id).map(Op::full)
    }

    fn list(&self) -> Vec<(u64, Value)> {
        let mut g = self.lock();
        prune(&mut g, Instant::now());
        g.values().map(|o| (o.started_at_ms, o.summary())).collect()
    }
}

/// Finished operations past their time, then the oldest finished ones past the count or the
/// total size. A running one is never dropped: nothing else knows about it.
fn prune(ops: &mut HashMap<String, Op>, now: Instant) {
    ops.retain(|_, o| {
        o.finished
            .is_none_or(|t| now.saturating_duration_since(t) < KEEP_FINISHED)
    });
    let mut total: usize = ops.values().map(|o| o.body_len).sum();
    if ops.len() < MAX_OPS && total <= MAX_TOTAL_BODY {
        return;
    }
    let mut done: Vec<(Instant, String, usize)> = ops
        .values()
        .filter_map(|o| o.finished.map(|t| (t, o.id.clone(), o.body_len)))
        .collect();
    done.sort();
    for (_, id, len) in done {
        if ops.len() < MAX_OPS && total <= MAX_TOTAL_BODY {
            break;
        }
        ops.remove(&id);
        total -= len;
    }
}

/// `Prefer: respond-async`, among any other preferences (RFC 7240: comma-separated, each with
/// optional `;` parameters, tokens not case-sensitive).
fn prefers_async(headers: &HeaderMap) -> bool {
    headers
        .get_all("prefer")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|p| {
            p.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("respond-async")
        })
}

/// An id for a console file operation that goes out in `fs_op_id`, so it stays under 2^53: a
/// JavaScript client reads a bigger JSON number wrongly and would then ask about another operation.
fn next_readable_fs_op_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new((1 << 52) + 1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// For a route with its own cancel: the body with an `op_id` (the caller's, or a new one), and
/// the console and id to cancel with. A body that does not parse goes through as it is, for the
/// route to refuse it.
fn prepare_fs_op(path: &str, body: Bytes) -> (Bytes, Option<FsOp>) {
    if !FS_OP_ROUTES.contains(&path) {
        return (body, None);
    }
    let Ok(Value::Object(mut map)) = serde_json::from_slice::<Value>(&body) else {
        return (body, None);
    };
    let op_id = match map.get("op_id").and_then(Value::as_u64) {
        Some(id) if id != 0 => id,
        _ => {
            let id = next_readable_fs_op_id();
            map.insert("op_id".into(), json!(id));
            id
        }
    };
    let addr = ["addr", "console", "host"]
        .iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))
        .map(str::to_string);
    let body = serde_json::to_vec(&Value::Object(map))
        .map(Bytes::from)
        .unwrap_or(body);
    (body, Some(FsOp { addr, op_id }))
}

/// An answer as `{status, body}` holds it: its JSON, or its text.
async fn read_answer(resp: Response) -> (StatusCode, Value, bool, usize) {
    let resp = crate::api_errors::normalize(resp).await;
    let status = resp.status();
    match to_bytes(resp.into_body(), MAX_RESPONSE_BODY).await {
        Ok(b) => {
            let body = match serde_json::from_slice::<Value>(&b) {
                Ok(v) => v,
                Err(_) if b.is_empty() => Value::Null,
                Err(_) => Value::String(String::from_utf8_lossy(&b).into_owned()),
            };
            (status, body, false, b.len())
        }
        Err(_) => (status, Value::Null, true, 0),
    }
}

/// The layer: see the module comment. Inside the guards, so a refused caller starts nothing, and
/// outside the request log, so the log shows the route's real answer.
pub(crate) async fn respond_async(
    State(reg): State<Arc<OpRegistry>>,
    req: Request,
    next: Next,
) -> Response {
    let path = request_path(&req);
    let eligible = req.method() == Method::POST
        && prefers_async(req.headers())
        && crate::api_errors::is_api_path(&path)
        && path != "/api/ops"
        && !path.starts_with("/api/ops/")
        // No route: the 404 belongs in this answer.
        && req.extensions().get::<MatchedPath>().is_some();
    if !eligible {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    // Where the status route is for this client: under the engine's URL prefix, if any.
    let prefix = parts
        .uri
        .path()
        .strip_suffix(path.as_str())
        .unwrap_or("")
        .to_string();
    let body = match to_bytes(body, MAX_REQUEST_BODY).await {
        Ok(b) => b,
        Err(_) => {
            let status = StatusCode::PAYLOAD_TOO_LARGE;
            return (
                status,
                Json(error_body(
                    status,
                    "this body is too big to run in the background; send it without \
                     Prefer: respond-async",
                )),
            )
                .into_response();
        }
    };
    let (body, fs_op) = prepare_fs_op(&path, body);
    let id = new_op_id();
    let op = Op {
        id: id.clone(),
        method: parts.method.to_string(),
        path: path.clone(),
        started_at_ms: ps5upload_core::events::now_ms(),
        finished_at_ms: None,
        finished: None,
        http_status: None,
        body: Value::Null,
        body_truncated: false,
        body_len: 0,
        abandoned: false,
        fs_op,
        abort: None,
    };
    let started = op.summary();
    if !reg.insert(op) {
        let status = StatusCode::TOO_MANY_REQUESTS;
        return (
            status,
            Json(error_body(
                status,
                "too many operations are running; wait for one to finish",
            )),
        )
            .into_response();
    }
    bus().publish_json("op", &started);

    let req = Request::from_parts(parts, Body::from(body));
    let task = tokio::spawn(async move { read_answer(next.run(req).await).await });
    reg.set_abort(&id, task.abort_handle());
    let (watcher_reg, watcher_id) = (reg.clone(), id.clone());
    // A separate task, so a route that panics still ends as an answer.
    tokio::spawn(async move {
        match task.await {
            Ok((status, body, truncated, len)) => {
                watcher_reg.finish(&watcher_id, status, body, truncated, len)
            }
            // Aborted: the cancel already recorded it.
            Err(e) if e.is_cancelled() => {}
            Err(e) => {
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                crate::log_warn!("operation {watcher_id} failed: {e}");
                watcher_reg.finish(
                    &watcher_id,
                    status,
                    error_body(status, "the call stopped unexpectedly (see the engine log)"),
                    false,
                    0,
                );
            }
        }
    });

    let mut resp = (StatusCode::ACCEPTED, Json(json!({ "op_id": id }))).into_response();
    let headers = resp.headers_mut();
    headers.insert(
        "preference-applied",
        HeaderValue::from_static("respond-async"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("{prefix}/api/ops/{id}")) {
        headers.insert(header::LOCATION, v);
    }
    resp
}

/// What the routes read: the operations, and the job and install tables they also serve.
#[derive(Clone)]
struct OpsCtx {
    reg: Arc<OpRegistry>,
    app: crate::AppState,
    installs: crate::pkg_install::PkgInstallStateHandle,
}

fn job_row(id: uuid::Uuid, job: &JobState) -> (u64, Value) {
    let (status, http_status) = match job {
        JobState::Running { .. } => ("running", None),
        JobState::Done { .. } => ("done", Some(200)),
        JobState::Failed { .. } => ("done", Some(500)),
    };
    (
        job.started_at_ms(),
        json!({
            "id": format!("job:{id}"),
            "kind": "job",
            "status": status,
            "http_status": http_status,
            "started_at_ms": job.started_at_ms(),
        }),
    )
}

fn install_row(st: &crate::install::status::InstallStatus, running: bool) -> (u64, Value) {
    use crate::install::status::{Phase, Verdict};
    let done = !running || matches!(st.phase, Phase::Done | Phase::Failed);
    let failed = matches!(st.phase, Phase::Failed) || matches!(st.verdict, Some(Verdict::Failed));
    let started_at_ms = st.started_at.saturating_mul(1000);
    (
        started_at_ms,
        json!({
            "id": format!("install:{}", st.job),
            "kind": "install",
            "status": if done { "done" } else { "running" },
            "http_status": done.then_some(if failed { 500 } else { 200 }),
            "started_at_ms": started_at_ms,
        }),
    )
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<usize>,
}

/// `GET /api/ops?limit=`: operations, transfer jobs and installs, newest first, without bodies.
async fn list(State(ctx): State<OpsCtx>, Query(q): Query<ListQuery>) -> Response {
    let mut rows = ctx.reg.list();
    {
        let jobs = ctx.app.jobs.lock().unwrap_or_else(|e| e.into_inner());
        rows.extend(jobs.iter().map(|(id, j)| job_row(*id, j)));
    }
    let running: Vec<String> = ctx
        .installs
        .jobs
        .running()
        .into_iter()
        .map(|s| s.job)
        .collect();
    rows.extend(
        ctx.installs
            .jobs
            .all()
            .iter()
            .map(|st| install_row(st, running.contains(&st.job))),
    );
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let ops: Vec<Value> = rows
        .into_iter()
        .take(q.limit.unwrap_or(usize::MAX))
        .map(|r| r.1)
        .collect();
    Json(json!({ "ops": ops })).into_response()
}

/// `GET /api/ops/{id}`: `{status, http_status, body, ...}` for an operation, `job:` or `install:`.
async fn get_op(State(ctx): State<OpsCtx>, Path(id): Path<String>) -> Response {
    let found = if let Some(job) = id.strip_prefix("job:") {
        job.parse::<uuid::Uuid>().ok().and_then(|uuid| {
            let jobs = ctx.app.jobs.lock().unwrap_or_else(|e| e.into_inner());
            jobs.get(&uuid).map(|j| {
                let mut v = job_row(uuid, j).1;
                v["body"] = crate::with_live_notes(uuid, json!(j));
                v
            })
        })
    } else if let Some(job) = id.strip_prefix("install:") {
        ctx.installs.jobs.get(job).map(|st| {
            let running = ctx.installs.jobs.running().iter().any(|s| s.job == st.job);
            let mut v = install_row(&st, running).1;
            v["body"] = json!(st);
            v
        })
    } else {
        ctx.reg.get(&id)
    };
    match found {
        Some(v) => Json(v).into_response(),
        None => json_err(StatusCode::NOT_FOUND, "no such operation").into_response(),
    }
}

/// `POST /api/ops/{id}/cancel`.
async fn cancel(State(ctx): State<OpsCtx>, Path(id): Path<String>) -> Response {
    if let Some(job) = id.strip_prefix("job:") {
        let Ok(uuid) = job.parse::<uuid::Uuid>() else {
            return json_err(StatusCode::NOT_FOUND, "no such operation").into_response();
        };
        return Json(json!({ "cancelled": crate::signal_transfer_cancel(uuid) })).into_response();
    }
    if let Some(job) = id.strip_prefix("install:") {
        if ctx.installs.jobs.get(job).is_none() {
            return json_err(StatusCode::NOT_FOUND, "no such operation").into_response();
        }
        return Json(json!({ "cancelled": ctx.installs.jobs.request_cancel(job) })).into_response();
    }
    enum Plan {
        Done,
        FsOp(FsOp),
        Abandon(Option<AbortHandle>),
    }
    let plan = {
        let mut g = ctx.reg.lock();
        let Some(op) = g.get_mut(&id) else {
            return json_err(StatusCode::NOT_FOUND, "no such operation").into_response();
        };
        if op.finished.is_some() {
            Plan::Done
        } else if let Some(f) = &op.fs_op {
            Plan::FsOp(f.clone())
        } else {
            Plan::Abandon(op.abort.take())
        }
    };
    match plan {
        Plan::Done => Json(json!({ "cancelled": false, "status": "done" })).into_response(),
        Plan::FsOp(f) => {
            let addr = crate::console_addr_or_default(f.addr, &ctx.app.default_ps5_addr);
            match crate::cancel_fs_op(addr, f.op_id).await {
                // The route itself then ends, 409 "cancelled", which is the operation's answer.
                Ok(found) => Json(json!({ "cancelled": found })).into_response(),
                Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
            }
        }
        Plan::Abandon(abort) => {
            if let Some(a) = abort {
                a.abort();
            }
            let status = StatusCode::from_u16(499).unwrap_or(StatusCode::CONFLICT);
            let body = json!({
                "error": "stopped waiting for this call; it has no cancel of its own, so it may \
                          still finish on the console",
                "code": "abandoned",
            });
            let mut g = ctx.reg.lock();
            if let Some(op) = g.get_mut(&id) {
                op.abandoned = true;
            }
            drop(g);
            ctx.reg.finish(&id, status, body, false, 0);
            Json(json!({
                "cancelled": false,
                "abandoned": true,
                "message": "This call has no cancel of its own. The engine stopped waiting for \
                            it, but the work may still finish on the console.",
            }))
            .into_response()
        }
    }
}

pub(crate) fn router(
    reg: Arc<OpRegistry>,
    app: crate::AppState,
    installs: crate::pkg_install::PkgInstallStateHandle,
) -> Router {
    Router::new()
        .route("/api/ops", get(list))
        .route("/api/ops/{id}", get(get_op))
        .route("/api/ops/{id}/cancel", post(cancel))
        .with_state(OpsCtx { reg, app, installs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("prefer", HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn prefer_is_read_the_way_rfc_7240_writes_it() {
        assert!(prefers_async(&headers("respond-async")));
        assert!(prefers_async(&headers("Respond-Async")));
        assert!(prefers_async(&headers("wait=10, respond-async")));
        assert!(prefers_async(&headers("respond-async; foo=bar")));
        assert!(!prefers_async(&headers("wait=10")));
        assert!(!prefers_async(&headers("respond-asyncish")));
        assert!(!prefers_async(&HeaderMap::new()));
    }

    #[test]
    fn a_route_with_its_own_cancel_gets_an_op_id_unless_it_sent_one() {
        let (body, op) = prepare_fs_op(
            "/api/ps5/fs/copy",
            Bytes::from_static(br#"{"addr":"10.0.0.2","from":"/a","to":"/b"}"#),
        );
        let v: Value = serde_json::from_slice(&body).unwrap();
        let op = op.unwrap();
        assert_eq!(v["op_id"].as_u64(), Some(op.op_id));
        assert_ne!(op.op_id, 0);
        assert!(op.op_id < 1 << 53, "a JSON client reads it exactly");
        assert_eq!(op.addr.as_deref(), Some("10.0.0.2"));
        assert_eq!(v["from"], "/a", "the rest of the body is kept");

        let (body, op) = prepare_fs_op(
            "/api/ps5/fs/delete",
            Bytes::from_static(br#"{"console":"10.0.0.3","path":"/a","op_id":77}"#),
        );
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["op_id"], 77);
        let op = op.unwrap();
        assert_eq!(op.op_id, 77);
        assert_eq!(op.addr.as_deref(), Some("10.0.0.3"));

        let (_, op) = prepare_fs_op("/api/ps5/fs/delete", Bytes::from_static(b"not json"));
        assert!(op.is_none(), "the route refuses it, not this layer");
        let (body, op) = prepare_fs_op("/api/ps5/users/create", Bytes::from_static(b"{}"));
        assert!(op.is_none());
        assert_eq!(&body[..], b"{}", "other routes are left alone");
    }

    fn finished(id: &str, age: Duration, len: usize, now: Instant) -> Op {
        Op {
            id: id.into(),
            method: "POST".into(),
            path: "/api/x".into(),
            started_at_ms: 0,
            finished_at_ms: Some(0),
            finished: Some(now - age),
            http_status: Some(200),
            body: Value::Null,
            body_truncated: false,
            body_len: len,
            abandoned: false,
            fs_op: None,
            abort: None,
        }
    }

    #[test]
    fn retention_drops_by_age_then_count_then_size_but_never_a_running_op() {
        let now = Instant::now() + Duration::from_secs(7200);
        let mut ops = HashMap::new();
        ops.insert(
            "old".to_string(),
            finished("old", KEEP_FINISHED + Duration::from_secs(1), 0, now),
        );
        ops.insert(
            "new".to_string(),
            finished("new", Duration::from_secs(1), 0, now),
        );
        let mut running = finished("run", Duration::ZERO, 0, now);
        running.finished = None;
        ops.insert("run".to_string(), running);
        prune(&mut ops, now);
        assert!(!ops.contains_key("old"));
        assert!(ops.contains_key("new") && ops.contains_key("run"));

        let mut ops = HashMap::new();
        for i in 0..(MAX_OPS + 10) {
            let id = format!("o{i}");
            ops.insert(
                id.clone(),
                finished(&id, Duration::from_secs(i as u64 + 2), 0, now),
            );
        }
        let mut running = finished("run", Duration::ZERO, 0, now);
        running.finished = None;
        ops.insert("run".to_string(), running);
        prune(&mut ops, now);
        assert!(ops.len() < MAX_OPS);
        assert!(ops.contains_key("run"));
        assert!(ops.contains_key("o0"), "the newest finished one stays");

        let mut ops = HashMap::new();
        ops.insert(
            "big-old".to_string(),
            finished("big-old", Duration::from_secs(9), MAX_TOTAL_BODY, now),
        );
        ops.insert(
            "big-new".to_string(),
            finished("big-new", Duration::from_secs(1), MAX_TOTAL_BODY, now),
        );
        prune(&mut ops, now);
        assert!(!ops.contains_key("big-old"));
        assert!(ops.contains_key("big-new"));
    }
}
