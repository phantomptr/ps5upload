// Release-mode on Windows: no console window. The engine is a daemon
// spawned as a child of the Tauri desktop exe, which pipes the
// engine's stdout/stderr back via `pipe_tagged`. Without this
// attribute Windows allocates a fresh console for the engine every
// time the desktop app starts it, flashing a terminal window next to
// the UI. Debug builds keep the default (console) subsystem so
// `cargo run -p ps5upload-engine` still shows log output in the
// terminal for local diagnostics.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

//! ps5upload-engine — local HTTP service that drives FTX2 transfers.
//!
//! Listens on 127.0.0.1:19113 by default (set PS5UPLOAD_ENGINE_PORT env var to override).
//! Historical note: this was `9114` through 2.1.x, but `9114` is also the
//! PS5-payload management port. The two live on different machines so
//! no real collision — but the shared number confused users and logs.
//! PS5 address defaults to 192.168.137.2:9113 (set PS5_ADDR to override).
//!
//! API
//! ───
//!   GET  /                            → dashboard UI (HTML)
//!   GET  /api/ps5/status              → PS5 runtime STATUS_ACK body (JSON)
//!   POST /api/transfer/file           → start single-file transfer job
//!   POST /api/transfer/dir            → start directory transfer job
//!   POST /api/transfer/file-list      → start multi-file transfer from explicit list
//!   GET  /api/jobs/{id}               → poll job status/result
//!   GET  /api/jobs                    → list all jobs (summary)
//!   GET  /api/events                  → SSE stream of job state changes
//!   POST /api/ps5/cleanup             → recursively remove a path under PS5 allowlist
//!   GET  /api/ps5/volumes             → list storage volumes detected by the payload
//!   GET  /api/ps5/list-dir?path=...   → list immediate children of a directory on PS5

mod engine_log;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use ftx2_proto::FrameType;
use ps5upload_core::{
    cleanup::{cleanup_path, CleanupResult},
    connection::Connection,
    download::{download_to_local, enumerate_download_set, DownloadKind},
    fs_ops::{
        fs_chmod, fs_copy_with_op_id, fs_delete, fs_mkdir, fs_mount, fs_move_with_timeout,
        fs_op_cancel, fs_op_status, fs_read, fs_unmount, list_dir, reconcile, walk_local_inventory,
        DirListing, ListDirOptions, MountResult, ReconcileFile, ReconcileMode, ReconcilePlan,
    },
    game_meta::parse_param_json_bytes,
    hw::{
        hw_info, hw_power, hw_set_fan_threshold, hw_temps, proc_list, HwInfo, HwPower, HwTemps,
        ProcList,
    },
    transfer::{
        transfer_dir_resumable, transfer_file_list_resumable, transfer_file_path_resumable,
        FileListEntry, TransferConfig, TX_FLAG_RESUME,
    },
    volumes::{list_volumes, VolumeList},
};

/// Build a `TransferConfig` for the given address, applying `FTX2_INFLIGHT_SHARDS`
/// and `FTX2_INFLIGHT_BYTES` env overrides if set. Invalid values fall back
/// silently to defaults — this is a tuning lever, not a correctness gate.
fn make_transfer_config(addr: &str) -> TransferConfig {
    let mut cfg = TransferConfig::new(addr);
    if let Ok(v) = std::env::var("FTX2_INFLIGHT_SHARDS") {
        if let Ok(n) = v.parse::<usize>() {
            if n >= 1 {
                cfg.inflight_shards = n;
            }
        }
    }
    if let Ok(v) = std::env::var("FTX2_INFLIGHT_BYTES") {
        if let Ok(n) = v.parse::<usize>() {
            if n >= 1 {
                cfg.inflight_bytes = n;
            }
        }
    }
    if let Ok(v) = std::env::var("FTX2_PACK_SIZE") {
        if let Ok(n) = v.parse::<usize>() {
            cfg.pack_size = n; // 0 disables packing
        }
    }
    if let Ok(v) = std::env::var("FTX2_PACK_FILE_MAX") {
        if let Ok(n) = v.parse::<usize>() {
            cfg.pack_file_max = n;
        }
    }
    cfg
}
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;
use tokio_stream::{wrappers::BroadcastStream, StreamExt as _};
use uuid::Uuid;

// ─── Shared state ─────────────────────────────────────────────────────────────

/// One entry in a job's planned file list. Path is relative to
/// the upload's dest_root; size is source-side bytes (what will be
/// sent). For single-file uploads `rel_path` is just the basename.
#[derive(Debug, Clone, Serialize)]
struct PlannedFile {
    rel_path: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum JobState {
    Running {
        started_at_ms: u64,
        /// Bytes sent so far. Updated on a timer (200 ms) that reads the
        /// shared AtomicU64 the shard loop increments in
        /// `ps5upload_core::TransferConfig::progress_bytes`. Lets the UI
        /// render a real progress bar + speed + ETA during a running
        /// transfer, instead of guessing from elapsed time alone.
        #[serde(default)]
        bytes_sent: u64,
        /// Total bytes expected for this job (source size). 0 if
        /// unknown at job start (should only happen for transfers that
        /// fail to stat the source, which would error before Running).
        #[serde(default)]
        total_bytes: u64,
        /// Ordered list of files this job will send. Shipped once on
        /// the first Running tick so the UI can render per-file status.
        /// For folder uploads this is the planned delta (reconcile) or
        /// the full tree walk (plain dir). For single files / file-list
        /// uploads, it's the requested file(s). Empty-by-default keeps
        /// wire size small for small jobs where the list isn't useful.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<PlannedFile>,
        /// Files already present on the PS5 that were skipped this run
        /// (reconcile mode only). 0 for non-reconcile uploads.
        #[serde(default)]
        skipped_files: u64,
        /// Bytes those skipped files represent. 0 for non-reconcile.
        #[serde(default)]
        skipped_bytes: u64,
    },
    Done {
        started_at_ms: u64,
        completed_at_ms: u64,
        elapsed_ms: u64,
        tx_id_hex: String,
        shards_sent: u64,
        bytes_sent: u64,
        dest: String,
        /// File count + skipped count for the summary card. `files_sent`
        /// excludes skipped files; `skipped_files` counts what reconcile
        /// mode found already-present on the PS5.
        #[serde(default)]
        files_sent: u64,
        #[serde(default)]
        skipped_files: u64,
        #[serde(default)]
        skipped_bytes: u64,
        /// Full COMMIT_TX_ACK body as a parsed JSON value — surfaces PS5-side
        /// timing (timing_us) and pool counters (pack_records etc.) in the
        /// engine job record so bench sweeps can persist them without an
        /// extra round-trip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        commit_ack: Option<serde_json::Value>,
    },
    Failed {
        started_at_ms: u64,
        completed_at_ms: u64,
        elapsed_ms: u64,
        error: String,
    },
}

/// Sum file sizes recursively under `root`. Used by transfer_dir_handler
/// to pre-compute `total_bytes` for progress rendering — transfer_dir
/// itself walks too but doesn't expose the total before sending starts,
/// and we want the progress bar to have a denominator on the first tick.
/// Errors are silently skipped (unreadable entries contribute 0); this
/// matches the permissive walk behavior elsewhere in core.
/// Swap a PS5 transfer-port addr (`ip:9113`) to the payload's management-
/// port addr (`ip:9114`). Used by reconcile: the public `addr` from the
/// client is the transfer-side address (because that's where the actual
/// upload goes), but the pre-flight FS_LIST_DIR / FS_HASH frames have
/// to hit the payload's management listener. The payload's management
/// port is a stable constant (see `PS5UPLOAD2_MGMT_PORT`).
fn mgmt_addr_for(transfer_addr: &str) -> String {
    match transfer_addr.rsplit_once(':') {
        Some((host, _)) => format!("{host}:9114"),
        None => format!("{transfer_addr}:9114"),
    }
}

fn mgmt_addr_or_default(addr: Option<String>, default_addr: &str) -> String {
    mgmt_addr_for(addr.as_deref().unwrap_or(default_addr))
}

/// Walk a directory and return `(total_bytes, planned_files)` — collects
/// rel_path + size
/// for each regular file so the UI can render per-file progress.
/// Errors silently skipped (matches the permissive walk elsewhere).
fn walk_plan(root: &std::path::Path, excludes: &[String]) -> (u64, Vec<PlannedFile>) {
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() {
                if ps5upload_core::excludes::is_excluded_strings(&path, excludes) {
                    continue;
                }
                if let Ok(m) = entry.metadata() {
                    let size = m.len();
                    total += size;
                    let rel = path.strip_prefix(root).unwrap_or(&path);
                    let rel_str = rel
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    out.push(PlannedFile {
                        rel_path: rel_str,
                        size,
                    });
                }
            }
        }
    }
    // Alphabetical order → stable rendering in the UI.
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    (total, out)
}

/// Spawn a 200 ms timer that republishes the Running job state with the
/// latest `bytes_sent` pulled from the shared progress counter the
/// transfer loop is incrementing. Returns the stop flag — caller sets
/// it to `true` **before** writing Done/Failed so a stale Running
/// update can't race past the terminal state. The timer exits promptly
/// once it sees the flag.
/// Sidecar data the ticker republishes each tick so Running state is
/// stable across polls — UI only has to read the latest snapshot.
///
/// Intentionally does NOT carry `files: Vec<PlannedFile>`. The handler
/// writes the files list once on the initial Running set_job; the
/// ticker then only updates the scalar counters, preserving whatever
/// files list the handler stored. For jobs with thousands of files
/// (large reconcile deltas) this drops the per-tick SSE payload from
/// O(files × path_len) back down to O(1), and the UI already caches
/// the files list on its first snapshot, so the visible behavior is
/// identical.
#[derive(Clone)]
struct TickerContext {
    started_at_ms: u64,
    total_bytes: u64,
    skipped_files: u64,
    skipped_bytes: u64,
}

fn spawn_progress_ticker(
    jobs: Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: broadcast::Sender<String>,
    job_id: Uuid,
    ctx: TickerContext,
    progress: Arc<AtomicU64>,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_tick = Arc::clone(&stop);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.tick().await; // consume the immediate first tick
        let mut last_bytes = u64::MAX; // forces a broadcast on the first real tick
        loop {
            interval.tick().await;
            // `Acquire` pairs with the `Release` store in
            // `TickerStopGuard::drop` (which fires when the
            // spawn_blocking closure ends, success or panic). On ARM
            // (Apple Silicon dev, AArch64 Linux CI), a plain `Relaxed`
            // store isn't guaranteed to be observed by this load
            // before the handler's subsequent `set_job(Done/Failed)`
            // completes. Without the pair, the ticker could wake
            // after the store but read stop=false from its own cache
            // and race past the guard into a Running write that
            // clobbers Done. On x86 TSO this never manifests; on
            // Apple Silicon single-file uploads completing in < 200
            // ms can trip it.
            if stop_for_tick.load(Ordering::Acquire) {
                break;
            }
            let bytes_sent = progress.load(Ordering::Relaxed);
            // Skip the HashMap mutation + SSE broadcast when the counter
            // hasn't moved. Saves the per-tick overhead (including JSON
            // serialization of potentially-thousands-of-files state) during
            // pre-transfer stalls — BEGIN_TX waiting on a slow-to-ack
            // payload, disk flush pauses mid-shard, etc.
            if bytes_sent == last_bytes {
                continue;
            }
            last_bytes = bytes_sent;
            // Mutate in place so the handler's initial `files` list is
            // preserved across ticks — we no longer carry it in the ctx.
            let maybe_snapshot = {
                // Poison-safe: a panic in any other lock holder must not
                // cascade through every subsequent ticker spawn. Same
                // pattern as engine_log.rs::record — the contained
                // HashMap is safe to read/mutate even after a partial
                // mutation.
                let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
                match g.get_mut(&job_id) {
                    Some(JobState::Running {
                        bytes_sent: b,
                        total_bytes: t,
                        started_at_ms: s,
                        skipped_files: sf,
                        skipped_bytes: sb,
                        ..
                    }) => {
                        *b = bytes_sent;
                        *t = ctx.total_bytes;
                        *s = ctx.started_at_ms;
                        *sf = ctx.skipped_files;
                        *sb = ctx.skipped_bytes;
                        // Clone once for the SSE broadcast path; the
                        // lock-held section stays short.
                        Some(g.get(&job_id).cloned())
                    }
                    // Job moved to terminal state (Done/Failed) — stop
                    // ticking to avoid writing over the terminal record.
                    _ => None,
                }
            };
            match maybe_snapshot {
                Some(Some(state)) => {
                    let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": state });
                    let _ = events_tx.send(msg.to_string());
                }
                _ => break,
            }
        }
    });
    stop
}

/// RAII guard that flips the ticker's stop flag when dropped, so a
/// panic between `spawn_progress_ticker` and the handler's manual
/// `stop_ticker.store(true)` doesn't leak the spawned tokio task
/// forever (the ticker would otherwise loop every 200 ms forever,
/// dirtying job state for a job that's gone).
///
/// Usage:
///   let stop = spawn_progress_ticker(...);
///   let _stop_guard = TickerStopGuard::new(stop);
///   // ... transfer work that may panic ...
///   // _stop_guard's Drop fires regardless of success/panic path.
struct TickerStopGuard(Arc<AtomicBool>);

impl TickerStopGuard {
    fn new(stop: Arc<AtomicBool>) -> Self {
        TickerStopGuard(stop)
    }
}

impl Drop for TickerStopGuard {
    fn drop(&mut self) {
        // Release ordering matches the Acquire in the ticker's stop
        // check (see comment at the load site). On Apple Silicon
        // a Relaxed store here would not be guaranteed to be
        // observed before the panic-unwind unmounts subsequent
        // shared state.
        self.0.store(true, Ordering::Release);
    }
}

/// RAII guard that transitions a job to Failed on Drop unless the
/// caller explicitly calls `mark_succeeded()` first.
///
/// Without this, a panic inside a `spawn_blocking` transfer closure
/// (e.g. an unwrap on a None deep inside the path-resumable core)
/// would leave the job map record stuck on `Running` forever — the
/// Tauri client would poll the job status and see Running with a
/// frozen `bytes_sent` counter, with no terminal transition to give
/// the UI a clear failure to surface. The TickerStopGuard handles
/// the *ticker* leak; this guard handles the *job state* leak.
///
/// Lock acquisition uses the same poison-safe pattern as elsewhere
/// (set_job → jobs.lock().unwrap_or_else(...)) so a Drop running
/// during panic-unwind doesn't double-panic on a poisoned mutex.
///
/// Usage:
///   let mut fail_guard = JobFailOnDropGuard::new(...);
///   let _stop_guard = TickerStopGuard::new(stop_ticker);
///   // ... work that may panic ...
///   match result { ... set_job(Done|Failed) ... };
///   fail_guard.mark_succeeded();   // <-- only after explicit set_job
struct JobFailOnDropGuard {
    jobs: Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: broadcast::Sender<String>,
    job_id: Uuid,
    started_at_ms: u64,
    succeeded: bool,
}

impl JobFailOnDropGuard {
    fn new(
        jobs: Arc<Mutex<HashMap<Uuid, JobState>>>,
        events_tx: broadcast::Sender<String>,
        job_id: Uuid,
        started_at_ms: u64,
    ) -> Self {
        JobFailOnDropGuard {
            jobs,
            events_tx,
            job_id,
            started_at_ms,
            succeeded: false,
        }
    }

    fn mark_succeeded(&mut self) {
        self.succeeded = true;
    }
}

impl Drop for JobFailOnDropGuard {
    fn drop(&mut self) {
        if self.succeeded {
            return;
        }
        let completed_at_ms = now_ms();
        set_job(
            &self.jobs,
            &self.events_tx,
            self.job_id,
            JobState::Failed {
                started_at_ms: self.started_at_ms,
                completed_at_ms,
                elapsed_ms: completed_at_ms.saturating_sub(self.started_at_ms),
                // Generic message — the actual panic payload is
                // already on stderr via Tokio's default panic
                // handler. Surfacing the full panic message here
                // would require std::panic::catch_unwind around the
                // whole closure, which adds complexity for marginal
                // user-facing benefit.
                error: "engine task panicked (see engine logs)".to_string(),
            },
        );
    }
}

#[derive(Clone)]
struct AppState {
    jobs: Arc<Mutex<HashMap<Uuid, JobState>>>,
    default_ps5_addr: String,
    events_tx: broadcast::Sender<String>,
}

/// Cap on the jobs map. Without a bound the engine would accumulate
/// every Done/Failed record forever, leaking ~100 B per completed
/// upload. 256 is enough for any realistic session history while
/// keeping the footprint bounded (~25 KB worst case including the
/// Vec<PlannedFile> children). Running jobs are never evicted — only
/// terminal states (Done/Failed) are dropped to make room.
const JOBS_MAP_CAP: usize = 256;

fn evict_oldest_terminal(jobs: &mut HashMap<Uuid, JobState>) {
    // Find the terminal entry with the smallest started_at_ms. If
    // the map is full of Running jobs (shouldn't normally happen),
    // do nothing — we'd rather briefly exceed the cap than drop a
    // live transfer's status.
    let victim = jobs
        .iter()
        .filter_map(|(id, s)| match s {
            JobState::Done { started_at_ms, .. } | JobState::Failed { started_at_ms, .. } => {
                Some((*id, *started_at_ms))
            }
            _ => None,
        })
        .min_by_key(|(_, t)| *t)
        .map(|(id, _)| id);
    if let Some(id) = victim {
        jobs.remove(&id);
    }
}

/// Update a job's state and broadcast the change over SSE.
fn set_job(
    jobs: &Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: &broadcast::Sender<String>,
    job_id: Uuid,
    state: JobState,
) {
    {
        let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
        if !g.contains_key(&job_id) && g.len() >= JOBS_MAP_CAP {
            evict_oldest_terminal(&mut g);
        }
        g.insert(job_id, state.clone());
    }
    let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": state });
    let _ = events_tx.send(msg.to_string());
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

// ─── Request / response types ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct TransferFileReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest: String,
    src: String,
}

#[derive(Deserialize)]
struct TransferDirReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    src_dir: String,
    #[serde(default)]
    excludes: Vec<String>,
}

#[derive(Deserialize)]
struct FileListEntryReq {
    src: String,
    dest: String,
}

#[derive(Deserialize)]
struct TransferFileListReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    files: Vec<FileListEntryReq>,
}

#[derive(Deserialize)]
struct TransferDirReconcileReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    src_dir: String,
    /// "fast" = size-only equality (default), "safe" = size + BLAKE3 hash.
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    excludes: Vec<String>,
}

#[derive(Serialize)]
struct JobCreated {
    job_id: String,
}

#[derive(Deserialize)]
struct AddrQuery {
    addr: Option<String>,
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_or_random_tx_id(hex: Option<&str>) -> anyhow::Result<[u8; 16]> {
    match hex {
        Some(h) => {
            if h.len() != 32 {
                anyhow::bail!("tx_id must be 32 hex chars");
            }
            let mut out = [0u8; 16];
            for (i, chunk) in h.as_bytes().chunks(2).enumerate() {
                let hi = hex_val(chunk[0])?;
                let lo = hex_val(chunk[1])?;
                out[i] = (hi << 4) | lo;
            }
            Ok(out)
        }
        None => Ok(*Uuid::new_v4().as_bytes()),
    }
}

fn hex_val(b: u8) -> anyhow::Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + b - b'a'),
        b'A'..=b'F' => Ok(10 + b - b'A'),
        _ => anyhow::bail!("invalid hex char: {}", b as char),
    }
}

fn json_err(code: StatusCode, msg: impl Into<String>) -> impl IntoResponse {
    (code, Json(serde_json::json!({ "error": msg.into() })))
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// GET / — dashboard UI
async fn ui_handler() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../static/index.html"),
    )
}

/// GET /api/events — SSE stream of job state changes
async fn events_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(data) => Some(Ok(Event::default().data(data))),
        Err(_) => None, // lagged or channel closed — skip
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct CleanupReq {
    addr: Option<String>,
    path: String,
}

/// POST /api/ps5/cleanup — asks the PS5 payload to rm -rf a path.
/// Safe: payload refuses paths outside `/data/ps5upload-{bench,sweep,smoke}/`.
async fn ps5_cleanup(
    State(state): State<AppState>,
    Json(req): Json<CleanupReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let path = req.path.clone();
    let started = std::time::Instant::now();
    crate::log_info!("cleanup: addr={addr} path={path}");
    let result: Result<CleanupResult, anyhow::Error> =
        tokio::task::spawn_blocking(move || cleanup_path(&addr, &path))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(r) => {
            crate::log_info!(
                "cleanup ok: removed {} files / {} dirs in {} ms",
                r.removed_files,
                r.removed_dirs,
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "cleanup failed in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

#[derive(Deserialize)]
struct ListDirQuery {
    addr: Option<String>,
    path: String,
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
}

/// GET /api/ps5/list-dir?path=/data&offset=0&limit=256&addr=IP:PORT
async fn ps5_list_dir(
    State(state): State<AppState>,
    Query(q): Query<ListDirQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let path = q.path.clone();
    let opts = ListDirOptions {
        offset: q.offset.unwrap_or(0),
        limit: q.limit.unwrap_or(256),
    };
    let result: Result<DirListing, anyhow::Error> =
        tokio::task::spawn_blocking(move || list_dir(&addr, &path, opts))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ─── Destructive FS ops ─────────────────────────────────────────────────────
//
// All four share shape: take a JSON body, call the core fs_ops
// function in a blocking thread, return empty 200 or 502 with error.

#[derive(Deserialize)]
struct FsPathReq {
    addr: Option<String>,
    path: String,
}

#[derive(Deserialize)]
struct FsMoveReq {
    addr: Option<String>,
    from: String,
    to: String,
    /// Optional unique 64-bit identifier the client generates so it
    /// can poll progress (`/api/ps5/fs/op-status`) and cancel
    /// (`/api/ps5/fs/op-cancel`) the in-flight copy. The engine
    /// stamps this into the FS_COPY frame's trace_id so the payload's
    /// in-flight ops table is keyed on it. Pass 0 (or omit) when no
    /// progress/cancel is needed; the operation runs the same way
    /// the old endpoint did.
    #[serde(default)]
    op_id: u64,
}

#[derive(Deserialize)]
struct FsChmodReq {
    addr: Option<String>,
    path: String,
    /// Octal string like "0777". String (not u32) so JSON parsers
    /// don't coerce to decimal and change the meaning.
    mode: String,
    #[serde(default)]
    recursive: bool,
}

async fn ps5_fs_delete(
    State(state): State<AppState>,
    Json(req): Json<FsPathReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let path = req.path;
    let started = std::time::Instant::now();
    crate::log_info!("fs_delete: addr={addr} path={path}");
    let path_for_log = path.clone();
    match tokio::task::spawn_blocking(move || fs_delete(&addr, &path))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "fs_delete ok: {path_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "fs_delete failed: {path_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_fs_move(
    State(state): State<AppState>,
    Json(req): Json<FsMoveReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let from = req.from;
    let to = req.to;
    let started = std::time::Instant::now();
    crate::log_info!("fs_move: addr={addr} from={from} to={to}");
    let from_for_log = from.clone();
    let to_for_log = to.clone();
    // 1-hour deadline: most fs_move calls return in milliseconds
    // (rename(2) is metadata-only), but cross-volume moves that the
    // payload retries via copy-then-delete inherit the same long
    // bound as fs_copy. The default 30 s socket timeout would fire
    // mid-copy of any multi-GiB file and surface as the cryptic "read
    // frame header" 502.
    let io_timeout = std::time::Duration::from_secs(60 * 60);
    match tokio::task::spawn_blocking(move || {
        fs_move_with_timeout(&addr, &from, &to, Some(io_timeout))
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "fs_move ok: {from_for_log} -> {to_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "fs_move failed: {from_for_log} -> {to_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_fs_copy(
    State(state): State<AppState>,
    Json(req): Json<FsMoveReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let from = req.from;
    let to = req.to;
    let started = std::time::Instant::now();
    crate::log_info!("fs_copy: addr={addr} from={from} to={to}");
    let from_for_log = from.clone();
    let to_for_log = to.clone();
    // 1-hour deadline: fs_copy is a single-shot RPC where the payload
    // performs the entire recursive copy and only sends FS_COPY_ACK
    // at the end. A 35 GiB game image on PS5 UFS takes minutes to
    // copy; the previous 30 s socket timeout fired mid-copy and
    // surfaced as "read frame header" 502 with no progress visible.
    // Real progress reporting would require a payload protocol
    // change (per-shard progress events); for now the long deadline
    // at least lets the operation complete.
    let io_timeout = std::time::Duration::from_secs(60 * 60);
    let op_id = req.op_id;
    match tokio::task::spawn_blocking(move || {
        fs_copy_with_op_id(&addr, &from, &to, op_id, Some(io_timeout))
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "fs_copy ok: {from_for_log} -> {to_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            // Cancellation surfaces as `Err("cancelled")` from
            // fs_copy_with_op_id. Translate to a distinct 499-style
            // response so the client can tell it apart from a real
            // FS_COPY failure (different banner, different log).
            let msg = e.to_string();
            if msg == "cancelled" {
                crate::log_info!(
                    "fs_copy cancelled: {from_for_log} -> {to_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
                return json_err(StatusCode::CONFLICT, "cancelled").into_response();
            }
            crate::log_warn!(
                "fs_copy failed: {from_for_log} -> {to_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, msg).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct FsMountReq {
    addr: Option<String>,
    image_path: String,
    #[serde(default)]
    mount_name: Option<String>,
}

async fn ps5_fs_mount(
    State(state): State<AppState>,
    Json(req): Json<FsMountReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let image_path = req.image_path;
    let mount_name = req.mount_name;
    let started = std::time::Instant::now();
    crate::log_info!(
        "fs_mount: addr={addr} image_path={image_path} mount_name={:?}",
        mount_name
    );
    let image_for_log = image_path.clone();
    let result: Result<MountResult, anyhow::Error> =
        tokio::task::spawn_blocking(move || fs_mount(&addr, &image_path, mount_name.as_deref()))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match result {
        Ok(r) => {
            crate::log_info!(
                "fs_mount ok: {image_for_log} -> {} ({}, {}) in {} ms",
                r.mount_point,
                r.dev_node,
                r.fstype,
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "fs_mount failed: {image_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct FsUnmountReq {
    addr: Option<String>,
    mount_point: String,
}

async fn ps5_fs_unmount(
    State(state): State<AppState>,
    Json(req): Json<FsUnmountReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let mount_point = req.mount_point;
    let started = std::time::Instant::now();
    crate::log_info!("fs_unmount: addr={addr} mount_point={mount_point}");
    let mp_for_log = mount_point.clone();
    match tokio::task::spawn_blocking(move || fs_unmount(&addr, &mount_point))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "fs_unmount ok: {mp_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "fs_unmount failed: {mp_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_fs_chmod(
    State(state): State<AppState>,
    Json(req): Json<FsChmodReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let path = req.path;
    let mode = req.mode;
    let recursive = req.recursive;
    let started = std::time::Instant::now();
    crate::log_info!("fs_chmod: addr={addr} path={path} mode={mode} recursive={recursive}");
    let path_for_log = path.clone();
    match tokio::task::spawn_blocking(move || fs_chmod(&addr, &path, &mode, recursive))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "fs_chmod ok: {path_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "fs_chmod failed: {path_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

/// GET /api/ps5/fs/op-status?op_id=N&addr=...
///
/// Snapshot the in-flight FS op identified by `op_id` (a 64-bit
/// value the client generated and passed to fs/copy). Used by the
/// client to drive a per-byte progress bar + speed indicator while
/// the FS_COPY HTTP call is still blocked. Returns 404 if the op
/// isn't currently registered (already finished or never started).
#[derive(Deserialize)]
struct FsOpStatusQuery {
    op_id: u64,
    addr: Option<String>,
}

async fn ps5_fs_op_status(
    State(state): State<AppState>,
    Query(q): Query<FsOpStatusQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let op_id = q.op_id;
    match tokio::task::spawn_blocking(move || fs_op_status(&addr, op_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(snap) => {
            if !snap.found {
                return json_err(StatusCode::NOT_FOUND, "op_id not in flight").into_response();
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "op_id": snap.op_id,
                    "kind": snap.kind,
                    "from": snap.from,
                    "to": snap.to,
                    "total_bytes": snap.total_bytes,
                    "bytes_copied": snap.bytes_copied,
                    "cancel_requested": snap.cancel_requested,
                })),
            )
                .into_response()
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/fs/op-cancel — body `{"op_id": N, "addr": "..."}`.
///
/// Asks the payload to set the cancel flag on op N. The actual
/// cp_rf loop checks the flag every 4 MiB, so a multi-GiB copy
/// stops within ~one disk-IO worth of bytes (sub-second on PS5
/// NVMe). Returns `{"cancelled": true}` on success, `{"cancelled":
/// false}` if the op_id wasn't recognized (already finished or
/// never registered — both treated as success from the client's
/// perspective: "the op you wanted to cancel isn't running").
#[derive(Deserialize)]
struct FsOpCancelReq {
    op_id: u64,
    addr: Option<String>,
}

async fn ps5_fs_op_cancel(
    State(state): State<AppState>,
    Json(req): Json<FsOpCancelReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let op_id = req.op_id;
    crate::log_info!("fs_op_cancel: op_id={op_id}");
    match tokio::task::spawn_blocking(move || fs_op_cancel(&addr, op_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(found) => (
            StatusCode::OK,
            Json(serde_json::json!({ "cancelled": found })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_fs_mkdir(
    State(state): State<AppState>,
    Json(req): Json<FsPathReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let path = req.path;
    let started = std::time::Instant::now();
    crate::log_info!("fs_mkdir: addr={addr} path={path}");
    let path_for_log = path.clone();
    match tokio::task::spawn_blocking(move || fs_mkdir(&addr, &path))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "fs_mkdir ok: {path_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "fs_mkdir failed: {path_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

// ─── Hardware monitoring ─────────────────────────────────────────────

async fn ps5_hw_info(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<HwInfo, anyhow::Error> = tokio::task::spawn_blocking(move || hw_info(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_hw_temps(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<HwTemps, anyhow::Error> = tokio::task::spawn_blocking(move || hw_temps(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_hw_power(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<HwPower, anyhow::Error> = tokio::task::spawn_blocking(move || hw_power(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// Snapshot of running PS5 processes. The payload walks `allproc` via
/// kernel R/W and returns JSON directly; we pass it through largely
/// untouched after a small reshape into the typed `ProcList` shape.
async fn ps5_proc_list(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ProcList, anyhow::Error> = tokio::task::spawn_blocking(move || proc_list(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct FanThresholdReq {
    addr: Option<String>,
    threshold_c: u8,
}

/// POST /api/ps5/hw/fan-threshold
/// Body: `{ "addr": "IP:MGMT_PORT", "threshold_c": 60 }`
///
/// Out-of-range inputs return 400 with the specific range error —
/// BAD_GATEWAY is reserved for payload/transport failures.
async fn ps5_hw_set_fan_threshold(
    State(state): State<AppState>,
    Json(q): Json<FanThresholdReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let threshold = q.threshold_c;
    crate::log_info!("hw_set_fan_threshold: addr={addr} threshold_c={threshold}");
    match tokio::task::spawn_blocking(move || hw_set_fan_threshold(&addr, threshold))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!("hw_set_fan_threshold ok: threshold_c={threshold}");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "threshold_c": threshold })),
            )
                .into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            crate::log_warn!("hw_set_fan_threshold failed (threshold_c={threshold}): {msg}");
            // Client-side validation failures (range check) → 400 so
            // the UI can distinguish them from true payload/network errors.
            let code = if msg.contains("safe range") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::BAD_GATEWAY
            };
            json_err(code, msg).into_response()
        }
    }
}

// ─── Game metadata (title + cover) ───────────────────────────────────────────

/// GET /api/ps5/game-meta?addr=IP:MGMT_PORT&path=/mnt/ext1/homebrew/FooBar
///
/// Reads `sce_sys/param.json` via FS_READ on the PS5 and returns the
/// localized title, title-id, content-id, and version fields. Used by
/// the Library screen to upgrade plain folder names into
/// "My Title · PPSA00000" style labels without needing a local
/// copy of the game. Failures (no param.json, bad JSON, path denied)
/// return 404 so the UI can fall back to the folder name cleanly.
#[derive(Debug, serde::Deserialize)]
struct GameMetaQuery {
    addr: Option<String>,
    path: String,
}

#[derive(Debug, serde::Serialize)]
struct GameMetaResponse {
    title: Option<String>,
    title_id: Option<String>,
    content_id: Option<String>,
    content_version: Option<String>,
    application_category_type: Option<i64>,
    /// True iff `sce_sys/icon0.png` exists and is non-empty on the PS5.
    /// The UI uses this to decide whether to render an <img> pointing at
    /// /api/ps5/game-icon — skipping the request for folders without an
    /// icon avoids a pointless 404 round-trip.
    has_icon: bool,
}

/// Reject `path` inputs that can't sanely resolve to a game folder on
/// the PS5: non-absolute, containing `..`, or empty. The payload's
/// `is_path_allowed` catches these too, but failing fast here means a
/// tighter error message and no wasted round-trip.
fn validate_meta_path(path: &str) -> Result<(), (StatusCode, String)> {
    if path.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "path is required".into()));
    }
    if !path.starts_with('/') {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must be absolute (start with /)".into(),
        ));
    }
    if path.contains("..") {
        return Err((StatusCode::BAD_REQUEST, "path must not contain '..'".into()));
    }
    Ok(())
}

async fn ps5_game_meta(
    State(state): State<AppState>,
    Query(q): Query<GameMetaQuery>,
) -> impl IntoResponse {
    if let Err((code, msg)) = validate_meta_path(&q.path) {
        return json_err(code, msg).into_response();
    }
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let path = q.path;
    let result: Result<GameMetaResponse, anyhow::Error> = tokio::task::spawn_blocking(move || {
        // param.json — tiny (~1 KiB for real PS5 titles), just pull the
        // whole thing in one FS_READ. If the file isn't there, return
        // a default response; the UI will still show the folder name.
        let param_path = format!("{}/sce_sys/param.json", path.trim_end_matches('/'));
        let (title, title_id, content_id, content_version, application_category_type) =
            match fs_read(&addr, &param_path, 0, 256 * 1024) {
                Ok(bytes) if !bytes.is_empty() => match parse_param_json_bytes(&bytes) {
                    Ok(r) => (
                        r.title,
                        r.title_id,
                        r.content_id,
                        r.content_version,
                        r.application_category_type,
                    ),
                    Err(_) => (None, None, None, None, None),
                },
                _ => (None, None, None, None, None),
            };
        // icon0.png probe — read the first byte to confirm it exists.
        // Avoids pulling the full image just to know whether to set
        // `has_icon`. Errors (path denied, not found) treated as "no icon".
        let icon_path = format!("{}/sce_sys/icon0.png", path.trim_end_matches('/'));
        let has_icon = fs_read(&addr, &icon_path, 0, 1)
            .map(|b| !b.is_empty())
            .unwrap_or(false);
        Ok(GameMetaResponse {
            title,
            title_id,
            content_id,
            content_version,
            application_category_type,
            has_icon,
        })
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|inner| inner);
    match result {
        Ok(r) => (StatusCode::OK, Json(r)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/game-icon?addr=IP:MGMT_PORT&path=/mnt/ext1/homebrew/FooBar
///
/// Streams the folder's `sce_sys/icon0.png` back as `image/png`. The
/// payload caps FS_READ at 2 MiB, comfortably above the largest icon0
/// we've observed (~700 KiB). Failures return 404 so `<img onerror>`
/// handlers can fall back to a placeholder without parsing a JSON body.
async fn ps5_game_icon(
    State(state): State<AppState>,
    Query(q): Query<GameMetaQuery>,
) -> impl IntoResponse {
    if let Err((code, msg)) = validate_meta_path(&q.path) {
        return (code, msg).into_response();
    }
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let path = q.path;
    let icon_path = format!("{}/sce_sys/icon0.png", path.trim_end_matches('/'));
    let result: Result<Vec<u8>, anyhow::Error> =
        tokio::task::spawn_blocking(move || fs_read(&addr, &icon_path, 0, 2 * 1024 * 1024))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(bytes) if !bytes.is_empty() => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                // Icons rarely change once uploaded — a 5-minute cache
                // keeps the Library scroll smooth across refreshes
                // without sticking forever on a stale file.
                (header::CACHE_CONTROL, "private, max-age=300"),
            ],
            bytes,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "no icon").into_response(),
    }
}

/// GET /api/ps5/volumes?addr=IP:PORT — enumerate mounted PS5 storage volumes.
async fn ps5_volumes(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let result: Result<VolumeList, anyhow::Error> =
        tokio::task::spawn_blocking(move || list_volumes(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/status?addr=IP:PORT
async fn ps5_status(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let result = tokio::task::spawn_blocking(move || {
        let mut c = Connection::connect(&addr)?;
        c.send_frame(FrameType::Status, b"")?;
        let (hdr, body) = c.recv_frame()?;
        let ft = hdr.frame_type().unwrap_or(FrameType::Error);
        if ft != FrameType::StatusAck {
            anyhow::bail!("expected STATUS_ACK, got {ft:?}");
        }
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        Ok::<_, anyhow::Error>(json)
    })
    .await;

    match result {
        Ok(Ok(json)) => (StatusCode::OK, Json(json)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// POST /api/transfer/file
async fn transfer_file_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferFileReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_file: job={job_id} addr={addr} src={} dest={}",
        req.src,
        req.dest
    );
    // Pre-stat the source — used both for the progress-bar denominator
    // and as a fail-fast check before we accept the job. Previously the
    // metadata error was silently swallowed (`unwrap_or(0)`), so a
    // missing source produced "Running, 0 bytes total" for several
    // seconds before the actual transfer attempt failed. Returning
    // 400 here surfaces the user error immediately at the API level.
    let total_bytes = match std::fs::metadata(&req.src) {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("source is not a regular file: {}", req.src),
            )
            .into_response();
        }
        Err(e) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("cannot read source {}: {e}", req.src),
            )
            .into_response();
        }
    };
    let src_basename = std::path::Path::new(&req.src)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| req.src.clone());
    let files = vec![PlannedFile {
        rel_path: src_basename,
        size: total_bytes,
    }];
    let progress = Arc::new(AtomicU64::new(0));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            started_at_ms,
            bytes_sent: 0,
            total_bytes,
            files: files.clone(),
            skipped_files: 0,
            skipped_bytes: 0,
        },
    );

    let jobs = Arc::clone(&state.jobs);
    let events_tx = state.events_tx.clone();
    let stop_ticker = spawn_progress_ticker(
        Arc::clone(&jobs),
        events_tx.clone(),
        job_id,
        ctx,
        Arc::clone(&progress),
    );

    tokio::task::spawn_blocking(move || {
        // Drops at closure end (success OR panic), stopping the
        // progress ticker. Without this, a panic in
        // transfer_file_path_resumable would leak the ticker task
        // forever, dirtying state for a finished job.
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        // Drops on panic-unwind and writes Failed to the job map so a
        // panicked transfer doesn't leave the record stuck on
        // Running. Explicitly mark_succeeded() at the end of the
        // closure once we've written our own terminal state.
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        cfg.progress_bytes = Some(Arc::clone(&progress));
        // Resume-on-drop for single-file uploads: 1 fresh attempt + 2
        // retries. Without this wrapper a connection hiccup on a multi-
        // GiB single-file upload (e.g. a .pkg / .ffpkg image) would be
        // unrecoverable without a full re-transfer.
        //
        // The path-based core reads one shard at a time. It avoids both
        // whole-file Vec allocation and mmap address-space/page-cache
        // failure modes that can look like OOM on Windows/Linux with
        // 50-100 GiB game images.
        let src_path = std::path::PathBuf::from(&req.src);
        let result = transfer_file_path_resumable(&cfg, tx_id, &req.dest, &src_path, 2);
        let files_sent_count: u64 = 1;
        let skipped_files_count: u64 = 0;
        let skipped_bytes_count: u64 = 0;
        match result {
            Ok(r) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: r.tx_id_hex,
                        shards_sent: r.shards_sent,
                        bytes_sent: r.bytes_sent,
                        dest: r.dest,
                        files_sent: files_sent_count,
                        skipped_files: skipped_files_count,
                        skipped_bytes: skipped_bytes_count,
                        commit_ack: serde_json::from_str(&r.commit_ack_body).ok(),
                    },
                )
            }
            Err(e) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: e.to_string(),
                    },
                )
            }
        }
        fail_guard.mark_succeeded();
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

/// POST /api/transfer/dir
async fn transfer_dir_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferDirReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    // A caller-supplied tx_id signals "reuse if already in the payload's
    // journal" — the cross-session resume flow. Engine-minted tx_ids
    // mean "fresh upload." Encode that into the initial BEGIN_TX flags
    // so the payload's BEGIN_TX branch picks the right outcome (adopt
    // vs fresh-allocate) instead of falling into RESTART on an
    // existing-entry collision.
    let caller_supplied_tx_id = req.tx_id.is_some();
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let initial_flags = if caller_supplied_tx_id {
        TX_FLAG_RESUME
    } else {
        0
    };

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_dir: job={job_id} addr={addr} src_dir={} dest_root={} resume={} excludes={}",
        req.src_dir,
        req.dest_root,
        caller_supplied_tx_id,
        req.excludes.len()
    );
    let (total_bytes, files) = walk_plan(std::path::Path::new(&req.src_dir), &req.excludes);
    let files_sent_count = files.len() as u64;
    let progress = Arc::new(AtomicU64::new(0));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            started_at_ms,
            bytes_sent: 0,
            total_bytes,
            files,
            skipped_files: 0,
            skipped_bytes: 0,
        },
    );

    let jobs = Arc::clone(&state.jobs);
    let events_tx = state.events_tx.clone();
    let stop_ticker = spawn_progress_ticker(
        Arc::clone(&jobs),
        events_tx.clone(),
        job_id,
        ctx,
        Arc::clone(&progress),
    );

    tokio::task::spawn_blocking(move || {
        // See ticker stop-guard rationale at the file-upload spawn site.
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        // 3 attempts = 1 fresh + 2 resumes. Matches the reconcile handler.
        // An Override upload used to give up on the first network blip;
        // this retry wrapper puts it on equal footing with Resume uploads.
        let result = transfer_dir_resumable(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.src_dir),
            2,
            initial_flags,
        );
        let skipped_files_count: u64 = 0;
        let skipped_bytes_count: u64 = 0;
        match result {
            Ok(r) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: r.tx_id_hex,
                        shards_sent: r.shards_sent,
                        bytes_sent: r.bytes_sent,
                        dest: r.dest,
                        files_sent: files_sent_count,
                        skipped_files: skipped_files_count,
                        skipped_bytes: skipped_bytes_count,
                        commit_ack: serde_json::from_str(&r.commit_ack_body).ok(),
                    },
                )
            }
            Err(e) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: e.to_string(),
                    },
                )
            }
        }
        fail_guard.mark_succeeded();
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

/// POST /api/transfer/file-list
async fn transfer_file_list_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferFileListReq>,
) -> impl IntoResponse {
    if req.files.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "files list is empty").into_response();
    }
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let caller_supplied_tx_id = req.tx_id.is_some();
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let initial_flags = if caller_supplied_tx_id {
        TX_FLAG_RESUME
    } else {
        0
    };

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    let entries: Vec<FileListEntry> = req
        .files
        .into_iter()
        .map(|f| FileListEntry {
            src: f.src,
            dest: f.dest,
        })
        .collect();
    // Sum source sizes now + build the planned file list so Running
    // has a denominator + per-file progress from the first tick.
    let files: Vec<PlannedFile> = entries
        .iter()
        .map(|e| {
            let size = std::fs::metadata(&e.src).map(|m| m.len()).unwrap_or(0);
            let rel = std::path::Path::new(&e.dest)
                .strip_prefix(std::path::Path::new(&req.dest_root))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| e.dest.clone())
                .replace(std::path::MAIN_SEPARATOR, "/");
            PlannedFile {
                rel_path: rel,
                size,
            }
        })
        .collect();
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    let files_sent_count = files.len() as u64;
    let progress = Arc::new(AtomicU64::new(0));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            started_at_ms,
            bytes_sent: 0,
            total_bytes,
            files,
            skipped_files: 0,
            skipped_bytes: 0,
        },
    );

    let jobs = Arc::clone(&state.jobs);
    let events_tx = state.events_tx.clone();
    let stop_ticker = spawn_progress_ticker(
        Arc::clone(&jobs),
        events_tx.clone(),
        job_id,
        ctx,
        Arc::clone(&progress),
    );

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        cfg.progress_bytes = Some(Arc::clone(&progress));
        // All transfer endpoints share the same 3-attempt resume policy
        // (1 fresh + 2 resumes). See `transfer_dir_handler` for rationale.
        let result =
            transfer_file_list_resumable(&cfg, tx_id, &req.dest_root, &entries, 2, initial_flags);
        let skipped_files_count: u64 = 0;
        let skipped_bytes_count: u64 = 0;
        match result {
            Ok(r) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: r.tx_id_hex,
                        shards_sent: r.shards_sent,
                        bytes_sent: r.bytes_sent,
                        dest: r.dest,
                        files_sent: files_sent_count,
                        skipped_files: skipped_files_count,
                        skipped_bytes: skipped_bytes_count,
                        commit_ack: serde_json::from_str(&r.commit_ack_body).ok(),
                    },
                )
            }
            Err(e) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: e.to_string(),
                    },
                )
            }
        }
        fail_guard.mark_succeeded();
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

// ─── Download (PS5 → host) ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct TransferDownloadReq {
    /// Transfer-port addr (`ip:9113`); we'll route to mgmt via
    /// `mgmt_addr_for` since downloads use FS_LIST_DIR + FS_READ.
    addr: Option<String>,
    /// Path on the PS5 to download. For `kind: "folder"` this is the
    /// root of the tree; for `kind: "file"` it's the file itself.
    src_path: String,
    /// Local directory the download lands inside. The remote
    /// basename is appended underneath this — so `dest_dir=/tmp/x`
    /// with `src_path=/data/foo` produces `/tmp/x/foo` (file or
    /// folder, mirroring the upload "one folder per title" rule).
    dest_dir: String,
    /// "file" or "folder". The caller already knows from context
    /// (Library/FileSystem row) so we trust the hint and skip a
    /// stat round-trip just to classify.
    kind: String,
}

/// POST /api/transfer/download — PS5 → host file/folder pull.
///
/// Mirrors the upload job machinery: returns a job_id immediately,
/// the heavy work runs on a blocking task, progress lands in the
/// shared bytes counter that the 200 ms ticker republishes through
/// the SSE stream the same way uploads do.
async fn transfer_download_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferDownloadReq>,
) -> impl IntoResponse {
    let mgmt_addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    crate::log_info!(
        "transfer_download: addr={mgmt_addr} src_path={} dest_dir={} kind={}",
        req.src_path,
        req.dest_dir,
        req.kind
    );
    let kind = match req.kind.as_str() {
        "file" => DownloadKind::File,
        "folder" => DownloadKind::Folder,
        other => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("kind must be 'file' or 'folder', got '{other}'"),
            )
            .into_response();
        }
    };
    // Reject malformed src_paths up-front. Without this check, a "/"
    // or empty source produced "<dest>/download" with the dest
    // basename derivation falling through to the unwrap_or default.
    // Better to surface the user's bad input now than silently
    // produce a confusingly-named output.
    let trimmed_src = req.src_path.trim_end_matches('/');
    if trimmed_src.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "src_path cannot be empty or '/'")
            .into_response();
    }
    if req.dest_dir.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "dest_dir cannot be empty").into_response();
    }

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();

    // Enumerate first so we have an honest total_bytes from tick #1.
    // Heavy enumeration only happens for huge folders (multi-thousand-
    // file game dirs); for single files this is one parent list_dir
    // call. Failing to enumerate at all is fatal — the user picked
    // something we can't see — so surface as a Failed job rather
    // than silently returning an empty manifest.
    let src_path_clone = req.src_path.clone();
    let mgmt_addr_for_enum = mgmt_addr.clone();
    let plan = match tokio::task::spawn_blocking(move || {
        enumerate_download_set(&mgmt_addr_for_enum, &src_path_clone, kind)
    })
    .await
    {
        Ok(Ok(m)) => m,
        Ok(Err(e)) => return json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        Err(e) => {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
    };
    let manifest = plan.manifest;
    let skipped_count = plan.skipped.len() as u64;
    if skipped_count > 0 {
        // Log skipped non-regular entries so users grepping engine.log
        // can see exactly which symlinks/special files weren't pulled.
        // Cap the log spam — millions of skips on a pathological tree
        // shouldn't fill the log file.
        let preview: Vec<_> = plan
            .skipped
            .iter()
            .take(20)
            .map(|s| format!("  {} ({})", s.remote_path, s.kind))
            .collect();
        crate::log_warn!(
            "download: skipped {} non-regular entries (only regular files are pulled). First {}:\n{}{}",
            skipped_count,
            preview.len(),
            preview.join("\n"),
            if plan.skipped.len() > preview.len() {
                format!("\n  … and {} more", plan.skipped.len() - preview.len())
            } else {
                String::new()
            },
        );
    }
    let total_bytes: u64 = manifest.iter().map(|e| e.size).sum();
    let files: Vec<PlannedFile> = manifest
        .iter()
        .map(|e| PlannedFile {
            rel_path: e.rel_path.clone(),
            size: e.size,
        })
        .collect();
    let files_count = files.len() as u64;

    let progress = Arc::new(AtomicU64::new(0));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            started_at_ms,
            bytes_sent: 0,
            total_bytes,
            files,
            skipped_files: 0,
            skipped_bytes: 0,
        },
    );

    let jobs = Arc::clone(&state.jobs);
    let events_tx = state.events_tx.clone();
    let stop_ticker = spawn_progress_ticker(
        Arc::clone(&jobs),
        events_tx.clone(),
        job_id,
        ctx,
        Arc::clone(&progress),
    );

    let dest_dir = std::path::PathBuf::from(req.dest_dir);
    // Append the remote basename so a single source produces a
    // single named output even when the user picked a parent dir
    // ("save to ~/Downloads" → "~/Downloads/MyGame"). The empty/
    // root-only check above guarantees `trimmed_src` and the rsplit
    // are non-empty, so the unwrap is safe.
    let basename = trimmed_src
        .rsplit('/')
        .next()
        .expect("src_path emptiness already validated");
    // Path-traversal guard. A src_path like `/data/foo/..` produces
    // basename `..`, and `dest_dir.join("..")` walks one level above
    // the host folder the user picked — files would land outside the
    // dialog-selected destination. Same risk for `/data/foo/.` (a
    // literal `.` segment on the host) and any backslash-bearing
    // remote name. Reject up-front rather than discover the escape
    // mid-write.
    if basename == "." || basename == ".." || basename.contains('/') || basename.contains('\\') {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!(
                "src_path produces an invalid destination basename ({basename:?}); refusing to download"
            ),
        )
        .into_response();
    }
    // Pre-flight the host destination so the user finds out about
    // a missing/un-writable directory immediately, not after the
    // job has been polling for several seconds. We require dest_dir
    // to exist and be a directory; create_dir_all happens later
    // per-file under dest_root, so we don't need to mkdir here.
    match std::fs::metadata(&dest_dir) {
        Ok(md) if md.is_dir() => {}
        Ok(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("dest_dir is not a directory: {}", dest_dir.display()),
            )
            .into_response();
        }
        Err(e) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("cannot access dest_dir {}: {e}", dest_dir.display()),
            )
            .into_response();
        }
    }
    let dest_root = dest_dir.join(basename);

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let result = download_to_local(&mgmt_addr, &dest_root, &manifest, Some(&progress));
        match result {
            Ok(bytes_written) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: String::new(),
                        shards_sent: 0,
                        bytes_sent: bytes_written,
                        dest: dest_root.to_string_lossy().to_string(),
                        files_sent: files_count,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(e) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: e.to_string(),
                    },
                );
            }
        }
        fail_guard.mark_succeeded();
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

/// POST /api/transfer/dir-reconcile
///
/// Resume-friendly directory upload: walks the destination tree on the
/// PS5, diffs against the local source by file size (Fast mode) or by
/// BLAKE3 hash (Safe mode), and uploads only the delta via the existing
/// `transfer_file_list` path. The job's `total_bytes` + progress bar
/// reflect the *delta* — what the user actually sees uploading.
///
/// Request body mirrors `TransferDirReq` plus an optional `mode`
/// ("fast"|"safe"; default "fast"). Response is the same `JobCreated`
/// shape as the other transfer handlers.
async fn transfer_dir_reconcile_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferDirReconcileReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let caller_supplied_tx_id = req.tx_id.is_some();
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    // Reconcile is the user's explicit "Resume" endpoint. If they
    // supplied a tx_id we treat attempt 0 as a resume (payload adopts
    // any existing entry); if they didn't, this is a first-time
    // reconcile against a fresh random id and attempt 0 runs as a
    // normal fresh BEGIN_TX.
    let initial_flags = if caller_supplied_tx_id {
        TX_FLAG_RESUME
    } else {
        0
    };
    let mode = match req.mode.as_deref().unwrap_or("fast") {
        "fast" => ReconcileMode::Fast,
        "safe" => ReconcileMode::Safe,
        other => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("unknown reconcile mode: {other}"),
            )
            .into_response();
        }
    };

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            started_at_ms,
            bytes_sent: 0,
            total_bytes: 0, // unknown until reconcile finishes
            files: vec![],
            skipped_files: 0,
            skipped_bytes: 0,
        },
    );

    let jobs = Arc::clone(&state.jobs);
    let events_tx = state.events_tx.clone();

    tokio::task::spawn_blocking(move || {
        let src_path = std::path::PathBuf::from(&req.src_dir);
        let mgmt = mgmt_addr_for(&addr);
        crate::log_info!(
            "resume: job={job_id} src={src} dest={dest} mode={mode:?} mgmt={mgmt}",
            job_id = job_id,
            src = src_path.display(),
            dest = req.dest_root,
            mode = mode,
            mgmt = mgmt,
        );
        // ── Phase 1: best-effort reconcile. We ATTEMPT to compute which
        //    files are already present on the PS5 (skip list), but we
        //    don't let a reconcile failure block the upload. If the
        //    mgmt service is busy/crashed/slow, we fall through to
        //    "upload everything" on the transfer port — which doesn't
        //    need the mgmt port at all. The user still gets their
        //    upload; they just lose the per-file skip optimization.
        //    Shard-level resume (TX_FLAG_RESUME, see
        //    transfer_file_list_resumable below) still works either way,
        //    so an interrupted upload picks up from the last acked
        //    shard even in the fallback path.
        //
        //    One attempt only: the reconcile has its own 10 s per-call
        //    timeout inside list_dir_with_timeout. A second attempt
        //    after that already-generous budget wouldn't change the
        //    outcome — it would just double the pre-transfer stall
        //    before the fallback kicks in.
        let reconcile_started = std::time::Instant::now();
        let plan: ReconcilePlan = match reconcile(
            &mgmt,
            &src_path,
            &req.dest_root,
            mode,
            &req.excludes,
        ) {
            Ok(p) => {
                crate::log_info!(
                    "resume: reconcile OK in {} ms — to_send={} bytes={} already={} already_bytes={}",
                    reconcile_started.elapsed().as_millis(),
                    p.to_send.len(),
                    p.bytes_to_send,
                    p.already_present,
                    p.bytes_already_present,
                );
                p
            }
            Err(e) => {
                crate::log_warn!(
                    "resume: reconcile failed after {} ms ({}), falling back to uploading all local files without skip optimization",
                    reconcile_started.elapsed().as_millis(),
                    e,
                );
                // Fallback: walk the local tree and treat every file as
                // to-send. The upload proceeds on the transfer port;
                // shard-level TX_FLAG_RESUME below still picks up any
                // interrupted prior attempt.
                match walk_local_inventory(&src_path, &req.excludes) {
                    Ok(local) => {
                        let to_send: Vec<ReconcileFile> = local
                            .into_iter()
                            .map(|(rel_path, size)| ReconcileFile { rel_path, size })
                            .collect();
                        let bytes_to_send: u64 = to_send.iter().map(|f| f.size).sum();
                        crate::log_info!(
                            "resume: fallback local walk produced {} file(s) / {} bytes",
                            to_send.len(),
                            bytes_to_send,
                        );
                        ReconcilePlan {
                            to_send,
                            bytes_to_send,
                            already_present: 0,
                            bytes_already_present: 0,
                        }
                    }
                    Err(walk_err) => {
                        // Local walk itself failed — can't even enumerate
                        // the source. This is genuinely fatal (source
                        // doesn't exist or permission denied); surface
                        // with a clear error.
                        let completed_at_ms = now_ms();
                        set_job(
                            &jobs,
                            &events_tx,
                            job_id,
                            JobState::Failed {
                                started_at_ms,
                                completed_at_ms,
                                elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                                error: format!("can't read source folder: {walk_err}"),
                            },
                        );
                        return;
                    }
                }
            }
        };
        let total_bytes = plan.bytes_to_send;
        let skipped_files_count = plan.already_present;
        let skipped_bytes_count = plan.bytes_already_present;
        let files_sent_count = plan.to_send.len() as u64;
        if plan.to_send.is_empty() {
            // Nothing to do — mark done immediately.
            let completed_at_ms = now_ms();
            set_job(
                &jobs,
                &events_tx,
                job_id,
                JobState::Done {
                    started_at_ms,
                    completed_at_ms,
                    elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                    tx_id_hex: "".to_string(),
                    shards_sent: 0,
                    bytes_sent: 0,
                    dest: req.dest_root.clone(),
                    files_sent: 0,
                    skipped_files: skipped_files_count,
                    skipped_bytes: skipped_bytes_count,
                    commit_ack: None,
                },
            );
            return;
        }

        // ── Phase 2: transfer_file_list on the delta. From here on the
        //    progress ticker owns Running.bytes_sent. The `files` list
        //    surfaced to the UI is the planned delta — not the full
        //    tree — so the file-progress view only shows what's
        //    actually being sent.
        let files: Vec<PlannedFile> = plan
            .to_send
            .iter()
            .map(|f| PlannedFile {
                rel_path: f.rel_path.clone(),
                size: f.size,
            })
            .collect();
        let progress = Arc::new(AtomicU64::new(0));
        let ctx = TickerContext {
            started_at_ms,
            total_bytes,
            skipped_files: skipped_files_count,
            skipped_bytes: skipped_bytes_count,
        };
        set_job(
            &jobs,
            &events_tx,
            job_id,
            JobState::Running {
                started_at_ms,
                bytes_sent: 0,
                total_bytes,
                files,
                skipped_files: skipped_files_count,
                skipped_bytes: skipped_bytes_count,
            },
        );
        let stop_ticker = spawn_progress_ticker(
            Arc::clone(&jobs),
            events_tx.clone(),
            job_id,
            ctx,
            Arc::clone(&progress),
        );
        // Same panic-survive contract as the other transfer endpoints.
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);

        let entries: Vec<FileListEntry> = plan
            .to_send
            .iter()
            .map(|f| {
                let local = src_path.join(f.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
                FileListEntry {
                    src: local.to_string_lossy().into_owned(),
                    dest: format!("{}/{}", req.dest_root, f.rel_path),
                }
            })
            .collect();

        let mut cfg = make_transfer_config(&addr);
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        // 3 attempts total — 1 fresh + 2 resumes. Covers the realistic
        // case of a single payload hiccup mid-transfer; if the payload
        // is hard-dead, the third attempt's connect will fail fast and
        // we surface the underlying error. Signature is max_retries (not
        // max_attempts), so 2 here means "up to 2 RESUME retries".
        let result =
            transfer_file_list_resumable(&cfg, tx_id, &req.dest_root, &entries, 2, initial_flags);
        match result {
            Ok(r) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: r.tx_id_hex,
                        shards_sent: r.shards_sent,
                        bytes_sent: r.bytes_sent,
                        dest: r.dest,
                        files_sent: files_sent_count,
                        skipped_files: skipped_files_count,
                        skipped_bytes: skipped_bytes_count,
                        commit_ack: serde_json::from_str(&r.commit_ack_body).ok(),
                    },
                )
            }
            Err(e) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: e.to_string(),
                    },
                )
            }
        }
        fail_guard.mark_succeeded();
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

/// GET /api/jobs/{id}
async fn get_job(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let uuid = match id.parse::<Uuid>() {
        Ok(u) => u,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    match state
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&uuid)
        .cloned()
    {
        Some(job) => (StatusCode::OK, Json(job)).into_response(),
        None => json_err(StatusCode::NOT_FOUND, "job not found").into_response(),
    }
}

#[derive(Deserialize)]
struct EngineLogsQuery {
    /// Return only entries whose seq is strictly greater than this. First
    /// call should pass `since=0` (or omit); subsequent calls pass the
    /// highest seq seen to receive only new lines.
    #[serde(default)]
    since: u64,
}

/// GET /api/engine-logs?since=<seq> — tail the engine log ring so the
/// renderer can surface recent engine activity in its own Log tab.
async fn engine_logs_tail(Query(q): Query<EngineLogsQuery>) -> impl IntoResponse {
    let entries = engine_log::tail_since(q.since);
    let next_seq = entries.last().map(|e| e.seq).unwrap_or(q.since);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "entries": entries,
            "next_seq": next_seq,
        })),
    )
}

/// GET /api/version — engine self-identification.
///
/// Returned shape: `{"version": "x.y.z"}`. Used by the Tauri shell
/// to detect a version-mismatched sibling engine on the bound port:
/// if /api/version disagrees with the version the shell was built
/// against, the shell kills the old engine + respawns its own.
/// Without this, an upgrade-and-relaunch cycle could leave the shell
/// talking to the prior version's engine indefinitely (silently
/// missing any newly-added routes — e.g. the FS_OP frames added in
/// 2.2.7).
async fn engine_version() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// GET /api/jobs
async fn list_jobs(State(state): State<AppState>) -> impl IntoResponse {
    let jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
    let summary: Vec<serde_json::Value> = jobs
        .iter()
        .map(|(id, s)| {
            serde_json::json!({
                "job_id": id.to_string(),
                "status": match s {
                    JobState::Running { .. } => "running",
                    JobState::Done {..} => "done",
                    JobState::Failed {..} => "failed",
                },
                "job": s,
            })
        })
        .collect();
    Json(summary)
}

// ─── Entry point ──────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    // Route ps5upload-core's log stream into the same ring the engine's
    // own logs land in, so the renderer's Log tab sees *both* sources
    // (reconcile per-parent progress, transfer retries, etc.) without
    // having to install a separate pipe for core diagnostics.
    ps5upload_core::log::set_sink(|msg| engine_log::record("info", msg.to_string()));

    // `PS5UPLOAD_ENGINE_PORT` matches the panic message below and the
    // name the desktop client sets when it spawns the sidecar. Previous
    // generic `ENGINE_PORT` was too easy to collide with other tools
    // and the panic message pointed users at a name that didn't work.
    let port: u16 = std::env::var("PS5UPLOAD_ENGINE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(19113);
    let ps5_addr = std::env::var("PS5_ADDR").unwrap_or_else(|_| "192.168.137.2:9113".to_string());

    let (events_tx, _) = broadcast::channel(512);

    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        default_ps5_addr: ps5_addr.clone(),
        events_tx,
    };

    let app = Router::new()
        .route("/", get(ui_handler))
        .route("/api/ps5/status", get(ps5_status))
        .route("/api/ps5/cleanup", post(ps5_cleanup))
        .route("/api/ps5/volumes", get(ps5_volumes))
        .route("/api/ps5/list-dir", get(ps5_list_dir))
        .route("/api/ps5/fs/delete", post(ps5_fs_delete))
        .route("/api/ps5/fs/move", post(ps5_fs_move))
        .route("/api/ps5/fs/copy", post(ps5_fs_copy))
        .route("/api/ps5/fs/op-status", get(ps5_fs_op_status))
        .route("/api/ps5/fs/op-cancel", post(ps5_fs_op_cancel))
        .route("/api/ps5/fs/mount", post(ps5_fs_mount))
        .route("/api/ps5/fs/unmount", post(ps5_fs_unmount))
        .route("/api/ps5/hw/info", get(ps5_hw_info))
        .route("/api/ps5/hw/temps", get(ps5_hw_temps))
        .route("/api/ps5/hw/power", get(ps5_hw_power))
        .route("/api/ps5/proc/list", get(ps5_proc_list))
        .route("/api/ps5/hw/fan-threshold", post(ps5_hw_set_fan_threshold))
        .route("/api/ps5/fs/chmod", post(ps5_fs_chmod))
        .route("/api/ps5/fs/mkdir", post(ps5_fs_mkdir))
        .route("/api/ps5/game-meta", get(ps5_game_meta))
        .route("/api/ps5/game-icon", get(ps5_game_icon))
        .route("/api/transfer/file", post(transfer_file_handler))
        .route("/api/transfer/dir", post(transfer_dir_handler))
        .route("/api/transfer/file-list", post(transfer_file_list_handler))
        .route("/api/transfer/download", post(transfer_download_handler))
        .route(
            "/api/transfer/dir-reconcile",
            post(transfer_dir_reconcile_handler),
        )
        .route("/api/version", get(engine_version))
        .route("/api/jobs", get(list_jobs))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/events", get(events_stream))
        .route("/api/engine-logs", get(engine_logs_tail))
        .with_state(state)
        // The engine binds 127.0.0.1 only, so cross-origin fetches come
        // from the local Tauri webview (dev: http://localhost:1420, prod:
        // tauri://localhost) or any local script (`curl`, tests,
        // smoke-hardware.mjs). Permissive CORS is fine here; there's no
        // real XSRF surface to protect on a localhost-bound service.
        .layer(tower_http::cors::CorsLayer::permissive());

    let bind = format!("127.0.0.1:{port}");
    println!("[ps5upload-engine] listening on http://{bind}  (ps5={ps5_addr})");
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            // Port already bound. Don't panic — that's both noisy in
            // the user's engine.log and surfaces to the renderer as
            // "engine request failed" with no useful diagnostic.
            // Probe whether the port answers a TCP connect at all
            // (cheap, no extra HTTP-client dep). If it does, an
            // engine — ours or otherwise — is up and the Tauri
            // shell's probe-then-spawn flow on the next start() will
            // pick the right action (use it / surface an error).
            // Exit 0 in that case. If even the connect fails, the
            // port's in some half-bound state (e.g. TIME_WAIT or
            // permissions) — exit non-zero with a clear log.
            let connectable = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                tokio::net::TcpStream::connect(&bind),
            )
            .await
            .ok()
            .and_then(|r| r.ok())
            .is_some();
            if connectable {
                eprintln!(
                    "[ps5upload-engine] {bind} already bound by another \
                     process (likely a sibling engine); exiting 0",
                );
                return;
            }
            eprintln!(
                "[ps5upload-engine] failed to bind {bind}: {e} \
                 (port held by an unresponsive process — kill it or \
                 override with PS5UPLOAD_ENGINE_PORT)",
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("[ps5upload-engine] axum serve terminated: {e}");
        std::process::exit(3);
    }
}
