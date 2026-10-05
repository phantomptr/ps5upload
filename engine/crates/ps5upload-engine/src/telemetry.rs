//! Structured per-job telemetry (review 009 #4): opt-out, local-only.
//!
//! One `job_summary` JSON per finished, failed or cancelled job, written to
//! `<data dir>/jobs/<job id>.json` (the newest `MAX_SUMMARIES` are kept). It answers "why was
//! that slow?" from one file a user can attach: where the job's time went, how it ended, and
//! the console's own end-of-job line.
//!
//! Local only. Nothing here sends anything anywhere; the record is shown in the app
//! (`GET /api/jobs/{id}/summary`, `GET /api/jobs/summaries`) and goes into the bug bundle,
//! which users share publicly. So the record holds no address and no local path: the console
//! is a hash of its key (`ps5upload_ava1::telemetry::console_hash`), the destination is only
//! its drive, the source path is never stored, and every free-text field is scrubbed of the
//! user's home folder and IP addresses (`scrub`).
//!
//! `PS5UPLOAD_JOB_SUMMARIES=0` turns the record off.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ps5upload_core::transfer::LiveNotes;
use serde_json::{json, Value};
use uuid::Uuid;

/// Records kept on disk; the oldest go first.
pub const MAX_SUMMARIES: usize = 200;
/// Longest free-text field in a record, in characters.
const MAX_TEXT: usize = 2000;
/// A share below this is not "the" limit: nothing dominated.
const DOMINANT_MIN_PCT: f64 = 30.0;

pub fn enabled() -> bool {
    !matches!(
        std::env::var("PS5UPLOAD_JOB_SUMMARIES").as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    )
}

// ─── Which jobs get a record ─────────────────────────────────────────────────

struct Held {
    kind: &'static str,
    /// The drive the job writes to (`drive_of` the destination), known before it ends so a
    /// failed job, which reports no destination, still says where it was going.
    drive: Option<String>,
    notes: Option<Arc<LiveNotes>>,
}

fn held() -> &'static Mutex<HashMap<Uuid, Held>> {
    static H: OnceLock<Mutex<HashMap<Uuid, Held>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Marks a job as one that gets a record when it ends. `kind` is the transfer's shape
/// (`file`, `dir`, `zip`, `7z`, `rar`, `file_list`, `download`, `relay`, …).
pub(crate) fn tag(job_id: Uuid, kind: &'static str) {
    held()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(job_id)
        .or_insert(Held {
            kind,
            drive: None,
            notes: None,
        });
}

/// Records the drive a tagged job writes to; only the drive is kept, never the path.
pub(crate) fn set_drive(job_id: Uuid, dest: &str) {
    if let Some(h) = held()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&job_id)
    {
        h.drive = Some(drive_of(dest));
    }
}

/// Keeps a tagged job's live notes alive until it ends: the transfer drops its own copy when it
/// returns, which is before the engine marks the job finished.
pub(crate) fn hold_notes(job_id: Uuid, notes: Arc<LiveNotes>) {
    let mut g = held().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(h) = g.get_mut(&job_id) {
        h.notes = Some(notes);
    }
}

/// Called with every job state the engine records. A terminal state of a tagged job writes
/// its record (once); anything else is ignored.
pub(crate) fn on_state(job_id: Uuid, state: &Value) {
    let status = state.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "done" && status != "failed" {
        return;
    }
    let Some(h) = held()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&job_id)
    else {
        return;
    };
    if !enabled() {
        return;
    }
    let ava1 = h.notes.as_ref().and_then(|n| {
        n.telemetry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    });
    let mut record = build_record(job_id, h.kind, state, ava1.as_ref(), now_ms());
    if record["drive"] == "unknown" {
        if let (Some(d), Some(o)) = (h.drive, record.as_object_mut()) {
            o.insert("drive".into(), d.into());
        }
    }
    metrics().record(h.kind, &record);
    let Some(dir) = jobs_dir() else { return };
    if let Err(e) = write_record(&dir, job_id, &record) {
        crate::log_warn!("job summary for {job_id} not written: {e}");
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ─── The record ──────────────────────────────────────────────────────────────

/// `/data`, `/mnt/usb0`, `/mnt/ext0`, … — the drive a console path lives on, and nothing
/// below it.
pub fn drive_of(dest: &str) -> String {
    let mut parts = dest.split('/').filter(|p| !p.is_empty());
    match (parts.next(), parts.next()) {
        (Some("mnt"), Some(d)) => format!("/mnt/{}", scrub_segment(d)),
        (Some(first), _) => format!("/{}", scrub_segment(first)),
        _ => "unknown".into(),
    }
}

fn scrub_segment(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(24)
        .collect()
}

/// Free text with the user's home folder and IP addresses taken out, and bounded in length.
pub fn scrub(text: &str) -> String {
    let home = ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|h| h.trim().len() > 1)
        .unwrap_or_default();
    scrub_with(text, &home)
}

pub fn scrub_with(text: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    let mut markers: Vec<String> = ["/Users/", "/home/", "\\Users\\", "/users/", "\\users\\"]
        .iter()
        .map(|m| m.to_string())
        .collect();
    if home.len() > 1 {
        markers.push(home.to_string());
        markers.push(home.replace('\\', "/"));
        markers.push(home.replace('/', "\\"));
    }
    let mut s = strip_local_paths(text, &markers);
    s = mask_ipv4(&s);
    if s.chars().count() > MAX_TEXT {
        s = s.chars().take(MAX_TEXT).collect::<String>() + "…";
    }
    s
}

/// Where a path embedded in a sentence ends: a quote, a line end, a separator the sentence
/// uses after it, or a `: ` (a path can hold spaces, so this errs on the side of taking more).
fn path_end(after: &str) -> usize {
    let b = after.as_bytes();
    for (i, c) in after.char_indices() {
        match c {
            '"' | '\'' | '`' | '\n' | '\r' | ';' | ',' | ')' | ']' | '>' => return i,
            ':' if b.get(i + 1).is_none_or(|n| n.is_ascii_whitespace()) => return i,
            _ => {}
        }
    }
    after.len()
}

/// Every local path that starts at one of `markers` (a home folder, `/Users/`, `C:\Users\`)
/// becomes `<local path>`: the folder, the user's name and the file names under it.
fn strip_local_paths(s: &str, markers: &[String]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let best = markers
            .iter()
            .filter_map(|m| rest.find(m.as_str()).map(|i| (i, m.len())))
            .min_by_key(|&(i, _)| i);
        let Some((i, mlen)) = best else {
            out.push_str(rest);
            return out;
        };
        // A drive letter and colon before the marker go with it.
        let mut head = &rest[..i];
        if head.len() >= 2
            && head.ends_with(':')
            && head.as_bytes()[head.len() - 2].is_ascii_alphabetic()
        {
            head = &head[..head.len() - 2];
        }
        out.push_str(head);
        out.push_str("<local path>");
        let after = &rest[i + mlen..];
        rest = &after[path_end(after)..];
    }
}

fn mask_ipv4(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i].is_ascii_digit()
            && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'.'))
        {
            if let Some(end) = ipv4_at(b, i) {
                out.push_str(&s[copied..i]);
                out.push_str("<ip>");
                i = end;
                copied = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

/// The end of a dotted quad starting at `i`, if one does.
fn ipv4_at(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    for octet in 0..4 {
        let start = j;
        while j < b.len() && b[j].is_ascii_digit() && j - start < 3 {
            j += 1;
        }
        if j == start
            || std::str::from_utf8(&b[start..j])
                .ok()?
                .parse::<u16>()
                .ok()?
                > 255
        {
            return None;
        }
        if octet < 3 {
            if b.get(j) != Some(&b'.') {
                return None;
            }
            j += 1;
        }
    }
    // Not the front of a longer number or word (1.2.3.4.5, 1.2.3.45x).
    match b.get(j) {
        Some(c)
            if c.is_ascii_alphanumeric()
                || *c == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit) =>
        {
            None
        }
        _ => Some(j),
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn u64_of(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

/// How a job ended: `done`, `cancelled` or `failed`.
fn result_of(state: &Value) -> &'static str {
    if str_of(state, "status") == Some("done") {
        return "done";
    }
    let reason = str_of(state, "error_reason").unwrap_or("");
    let error = str_of(state, "error").unwrap_or("");
    if reason.contains("cancel") || error.contains("transfer_cancelled") || error == "cancelled" {
        "cancelled"
    } else {
        "failed"
    }
}

/// The short machine token for how a job ended (`ava1_stalled`, `ava1_commit_cross_device`, …).
fn code_of(state: &Value) -> Option<String> {
    if let Some(r) = str_of(state, "error_reason") {
        return Some(scrub_segment(r));
    }
    let error = str_of(state, "error")?;
    if error.contains("no durable progress") {
        Some("ava1_stalled".into())
    } else if error.contains("transfer_cancelled") {
        Some("transfer_cancelled".into())
    } else {
        None
    }
}

/// Builds the record for a finished job. `state` is the engine's job snapshot (`done` or
/// `failed`); `ava1` is the transport's telemetry (`ps5upload_ava1::telemetry::snapshot`),
/// absent for a job that never reached the sender or does not use it.
pub fn build_record(
    job_id: Uuid,
    kind: &str,
    state: &Value,
    ava1: Option<&Value>,
    now: u64,
) -> Value {
    let result = result_of(state);
    let null = Value::Null;
    let a = ava1.unwrap_or(&null);
    let ack = state.get("commit_ack").unwrap_or(&null);
    let started = u64_of(state, "started_at_ms");
    let ended = match u64_of(state, "completed_at_ms") {
        0 => now,
        t => t,
    };
    let bytes = [
        u64_of(ack, "bytes"),
        u64_of(state, "bytes_sent"),
        u64_of(a, "bytes_durable"),
    ]
    .into_iter()
    .find(|b| *b > 0)
    .unwrap_or(0);
    let files = [
        u64_of(ack, "files"),
        u64_of(a, "files_durable"),
        u64_of(state, "files_sent"),
    ]
    .into_iter()
    .find(|b| *b > 0)
    .unwrap_or(0);
    let message = str_of(state, "error_detail")
        .or_else(|| str_of(state, "error"))
        .or_else(|| str_of(ack, "warning"))
        .map(scrub);
    let shares = a.get("shares").cloned().unwrap_or(Value::Null);
    let why = interpret(&shares, a, result);
    let mut rec = json!({
        "schema": 1,
        "type": "job_summary",
        "job_id": job_id.to_string(),
        "kind": kind,
        "console": a.get("console"),
        "started_at_ms": started,
        "ended_at_ms": ended,
        "elapsed_ms": u64_of(state, "elapsed_ms"),
        "result": result,
        "code": code_of(state),
        "message": message,
        "files": files,
        "bytes": bytes,
        "skipped_files": u64_of(state, "skipped_files").max(u64_of(a, "skipped_files")),
        "skipped_bytes": u64_of(state, "skipped_bytes").max(u64_of(a, "skipped_bytes")),
        "resumed": a.get("resumed").and_then(Value::as_bool).unwrap_or(false),
        "attempts": u64_of(a, "attempts"),
        "drive": str_of(state, "dest").map(drive_of).unwrap_or_else(|| "unknown".into()),
        "engine_version": env!("CARGO_PKG_VERSION"),
        "shares": shares,
        "why": why,
    });
    if let (Some(o), Some(src)) = (rec.as_object_mut(), a.as_object()) {
        for k in [
            "lanes_avg",
            "lanes_max",
            "chunk_avg_kib",
            "history",
            "slow_drive_switch",
            "settle_ms",
            "unswept_peak",
            "resent_bytes",
        ] {
            if let Some(v) = src.get(k) {
                o.insert(k.into(), v.clone());
            }
        }
        // The console's line is its own text, verbatim except for what `scrub` removes.
        if let Some(l) = src.get("console_line").and_then(Value::as_str) {
            o.insert("console_line".into(), scrub(l).into());
        }
    }
    rec
}

/// The dominant share of a job's time and one sentence on what it means. `shares` is the
/// record's `shares` object. The receiver-bound share is a part of the credit-starved one
/// (a starved sender whose console reported its drive or workers as the limit), so the
/// remainder is reported as plain credit starvation.
pub fn interpret(shares: &Value, ava1: &Value, result: &str) -> Value {
    let ticks = u64_of(shares, "ticks");
    if ticks == 0 {
        let text = if result == "done" {
            "The job was too short to measure."
        } else {
            "The job ended before it could be measured."
        };
        return json!({"dominant": "unmeasured", "pct": 0.0, "text": text});
    }
    let f = |k: &str| shares.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let receiver = f("receiver_bound_pct");
    let credit = (f("credit_starved_pct") - receiver).max(0.0);
    let source = f("source_starved_pct");
    // Ties go to the console, then the source: the cause a person can act on first.
    let (dominant, pct) = [
        ("receiver_bound", receiver),
        ("source_starved", source),
        ("credit_starved", credit),
    ]
    .into_iter()
    .fold(("none", 0.0), |best, c| if c.1 > best.1 { c } else { best });
    if pct < DOMINANT_MIN_PCT {
        return json!({
            "dominant": "network",
            "pct": 0.0,
            "text": "No single limit dominated: neither the console nor the source held the job back, so the network link was the limit.",
        });
    }
    let shown = pct.round();
    let drive = str_of(shares, "receiver_bottleneck") == Some("console drive");
    let text = match dominant {
        "receiver_bound" => {
            let what = if drive {
                "the console drive's write rate"
            } else {
                "the console's write workers"
            };
            let tail = if ava1.get("slow_drive_switch").and_then(Value::as_bool) == Some(true) {
                "; the transfer switched to sequential writes for the slow drive"
            } else {
                ""
            };
            format!("receiver-bound {shown} %: the console could not take data faster than {what}{tail}.")
        }
        "source_starved" => format!(
            "source-starved {shown} %: reading the source was the limit (a slow disk, a network share, or archive decoding on this computer)."
        ),
        _ => format!(
            "credit-starved {shown} %: the console's receive window was full, so it was the limit (its memory, not its drive)."
        ),
    };
    json!({"dominant": dominant, "pct": shown, "text": text})
}

// ─── Storage ─────────────────────────────────────────────────────────────────

/// Tests never touch the real data directory: under `cfg(test)` the records go where
/// `test_dir` says, or nowhere.
#[cfg(test)]
pub(crate) fn test_dir() -> &'static Mutex<Option<PathBuf>> {
    static D: Mutex<Option<PathBuf>> = Mutex::new(None);
    &D
}

/// Runs `f` with the records going to `dir`, alone: the folder is process-wide.
#[cfg(test)]
pub(crate) fn with_test_dir<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    static L: Mutex<()> = Mutex::new(());
    let _g = L.lock().unwrap_or_else(|e| e.into_inner());
    *test_dir().lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    let r = f();
    *test_dir().lock().unwrap_or_else(|e| e.into_inner()) = None;
    r
}

/// `<data dir>/jobs`, or `None` without a data directory.
pub fn jobs_dir() -> Option<PathBuf> {
    #[cfg(test)]
    return test_dir().lock().unwrap_or_else(|e| e.into_inner()).clone();
    #[cfg(not(test))]
    crate::remote::store::data_dir().map(|d| d.join("jobs"))
}

pub fn write_record(dir: &Path, job_id: Uuid, record: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| crate::state_io::io_error("job summaries", dir, e))?;
    let tmp = dir.join(format!("{job_id}.json.tmp"));
    let dst = dir.join(format!("{job_id}.json"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(record).unwrap_or_default())?;
    // Same directory: a rename, never across mounts.
    std::fs::rename(&tmp, &dst)?;
    rotate(dir, MAX_SUMMARIES);
    Ok(())
}

fn record_files(dir: &Path) -> Vec<(u64, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(u64, PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.parse::<Uuid>().is_ok())
        })
        .map(|p| {
            let t = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos() as u64);
            (t, p)
        })
        .collect();
    // Newest first; the name breaks a tie so the order is stable.
    files.sort_by(|a, b| b.cmp(a));
    files
}

/// Keeps the newest `keep` records.
pub fn rotate(dir: &Path, keep: usize) {
    for (_, p) in record_files(dir).into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

pub fn read_record(dir: &Path, job_id: Uuid) -> Option<Value> {
    let b = std::fs::read(dir.join(format!("{job_id}.json"))).ok()?;
    serde_json::from_slice(&b).ok()
}

/// The newest `limit` records, newest first.
pub fn list_records(dir: &Path, limit: usize) -> Vec<Value> {
    record_files(dir)
        .into_iter()
        .filter_map(|(_, p)| std::fs::read(p).ok())
        .filter_map(|b| serde_json::from_slice::<Value>(&b).ok())
        .take(limit)
        .collect()
}

// ─── Counters (GET /api/metrics) ─────────────────────────────────────────────

#[derive(Default)]
pub struct Metrics {
    jobs: Mutex<HashMap<(String, &'static str), u64>>,
    bytes: AtomicU64,
    stalls: AtomicU64,
    cross_device: AtomicU64,
}

impl Metrics {
    pub fn record(&self, kind: &str, rec: &Value) {
        let result = match rec.get("result").and_then(Value::as_str) {
            Some("done") => "done",
            Some("cancelled") => "cancelled",
            _ => "failed",
        };
        *self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((kind.to_string(), result))
            .or_insert(0) += 1;
        self.bytes
            .fetch_add(u64_of(rec, "bytes"), Ordering::Relaxed);
        match rec.get("code").and_then(Value::as_str) {
            Some("ava1_stalled") => {
                self.stalls.fetch_add(1, Ordering::Relaxed);
            }
            Some("ava1_commit_cross_device") => {
                self.cross_device.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# HELP ps5upload_jobs_total Finished transfer jobs since the engine started.\n",
        );
        out.push_str("# TYPE ps5upload_jobs_total counter\n");
        let mut rows: Vec<_> = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|((k, r), n)| (k.clone(), *r, *n))
            .collect();
        rows.sort();
        for (kind, result, n) in rows {
            out.push_str(&format!(
                "ps5upload_jobs_total{{kind=\"{}\",result=\"{result}\"}} {n}\n",
                scrub_segment(&kind)
            ));
        }
        for (name, help, v) in [
            (
                "ps5upload_job_bytes_total",
                "Payload bytes of finished jobs.",
                self.bytes.load(Ordering::Relaxed),
            ),
            (
                "ps5upload_job_stalls_total",
                "Jobs that ended because the console made no durable progress.",
                self.stalls.load(Ordering::Relaxed),
            ),
            (
                "ps5upload_job_cross_device_total",
                "Jobs the console refused because the destination is on another drive.",
                self.cross_device.load(Ordering::Relaxed),
            ),
        ] {
            out.push_str(&format!(
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {v}\n"
            ));
        }
        out
    }
}

pub fn metrics() -> &'static Metrics {
    static M: OnceLock<Metrics> = OnceLock::new();
    M.get_or_init(Metrics::default)
}

// ─── HTTP ────────────────────────────────────────────────────────────────────

use axum::extract::{Path as AxumPath, Query};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;

#[derive(serde::Deserialize)]
pub(crate) struct ListQuery {
    limit: Option<usize>,
}

/// `GET /api/jobs/{id}/summary`.
pub(crate) async fn summary_handler(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let Ok(uuid) = id.parse::<Uuid>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok": false, "error": "invalid job id"})),
        )
            .into_response();
    };
    match jobs_dir().and_then(|d| read_record(&d, uuid)) {
        Some(r) => (StatusCode::OK, Json(r)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "no summary for this job (it is still running, or was not recorded)"})),
        )
            .into_response(),
    }
}

/// `GET /api/jobs/summaries?limit=` (default 20, at most `MAX_SUMMARIES`), newest first.
pub(crate) async fn summaries_handler(Query(q): Query<ListQuery>) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(20).clamp(1, MAX_SUMMARIES);
    let list = jobs_dir()
        .map(|d| list_records(&d, limit))
        .unwrap_or_default();
    Json(json!({ "summaries": list }))
}

/// `GET /api/metrics`.
pub(crate) async fn metrics_handler() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics().render(),
    )
}

#[cfg(test)]
mod tests;
