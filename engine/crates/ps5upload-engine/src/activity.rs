//! What the engine is working on and what just ended, for the app's background watcher (#457).
//!
//! The renderer used to be the only thing that noticed a job finish, and a hidden or frozen
//! WebView notices nothing. The Tauri host polls `GET /api/activity` instead: running jobs
//! (transfers and installs) with their progress, plus a ring of recently finished ones with a
//! monotonic `seq` so a poller sees each ending once. Labels ("Astro Bot", a folder name) come
//! from the client through `POST /api/activity/label`; the engine only knows ids and paths.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

/// Finished entries kept for pollers. A poller one second behind needs a handful; 64 covers a
/// watcher that was paused for a while without growing with the session.
pub const RING_CAP: usize = 64;
/// Labels for jobs that never reported an ending (a label posted for an unknown id).
const LABELS_CAP: usize = 256;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Active {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Finished {
    pub seq: u64,
    pub id: String,
    pub kind: String,
    pub label: String,
    pub ok: bool,
    /// The user stopped it: nothing to tell them.
    pub cancelled: bool,
    pub error: Option<String>,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default)]
struct Tag {
    kind: Option<String>,
    label: Option<String>,
    /// Insertion order, for dropping the oldest past `LABELS_CAP`.
    at: u64,
}

#[derive(Default)]
struct Hub {
    seq: u64,
    tags: HashMap<String, Tag>,
    tag_counter: u64,
    ring: VecDeque<Finished>,
}

impl Hub {
    fn tag_mut(&mut self, id: &str) -> &mut Tag {
        if !self.tags.contains_key(id) && self.tags.len() >= LABELS_CAP {
            if let Some(oldest) = self
                .tags
                .iter()
                .min_by_key(|(_, t)| t.at)
                .map(|(k, _)| k.clone())
            {
                self.tags.remove(&oldest);
            }
        }
        self.tag_counter += 1;
        let at = self.tag_counter;
        self.tags.entry(id.to_string()).or_insert(Tag {
            at,
            ..Tag::default()
        })
    }

    fn kind_label(&self, id: &str, default_kind: &str) -> (String, String) {
        let t = self.tags.get(id);
        (
            t.and_then(|t| t.kind.clone())
                .unwrap_or_else(|| default_kind.to_string()),
            t.and_then(|t| t.label.clone()).unwrap_or_default(),
        )
    }

    fn set_kind(&mut self, id: &str, kind: &str) {
        let t = self.tag_mut(id);
        if t.kind.is_none() {
            t.kind = Some(kind.to_string());
        }
    }

    fn set_label(&mut self, id: &str, kind: Option<&str>, label: &str) {
        // The job may have ended before the label arrived (a tiny upload): name its entry.
        if let Some(e) = self.ring.iter_mut().find(|e| e.id == id) {
            e.label = label.to_string();
            return;
        }
        let t = self.tag_mut(id);
        t.label = Some(label.to_string());
        if let Some(k) = kind.filter(|k| !k.is_empty()) {
            t.kind = Some(k.to_string());
        }
    }

    fn snapshot(&self, since: u64, active: Vec<Active>) -> serde_json::Value {
        let finished: Vec<&Finished> = self.ring.iter().filter(|e| e.seq > since).collect();
        serde_json::json!({ "seq": self.seq, "active": active, "finished": finished })
    }

    fn finish(&mut self, mut f: Finished) {
        // A job can be written terminal twice (a late ticker, a retry of the final write);
        // a poller must see its ending once.
        if self.ring.iter().any(|e| e.id == f.id) {
            return;
        }
        let (kind, label) = self.kind_label(&f.id, &f.kind);
        self.tags.remove(&f.id);
        self.seq += 1;
        f.seq = self.seq;
        f.kind = kind;
        // The client's name beats the engine's fallback (an install's title id).
        if !label.is_empty() {
            f.label = label;
        }
        if self.ring.len() >= RING_CAP {
            self.ring.pop_front();
        }
        self.ring.push_back(f);
    }
}

fn hub() -> &'static Mutex<Hub> {
    static H: OnceLock<Mutex<Hub>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(Hub::default()))
}

fn lock() -> std::sync::MutexGuard<'static, Hub> {
    hub().lock().unwrap_or_else(|e| e.into_inner())
}

/// The kind a transfer's telemetry shape (`file`, `dir`, `download`, …) is shown as.
pub fn kind_of_shape(shape: &str) -> &'static str {
    match shape {
        "download" | "link_download" => "download",
        _ => "upload",
    }
}

/// Records what kind of job `id` is, unless the client already said.
pub(crate) fn set_kind(id: &str, kind: &str) {
    lock().set_kind(id, kind);
}

/// A readable name for a job, from the client. `kind` overrides the engine's guess.
pub fn set_label(id: &str, kind: Option<&str>, label: &str) {
    lock().set_label(id, kind, label);
}

/// A transfer (or any `JobState` job) reached `done` / `failed`; `state` is its snapshot.
pub(crate) fn transfer_ended(id: &str, state: &serde_json::Value) {
    if let Some(f) = transfer_finished(id, state) {
        lock().finish(f);
    }
}

/// An install job ended (the unified install's finish path).
pub(crate) fn install_ended(st: &crate::install::status::InstallStatus) {
    lock().finish(install_finished(st));
}

/// A Share transfer ended (share/mod.rs): sending to a friend (`kind` "share") or receiving
/// (`receive`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn share_ended(
    id: &str,
    kind: &str,
    label: &str,
    ok: bool,
    cancelled: bool,
    error: Option<String>,
    bytes: u64,
    elapsed_ms: u64,
) {
    lock().finish(Finished {
        seq: 0,
        id: id.to_string(),
        kind: kind.to_string(),
        label: label.to_string(),
        ok,
        cancelled,
        error: if ok { None } else { error },
        bytes,
        elapsed_ms,
    });
}

/// The ending a terminal job snapshot describes; `None` for any other state.
fn transfer_finished(id: &str, state: &serde_json::Value) -> Option<Finished> {
    let status = state.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let ok = status == "done";
    if !ok && status != "failed" {
        return None;
    }
    let error = state
        .get("error")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reason = state
        .get("error_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let err = error.as_deref().unwrap_or("");
    // Same rule as the job summaries (telemetry::result_of).
    let cancelled = !ok
        && (reason.contains("cancel") || err.contains("transfer_cancelled") || err == "cancelled");
    let num = |k: &str| state.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    Some(Finished {
        seq: 0,
        id: id.to_string(),
        kind: "job".into(),
        label: String::new(),
        ok,
        cancelled,
        error: if ok { None } else { error },
        bytes: num("bytes_sent"),
        elapsed_ms: num("elapsed_ms"),
    })
}

fn install_finished(st: &crate::install::status::InstallStatus) -> Finished {
    use crate::install::status::{FailReason, Phase, Verdict};
    let ok = st.phase == Phase::Done && st.verdict != Some(Verdict::Failed);
    Finished {
        seq: 0,
        id: st.job.clone(),
        kind: "install".into(),
        label: st.title_id.clone().unwrap_or_else(|| st.content_id.clone()),
        ok,
        cancelled: st.reason == Some(FailReason::Cancelled),
        error: if ok {
            None
        } else {
            Some(st.hint.clone().unwrap_or_else(|| "install failed".into()))
        },
        bytes: st.metrics.served_bytes,
        elapsed_ms: st.updated_at.saturating_sub(st.started_at) * 1000,
    }
}

/// The running install `st`, as an activity entry.
pub(crate) fn install_active(st: &crate::install::status::InstallStatus) -> Active {
    let (kind, label) = lock().kind_label(&st.job, "install");
    Active {
        id: st.job.clone(),
        kind,
        label: if label.is_empty() {
            st.title_id.clone().unwrap_or_else(|| st.content_id.clone())
        } else {
            label
        },
        bytes_done: st.metrics.served_bytes,
        bytes_total: st.metrics.total_bytes,
        started_at_ms: st.started_at * 1000,
    }
}

/// The running transfer `id` with its progress, as an activity entry.
pub(crate) fn transfer_active(id: &str, done: u64, total: u64, started_at_ms: u64) -> Active {
    let (kind, label) = lock().kind_label(id, "job");
    Active {
        id: id.to_string(),
        kind,
        label,
        bytes_done: done,
        bytes_total: total,
        started_at_ms,
    }
}

/// `{seq, active, finished}`: entries finished after `since`, oldest first.
pub fn snapshot(since: u64, active: Vec<Active>) -> serde_json::Value {
    lock().snapshot(since, active)
}

#[derive(Debug, Deserialize)]
pub struct SinceQuery {
    #[serde(default)]
    pub since: u64,
}

#[derive(Debug, Deserialize)]
pub struct LabelReq {
    pub job_id: String,
    #[serde(default)]
    pub kind: Option<String>,
    pub label: String,
}

/// `POST /api/activity/label {job_id, kind?, label}`.
pub async fn label_handler(axum::Json(req): axum::Json<LabelReq>) -> axum::response::Response {
    use axum::response::IntoResponse;
    if req.job_id.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"ok": false, "error": "job_id is required"})),
        )
            .into_response();
    }
    // A name for a notification, not a document: keep it short.
    let label: String = req.label.trim().chars().take(120).collect();
    set_label(&req.job_id, req.kind.as_deref(), &label);
    axum::Json(serde_json::json!({"ok": true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn end(h: &mut Hub, id: &str, state: serde_json::Value) {
        if let Some(f) = transfer_finished(id, &state) {
            h.finish(f);
        }
    }

    #[test]
    fn a_finished_job_is_listed_once_with_its_label_and_kind() {
        let mut h = Hub::default();
        h.set_kind("a", "upload");
        h.set_label("a", None, "Astro Bot");
        end(
            &mut h,
            "a",
            json!({"status":"done","bytes_sent":10,"elapsed_ms":5}),
        );
        end(
            &mut h,
            "a",
            json!({"status":"done","bytes_sent":10,"elapsed_ms":5}),
        );
        let v = h.snapshot(0, vec![]);
        assert_eq!(v["seq"], 1);
        let f = v["finished"].as_array().unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0]["label"], "Astro Bot");
        assert_eq!(f[0]["kind"], "upload");
        assert_eq!(f[0]["ok"], true);
        assert_eq!(f[0]["bytes"], 10);
        assert!(h.tags.is_empty(), "the label is dropped with the ending");
        // Nothing new after the seq a poller already has.
        assert!(h.snapshot(1, vec![])["finished"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_client_kind_wins_and_a_late_label_names_the_entry() {
        let mut h = Hub::default();
        h.set_label("b", Some("download"), "Saves");
        h.set_kind("b", "upload");
        end(
            &mut h,
            "b",
            json!({"status":"failed","error":"transfer_cancelled"}),
        );
        end(&mut h, "c", json!({"status":"failed","error":"boom"}));
        h.set_label("c", None, "Late");
        let v = h.snapshot(0, vec![]);
        let f = v["finished"].as_array().unwrap();
        assert_eq!(f[0]["kind"], "download");
        assert_eq!(f[0]["cancelled"], true);
        assert_eq!(f[1]["kind"], "job");
        assert_eq!(f[1]["label"], "Late");
        assert_eq!(f[1]["ok"], false);
        assert_eq!(f[1]["cancelled"], false);
        assert_eq!(f[1]["error"], "boom");
    }

    #[test]
    fn running_states_are_not_endings() {
        assert!(transfer_finished("r", &json!({"status":"running"})).is_none());
    }

    #[test]
    fn the_ring_keeps_the_newest_and_seq_keeps_climbing() {
        let mut h = Hub::default();
        for i in 0..(RING_CAP + 10) {
            end(&mut h, &format!("j{i}"), json!({"status":"done"}));
        }
        let v = h.snapshot(0, vec![]);
        let f = v["finished"].as_array().unwrap();
        assert_eq!(f.len(), RING_CAP);
        assert_eq!(v["seq"], (RING_CAP + 10) as u64);
        assert_eq!(f[0]["seq"], 11);
        assert_eq!(f[RING_CAP - 1]["id"], format!("j{}", RING_CAP + 9));
    }

    #[test]
    fn labels_for_jobs_that_never_end_are_bounded() {
        let mut h = Hub::default();
        for i in 0..(LABELS_CAP + 5) {
            h.set_label(&format!("x{i}"), None, "n");
        }
        assert_eq!(h.tags.len(), LABELS_CAP);
        assert!(h.tags.contains_key(&format!("x{}", LABELS_CAP + 4)));
        assert!(!h.tags.contains_key("x0"));
    }

    #[test]
    fn an_install_ending_carries_its_verdict_and_the_clients_name() {
        use crate::install::status::{FailReason, InstallStatus, Phase, Verdict};
        let mut h = Hub::default();
        let mut st = InstallStatus::new("1-1", "ps5", "UP0000-PPSA00000_00");
        st.title_id = Some("PPSA00000".into());
        st.phase = Phase::Done;
        st.verdict = Some(Verdict::MayNotLaunch);
        st.metrics.served_bytes = 7;
        h.finish(install_finished(&st));
        let mut st2 = InstallStatus::new("1-2", "ps5", "c");
        st2.phase = Phase::Failed;
        st2.verdict = Some(Verdict::Failed);
        st2.reason = Some(FailReason::Cancelled);
        h.set_label("1-2", None, "Jak X");
        h.finish(install_finished(&st2));
        let v = h.snapshot(0, vec![]);
        let f = v["finished"].as_array().unwrap();
        assert_eq!(f[0]["kind"], "install");
        assert_eq!(f[0]["ok"], true);
        assert_eq!(f[0]["label"], "PPSA00000");
        assert_eq!(f[0]["bytes"], 7);
        assert_eq!(f[1]["ok"], false);
        assert_eq!(f[1]["cancelled"], true);
        assert_eq!(f[1]["label"], "Jak X");
    }

    // The process-wide hub through the label route. Other tests end jobs concurrently, so this
    // only looks for its own id.
    #[tokio::test]
    async fn the_label_route_names_a_job_the_activity_snapshot_reports() {
        let id = format!("route-{}", uuid::Uuid::new_v4());
        let r = label_handler(axum::Json(LabelReq {
            job_id: id.clone(),
            kind: Some("upload".into()),
            label: format!("  {}  ", "x".repeat(200)),
        }))
        .await;
        assert_eq!(r.status(), axum::http::StatusCode::OK);
        let active = transfer_active(&id, 5, 10, 1);
        assert_eq!(active.kind, "upload");
        assert_eq!(active.label.chars().count(), 120);
        transfer_ended(&id, &json!({"status":"done","bytes_sent":10}));
        let v = snapshot(0, vec![]);
        let mine: Vec<_> = v["finished"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["id"] == id.as_str())
            .collect();
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["kind"], "upload");
        let bad = label_handler(axum::Json(LabelReq {
            job_id: String::new(),
            kind: None,
            label: "x".into(),
        }))
        .await;
        assert_eq!(bad.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}
