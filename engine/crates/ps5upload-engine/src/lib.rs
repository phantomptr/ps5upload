//! ps5upload-engine — local HTTP service that drives FTX2 transfers.
//!
//! This is a library crate so the engine can be consumed two ways: the
//! desktop sidecar binary (`src/main.rs`) calls `run_cli()`, while the
//! Tauri mobile build links the crate and calls `serve_in_process()` on
//! a background task (Android/iOS have no sidecar-binary spawn model, so
//! the server runs inside the app process). The `windows_subsystem`
//! console suppression lives on the binary (`main.rs`), not here.
//!
//! Listens on 0.0.0.0:19113 by default (set PS5UPLOAD_ENGINE_PORT env var to override).
//! API routes are LAN-guarded via the `loopback_guard` middleware — only
//! `/pkg-host/*` accepts off-loopback peers (so the PS5 can fetch fakepkg
//! bytes during install). Everything else 403s any non-loopback source,
//! except the IPs in PS5UPLOAD_ALLOW_IP (comma-separated, for remote clients).
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
//!   POST /api/transfer/zip            → start zip-archive transfer (decompress on host)
//!   POST /api/zip/inspect             → preview a .zip (counts/sizes/game meta)
//!   POST /api/transfer/file-list      → start multi-file transfer from explicit list
//!   GET  /api/jobs/{id}               → poll job status/result
//!   GET  /api/jobs                    → list all jobs (summary)
//!   GET  /api/events                  → SSE stream of job state changes
//!   POST /api/ps5/cleanup             → recursively remove a path under PS5 allowlist
//!   GET  /api/ps5/volumes             → list storage volumes detected by the payload
//!   GET  /api/ps5/list-dir?path=...   → list immediate children of a directory on PS5

mod engine_log;
mod pkg_install;
#[cfg(feature = "webui")]
mod webui;

use axum::{
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
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
    download::{
        download_to_local_multistream, enumerate_download_set, DownloadKind, MAX_DOWNLOAD_STREAMS,
    },
    fs_ops::{
        app_launch, app_list_registered, app_register, app_unregister, fs_copy_robust,
        fs_delete_with_op_id, fs_mkdir, fs_mount, fs_move_with_timeout, fs_op_cancel, fs_op_status,
        fs_read, fs_unmount, list_dir, reconcile, walk_local_inventory, DirListing, ListDirOptions,
        MountResult, ReconcileFile, ReconcileMode, ReconcilePlan, RegisterResult,
    },
    game_meta::parse_param_json_bytes,
    hw::{
        hw_info, hw_power, hw_set_fan_threshold, hw_storage, hw_temps, proc_list, HwInfo, HwPower,
        HwStorage, HwTemps, ProcList,
    },
    sys_time::{
        humanize_err as sys_time_humanize, ps5_time_get, ps5_time_set, PsTime, PsTimeSetResult,
    },
    transfer::{
        inspect_7z, inspect_zip, sevenz_plan_preview, transfer_7z_resumable,
        transfer_dir_resumable, transfer_file_list_multistream, transfer_file_list_resumable,
        transfer_file_path_resumable, transfer_zip_resumable, zip_plan_preview, FileListEntry,
        TransferConfig, DEFAULT_RESUME_RETRIES, DEFAULT_ZIP_ENTRY_RAM_THRESHOLD, TX_FLAG_RESUME,
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
    // Bandwidth cap. `FTX2_BANDWIDTH_MBPS=10` caps outbound at
    // 10 MB/s; setting `0` (or unsetting) disables the cap. The
    // throttle is enforced inside the pipelined sender — see
    // `BandwidthThrottle` in transfer.rs.
    if let Ok(v) = std::env::var("FTX2_BANDWIDTH_MBPS") {
        if let Ok(n) = v.parse::<f64>() {
            if n > 0.0 {
                cfg.bandwidth_cap_bps = Some((n * 1024.0 * 1024.0) as u64);
            }
        }
    }
    cfg
}

/// Apply a per-request bandwidth cap to the config. None / 0 / negative
/// = leave the existing cap (env-var default) in place; positive values
/// override. Centralised so all four transfer entry points apply the
/// same precedence rule.
fn apply_per_request_bandwidth(cfg: &mut TransferConfig, cap_mbps: Option<f64>) {
    if let Some(n) = cap_mbps {
        if n > 0.0 {
            cfg.bandwidth_cap_bps = Some((n * 1024.0 * 1024.0) as u64);
        } else if n == 0.0 {
            // Explicit 0 = override "unlimited" — useful for callers
            // that want to ignore the env-var default.
            cfg.bandwidth_cap_bps = None;
        }
    }
}
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{
    wrappers::{BroadcastStream, ReceiverStream},
    StreamExt as _,
};
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
        /// Per-file progress — climbs as each source file is read into a
        /// pack frame or as its first chunk goes out. The shard-ACK-based
        /// `bytes_sent` looks like "start → finished" on 46 k-file game
        /// folders (each packed ACK completes ~200 files at once); this
        /// counter ticks smoothly through the read-many-tiny-files phase
        /// so the user sees the upload is alive. 0 means "not provided
        /// by this transfer path" (e.g. single-file uploads), in which
        /// case the UI falls back to its size-derived estimate.
        #[serde(default)]
        files_processing: u64,
        /// P3 / v2.18.0 — files the PS5 has fully written to their
        /// destination paths during the post-100% COMMIT_TX apply
        /// loop. Only ticks once `bytes_sent == total_bytes`; until
        /// then this stays at 0. The payload emits APPLY_PROGRESS
        /// frames every ~1 sec or 1024 files during apply when the
        /// engine sent `TX_FLAG_APPLY_PROGRESS_REQUESTED` on
        /// BEGIN_TX (default for multi-file uploads). UI surfaces
        /// this as "Finalized N of M files" during the finalize
        /// phase so users see real motion through what used to be
        /// a silent 10-30 min wait.
        ///
        /// 0 for single-file uploads (apply is one fsync, no
        /// progress reporting), for old payloads that don't emit
        /// APPLY_PROGRESS (graceful degradation — UI shows the
        /// plain "Finalizing on PS5…" pill from v2.17.3), and
        /// during the pre-finalize bytes-on-wire phase.
        #[serde(default)]
        files_finalized: u64,
        /// Total files the payload will commit. Same value as
        /// `files.len()` on the planned manifest, surfaced here as
        /// a u64 so the UI doesn't have to count `files`. 0 means
        /// "unknown" — single-file uploads or pre-FIRST-frame
        /// (we set this on the first APPLY_PROGRESS arrival).
        #[serde(default)]
        files_finalizing_total: u64,
        /// Cumulative bytes the payload has finalized so far during
        /// apply. Lets the UI show a second progress dimension
        /// (bytes-finalized / total-bytes) for cases where file
        /// sizes vary wildly. 0 outside the finalize phase.
        #[serde(default)]
        bytes_finalized: u64,
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
        /// Raw stringified error chain. Kept for debuggability and as
        /// the fallback rendering when structured fields below are
        /// absent (older payloads, non-payload-origin errors).
        error: String,
        /// Machine-parseable error category lifted from the payload's
        /// error frame body (`{"error":"direct_writer_io_error",…}`).
        /// `None` for errors that didn't originate from the payload's
        /// JSON-bodied error frames — e.g. local I/O, connection
        /// refused at TCP-connect time. UI uses this to humanize the
        /// surface text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_reason: Option<String>,
        /// Human-readable detail from the payload's error frame
        /// `"detail"` field. Often pinpoints the on-PS5 path or the
        /// underlying errno. Shown to the user as the secondary line
        /// in the error card.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_detail: Option<String>,
    },
}

/// Build a `JobState::Failed` from a transfer error, populating the
/// structured reason/detail fields when the payload's error body is
/// parseable. Single call site for every transfer handler's `Err(e)`
/// branch so the structured-field plumbing stays consistent.
fn job_failed_from_err(started_at_ms: u64, completed_at_ms: u64, err: &anyhow::Error) -> JobState {
    let (reason, detail) = extract_payload_error(err);
    // Central choke point for ALL async transfer-job failures (file, dir,
    // reconcile, download). Logging here means a mid-transfer death — the
    // "upload failed halfway through" case — shows the engine's full error
    // chain in engine.log, instead of the handler logging only the START.
    log_warn!(
        "transfer job failed: {err:#}{}",
        reason
            .as_deref()
            .map(|r| format!(" (reason={r})"))
            .unwrap_or_default()
    );
    JobState::Failed {
        started_at_ms,
        completed_at_ms,
        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
        error: format!("{err:#}"),
        error_reason: reason,
        error_detail: detail,
    }
}

/// Walk an `anyhow::Error` chain looking for a JSON object body of the
/// form `{"error":"…","detail":"…"}` — the shape payload error frames
/// (and the engine's pass-through of them) carry. Returns the parsed
/// `(reason, detail)` if a match is found anywhere in the chain. Both
/// fields are independently optional so a body with only `"error"` is
/// still useful.
///
/// The payload emits two body shapes today:
///
///   1. Bare token, e.g. `"fs_delete_path_not_allowed"` (no JSON).
///   2. JSON with `"error"` + optional `"detail"` + tx-context.
///
/// This parser ignores (1) — the raw error string already carries the
/// token verbatim, and there's nothing further to humanize. (2) is
/// where the value comes from.
fn extract_payload_error(err: &anyhow::Error) -> (Option<String>, Option<String>) {
    // Each cause in the chain has a Display impl. We look for an
    // embedded `{...}` substring with our shape inside.
    for cause in err.chain() {
        let s = cause.to_string();
        // Cheap pre-filter: skip causes that obviously don't contain a
        // JSON object so we don't pay the regex/parser cost.
        let Some(open) = s.find('{') else { continue };
        let close = match s.rfind('}') {
            Some(c) if c > open => c,
            _ => continue,
        };
        let candidate = &s[open..=close];
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(candidate);
        if let Ok(v) = parsed {
            let reason = v
                .get("error")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let detail = v
                .get("detail")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            if reason.is_some() || detail.is_some() {
                return (reason, detail);
            }
        }
    }
    (None, None)
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

/// Recursively `chmod 0777` a destination tree on the PS5.
///
/// **No longer called automatically after uploads** (v2.16.1+): the payload
/// now `umask(0)`s at startup, opens game files at `0777`, and `fchmod`s
/// after every open — so freshly-uploaded files are world-rwx already and
/// the per-upload recursive walk (which took ~30 s on a 22k-file folder)
/// is redundant overhead. Kept for explicit use: a future Library "Fix
/// permissions" button, or for repairing old uploads written by pre-2.16.1
/// payloads (which created files at `0644` → Sony loader returns CE-107750-0
/// "can't start game or app").
#[allow(dead_code)]
fn auto_chmod_uploaded_tree(transfer_addr: &str, dest: &str) {
    let mgmt = mgmt_addr_for(transfer_addr);
    let started = std::time::Instant::now();
    match ps5upload_core::fs_ops::fs_chmod_with_timeout(
        &mgmt,
        dest,
        "0777",
        true,
        Some(Duration::from_secs(600)),
    ) {
        Ok(()) => {
            crate::log_info!(
                "auto-chmod 0777 -R OK on {} ({} ms)",
                dest,
                started.elapsed().as_millis()
            );
        }
        Err(e) => {
            crate::log_warn!(
                "auto-chmod 0777 -R on {} failed after {} ms ({}); upload is byte-exact, \
                 user can re-chmod manually via File System tab if Sony's loader rejects \
                 the title with CE-107750-0",
                dest,
                started.elapsed().as_millis(),
                e
            );
        }
    }
}

/// Loopback guard for the API surface. Pre-2.2.52 the engine bound
/// `127.0.0.1` only, which kept the API safe from the LAN by accident
/// — but also broke `.pkg` install because the PS5 couldn't reach
/// `/pkg-host/*` either. We now bind `0.0.0.0` and gate routes by the
/// peer's source address: the Tauri webview, CLI tools, and our own
/// integration tests all connect from `127.0.0.0/8` (or `::1`). Anything
/// off-loopback hitting an `/api/*` route is a third party that has no
/// business calling our engine and gets a 403.
///
/// `/pkg-host/*` is the single PS5-facing route; it's exempted from the
/// guard so the console can fetch installable bytes. The route itself
/// uses a UUIDv4 token in the URL as the auth gate (~122 bits of
/// entropy, rotated per install — the trust boundary is the local LAN,
/// same as any other on-LAN homebrew installer).
/// Log every (allowed) request: method, path, status, duration. Recorded at
/// `debug` so it always lands in engine.log (rotated, crash-survivable) for a
/// complete "what was the engine doing when it hung" trace, but only reaches
/// the renderer's windowed app.jsonl when the user lowers the level to
/// debug/trace. 5xx is bumped to `warn` so failures show at the default level.
async fn log_requests(req: Request, next: Next) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let start = std::time::Instant::now();
    let resp = next.run(req).await;
    let ms = start.elapsed().as_millis();
    let status = resp.status().as_u16();
    if status >= 500 {
        log_warn!("{method} {path} -> {status} ({ms}ms)");
    } else {
        log_debug!("{method} {path} -> {status} ({ms}ms)");
    }
    resp
}

/// Config for `loopback_guard`: extra IPs allowed besides loopback.
/// `Arc<[..]>` so the per-request middleware-state clone is a refcount bump,
/// not a Vec copy.
#[derive(Clone)]
struct LoopbackGuardConfig {
    allowed_ips: std::sync::Arc<[std::net::IpAddr]>,
}

/// Pure allow/deny decision so it can be unit-tested without a live server.
/// Allow when the path is the PS5-facing `/pkg-host/*`, the peer is on
/// loopback, or the peer is one of the configured `allowed_ips`.
fn loopback_allows(cfg: &LoopbackGuardConfig, peer: std::net::IpAddr, path: &str) -> bool {
    const OFF_LOOPBACK_ALLOWED: &[&str] = &["/pkg-host/"];
    OFF_LOOPBACK_ALLOWED.iter().any(|p| path.starts_with(p))
        || peer.is_loopback()
        || cfg.allowed_ips.contains(&peer)
}

/// Parse a comma-separated `PS5UPLOAD_ALLOW_IP` value into IPs, trimming
/// whitespace and silently dropping blank/unparseable entries.
fn parse_allow_ips(raw: &str) -> Vec<std::net::IpAddr> {
    raw.split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

async fn loopback_guard(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    State(cfg): State<LoopbackGuardConfig>,
    req: Request,
    next: Next,
) -> impl IntoResponse {
    let path = req.uri().path();
    if loopback_allows(&cfg, peer.ip(), path) {
        return next.run(req).await.into_response();
    }
    eprintln!("[ps5upload-engine] refusing off-loopback request to {path} from {peer}");
    (StatusCode::FORBIDDEN, "loopback only").into_response()
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

// Eight parameters reflects the actual lifecycle data this ticker
// needs to broadcast and the four progress sinks it reads. Bundling
// them into a struct would just move the surface area without
// reducing it. Allow rather than restructure.
#[allow(clippy::too_many_arguments)]
fn spawn_progress_ticker(
    jobs: Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: broadcast::Sender<String>,
    job_id: Uuid,
    ctx: TickerContext,
    progress: Arc<AtomicU64>,
    progress_files: Arc<AtomicU64>,
    // P3 / v2.18.0: apply-phase counters fed by the engine's
    // commit-wait loop reading APPLY_PROGRESS frames from the
    // payload. Same Arc<AtomicU64> pattern as the existing pre-
    // commit counters so the ticker can read both with one
    // Relaxed load each. Optional because single-file paths
    // don't allocate these.
    progress_files_finalized: Arc<AtomicU64>,
    progress_bytes_finalized: Arc<AtomicU64>,
) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_tick = Arc::clone(&stop);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.tick().await; // consume the immediate first tick
        let mut last_bytes = u64::MAX; // forces a broadcast on the first real tick
        let mut last_files = u64::MAX;
        let mut last_ff = u64::MAX;
        let mut last_bf = u64::MAX;
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
            // Clamp to the known total: the progress counter is CUMULATIVE
            // bytes pushed, so a resume-on-drop (which re-sends from the last
            // durable offset) makes it climb PAST the file size — the UI then
            // showed "36 GiB / 24.6 GiB". A progress counter can't meaningfully
            // exceed the target, so cap it (when the total is known); the bar
            // pins at 100% instead of overshooting. Dedup below then also sees
            // a steady value once capped, so it stops emitting noise ticks.
            let raw_bytes = progress.load(Ordering::Relaxed);
            let bytes_sent = if ctx.total_bytes > 0 {
                raw_bytes.min(ctx.total_bytes)
            } else {
                raw_bytes
            };
            let files_processing = progress_files.load(Ordering::Relaxed);
            let files_finalized = progress_files_finalized.load(Ordering::Relaxed);
            let bytes_finalized = progress_bytes_finalized.load(Ordering::Relaxed);
            // Skip the HashMap mutation + SSE broadcast when NONE
            // of the counters have moved. Same rationale as the
            // pre-2.18 path: avoid per-tick allocation + SSE
            // serialization when nothing changed. Apply-phase
            // counters added here keep us broadcasting during the
            // post-100% finalize window where bytes_sent and
            // files_processing have stopped but files_finalized is
            // ticking up — exactly the case that motivated P3.
            if bytes_sent == last_bytes
                && files_processing == last_files
                && files_finalized == last_ff
                && bytes_finalized == last_bf
            {
                continue;
            }
            last_bytes = bytes_sent;
            last_files = files_processing;
            last_ff = files_finalized;
            last_bf = bytes_finalized;
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
                        files_processing: fp,
                        files_finalized: ff,
                        bytes_finalized: bf,
                        ..
                    }) => {
                        *b = bytes_sent;
                        *t = ctx.total_bytes;
                        *s = ctx.started_at_ms;
                        *sf = ctx.skipped_files;
                        *sb = ctx.skipped_bytes;
                        *fp = files_processing;
                        *ff = files_finalized;
                        *bf = bytes_finalized;
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
                error_reason: None,
                error_detail: None,
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

/// Process-global cancel registry: maps a transfer `job_id` to the
/// `Arc<AtomicBool>` its `TransferConfig::cancel` watches. A transfer registers
/// its flag here (inside the spawn_blocking closure, which has the `job_id`),
/// and `POST /api/jobs/{id}/cancel` flips it — the core then aborts at its next
/// shard boundary with `transfer_cancelled`, leaving the partial tx resumable
/// (same as a dropped connection). A static avoids threading `AppState` into
/// the blocking transfer closures (which only capture `job_id`).
///
/// Self-pruning: while a transfer runs, the core holds clone(s) of the flag, so
/// `Arc::strong_count > 1`; once it finishes those drop and only the registry's
/// clone remains (count 1). `register` drops every count-1 entry before
/// inserting, so the map stays ≈ the live-transfer count with no terminal
/// bookkeeping.
fn cancel_registry() -> &'static Mutex<HashMap<Uuid, Arc<AtomicBool>>> {
    static REG: OnceLock<Mutex<HashMap<Uuid, Arc<AtomicBool>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a fresh cancel flag for `job_id` and return it to thread into
/// `TransferConfig::cancel`. Prunes flags whose transfer has finished.
fn register_transfer_cancel(job_id: Uuid) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let mut g = cancel_registry().lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|_, v| Arc::strong_count(v) > 1);
    g.insert(job_id, Arc::clone(&flag));
    flag
}

/// Flip a job's cancel flag if it's registered (i.e. still running). Returns
/// true if a flag was found. Idempotent.
fn signal_transfer_cancel(job_id: Uuid) -> bool {
    let g = cancel_registry().lock().unwrap_or_else(|e| e.into_inner());
    match g.get(&job_id) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
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
    /// Per-job bandwidth cap in MB/s. None = use the engine's
    /// default (env-var-controlled). 0 also = no cap.
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

#[derive(Deserialize)]
struct TransferDirReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    src_dir: String,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

/// Upload a `.zip`'s contents, decompressing on the host so files land
/// already extracted on the PS5. Same shape as `TransferDirReq` but the
/// source is an archive path instead of a directory.
#[derive(Deserialize)]
struct TransferZipReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    zip_path: String,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
    /// Per-entry RAM-vs-temp inflate threshold, in MiB. None = engine default
    /// (`FTX2_ZIP_RAM_THRESHOLD_MB` env, else the core default).
    #[serde(default)]
    ram_threshold_mb: Option<u64>,
}

#[derive(Deserialize)]
struct ZipInspectReq {
    zip_path: String,
}

/// `/api/transfer/7z` request. Mirrors `TransferZipReq` minus the per-entry
/// RAM threshold — 7z streams forward-only with bounded memory, so there's no
/// inflate-to-RAM-vs-temp knob.
#[derive(Deserialize)]
struct Transfer7zReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    archive_path: String,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

/// `/api/7z/inspect[/stream]` request.
#[derive(Deserialize)]
struct SevenzInspectReq {
    archive_path: String,
}

/// `/api/transfer/rar` request. Like 7z plus an optional `password` for
/// encrypted archives. (RAR is desktop-only — the Android build cfg's out both
/// the real handler and this struct, so it lives behind the same gate.)
#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct TransferRarReq {
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    archive_path: String,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
    #[serde(default)]
    password: Option<String>,
}

/// `/api/rar/inspect` request. Desktop-only, same gate as the handler.
#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct RarInspectReq {
    archive_path: String,
    #[serde(default)]
    password: Option<String>,
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
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
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
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
    /// Parallel upload streams. The client resolves this as
    /// `min(user_setting, payload's max_transfer_streams)` and passes it here;
    /// the engine just hands it to the multi-stream orchestrator. Absent / <=1
    /// → single-stream (unchanged behaviour). See docs/multistream-upload.md.
    #[serde(default)]
    streams: Option<usize>,
}

#[derive(Serialize)]
struct JobCreated {
    job_id: String,
}

#[derive(Deserialize)]
struct AddrQuery {
    addr: Option<String>,
}

/// Query for the HW_TEMPS endpoint. `extended=1` requests the on-demand
/// telemetry (SoC power / CPU usage / fan duty / product shape); any other
/// value (or absent) is the basic, auto-poll-safe read.
#[derive(Deserialize)]
struct HwTempsQuery {
    addr: Option<String>,
    #[serde(default)]
    extended: Option<u8>,
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

/// GET / — full React SPA when the `webui` feature is compiled in; otherwise
/// the minimal transfer-and-jobs dashboard baked into `static/index.html`.
async fn ui_handler() -> impl IntoResponse {
    #[cfg(feature = "webui")]
    {
        webui::spa_response("index.html")
    }
    #[cfg(not(feature = "webui"))]
    {
        (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            include_str!("../static/index.html"),
        )
            .into_response()
    }
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
    /// Optional unique 64-bit identifier the client generates so it
    /// can poll progress (`/api/ps5/fs/op-status`) and cancel
    /// (`/api/ps5/fs/op-cancel`) the in-flight delete. Only used by
    /// `ps5_fs_delete`; other handlers that share this struct (e.g.
    /// `ps5_fs_mkdir`) ignore it. Pass 0 (or omit) for ops where
    /// progress/cancel isn't needed; the payload skips its
    /// in-flight-ops slot registration in that case so single-file
    /// unlinks don't burn one of MAX_FS_OPS=4 slots.
    #[serde(default)]
    op_id: u64,
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
    let op_id = req.op_id;
    let started = std::time::Instant::now();
    crate::log_info!("fs_delete: addr={addr} path={path} op_id={op_id}");
    let path_for_log = path.clone();
    // 1-hour deadline: fs_delete is a single-shot RPC; the payload runs
    // a recursive `rm -rf` and only sends FS_DELETE_ACK at the end. A
    // small-file-heavy game folder (PPSA01342: 223k files / 19k dirs ≈
    // 240k metadata syscalls) takes minutes to delete on PS5 UFS; the
    // default 30 s socket timeout fires mid-walk and the user sees a
    // "read frame header: Resource temporarily unavailable" 502 while
    // the payload keeps deleting in the background. Same long bound as
    // fs_copy / fs_move so behavior across the destructive trio matches.
    let io_timeout = std::time::Duration::from_secs(60 * 60);
    match tokio::task::spawn_blocking(move || {
        fs_delete_with_op_id(&addr, &path, op_id, Some(io_timeout))
    })
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
            // Cancellation surfaces as `Err("cancelled")` from
            // fs_delete_with_op_id. Mirror fs_copy: 409 Conflict so
            // the client can tell user-initiated stop apart from a
            // real delete failure (different banner, different log).
            let msg = e.to_string();
            if msg == "cancelled" {
                crate::log_info!(
                    "fs_delete cancelled: {path_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
                return json_err(StatusCode::CONFLICT, "cancelled").into_response();
            }
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
    // 1-hour deadline: an intra-volume fs_move returns in milliseconds
    // (rename(2) is metadata-only). A CROSS-volume move can't rename (the
    // payload refuses it — a cross-device rename panics this kernel) and
    // returns `fs_move_cross_mount`; the CLIENT then completes it as
    // copy-then-delete, which can run for minutes on a multi-GiB file. Keep the
    // generous bound so the default 30 s socket timeout can't fire mid-op and
    // surface as the cryptic "read frame header" 502.
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

/// Monotonic op-id source for an fs op the caller didn't tag with its own id,
/// so the robust copy is always trackable. Starts high (and steps by 1) to keep
/// clear of client-generated ids (random 64-bit) within a session.
fn next_fs_op_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0xE000_0000_0000_0001);
    COUNTER.fetch_add(1, Ordering::Relaxed)
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
    // Robust, drop-tolerant copy: a 25 GB USB→internal copy over flaky Wi-Fi
    // used to die "read frame header" minutes in, because the bare copy holds
    // ONE connection for the whole operation while the console keeps copying
    // fine. fs_copy_robust fires the copy with an op_id and tracks completion by
    // polling fs_op_status on fresh connections, so a connection blip no longer
    // aborts a healthy copy. A non-zero op_id is required for tracking; generate
    // one when the caller didn't supply theirs (they just won't get a % bar).
    let op_id = if req.op_id != 0 {
        req.op_id
    } else {
        next_fs_op_id()
    };
    // 3 min with zero bytes written ⇒ genuinely stuck (a live USB copy advances
    // steadily; this only trips on a wedged console / pulled drive).
    let stall = std::time::Duration::from_secs(180);
    match tokio::task::spawn_blocking(move || fs_copy_robust(&addr, &from, &to, op_id, stall))
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
    /// Optional full mount path. New in 2.2.25 — when provided, the
    /// payload mounts at this exact path instead of the legacy
    /// `/mnt/ps5upload/<name>/` location. Mutually exclusive with
    /// `mount_name`; payload prefers `mount_point` if both arrive.
    #[serde(default)]
    mount_point: Option<String>,
    /// Mount the image read-only. New in 2.2.26. Default false (RW).
    #[serde(default)]
    read_only: Option<bool>,
}

async fn ps5_fs_mount(
    State(state): State<AppState>,
    Json(req): Json<FsMountReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let image_path = req.image_path;
    let mount_name = req.mount_name;
    let mount_point = req.mount_point;
    let read_only = req.read_only.unwrap_or(false);
    let started = std::time::Instant::now();
    crate::log_info!(
        "fs_mount: addr={addr} image_path={image_path} mount_name={:?} mount_point={:?} read_only={read_only}",
        mount_name,
        mount_point,
    );
    let image_for_log = image_path.clone();
    let result: Result<MountResult, anyhow::Error> = tokio::task::spawn_blocking(move || {
        fs_mount(
            &addr,
            &image_path,
            mount_name.as_deref(),
            mount_point.as_deref(),
            read_only,
        )
    })
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

/// Launch a registered title via the payload's triple-strategy
/// `sceLncUtilLaunchApp` → `sceSystemServiceLaunchApp` flow. Title
/// must already be registered in app.db (we surface the existing
/// `register_title_*` flow elsewhere). Re-exposed in 2.2.26 after
/// previously being gated out of the UI.
#[derive(Debug, serde::Deserialize)]
struct AppLaunchReq {
    addr: Option<String>,
    title_id: String,
}

async fn ps5_app_launch(
    State(state): State<AppState>,
    Json(req): Json<AppLaunchReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let title_id = req.title_id;
    let started = std::time::Instant::now();
    crate::log_info!("app_launch: addr={addr} title_id={title_id}");
    let title_for_log = title_id.clone();
    match tokio::task::spawn_blocking(move || app_launch(&addr, &title_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "app_launch ok: {title_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "app_launch failed: {title_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

/// Stage + register a PS5 game folder so Sony's launcher picks it up
/// in the XMB. `src_path` may live on /data, /mnt/ext*, /mnt/usb*, or
/// inside a mounted /mnt/ps5upload/ image. Idempotent: re-registering
/// the same path is a no-op (Sony's installer returns 0x80990002,
/// which the payload normalises). Re-exposed in 2.2.26 — engine core
/// and payload were already wired but the HTTP/Tauri layer hadn't
/// been opened up.
#[derive(Debug, serde::Deserialize)]
struct AppRegisterReq {
    addr: Option<String>,
    src_path: String,
    /// 2.2.26 opt-in: rewrite `<src>/sce_sys/param.json`'s
    /// `applicationDrmType` to `"standard"` before staging. Default
    /// false (don't touch the user's source).
    #[serde(default)]
    patch_drm_type: Option<bool>,
}

async fn ps5_app_register(
    State(state): State<AppState>,
    Json(req): Json<AppRegisterReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let src_path = req.src_path;
    let patch_drm_type = req.patch_drm_type.unwrap_or(false);
    let started = std::time::Instant::now();
    crate::log_info!(
        "app_register: addr={addr} src_path={src_path} patch_drm_type={patch_drm_type}"
    );
    let path_for_log = src_path.clone();
    match tokio::task::spawn_blocking(move || app_register(&addr, &src_path, patch_drm_type))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(result) => {
            crate::log_info!(
                "app_register ok: {path_for_log} -> {} ({}) in {} ms",
                result.title_id,
                result.title_name,
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json::<RegisterResult>(result)).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "app_register failed: {path_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

/// Reverse of `app_register`. Best-effort — succeeds even when the
/// Sony AppUninstall API isn't available, as long as the nullfs
/// teardown succeeded.
#[derive(Debug, serde::Deserialize)]
struct AppUnregisterReq {
    addr: Option<String>,
    title_id: String,
}

async fn ps5_app_unregister(
    State(state): State<AppState>,
    Json(req): Json<AppUnregisterReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let title_id = req.title_id;
    let started = std::time::Instant::now();
    crate::log_info!("app_unregister: addr={addr} title_id={title_id}");
    let title_for_log = title_id.clone();
    match tokio::task::spawn_blocking(move || app_unregister(&addr, &title_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "app_unregister ok: {title_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "app_unregister failed: {title_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
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
    // Recursive chmod on a 20k-file game folder routinely runs past the
    // default 30 s socket timeout — the payload walks the tree serially
    // and ack-acks per directory. The Library "Fix permissions" button is
    // the main caller and would silently fail with a "PS5 stopped
    // responding" toast on a big folder. 600 s caps it at "user-noticeable
    // but won't infinite-hang."
    let io_timeout = if recursive {
        Some(std::time::Duration::from_secs(600))
    } else {
        None
    };
    match tokio::task::spawn_blocking(move || {
        ps5upload_core::fs_ops::fs_chmod_with_timeout(&addr, &path, &mode, recursive, io_timeout)
    })
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
    Query(q): Query<HwTempsQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let extended = q.extended.unwrap_or(0) != 0;
    let r: Result<HwTemps, anyhow::Error> =
        tokio::task::spawn_blocking(move || hw_temps(&addr, extended))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// Recent PS5 kernel log (`sysctl kern.msgbuf`). Surfaces "why didn't
/// the payload load / what silently failed" directly in the desktop's
/// diagnostics panel — no need to FTP/ssh into the console. Body is
/// raw text (kernel printf output); UI just shows it in a scrollable
/// monospace area.
async fn ps5_syslog_tail(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<String, anyhow::Error> =
        tokio::task::spawn_blocking(move || ps5upload_core::hw::syslog_tail(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(serde_json::json!({ "text": v }))).into_response(),
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

// ── System clock (TIME_GET / TIME_SET) ─────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct TimeSyncReq {
    addr: Option<String>,
    /// Target wall-clock time as unix seconds (UTC). Caller usually
    /// passes their own PC's clock; we pass it straight through to
    /// the payload.
    target_unix_seconds: i64,
}

#[derive(Debug, serde::Serialize)]
struct TimeSyncResp {
    /// Raw payload-reported `ok` field. Note `ok: true` doesn't mean
    /// the clock moved — see `stub_no_op` for the detection.
    ok: bool,
    err_code: u32,
    /// Short, user-readable reason for the err_code. Empty when ok.
    reason: String,
    prior_unix: i64,
    new_unix: i64,
    /// True when the payload reported `ok` but the post-set unix is
    /// still &gt;5s away from the requested target. Indicates the SDK
    /// stub returned success without actually touching the clock.
    stub_no_op: bool,
}

async fn ps5_time_get_route(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<PsTime, anyhow::Error> = tokio::task::spawn_blocking(move || ps5_time_get(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_time_sync_route(
    State(state): State<AppState>,
    Json(req): Json<TimeSyncReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let target = req.target_unix_seconds;
    let r: Result<PsTimeSetResult, anyhow::Error> =
        tokio::task::spawn_blocking(move || ps5_time_set(&addr, target))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => {
            // Stub-no-op heuristic: payload says ok, post-set get
            // succeeded (new_unix != -1), yet the clock is &gt;5s
            // away from what we asked for. Either the SDK stub is a
            // no-op on this firmware, or the underlying syscall is
            // refusing silently. Surface so the UI can warn.
            // i128 subtraction: `target` is request-controlled (i64 from
            // JSON) and `v.new_unix` is payload-derived, so `new_unix -
            // target` in i64 can overflow (e.g. target = i64::MIN) —
            // debug panics, release wraps to a wrong boolean. Widen so the
            // drift comparison is always correct.
            let stub_no_op =
                v.ok && v.new_unix >= 0 && (v.new_unix as i128 - target as i128).abs() > 5;
            let resp = TimeSyncResp {
                ok: v.ok,
                err_code: v.err_code,
                reason: sys_time_humanize(v.err_code),
                prior_unix: v.prior_unix,
                new_unix: v.new_unix,
                stub_no_op,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// Read the full PS5 Date & Time state (timezone, DST, NTP flag,
/// date/time format, tzdata version, NTP-error counter, cached NTP
/// tick, wall clock) in one round-trip. Best-effort: per-field
/// availability lets the UI degrade gracefully when the payload
/// can't read some keys on this firmware.
async fn ps5_time_state_get_route(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ps5upload_core::sys_time::PsTimeState, anyhow::Error> =
        tokio::task::spawn_blocking(move || ps5upload_core::sys_time::ps5_time_state_get(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// Write a partial subset of PS5 Date & Time state. Mirrors the
/// payload's partial-update semantics — only fields explicitly
/// present in the request JSON are written; everything else is
/// untouched. Returns per-field results so the UI can render which
/// writes took and which were rejected.
#[derive(serde::Deserialize)]
struct TimeStateSetReq {
    addr: Option<String>,
    #[serde(flatten)]
    fields: ps5upload_core::sys_time::PsTimeStateSetRequest,
}

async fn ps5_time_state_set_route(
    State(state): State<AppState>,
    Json(req): Json<TimeStateSetReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let fields = req.fields;
    let r: Result<ps5upload_core::sys_time::PsTimeStateSetResult, anyhow::Error> =
        tokio::task::spawn_blocking(move || {
            ps5upload_core::sys_time::ps5_time_state_set(&addr, &fields)
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// SMP-meta worker control. Wraps `smp_meta_control` in spawn_blocking
/// because the underlying Connection is sync TCP. The `addr` field on
/// the request body is optional — falls back to the engine's default
/// PS5 addr — keeping the route shape consistent with the rest of the
/// /api/ps5/* surface.
#[derive(serde::Deserialize)]
struct SmpMetaControlReq {
    addr: Option<String>,
    #[serde(flatten)]
    inner: ps5upload_core::smp_meta::SmpMetaControlRequest,
}

async fn ps5_smp_meta_control_route(
    State(state): State<AppState>,
    Json(req): Json<SmpMetaControlReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let inner = req.inner;
    let r: Result<ps5upload_core::smp_meta::SmpMetaControlAck, anyhow::Error> =
        tokio::task::spawn_blocking(move || {
            ps5upload_core::smp_meta::smp_meta_control(&addr, &inner)
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_smp_meta_stats_route(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ps5upload_core::smp_meta::SmpMetaStats, anyhow::Error> =
        tokio::task::spawn_blocking(move || ps5upload_core::smp_meta::smp_meta_stats(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// "Console Storage" aggregate. Same shape PS5 Settings shows: total
/// across `/user effective + /system_data + /system_ex`, free across
/// the same set, plus the per-partition breakdown for diagnostics.
async fn ps5_hw_storage(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<HwStorage, anyhow::Error> =
        tokio::task::spawn_blocking(move || hw_storage(&addr))
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
    // Reject `..` as a path *component* only — a substring check would
    // also reject legitimate names that merely contain ".." (e.g. a folder
    // literally named `my..game`). Traversal is what we're guarding
    // against, and that's always a standalone `..` segment.
    if path.split('/').any(|seg| seg == "..") {
        return Err((
            StatusCode::BAD_REQUEST,
            "path must not contain a '..' segment".into(),
        ));
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
        // Cap at the payload's single-read max (2 MiB), not 256 KiB: a
        // param.json with many localizedParameters locales can exceed
        // 256 KiB, and a truncated read fails to parse → silent "no
        // metadata". 2 MiB covers any realistic param.json in one call.
        let param_path = format!("{}/sce_sys/param.json", path.trim_end_matches('/'));
        let (title, title_id, content_id, content_version, application_category_type) =
            match fs_read(&addr, &param_path, 0, 2 * 1024 * 1024) {
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

// ─── Installed-apps inventory ─────────────────────────────────────────
//
// Lists every title the PS5 has metadata for (enumerated from
// /user/appmeta/<title_id>/ — Sony's per-title metadata cache), tagged by
// HOW it got there so the UI can group them:
//
//   origin="registered" — registered/mounted by ps5upload from a game
//                          folder, .exfat/.ffpkg image, or upload. We
//                          KNOW these: app_list_registered() reports them
//                          (they carry a /user/app/<id>/mount.lnk tracker)
//                          with the source path + whether a disk image
//                          backs them. Uninstalling unmounts our nullfs.
//   origin="pkg"        — installed from a .pkg through Sony's installer
//                          (AppInstUtil), or shipped with the console.
//                          No mount.lnk; Sony owns the app.db row + mount.
//                          NPXS-prefixed titles in this group (e.g. a Store
//                          fakepkg the user installed) carry system=true so
//                          the UI flags them as dangerous to uninstall — but
//                          they still belong to the "installed from package"
//                          group, since that's how they got there.
//
// Why filesystem enumeration and not Sony's app.db: dlopen'ing
// libSceSqlite hangs on FW 9.60 (see register.c), so app.db is off-limits.
// /user/appmeta is the safe, firmware-stable source — and it's where the
// cover art (icon0.png) lives, served by /api/ps5/app-icon below.

#[derive(Debug, serde::Serialize)]
struct InstalledApp {
    title_id: String,
    title_name: String,
    /// "registered" | "pkg" | "system" — see module comment above.
    origin: String,
    /// Only meaningful for origin=="registered": backed by a mounted
    /// disk image (.exfat/.ffpkg) vs a plain folder registration.
    image_backed: bool,
    /// Only meaningful for origin=="registered": the source path we
    /// registered the title from (empty otherwise).
    source: String,
    /// True for NPXS-prefixed system apps — the UI greys these and
    /// requires a stronger confirm before uninstalling.
    system: bool,
}

#[derive(Debug, serde::Serialize)]
struct InstalledAppsResponse {
    titles: Vec<InstalledApp>,
    /// True if app_list_registered() failed (e.g. older payload) — the
    /// UI then can't reliably tag the "registered" group, so it shows
    /// everything under "installed" with a soft note rather than erroring.
    registered_unavailable: bool,
}

/// PS5 title-ids are 4 uppercase letters + 5 digits (PPSA01234, NPXS39041,
/// CUSA00123, …). Used to filter stray non-title entries out of the
/// /user/appmeta listing.
fn looks_like_title_id(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 9
        && b[..4].iter().all(|c| c.is_ascii_uppercase())
        && b[4..].iter().all(|c| c.is_ascii_digit())
}

/// Best-effort title name from /user/appmeta/<title_id>/param.json. Returns
/// None on any failure (no param.json — common for system apps — bad JSON,
/// path denied); the caller falls back to the bare title_id.
fn appmeta_title_name(addr: &str, title_id: &str) -> Option<String> {
    let path = format!("/user/appmeta/{title_id}/param.json");
    let bytes = fs_read(addr, &path, 0, 256 * 1024).ok()?;
    let meta = parse_param_json_bytes(&bytes).ok()?;
    meta.title.filter(|t| !t.trim().is_empty())
}

/// GET /api/ps5/apps/installed?addr=IP:MGMT_PORT
async fn ps5_apps_installed(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let result: Result<InstalledAppsResponse, anyhow::Error> =
        tokio::task::spawn_blocking(move || {
            // Group B: titles WE registered/mounted (folder / image / upload).
            // Best-effort — if the payload can't report them, we degrade to
            // "everything is a pkg/system install" rather than failing.
            let (reg_map, registered_unavailable) = match app_list_registered(&addr) {
                Ok(r) => {
                    let m: std::collections::HashMap<String, _> = r
                        .apps
                        .into_iter()
                        .map(|a| (a.title_id.clone(), a))
                        .collect();
                    (m, false)
                }
                Err(e) => {
                    crate::log_warn!("apps/installed: app_list_registered failed: {e:#}");
                    (std::collections::HashMap::new(), true)
                }
            };

            // Full installed set: every /user/appmeta/<title_id>/ dir.
            let listing = list_dir(
                &addr,
                "/user/appmeta",
                ListDirOptions {
                    offset: 0,
                    limit: 512,
                },
            )?;

            let mut titles: Vec<InstalledApp> = Vec::new();
            for e in listing.entries {
                if !looks_like_title_id(&e.name) {
                    continue;
                }
                let tid = e.name;
                let system = tid.starts_with("NPXS");
                if let Some(reg) = reg_map.get(&tid) {
                    // Prefer the registered title_name; if it's empty or just
                    // the id (older registrations stored id-as-name), try
                    // param.json for a friendlier label.
                    let name = if reg.title_name.trim().is_empty() || reg.title_name == tid {
                        appmeta_title_name(&addr, &tid).unwrap_or_else(|| tid.clone())
                    } else {
                        reg.title_name.clone()
                    };
                    titles.push(InstalledApp {
                        title_id: tid.clone(),
                        title_name: name,
                        origin: "registered".to_string(),
                        image_backed: reg.image_backed,
                        source: reg.src.clone(),
                        system,
                    });
                } else {
                    let name = appmeta_title_name(&addr, &tid).unwrap_or_else(|| tid.clone());
                    titles.push(InstalledApp {
                        title_id: tid.clone(),
                        title_name: name,
                        // Always "pkg" for non-registered titles — including
                        // NPXS system-id fakepkgs the user installed. The
                        // `system` flag (below) is what the UI guards on, not
                        // a separate origin bucket.
                        origin: "pkg".to_string(),
                        image_backed: false,
                        source: String::new(),
                        system,
                    });
                }
            }
            // Stable order: registered first, then package installs; within
            // packages, system-flagged (dangerous) titles sort last; alpha
            // within each tier so the grid doesn't reshuffle between refreshes.
            titles.sort_by(|a, b| {
                fn rank(t: &InstalledApp) -> u8 {
                    match (t.origin.as_str(), t.system) {
                        ("registered", _) => 0,
                        ("pkg", false) => 1,
                        _ => 2, // pkg + system (e.g. Store fakepkg)
                    }
                }
                rank(a).cmp(&rank(b)).then_with(|| {
                    a.title_name
                        .to_lowercase()
                        .cmp(&b.title_name.to_lowercase())
                })
            });
            Ok(InstalledAppsResponse {
                titles,
                registered_unavailable,
            })
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|inner| inner);
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct AppIconQuery {
    addr: Option<String>,
    title_id: String,
}

/// GET /api/ps5/app-icon?addr=IP:MGMT_PORT&title_id=PPSA01234
///
/// Streams /user/appmeta/<title_id>/icon0.png as image/png — the cover
/// art for an installed title. Mirrors /api/ps5/game-icon but keyed by
/// title_id instead of folder path. 404 on any miss so `<img onerror>`
/// can fall back to a placeholder.
async fn ps5_app_icon(
    State(state): State<AppState>,
    Query(q): Query<AppIconQuery>,
) -> impl IntoResponse {
    // title_id is interpolated into a filesystem path, so validate hard:
    // PS5 title-ids are [A-Za-z0-9_] only. This blocks `..` / `/` traversal
    // outright rather than relying solely on the payload's is_path_allowed.
    if q.title_id.is_empty()
        || q.title_id.len() > 16
        || !q
            .title_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return (StatusCode::BAD_REQUEST, "invalid title_id").into_response();
    }
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let icon_path = format!("/user/appmeta/{}/icon0.png", q.title_id);
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
                (header::CACHE_CONTROL, "private, max-age=300"),
            ],
            bytes,
        )
            .into_response(),
        _ => (StatusCode::NOT_FOUND, "no icon").into_response(),
    }
}

/// One `.pkg` found on a connected external/USB drive.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExternalPkg {
    /// Absolute on-console path, e.g. `/mnt/usb0/games/foo.pkg`. This is a
    /// local PS5 path, so install runs straight through the normal
    /// install-from-path cascade — no upload/staging needed.
    pub path: String,
    /// The drive mount this was found under (`/mnt/usb0`, `/mnt/ext1`).
    pub drive: String,
    /// Basename (`foo.pkg`).
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// ContentID from the header (empty for `\x7FFIH` / unreadable headers).
    pub content_id: String,
    /// Title id (CUSA…/PPSA…) derived from the content id.
    pub title_id: String,
    /// `"ps4"` | `"ps5"` | `""` — from header magic + title-id prefix.
    pub platform: String,
}

/// Parse a `.pkg`'s first 0xA0 header bytes for the fields the scan needs.
/// Cheap (one `fs_read` of 160 bytes) vs. a full PARAM.SFO walk — enough for
/// the listing's name/size/platform badge.
fn external_pkg_header(head: &[u8]) -> (String, String, String) {
    if head.len() < 0xA0 {
        return (String::new(), String::new(), String::new());
    }
    let magic = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let content_id = if magic == ps5upload_pkg::PKG_MAGIC {
        let raw = &head[0x40..0x40 + 36];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(36);
        String::from_utf8_lossy(&raw[..end]).trim().to_string()
    } else {
        String::new()
    };
    let title_id =
        ps5upload_core::pkg_install::title_id_from_content_id(&content_id).unwrap_or_default();
    let platform = ps5upload_pkg::derive_platform(magic, &content_id, &title_id);
    (content_id, title_id, platform)
}

/// Scan connected external/USB drives for installable `.pkg` files.
///
/// Walks every real `/mnt/usb*` and `/mnt/ext*` mount depth-first, reading
/// each `.pkg`'s header for content_id + platform. Bounded on depth,
/// directories visited, and packages returned so a multi-thousand-file game
/// drive can't wedge the scan. Errors on individual dirs/files are skipped
/// (best-effort) rather than failing the whole scan.
pub fn scan_external_pkgs(addr: &str) -> anyhow::Result<Vec<ExternalPkg>> {
    use ps5upload_core::fs_ops::{fs_read, list_dir, ListDirOptions};
    const MAX_DEPTH: u32 = 5;
    const MAX_DIRS: usize = 600;
    const MAX_PKGS: usize = 256;

    let join = |dir: &str, name: &str| format!("{}/{}", dir.trim_end_matches('/'), name);
    let vols = list_volumes(addr)?;
    let mut out: Vec<ExternalPkg> = Vec::new();
    let mut dirs_visited = 0usize;

    for v in vols.volumes.iter() {
        let external = v.path.starts_with("/mnt/usb") || v.path.starts_with("/mnt/ext");
        // Skip placeholder/empty slots (a hot-plug tmpfs left behind). We do
        // NOT require `writable` — a read-only mount can still hold installable
        // .pkg files — only that it's a real, non-empty device.
        if !external || v.is_placeholder || v.total_bytes == 0 {
            continue;
        }
        let mut stack: Vec<(String, u32)> = vec![(v.path.clone(), 0)];
        while let Some((dir, depth)) = stack.pop() {
            if out.len() >= MAX_PKGS || dirs_visited >= MAX_DIRS {
                break;
            }
            dirs_visited += 1;
            let listing = match list_dir(addr, &dir, ListDirOptions::default()) {
                Ok(l) => l,
                Err(_) => continue,
            };
            for e in listing.entries {
                if e.kind == "dir" {
                    if depth + 1 < MAX_DEPTH {
                        stack.push((join(&dir, &e.name), depth + 1));
                    }
                } else if e.name.to_ascii_lowercase().ends_with(".pkg") {
                    if out.len() >= MAX_PKGS {
                        break;
                    }
                    let path = join(&dir, &e.name);
                    // Fast path: scene/store names almost always carry the
                    // title id (CUSA#####/PPSA#####), which is enough to badge
                    // the platform. Deriving it from the name avoids a
                    // per-file header read — the dominant cost of the scan was
                    // one blocking ~160-byte console RPC PER package, which on
                    // a drive of dozens of pkgs made the scan crawl. Only fall
                    // back to reading the header when the filename tells us
                    // nothing (content_id stays empty; the install path reads
                    // it from the package itself anyway).
                    let (content_id, title_id, platform) =
                        match ps5upload_pkg::title_id_from_filename(&e.name) {
                            Some(tid) => {
                                let platform = ps5upload_pkg::derive_platform(
                                    ps5upload_pkg::PKG_MAGIC,
                                    "",
                                    &tid,
                                );
                                (String::new(), tid, platform)
                            }
                            None => match fs_read(addr, &path, 0, 0xA0) {
                                Ok(bytes) => external_pkg_header(&bytes),
                                Err(_) => (String::new(), String::new(), String::new()),
                            },
                        };
                    out.push(ExternalPkg {
                        path,
                        drive: v.path.clone(),
                        name: e.name,
                        size: e.size,
                        content_id,
                        title_id,
                        platform,
                    });
                }
            }
        }
    }
    // Stable, human-friendly order: by drive then name.
    out.sort_by(|a, b| a.drive.cmp(&b.drive).then(a.name.cmp(&b.name)));
    Ok(out)
}

/// GET /api/ps5/pkg/scan-external?addr=IP:PORT — list `.pkg` files found on
/// connected external/USB drives, parsed for platform + content id. These
/// install in place (no upload) via the normal install-from-path cascade.
async fn ps5_pkg_scan_external(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let result: Result<Vec<ExternalPkg>, anyhow::Error> =
        tokio::task::spawn_blocking(move || scan_external_pkgs(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(v) => (StatusCode::OK, Json(serde_json::json!({ "packages": v }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct PkgMetadataQuery {
    addr: Option<String>,
    /// Absolute on-console pkg path, e.g. `/mnt/usb0/games/foo.pkg`. Subject to
    /// the same `fs_read` allowlist as every other read (so it can only reach
    /// the readable mounts — `/mnt/usb*`, `/mnt/ext*`, `/user`, `/data`, …).
    path: String,
}

/// GET /api/ps5/pkg/metadata?addr=IP:PORT&path=/mnt/usb0/foo.pkg
///
/// Parse one on-console pkg's content id + PARAM.SFO fields (title, category,
/// APP_VER) via a few ranged `fs_read`s. This lazily enriches the External
/// Packages listing — the bulk scan stays filename-fast; the client calls this
/// per row, in the background, only for what's on screen. Returns an empty
/// object for a non-`\x7FCNT` / unreadable pkg (the row keeps its scan data).
async fn ps5_pkg_metadata(
    State(state): State<AppState>,
    Query(q): Query<PkgMetadataQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let path = q.path;
    let result = tokio::task::spawn_blocking(move || {
        ps5upload_pkg::metadata_from_reader(|off, len| {
            ps5upload_core::fs_ops::fs_read(&addr, &path, off, len).ok()
        })
    })
    .await;
    match result {
        Ok(meta) => (StatusCode::OK, Json(meta.unwrap_or_default())).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
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

/// GET /api/ps5/readiness?addr=IP:MGMT_PORT
///
/// Lightweight "is the console in a stable state to install a .pkg" probe. It
/// round-trips the AppListRegistered frame — the exact request that goes
/// unanswered ("read frame header: failed to fill whole buffer" / ECONNRESET)
/// while the console is recovering from a prior install (the post-install
/// SceShellUI black-screen blip). A clean response ⇒ the console is settled and
/// ready to take another install; an error ⇒ it's still busy. Always returns
/// 200 with `{ ready, detail }` so the client reads `ready` directly instead of
/// having to treat a transient "busy" as an HTTP failure.
async fn ps5_readiness(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let result = tokio::task::spawn_blocking(move || app_list_registered(&addr)).await;
    let (ready, detail) = match result {
        Ok(Ok(_)) => (true, String::new()),
        Ok(Err(e)) => (false, format!("{e:#}")),
        Err(e) => (false, format!("{e:#}")),
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ready": ready, "detail": detail })),
    )
        .into_response()
}

/// POST /api/transfer/file
async fn transfer_file_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferFileReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    // Track whether the caller minted+supplied a tx_id (resume-capable
    // client) vs. asked us to mint one (fresh attempt with no prior
    // partial). Mirrors the dir handler's contract — a caller-supplied
    // tx_id signals "adopt the payload's existing entry for this id if
    // it has one." We pass TX_FLAG_RESUME on the very first BeginTx so
    // an interrupted prior upload's last_acked_shard is honored.
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
        "transfer_file: job={job_id} addr={addr} src={} dest={} resume={caller_supplied_tx_id}",
        req.src,
        req.dest
    );
    // Pre-stat the source — used both for the progress-bar denominator
    // and as a fail-fast check before we accept the job. Previously the
    // metadata error was silently swallowed (`unwrap_or(0)`), so a
    // missing source produced "Running, 0 bytes total" for several
    // seconds before the actual transfer attempt failed. Returning
    // 400 here surfaces the user error immediately at the API level.
    // Stat via spawn_blocking — sources can live on network mounts
    // where a blocking stat would stall the reactor for every console.
    let src_for_stat = req.src.clone();
    let total_bytes = match tokio::task::spawn_blocking(move || std::fs::metadata(&src_for_stat))
        .await
    {
        Ok(Ok(m)) if m.is_file() => m.len(),
        Ok(Ok(_)) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("source is not a regular file: {}", req.src),
            )
            .into_response();
        }
        Ok(Err(e)) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("cannot read source {}: {e}", req.src),
            )
            .into_response();
        }
        Err(e) => {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
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
    let progress_files = Arc::new(AtomicU64::new(0));
    // P3 / v2.18.0 — apply-phase counters. The engine's
    // send_commit_and_expect_ack reads APPLY_PROGRESS frames from
    // the payload during the commit wait and stores into these.
    // The ticker (spawn_progress_ticker) reads and writes them to
    // JobState::Running's files_finalized / bytes_finalized fields.
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            // P3 / v2.18.0 — apply-phase counters start at 0; the
            // ticker fills them in once APPLY_PROGRESS frames begin
            // arriving from the payload during commit.
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    let bandwidth_cap = req.bandwidth_cap_mbps;
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
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, bandwidth_cap);

        // Pre-flight free-space check. Catches the most common cause
        // of mid-stream `direct_writer_io_error` failures (PS5
        // destination drive ran out of space) before the user wastes
        // an hour uploading bytes that can't fit. Runs inside this
        // spawn_blocking task — the ~200-500 ms list_volumes round-
        // trip doesn't slow the API response, and any failure
        // surfaces as a Failed job with the standard error-card
        // plumbing (humanized hint + raw detail).
        //
        // We skip the check on resume attempts (caller_supplied_tx_id):
        // the destination already has shards from a prior attempt, so
        // free_bytes < total_bytes is the EXPECTED state — the bytes
        // we need to write are smaller than total minus what's
        // already there. Skipping avoids a false-positive block on
        // every Retry click for a long-running upload.
        //
        // Errors during the check (network blip, payload restarting)
        // are non-fatal: we log and proceed with the upload. Better
        // UX than refusing a transfer that might have succeeded.
        if !caller_supplied_tx_id {
            let mgmt = mgmt_addr_for(&addr);
            match list_volumes(&mgmt) {
                Ok(vlist) => {
                    if let Some(vol) = vlist.find_for_path(&req.dest) {
                        if vol.free_bytes < total_bytes {
                            let short = total_bytes - vol.free_bytes;
                            let completed_at_ms = now_ms();
                            set_job(
                                &jobs,
                                &events_tx,
                                job_id,
                                JobState::Failed {
                                    started_at_ms,
                                    completed_at_ms,
                                    elapsed_ms: completed_at_ms
                                        .saturating_sub(started_at_ms),
                                    error: format!(
                                        "Destination volume `{}` has only {} bytes free, but the source file is {} bytes ({} short).",
                                        vol.path, vol.free_bytes, total_bytes, short
                                    ),
                                    error_reason: Some(
                                        "preflight_insufficient_space".to_string(),
                                    ),
                                    error_detail: Some(format!(
                                        "{} short by {} bytes (have {}, need {}).",
                                        vol.path, short, vol.free_bytes, total_bytes
                                    )),
                                },
                            );
                            fail_guard.mark_succeeded();
                            return;
                        }
                    }
                    // No matching volume: don't block — better to let
                    // the upload try than to refuse on a stale or
                    // incomplete volume table.
                }
                Err(e) => {
                    eprintln!(
                        "[preflight] free-space check failed for {addr}: {e:#}; proceeding with upload"
                    );
                }
            }
        }

        // Resume-on-drop for single-file uploads: 1 fresh attempt +
        // DEFAULT_RESUME_RETRIES resumes. WiFi-only PS5s see multi-hour
        // uploads of 50+ GiB images, and a stack of retries with exponential
        // backoff (500 ms → 16 s capped) survives several wifi blips per
        // upload before giving up.
        //
        // The path-based core reads one shard at a time. It avoids both
        // whole-file Vec allocation and mmap address-space/page-cache
        // failure modes that can look like OOM on Windows/Linux with
        // 50-100 GiB game images.
        let src_path = std::path::PathBuf::from(&req.src);
        let result = transfer_file_path_resumable(
            &cfg,
            tx_id,
            &req.dest,
            &src_path,
            DEFAULT_RESUME_RETRIES,
            initial_flags,
        );
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

    // Validate the source and build the upload plan OFF the async reactor.
    // walk_plan does a recursive read_dir+metadata over the whole source tree
    // (46k-file games are routine; a slow SMB/UNC share can take minutes).
    // Running it inline on the bare #[tokio::main] runtime would park a worker
    // thread for that entire walk, stalling SSE, /pkg-host serving, and every
    // OTHER console's requests. The client already waits for this walk before
    // it receives the job_id (the plan must exist to seed JobState::Running),
    // so moving it to a blocking thread changes which thread blocks, not the
    // client-observed latency. (Mirrors transfer_download_handler's enumeration.)
    let (total_bytes, files) = {
        let src_dir = req.src_dir.clone();
        let excludes = req.excludes.clone();
        match tokio::task::spawn_blocking(move || {
            // Fail fast on a missing / non-directory source. walk_plan swallows
            // read errors (returns empty), so without this a typo'd path or a
            // permissions problem would start a "Running → 0 bytes" job (or a
            // fake "Done, 0 files") instead of a clear error — mirroring the
            // up-front stat that transfer_file_handler does for single files.
            let p = std::path::Path::new(&src_dir);
            if !p.is_dir() {
                return Err(format!(
                    "source directory not found or not a directory: {src_dir}"
                ));
            }
            Ok(walk_plan(p, &excludes))
        })
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(msg)) => return json_err(StatusCode::BAD_REQUEST, msg).into_response(),
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("source walk task panicked/cancelled: {e}"),
                )
                .into_response()
            }
        }
    };
    let files_sent_count = files.len() as u64;
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    // P3 / v2.18.0 — apply-phase counters. The engine's
    // send_commit_and_expect_ack reads APPLY_PROGRESS frames from
    // the payload during the commit wait and stores into these.
    // The ticker (spawn_progress_ticker) reads and writes them to
    // JobState::Running's files_finalized / bytes_finalized fields.
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            // P3 / v2.18.0 — apply-phase counters start at 0; the
            // ticker fills them in once APPLY_PROGRESS frames begin
            // arriving from the payload during commit.
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    tokio::task::spawn_blocking(move || {
        // See ticker stop-guard rationale at the file-upload spawn site.
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        // 1 fresh attempt + DEFAULT_RESUME_RETRIES resumes. Folder uploads
        // previously used only 2 retries while single-file used 5, so a single
        // transient blip (or the payload's serial accept loop briefly busy
        // draining the dropped connection) killed a multi-hour folder upload.
        // Now on equal footing with single-file + headroom. See
        // DEFAULT_RESUME_RETRIES.
        let result = transfer_dir_resumable(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.src_dir),
            DEFAULT_RESUME_RETRIES,
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

/// POST /api/zip/inspect — central-directory-only preview of a `.zip`
/// (file count, compressed vs uncompressed size, embedded game metadata).
/// Never inflates the bulk of the archive. Used by the Upload screen so the
/// user sees what a compressed dump expands to before sending it.
///
/// Pre-2.18.5 this handler logged nothing: when a user reported "engine
/// request failed: error sending request for url (/api/zip/inspect)" we
/// couldn't tell whether the engine had panicked, was still scanning a
/// cold-cache HDD, or had returned successfully after the client timeout
/// fired. Entry + outcome + duration logs make the next report trivially
/// diagnosable from engine.log alone.
async fn zip_inspect_handler(Json(req): Json<ZipInspectReq>) -> impl IntoResponse {
    let zip_path = req.zip_path;
    crate::log_info!("zip_inspect: zip={zip_path}");
    let started = std::time::Instant::now();
    let result = tokio::task::spawn_blocking({
        let zp = zip_path.clone();
        move || inspect_zip(std::path::Path::new(&zp))
    })
    .await;
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(Ok(inspect)) => {
            crate::log_info!(
                "zip_inspect ok: zip={zip_path} files={} compressed={} elapsed_ms={elapsed_ms}",
                inspect.file_count,
                inspect.compressed_size,
            );
            (StatusCode::OK, Json(inspect)).into_response()
        }
        Ok(Err(e)) => {
            crate::log_warn!(
                "zip_inspect failed: zip={zip_path} elapsed_ms={elapsed_ms} err={e:#}"
            );
            json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "zip_inspect panicked: zip={zip_path} elapsed_ms={elapsed_ms} err={e}"
            );
            json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// POST /api/zip/inspect/stream — same shape as `/api/zip/inspect` but
/// returns an `Sse` response so the client can render progress and the
/// connection can survive a slow cold-cache central-directory read.
///
/// **Why streaming instead of just the long-deadline `/api/zip/inspect`:**
/// even after Phase 2 (zero-seek CD parser) cut warm inspect to ~35 ms,
/// pathological cases (network mount, spun-down USB HDD, very large
/// zips with 100k+ entries) can still take several seconds. The user
/// reported "engine request failed: error sending request" was the
/// short-deadline (60 s) timer firing on the old per-entry-seek
/// pipeline; with this endpoint the client uses a dead-man-switch
/// watchdog instead — "no chunk for N seconds" — and shows live entry
/// counts during the wait. The handler emits three event types:
///
/// - `progress` — `{"entries_seen": N}` periodically while the central
///   directory is being walked. Drives the UI "Scanning archive… N
///   entries" indicator and resets the client watchdog.
/// - `done` — `{...ZipInspect...}` once. Final event; client closes
///   the connection on receipt.
/// - `error` — `{"error": "..."}` once. Final event on a parse / IO
///   failure. The HTTP status stays 200 because headers ship before
///   the worker knows whether inspect will succeed; this matches
///   `events_stream` (the existing SSE precedent in this engine).
///
/// `KeepAlive::interval(1s)` makes axum send an SSE comment line every
/// second when no real event is in flight, so the client's watchdog
/// sees forward motion even during a CD bulk-read that emits no
/// progress callbacks. The 1 s cadence is short enough to keep the UI
/// "Scanning…" indicator responsive without flooding the channel.
async fn zip_inspect_stream_handler(Json(req): Json<ZipInspectReq>) -> impl IntoResponse {
    let zip_path = req.zip_path;
    crate::log_info!("zip_inspect (stream): zip={zip_path}");
    let started = std::time::Instant::now();

    let (tx, rx) = mpsc::channel::<InspectStreamEvent>(64);

    let zip_path_for_worker = zip_path.clone();
    tokio::task::spawn_blocking(move || {
        let tx_progress = tx.clone();
        let result = ps5upload_core::transfer::inspect_zip_with_progress(
            std::path::Path::new(&zip_path_for_worker),
            move |n| {
                // `blocking_send` is the right primitive from inside
                // `spawn_blocking`: it blocks the worker thread (not a
                // tokio task) until the receiver has room. Dropped on
                // client disconnect — the parser keeps running but its
                // sends become no-ops, so the orphaned worker drains
                // quickly without blocking forever.
                let _ = tx_progress.blocking_send(InspectStreamEvent::Progress(n));
            },
        );
        // Log the outcome from the worker thread so we have engine.log
        // evidence even when the client disconnects mid-stream (curl
        // killed by `head`, renderer-unmount, browser-tab-closed). If
        // we logged in the SSE mapper instead, abandoned inspects would
        // leave no trace and the next user report would again be
        // "engine said nothing".
        let elapsed_ms = started.elapsed().as_millis();
        let final_event = match result {
            Ok(inspect) => {
                crate::log_info!(
                    "zip_inspect (stream) ok: zip={zip_path_for_worker} files={} compressed={} elapsed_ms={elapsed_ms}",
                    inspect.file_count,
                    inspect.compressed_size,
                );
                InspectStreamEvent::Done(Box::new(inspect))
            }
            Err(e) => {
                let err = format!("{e:#}");
                crate::log_warn!(
                    "zip_inspect (stream) failed: zip={zip_path_for_worker} elapsed_ms={elapsed_ms} err={err}"
                );
                InspectStreamEvent::Error(err)
            }
        };
        let _ = tx.blocking_send(final_event);
    });

    let stream = ReceiverStream::new(rx).map(|event| {
        let sse_event = match event {
            InspectStreamEvent::Progress(n) => Event::default()
                .event("progress")
                .data(serde_json::json!({ "entries_seen": n }).to_string()),
            InspectStreamEvent::Done(inspect) => Event::default()
                .event("done")
                .data(serde_json::to_string(&*inspect).unwrap_or_else(|_| "{}".to_string())),
            InspectStreamEvent::Error(err) => Event::default()
                .event("error")
                .data(serde_json::json!({ "error": err }).to_string()),
        };
        Ok::<_, std::convert::Infallible>(sse_event)
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(1))
            .text("heartbeat"),
    )
}

enum InspectStreamEvent {
    Progress(u64),
    // Box the inspect result so the variant doesn't bloat to 200+ bytes
    // and trigger clippy::large_enum_variant on the small Progress arm.
    Done(Box<ps5upload_core::transfer::ZipInspect>),
    Error(String),
}

/// POST /api/transfer/zip — start a zip-archive upload job. Mirrors
/// `transfer_dir_handler`: plan (central directory) → BEGIN_TX → stream shards
/// (inflating one entry at a time) → COMMIT_TX, with the same job/progress/SSE
/// plumbing and resume semantics. The progress denominator is the *total
/// uncompressed* size (what actually lands on the PS5).
async fn transfer_zip_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferZipReq>,
) -> impl IntoResponse {
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

    // Central-directory-only plan: total uncompressed bytes (progress
    // denominator) + the file list (UI tree). A corrupt/missing zip fails
    // here with a clear message instead of starting a doomed job.
    //
    // Plan duration logged because it's the chunk of work the client
    // blocks on before getting back a job_id: if the user reports
    // "engine request failed: error sending request" against
    // /api/transfer/zip, the elapsed_ms here tells us whether plan
    // legitimately blew the client timeout (yes → bigger zip than the
    // pipeline can plan in time) or completed quickly (no → look at
    // BEGIN_TX / connectivity).
    let plan_started = std::time::Instant::now();
    let (total_bytes, preview) = {
        // Central-directory read OFF the async reactor: a cold-HDD or
        // 100k-entry zip can take seconds, and inline on the bare runtime that
        // parks a reactor worker thread — stalling SSE, /pkg-host serving, and
        // every OTHER console. The client already blocks on this plan for its
        // job_id, so this only moves which thread waits. (zip_inspect_handler
        // already runs the same parser via spawn_blocking — this was the holdout.)
        let zip_path = req.zip_path.clone();
        let excludes = req.excludes.clone();
        let planned = tokio::task::spawn_blocking(move || {
            zip_plan_preview(std::path::Path::new(&zip_path), &excludes)
        })
        .await;
        match planned {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let elapsed_ms = plan_started.elapsed().as_millis();
                crate::log_warn!(
                    "transfer_zip plan failed: zip={} elapsed_ms={elapsed_ms} err={e:#}",
                    req.zip_path,
                );
                return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("zip planning task panicked/cancelled: {e}"),
                )
                .into_response()
            }
        }
    };
    let plan_elapsed_ms = plan_started.elapsed().as_millis();
    crate::log_info!(
        "transfer_zip plan: zip={} entries={} bytes={total_bytes} elapsed_ms={plan_elapsed_ms}",
        req.zip_path,
        preview.len(),
    );
    let files: Vec<PlannedFile> = preview
        .into_iter()
        .map(|(rel_path, size)| PlannedFile { rel_path, size })
        .collect();
    let files_sent_count = files.len() as u64;

    // RAM-vs-temp inflate threshold: request override (MiB) → env → core
    // default. Bounds the host memory/temp the zip path can use at once.
    let ram_threshold = req
        .ram_threshold_mb
        .map(|mb| mb.saturating_mul(1024 * 1024))
        .or_else(|| {
            std::env::var("FTX2_ZIP_RAM_THRESHOLD_MB")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|mb| mb.saturating_mul(1024 * 1024))
        })
        .unwrap_or(DEFAULT_ZIP_ENTRY_RAM_THRESHOLD);

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_zip: job={job_id} addr={addr} zip={} dest_root={} resume={} files={} bytes={total_bytes}",
        req.zip_path,
        req.dest_root,
        caller_supplied_tx_id,
        files_sent_count
    );
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    // P3 / v2.18.0 — apply-phase counters. The engine's
    // send_commit_and_expect_ack reads APPLY_PROGRESS frames from
    // the payload during the commit wait and stores into these.
    // The ticker (spawn_progress_ticker) reads and writes them to
    // JobState::Running's files_finalized / bytes_finalized fields.
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            // P3 / v2.18.0 — apply-phase counters start at 0; the
            // ticker fills them in once APPLY_PROGRESS frames begin
            // arriving from the payload during commit.
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        let result = transfer_zip_resumable(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.zip_path),
            ram_threshold,
            2,
            initial_flags,
        );
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
                        skipped_files: 0,
                        skipped_bytes: 0,
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

// ── .7z handlers ── mirror the zip handlers; see those for the rationale on
//    spawn_blocking (header read off the reactor), the SSE watchdog, and the
//    synchronous pre-flight plan before a job_id is returned. The only shape
//    difference is no ram_threshold (7z streams forward-only).

/// POST /api/7z/inspect — counts/sizes only (the header is tiny even for a
/// 124 GB archive). Reuses the `ZipInspect` response shape.
async fn sevenz_inspect_handler(Json(req): Json<SevenzInspectReq>) -> impl IntoResponse {
    let archive_path = req.archive_path;
    crate::log_info!("sevenz_inspect: archive={archive_path}");
    let started = std::time::Instant::now();
    let result = tokio::task::spawn_blocking({
        let p = archive_path.clone();
        move || inspect_7z(std::path::Path::new(&p))
    })
    .await;
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(Ok(inspect)) => {
            crate::log_info!(
                "sevenz_inspect ok: archive={archive_path} files={} compressed={} elapsed_ms={elapsed_ms}",
                inspect.file_count,
                inspect.compressed_size,
            );
            (StatusCode::OK, Json(inspect)).into_response()
        }
        Ok(Err(e)) => {
            crate::log_warn!(
                "sevenz_inspect failed: archive={archive_path} elapsed_ms={elapsed_ms} err={e:#}"
            );
            json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "sevenz_inspect panicked: archive={archive_path} elapsed_ms={elapsed_ms} err={e}"
            );
            json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// POST /api/7z/inspect/stream — SSE variant; same event shapes as the zip
/// stream (progress / done / error + heartbeat).
async fn sevenz_inspect_stream_handler(Json(req): Json<SevenzInspectReq>) -> impl IntoResponse {
    let archive_path = req.archive_path;
    crate::log_info!("sevenz_inspect (stream): archive={archive_path}");
    let started = std::time::Instant::now();

    let (tx, rx) = mpsc::channel::<InspectStreamEvent>(64);

    let path_for_worker = archive_path.clone();
    tokio::task::spawn_blocking(move || {
        let tx_progress = tx.clone();
        let result = ps5upload_core::transfer::inspect_7z_with_progress(
            std::path::Path::new(&path_for_worker),
            move |n| {
                let _ = tx_progress.blocking_send(InspectStreamEvent::Progress(n));
            },
        );
        let elapsed_ms = started.elapsed().as_millis();
        let final_event = match result {
            Ok(inspect) => {
                crate::log_info!(
                    "sevenz_inspect (stream) ok: archive={path_for_worker} files={} compressed={} elapsed_ms={elapsed_ms}",
                    inspect.file_count,
                    inspect.compressed_size,
                );
                InspectStreamEvent::Done(Box::new(inspect))
            }
            Err(e) => {
                let err = format!("{e:#}");
                crate::log_warn!(
                    "sevenz_inspect (stream) failed: archive={path_for_worker} elapsed_ms={elapsed_ms} err={err}"
                );
                InspectStreamEvent::Error(err)
            }
        };
        let _ = tx.blocking_send(final_event);
    });

    let stream = ReceiverStream::new(rx).map(|event| {
        let sse_event = match event {
            InspectStreamEvent::Progress(n) => Event::default()
                .event("progress")
                .data(serde_json::json!({ "entries_seen": n }).to_string()),
            InspectStreamEvent::Done(inspect) => Event::default()
                .event("done")
                .data(serde_json::to_string(&*inspect).unwrap_or_else(|_| "{}".to_string())),
            InspectStreamEvent::Error(err) => Event::default()
                .event("error")
                .data(serde_json::json!({ "error": err }).to_string()),
        };
        Ok::<_, std::convert::Infallible>(sse_event)
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(std::time::Duration::from_secs(1))
            .text("heartbeat"),
    )
}

/// POST /api/transfer/7z — start a 7z-archive upload job. Same job/progress/SSE
/// plumbing and resume semantics as the zip path; the progress denominator is
/// the total uncompressed size.
// ─── Profile (avatar + offline-account username) ────────────────────────────

#[derive(Deserialize)]
struct ProfileUsernameReq {
    addr: Option<String>,
    slot: i32,
    name: String,
}

#[derive(Deserialize)]
struct ProfileLocalUsernameReq {
    addr: Option<String>,
    uid: u32,
    name: String,
}

#[derive(Deserialize)]
struct ProfileActivateReq {
    addr: Option<String>,
    slot: i32,
    #[serde(default)]
    id: Option<u64>,
}

#[derive(Deserialize)]
struct ProfileSlotReq {
    addr: Option<String>,
    slot: i32,
}

#[derive(Deserialize)]
struct ProfileAvatarReq {
    addr: Option<String>,
    /// Host-side path to the source image the user picked (same model as
    /// the zip/7z handlers, which take a host `archive_path`).
    image_path: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize)]
struct ProfilePreviewReq {
    image_path: String,
    #[serde(default)]
    mode: Option<String>,
}

async fn profile_info_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::profile::profile_info(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn profile_username_handler(
    State(state): State<AppState>,
    Json(req): Json<ProfileUsernameReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let slot = req.slot;
    let name = req.name;
    crate::log_info!("profile_set_username: addr={addr} slot={slot}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_set_username(&addr, slot, &name)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn profile_local_username_handler(
    State(state): State<AppState>,
    Json(req): Json<ProfileLocalUsernameReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let uid = req.uid;
    let name = req.name;
    crate::log_info!("profile_set_local_username: addr={addr} uid={uid}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_set_local_username(&addr, uid, &name)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn profile_activate_handler(
    State(state): State<AppState>,
    Json(req): Json<ProfileActivateReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let slot = req.slot;
    let id = req.id;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_activate(&addr, slot, id)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(id) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "id": id })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn profile_clear_slot_handler(
    State(state): State<AppState>,
    Json(req): Json<ProfileSlotReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let slot = req.slot;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_clear_slot(&addr, slot)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn profile_avatar_preview_handler(Json(req): Json<ProfilePreviewReq>) -> impl IntoResponse {
    let mode = ps5upload_core::profile::SquareMode::parse(req.mode.as_deref().unwrap_or("crop"));
    let path = req.image_path;
    let r: Result<Vec<u8>, anyhow::Error> = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&path).map_err(|e| anyhow::anyhow!("read image {path}: {e}"))?;
        ps5upload_core::profile::avatar_preview_png(&bytes, mode)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(png) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
            let data_url = format!("data:image/png;base64,{b64}");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "data_url": data_url })),
            )
                .into_response()
        }
        Err(e) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct AvatarCurrentQuery {
    addr: Option<String>,
    uid: u32,
}

/// GET /api/profile/avatar/current?addr&uid — read the user's CURRENT avatar
/// image from Sony's profile cache so the UI can show it before a change. The
/// cache dir is `/system_data/priv/cache/profile/0x<UID>/` — UPPERCASE hex, to
/// match the payload's `0x%08X`; `avatar.png` (fall back to `picture.png`) is
/// the squared source our apply (and offact) writes there. 117 KB-ish, well
/// under the 2 MiB FS_READ cap, so one read suffices. A user who never set a
/// custom avatar may have no PNG there — then `data_url` is null and the UI
/// falls back to its placeholder.
async fn profile_avatar_current_handler(
    State(state): State<AppState>,
    Query(q): Query<AvatarCurrentQuery>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(q.addr, &state.default_ps5_addr);
    let uid = q.uid;
    let png: Option<Vec<u8>> = tokio::task::spawn_blocking(move || {
        let dir = format!("/system_data/priv/cache/profile/0x{uid:08X}");
        for name in ["avatar.png", "picture.png"] {
            let path = format!("{dir}/{name}");
            if let Ok(bytes) = ps5upload_core::fs_ops::fs_read(&addr, &path, 0, 2 * 1024 * 1024) {
                // Only trust a real PNG (the cache also holds .dds we can't show).
                if bytes.starts_with(b"\x89PNG") {
                    return Some(bytes);
                }
            }
        }
        None
    })
    .await
    .unwrap_or(None);
    let data_url = png.map(|bytes| {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        format!("data:image/png;base64,{b64}")
    });
    (
        StatusCode::OK,
        Json(serde_json::json!({ "data_url": data_url })),
    )
        .into_response()
}

async fn profile_avatar_handler(
    State(state): State<AppState>,
    Json(req): Json<ProfileAvatarReq>,
) -> impl IntoResponse {
    let addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    let mode = ps5upload_core::profile::SquareMode::parse(req.mode.as_deref().unwrap_or("crop"));
    let uid = req.uid.unwrap_or(0);
    let username = req.username;
    let image_path = req.image_path;
    let started = std::time::Instant::now();
    crate::log_info!("profile_avatar: addr={addr} image={image_path} mode={mode:?} uid={uid}");
    let r = tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&image_path)
            .map_err(|e| anyhow::anyhow!("read image {image_path}: {e}"))?;
        ps5upload_core::profile::profile_apply_avatar(&addr, uid, username.as_deref(), &bytes, mode)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(applied) => {
            crate::log_info!(
                "profile_avatar ok: uid={} files={} in {} ms",
                applied.uid,
                applied.files_copied,
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(applied)).into_response()
        }
        Err(e) => {
            crate::log_warn!(
                "profile_avatar failed in {} ms: {e:#}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn transfer_7z_handler(
    State(state): State<AppState>,
    Json(req): Json<Transfer7zReq>,
) -> impl IntoResponse {
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

    let plan_started = std::time::Instant::now();
    let (total_bytes, preview) = {
        let archive_path = req.archive_path.clone();
        let excludes = req.excludes.clone();
        let planned = tokio::task::spawn_blocking(move || {
            sevenz_plan_preview(std::path::Path::new(&archive_path), &excludes)
        })
        .await;
        match planned {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                let elapsed_ms = plan_started.elapsed().as_millis();
                crate::log_warn!(
                    "transfer_7z plan failed: archive={} elapsed_ms={elapsed_ms} err={e:#}",
                    req.archive_path,
                );
                return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("7z planning task panicked/cancelled: {e}"),
                )
                .into_response()
            }
        }
    };
    let plan_elapsed_ms = plan_started.elapsed().as_millis();
    crate::log_info!(
        "transfer_7z plan: archive={} entries={} bytes={total_bytes} elapsed_ms={plan_elapsed_ms}",
        req.archive_path,
        preview.len(),
    );
    let files: Vec<PlannedFile> = preview
        .into_iter()
        .map(|(rel_path, size)| PlannedFile { rel_path, size })
        .collect();
    let files_sent_count = files.len() as u64;

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_7z: job={job_id} addr={addr} archive={} dest_root={} resume={} files={} bytes={total_bytes}",
        req.archive_path,
        req.dest_root,
        caller_supplied_tx_id,
        files_sent_count
    );
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        let result = transfer_7z_resumable(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.archive_path),
            2,
            initial_flags,
        );
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
                        skipped_files: 0,
                        skipped_bytes: 0,
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

/// POST /api/rar/inspect — desktop only. Counts + uncompressed size of a
/// `.rar` (multi-volume opens from the first part; `password` for encrypted).
#[cfg(not(target_os = "android"))]
async fn rar_inspect_handler(Json(req): Json<RarInspectReq>) -> impl IntoResponse {
    let p = req.archive_path.clone();
    let pw = req.password.clone();
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::transfer::inspect_rar(std::path::Path::new(&p), pw.as_deref())
    })
    .await;
    match r {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("rar inspect task: {e}"),
        )
        .into_response(),
    }
}

#[cfg(target_os = "android")]
async fn rar_inspect_handler() -> impl IntoResponse {
    json_err(
        StatusCode::NOT_IMPLEMENTED,
        "RAR is not supported on this build",
    )
    .into_response()
}

/// POST /api/transfer/rar — desktop only. Host-extract the `.rar` (any volume
/// set, optional password) to a temp dir, then stream the tree to the PS5 via
/// the directory transfer. Mirrors `transfer_7z_handler`'s job model.
#[cfg(not(target_os = "android"))]
async fn transfer_rar_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferRarReq>,
) -> impl IntoResponse {
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

    // Plan: list entries (names + sizes) without extracting.
    let (total_bytes, preview) = {
        let archive_path = req.archive_path.clone();
        let excludes = req.excludes.clone();
        let pw = req.password.clone();
        let planned = tokio::task::spawn_blocking(move || {
            ps5upload_core::transfer::rar_plan_preview(
                std::path::Path::new(&archive_path),
                pw.as_deref(),
                &excludes,
            )
        })
        .await;
        match planned {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                crate::log_warn!(
                    "transfer_rar plan failed: archive={} err={e:#}",
                    req.archive_path
                );
                return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response();
            }
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("rar planning task panicked/cancelled: {e}"),
                )
                .into_response()
            }
        }
    };
    let files: Vec<PlannedFile> = preview
        .into_iter()
        .map(|(rel_path, size)| PlannedFile { rel_path, size })
        .collect();
    let files_sent_count = files.len() as u64;

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_rar: job={job_id} addr={addr} archive={} dest_root={} resume={} files={} bytes={total_bytes}",
        req.archive_path,
        req.dest_root,
        caller_supplied_tx_id,
        files_sent_count
    );
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        let result = ps5upload_core::transfer::transfer_rar_resumable(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.archive_path),
            req.password.as_deref(),
            2,
            initial_flags,
        );
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
                        skipped_files: 0,
                        skipped_bytes: 0,
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

#[cfg(target_os = "android")]
async fn transfer_rar_handler() -> impl IntoResponse {
    json_err(
        StatusCode::NOT_IMPLEMENTED,
        "RAR is not supported on this build",
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
    // Sum source sizes + build the planned file list so Running has a
    // denominator + per-file progress from the first tick — done OFF the async
    // reactor. A large file-list (tens of thousands of entries) or a slow /
    // network source would otherwise park a reactor worker thread inside
    // std::fs::metadata and stall SSE, /pkg-host serving, and every OTHER
    // console. The client already waits for this planning before it receives
    // the job_id (the plan seeds JobState::Running), so moving it to a blocking
    // thread changes only which thread blocks. `entries` (consumed by the
    // transfer below) moves through the blocking task and back out.
    //
    // (2.9.0) Metadata failures previously fell through to `size = 0`,
    // which corrupted the `total_bytes` denominator — the user saw
    // "47 of 100 GB" with wrong N. Worse, the file then hit the
    // shard-emit phase where `File::open` failed for the same path,
    // aborting the transfer with a misleading "open <path>: io error"
    // and leaving the user guessing whether metadata or open was the
    // problem. Skip-with-warn is strictly better: drop the entry from
    // the plan (denominator is honest), log the path + reason so a
    // user reading engine.log can diagnose, and let the transfer
    // proceed on the files we can actually read. Metadata-but-not-
    // open is rare (only Windows ACL + macOS sandboxed apps + raced
    // delete), so the skipped set should be small or empty.
    let (entries, files, planner_skipped) = {
        let dest_root = req.dest_root.clone();
        match tokio::task::spawn_blocking(move || {
            let mut planner_skipped: Vec<(String, String)> = Vec::new();
            let files: Vec<PlannedFile> = entries
                .iter()
                .filter_map(|e| {
                    let size = match std::fs::metadata(&e.src) {
                        Ok(m) => m.len(),
                        Err(err) => {
                            planner_skipped.push((e.src.clone(), err.to_string()));
                            return None;
                        }
                    };
                    let rel = std::path::Path::new(&e.dest)
                        .strip_prefix(std::path::Path::new(&dest_root))
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|_| e.dest.clone())
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    Some(PlannedFile {
                        rel_path: rel,
                        size,
                    })
                })
                .collect();
            (entries, files, planner_skipped)
        })
        .await
        {
            Ok(v) => v,
            Err(e) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("file-list planning task panicked/cancelled: {e}"),
                )
                .into_response()
            }
        }
    };
    if !planner_skipped.is_empty() {
        for (src, err) in &planner_skipped {
            log_warn!("planner: skipped {} (metadata failed: {})", src, err);
        }
    }
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    let files_sent_count = files.len() as u64;
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    // P3 / v2.18.0 — apply-phase counters. The engine's
    // send_commit_and_expect_ack reads APPLY_PROGRESS frames from
    // the payload during the commit wait and stores into these.
    // The ticker (spawn_progress_ticker) reads and writes them to
    // JobState::Running's files_finalized / bytes_finalized fields.
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            // P3 / v2.18.0 — apply-phase counters start at 0; the
            // ticker fills them in once APPLY_PROGRESS frames begin
            // arriving from the payload during commit.
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        // All transfer endpoints share the same 3-attempt resume policy
        // (1 fresh + 2 resumes). See `transfer_dir_handler` for rationale.
        let result = transfer_file_list_resumable(
            &cfg,
            tx_id,
            &req.dest_root,
            &entries,
            DEFAULT_RESUME_RETRIES,
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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
    /// Parallel download streams for a FOLDER pull (one connection per
    /// disjoint file subset). None / <=1 = single stream. Capped at
    /// `MAX_DOWNLOAD_STREAMS`. Ignored for single-file downloads.
    #[serde(default)]
    streams: Option<usize>,
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
    // Default to the max parallel streams so folder dumps parallelise across
    // files automatically (single-file pulls fall back to one stream inside
    // download_to_local_multistream). Capture before req.addr is moved below.
    let download_streams = req.streams.unwrap_or(MAX_DOWNLOAD_STREAMS);
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

    let dest_dir = std::path::PathBuf::from(&req.dest_dir);
    // rsplit on a non-empty string always yields at least one piece,
    // but a panic in a handler kills every console's transfers — fail
    // soft with a 400 rather than expecting the invariant holds.
    let Some(basename) = trimmed_src.rsplit('/').next() else {
        return json_err(StatusCode::BAD_REQUEST, "src_path cannot be empty or '/'")
            .into_response();
    };
    if basename == "." || basename == ".." || basename.contains('/') || basename.contains('\\') {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!(
                "src_path produces an invalid destination basename ({basename:?}); refusing to download"
            ),
        )
        .into_response();
    }
    // Stat via spawn_blocking — dest_dir can be a network mount where
    // a blocking stat would stall the reactor for every console.
    let dest_dir_for_stat = dest_dir.clone();
    match tokio::task::spawn_blocking(move || std::fs::metadata(&dest_dir_for_stat)).await {
        Ok(Ok(md)) if md.is_dir() => {}
        Ok(Ok(_)) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("dest_dir is not a directory: {}", dest_dir.display()),
            )
            .into_response();
        }
        Ok(Err(e)) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("cannot access dest_dir {}: {e}", dest_dir.display()),
            )
            .into_response();
        }
        Err(e) => {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
    }
    // `dest_root` is the LOGICAL landing path (dest_dir/<basename>) reported
    // back to the UI for display. It is NOT the write root: the download
    // manifest's rel_paths already begin with `<basename>` (walk_remote_dir
    // prefixes folder entries, and the single-file branch sets rel_path =
    // basename), so files are written under `dest_dir` directly — joining
    // basename again here as the write root double-nested everything as
    // `dest_dir/foo/foo/...` (confirmed on hardware). See download_to_local
    // call below, which now takes `dest_dir`.
    let dest_root = dest_dir.join(basename);

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
    let progress_files = Arc::new(AtomicU64::new(0));
    // P3 / v2.18.0 — apply-phase counters. The engine's
    // send_commit_and_expect_ack reads APPLY_PROGRESS frames from
    // the payload during the commit wait and stores into these.
    // The ticker (spawn_progress_ticker) reads and writes them to
    // JobState::Running's files_finalized / bytes_finalized fields.
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            // P3 / v2.18.0 — apply-phase counters start at 0; the
            // ticker fills them in once APPLY_PROGRESS frames begin
            // arriving from the payload during commit.
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        // Write root is `dest_dir` (NOT dest_root) — rel_paths already carry
        // the basename prefix; see the dest_root comment above. Multi-stream
        // for folders (parallel files); single-file falls back internally.
        let result = download_to_local_multistream(
            &mgmt_addr,
            &dest_dir,
            &manifest,
            download_streams,
            Some(&progress),
        );
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

#[derive(Deserialize)]
struct TransferDownloadZipReq {
    addr: Option<String>,
    /// Remote file or folder to archive.
    src_path: String,
    /// "file" | "folder".
    kind: String,
    /// Absolute host path of the `.zip` to create (the user-picked save path).
    dest_zip: String,
}

/// POST /api/transfer/download-zip — pull a PS5 file/folder straight into a
/// `.zip` on the host, streaming each file through Deflate as it downloads (no
/// scratch dir, no second pass). Same job machinery as `/transfer/download`:
/// returns a job_id immediately; progress lands in the shared bytes counter the
/// ticker republishes over SSE. Sequential by nature (one zip stream).
async fn transfer_download_zip_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferDownloadZipReq>,
) -> impl IntoResponse {
    let mgmt_addr = mgmt_addr_or_default(req.addr, &state.default_ps5_addr);
    crate::log_info!(
        "transfer_download_zip: addr={mgmt_addr} src_path={} dest_zip={} kind={}",
        req.src_path,
        req.dest_zip,
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
    if req.src_path.trim_end_matches('/').is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "src_path cannot be empty or '/'")
            .into_response();
    }
    if req.dest_zip.trim().is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "dest_zip cannot be empty").into_response();
    }
    let dest_zip = std::path::PathBuf::from(&req.dest_zip);
    // The save dialog hands us a path inside an existing dir, but verify the
    // parent is a real directory (off-reactor — it may be a network mount) so a
    // bad path fails fast with a clear message instead of mid-stream.
    if let Some(parent) = dest_zip.parent().map(|p| p.to_path_buf()) {
        match tokio::task::spawn_blocking(move || std::fs::metadata(&parent)).await {
            Ok(Ok(md)) if md.is_dir() => {}
            Ok(_) => {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    "dest_zip's parent folder doesn't exist or isn't a directory",
                )
                .into_response()
            }
            Err(e) => {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
                    .into_response()
            }
        }
    }

    let job_id = Uuid::new_v4();
    let started_at_ms = now_ms();

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
    let progress_files = Arc::new(AtomicU64::new(0));
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
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
        Arc::clone(&progress_files),
        Arc::clone(&progress_files_finalized),
        Arc::clone(&progress_bytes_finalized),
    );

    let dest_display = dest_zip.to_string_lossy().to_string();
    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        match ps5upload_core::download::download_to_zip(
            &mgmt_addr,
            &dest_zip,
            &manifest,
            Some(&progress),
        ) {
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
                        dest: dest_display,
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

/// POST /api/transfer/dir-diff-preview
///
/// Dry-run reconcile: same walk + diff as `dir-reconcile`, but returns
/// the plan instead of starting an upload. Lets the renderer show
/// "X new, Y replaced, Z unchanged" before the user clicks Upload.
/// Always uses Fast mode — the Safe-mode hash check would defeat the
/// "preview is cheap" promise.
async fn transfer_dir_diff_preview_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferDirReconcileReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let mgmt = mgmt_addr_for(&addr);
    let src_path = std::path::PathBuf::from(&req.src_dir);
    let dest_root = req.dest_root.clone();
    let excludes = req.excludes.clone();
    let res = tokio::task::spawn_blocking(move || {
        crate::log_info!(
            "diff-preview: src={src} dest={dest} mgmt={mgmt}",
            src = src_path.display(),
            dest = dest_root,
            mgmt = mgmt,
        );
        // false: best-effort preview — bail out fast (reconcile_busy) if a
        // real upload's remote walk is already running, rather than piling on
        // a second mgmt-port walk and risking a connection storm.
        reconcile(
            &mgmt,
            &src_path,
            &dest_root,
            ReconcileMode::Fast,
            &excludes,
            false,
        )
    })
    .await;
    match res {
        Ok(Err(ref e)) if e.to_string().contains("reconcile_busy") => {
            // A scan is already in progress; tell the renderer to keep
            // showing nothing (best-effort preview, not an error).
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "busy": true,
                    "to_send_count": 0,
                    "to_send_bytes": 0,
                    "already_present_count": 0,
                    "already_present_bytes": 0,
                    "sample_to_send": [],
                })),
            )
                .into_response()
        }
        Ok(Ok(plan)) => {
            // Sample first 32 to_send relpaths so the renderer can
            // show "what would change" without a 50K-file response.
            let sample: Vec<String> = plan
                .to_send
                .iter()
                .take(32)
                .map(|f| f.rel_path.clone())
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "to_send_count": plan.to_send.len(),
                    "to_send_bytes": plan.bytes_to_send,
                    "already_present_count": plan.already_present,
                    "already_present_bytes": plan.bytes_already_present,
                    "sample_to_send": sample,
                })),
            )
                .into_response()
        }
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("reconcile: {e}")).into_response(),
        Err(e) => {
            json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("task join: {e}")).into_response()
        }
    }
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
            files_processing: 0,
            // P3 / v2.18.0 — apply-phase counters start at 0; the
            // ticker fills them in once APPLY_PROGRESS frames begin
            // arriving from the payload during commit.
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
        },
    );

    let jobs = Arc::clone(&state.jobs);
    let events_tx = state.events_tx.clone();

    tokio::task::spawn_blocking(move || {
        // Install the panic-survive guard at the TOP of the closure so that
        // a panic anywhere in Phase 1 (reconcile, local walk) doesn't leave
        // the job stuck in Running forever. The other three transfer
        // handlers do the same; this one was previously deferring guard
        // install until Phase 2, which let pre-Phase-2 panics orphan jobs.
        // The two existing early-return branches (walk failure / empty
        // plan) call mark_succeeded() before returning since they already
        // set the terminal job state themselves.
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
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
            // true: this is the real upload — wait for the remote-walk gate
            // so we never run concurrently with a diff-preview walk.
            true,
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
                                error_reason: None,
                                error_detail: None,
                            },
                        );
                        fail_guard.mark_succeeded();
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
            fail_guard.mark_succeeded();
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
        let progress_files = Arc::new(AtomicU64::new(0));
        let progress_files_finalized = Arc::new(AtomicU64::new(0));
        let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
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
                files_processing: 0,
                files_finalized: 0,
                files_finalizing_total: 0,
                bytes_finalized: 0,
            },
        );
        let stop_ticker = spawn_progress_ticker(
            Arc::clone(&jobs),
            events_tx.clone(),
            job_id,
            ctx,
            Arc::clone(&progress),
            Arc::clone(&progress_files),
            Arc::clone(&progress_files_finalized),
            Arc::clone(&progress_bytes_finalized),
        );
        // Same panic-survive contract as the other transfer endpoints.
        // (fail_guard was installed at the top of this closure.)
        let _stop_guard = TickerStopGuard::new(stop_ticker);

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

        let streams = req.streams.unwrap_or(1);
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        // 1 fresh attempt + DEFAULT_RESUME_RETRIES resumes. Covers several
        // payload hiccups mid-transfer (incl. the serial accept loop briefly
        // busy draining a dropped connection); if the payload is hard-dead,
        // the reconnect fails fast (ConnectionRefused is non-retryable) and we
        // surface the underlying error.
        //
        // Multi-stream: when the client requests >1 stream (having confirmed the
        // payload advertises support), the orchestrator splits `entries` across
        // parallel connections. With streams<=1 it delegates to the exact
        // single-stream path above, so this is a no-op when disabled.
        let result = transfer_file_list_multistream(
            &cfg,
            tx_id,
            &req.dest_root,
            &entries,
            streams,
            DEFAULT_RESUME_RETRIES,
            initial_flags,
        );
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
                    job_failed_from_err(started_at_ms, completed_at_ms, &e),
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

/// POST /api/jobs/{id}/cancel — truly stop a running transfer. Flips the job's
/// registered cancel flag; the core aborts at its next shard boundary with
/// `transfer_cancelled` and the partial tx is left interrupted/resumable (same
/// as a dropped connection). Idempotent — cancelling an unknown/finished job is
/// a no-op 200 (`cancelled:false`); the client may race the job ending.
async fn cancel_job(Path(id): Path<String>) -> impl IntoResponse {
    let uuid = match id.parse::<Uuid>() {
        Ok(u) => u,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    let cancelled = signal_transfer_cancel(uuid);
    crate::log_info!("cancel_job: job={uuid} cancelled={cancelled}");
    (
        StatusCode::OK,
        Json(serde_json::json!({ "cancelled": cancelled })),
    )
        .into_response()
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

#[derive(Deserialize)]
struct DebugCrashQuery {
    mode: Option<String>,
}

/// GET /api/debug/crash?mode=panic|exit — fault-injection for VALIDATING the
/// crash-logging paths (panic hook → ring, stderr-EOF → engine-exit event).
/// Hard-gated behind `PS5UPLOAD_ENGINE_DEBUG=1`: with the env unset (every
/// normal app launch) it 404s, so it can never be triggered in production.
async fn debug_crash(Query(q): Query<DebugCrashQuery>) -> axum::response::Response {
    if std::env::var("PS5UPLOAD_ENGINE_DEBUG").ok().as_deref() != Some("1") {
        return (StatusCode::NOT_FOUND, "disabled").into_response();
    }
    match q.mode.as_deref() {
        // Unwinds the handler task; the global panic hook records the panic
        // into the ring (the thing that reaches the bug bundle). tokio keeps
        // the process alive, so the engine survives — this tests the HOOK.
        Some("panic") => panic!("forced debug panic (PS5UPLOAD_ENGINE_DEBUG)"),
        // Exits the process; the desktop parent's stderr-EOF watcher then
        // emits ps5upload-engine-exit. Tests process-death detection.
        Some("exit") => std::process::exit(7),
        _ => (StatusCode::BAD_REQUEST, "mode=panic|exit").into_response(),
    }
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
            // Capability flags the UI feature-detects. `rar` is desktop-only
            // (the UnRAR C dep is excluded from the Android build), so the
            // client hides the .rar option when this is false.
            "caps": { "rar": cfg!(not(target_os = "android")) },
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

/// Spawn the parent-watch thread when running under the desktop shell.
///
/// **Why:** the engine binds 19113. If the parent (Tauri shell) dies
/// abruptly — taskkill /F, segfault, panic, OOM, kernel kill, power
/// loss recovery — `kill_on_drop(true)` on the parent side never
/// fires (no `Drop` runs on a crashed process), and the engine is
/// orphaned holding the port. Next launch can't bind 19113 and the
/// user is stuck until they manually kill the orphan.
///
/// **How:** the parent spawns us with `Stdio::piped()` for stdin and
/// holds the write end; the OS guarantees that handle is closed when
/// the parent process dies, however it dies. We park a thread on a
/// blocking `stdin().read()`. When read returns Ok(0) (EOF), the
/// parent is gone — exit immediately. No FFI, no per-OS code, no env
/// crates needed. Works the same on Linux, macOS, Windows.
///
/// **Gating:** opt-in via `PS5UPLOAD_PARENT_WATCH=1` so a developer
/// running the engine standalone (`cargo run -p ps5upload-engine`)
/// doesn't get auto-killed when their stdin closes (e.g. piping a
/// file in, or running headless under nohup).
/// Cooperative shutdown signal. Set by either the parent-watcher
/// thread (stdin EOF when parent died) or a Unix signal handler
/// (SIGTERM / SIGINT — `kill` from systemd, ^C from a dev terminal).
/// `axum::serve(...).with_graceful_shutdown` awaits this future,
/// drains in-flight requests up to the cap, then returns. Replaces
/// a `process::exit(0)` torn-down hard exit that left in-flight
/// transfers / pkg-host streams dropped abruptly.
static SHUTDOWN: tokio::sync::OnceCell<tokio::sync::Notify> = tokio::sync::OnceCell::const_new();

async fn shutdown_signal() {
    let notify = SHUTDOWN
        .get_or_init(|| async { tokio::sync::Notify::new() })
        .await;
    // Race the cooperative notify with platform signal handlers so
    // both manual `kill` and parent-death paths get clean drain.
    //
    // Signal-handler install can fail under restrictive sandboxes
    // (Snap, Flatpak, some AppImage configs with seccomp). Prior
    // code used `.expect()` and panicked the runtime in that case;
    // now we fall back to "notify-only" so the engine still shuts
    // down cooperatively when the parent process exits.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut sigterm), Ok(mut sigint)) => {
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = sigterm.recv() => {
                        eprintln!("[ps5upload-engine] SIGTERM — shutting down");
                    }
                    _ = sigint.recv() => {
                        eprintln!("[ps5upload-engine] SIGINT — shutting down");
                    }
                }
            }
            _ => {
                crate::log_warn!(
                    "could not install SIGTERM/SIGINT handlers — falling back to notify-only shutdown (sandbox?)"
                );
                notify.notified().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        // Windows: ctrl-c only. SIGTERM-equivalent is the Windows
        // service control manager, out of scope for the dev path.
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[ps5upload-engine] Ctrl-C — shutting down");
            }
        }
    }
}

fn trigger_shutdown() {
    if let Some(notify) = SHUTDOWN.get() {
        notify.notify_waiters();
    }
}

fn spawn_parent_watcher() {
    if std::env::var("PS5UPLOAD_PARENT_WATCH").as_deref() != Ok("1") {
        return;
    }
    std::thread::spawn(|| {
        use std::io::Read;
        // Single-byte read in a loop: any data the parent sends is
        // ignored, but a 0-byte return means EOF (parent's pipe write
        // end was closed → parent died, however that happened).
        let mut buf = [0u8; 64];
        let mut stdin = std::io::stdin().lock();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    eprintln!(
                        "[engine] parent process died (stdin EOF); draining in-flight requests then exiting",
                    );
                    trigger_shutdown();
                    // Belt-and-braces watchdog: if axum's
                    // graceful-shutdown drain takes longer than
                    // 10 seconds (e.g. a stuck pkg-host range read),
                    // hard-exit so we don't keep the port held by a
                    // zombie engine after the parent is gone.
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    eprintln!("[engine] graceful shutdown timed out; hard exit");
                    std::process::exit(0);
                }
                Ok(_) => continue,
                Err(e) => {
                    eprintln!(
                        "[engine] parent-watch stdin read error: {e}; draining in-flight requests then exiting",
                    );
                    trigger_shutdown();
                    std::thread::sleep(std::time::Duration::from_secs(10));
                    eprintln!("[engine] graceful shutdown timed out; hard exit");
                    std::process::exit(0);
                }
            }
        }
    });
}

/// Configuration for the engine server. Shared by the desktop CLI
/// entry (`run_cli`) and the in-process mobile entry
/// (`serve_in_process`); the two differ only in these flags.
pub struct EngineConfig {
    /// Socket to bind. Desktop: `"0.0.0.0:19113"` (so the PS5 can reach
    /// `/pkg-host/*` for fakepkg installs). Mobile in-process:
    /// `"127.0.0.1:19113"` (loopback only; the renderer hits the same).
    pub bind: String,
    /// Default PS5 transfer address. Per-request `addr` params override
    /// it, so this is only a fallback.
    pub ps5_addr: String,
    /// Install the stdin-EOF parent-watcher. Desktop sidecar only — on
    /// mobile the engine shares the app process, so there is no parent
    /// pipe to watch.
    pub parent_watch: bool,
    /// On bind/serve failure, use the historic `process::exit` codes
    /// (desktop sidecar) instead of returning `Err` (mobile, where
    /// exiting would kill the whole app).
    pub exit_on_error: bool,
    /// Extra IPs allowed past the loopback guard (besides loopback), set
    /// from `PS5UPLOAD_ALLOW_IP` (comma-separated). Lets remote desktop
    /// clients reach the `/api/*` surface when self-hosting the engine.
    pub allow_ips: Vec<std::net::IpAddr>,
}

/// Core server entry. Builds the router, binds, and serves until
/// graceful shutdown. Behavior is identical to the former `main`; the
/// `EngineConfig` flags select desktop-sidecar vs. in-process behavior.
async fn run(cfg: EngineConfig) -> anyhow::Result<()> {
    if cfg.parent_watch {
        spawn_parent_watcher();
    }
    // Route ps5upload-core's log stream into the same ring the engine's
    // own logs land in, so the renderer's Log tab sees *both* sources
    // (reconcile per-parent progress, transfer retries, etc.) without
    // having to install a separate pipe for core diagnostics.
    ps5upload_core::log::set_sink(|msg| engine_log::record("info", msg.to_string()));

    let ps5_addr = cfg.ps5_addr.clone();
    let guard_cfg = LoopbackGuardConfig {
        allowed_ips: cfg.allow_ips.clone().into(),
    };

    // 2048, not 512: one process fans events for up to 12 consoles, and
    // a lagging SSE consumer that falls more than `capacity` behind
    // silently drops events — including terminal job events the UI
    // never recovers from. Headroom is cheap; lost terminals aren't.
    let (events_tx, _) = broadcast::channel(2048);

    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        default_ps5_addr: ps5_addr.clone(),
        events_tx,
    };

    let app = Router::new()
        .route("/", get(ui_handler))
        .route("/api/ps5/status", get(ps5_status))
        .route("/api/ps5/readiness", get(ps5_readiness))
        .route("/api/ps5/cleanup", post(ps5_cleanup))
        .route("/api/ps5/volumes", get(ps5_volumes))
        .route("/api/ps5/pkg/scan-external", get(ps5_pkg_scan_external))
        .route("/api/ps5/pkg/metadata", get(ps5_pkg_metadata))
        .route("/api/ps5/list-dir", get(ps5_list_dir))
        .route("/api/ps5/fs/delete", post(ps5_fs_delete))
        .route("/api/ps5/fs/move", post(ps5_fs_move))
        .route("/api/ps5/fs/copy", post(ps5_fs_copy))
        .route("/api/ps5/fs/op-status", get(ps5_fs_op_status))
        .route("/api/ps5/fs/op-cancel", post(ps5_fs_op_cancel))
        .route("/api/ps5/fs/mount", post(ps5_fs_mount))
        .route("/api/ps5/fs/unmount", post(ps5_fs_unmount))
        .route("/api/ps5/app/launch", post(ps5_app_launch))
        .route("/api/ps5/app/register", post(ps5_app_register))
        .route("/api/ps5/app/unregister", post(ps5_app_unregister))
        .route("/api/ps5/hw/info", get(ps5_hw_info))
        .route("/api/ps5/hw/temps", get(ps5_hw_temps))
        .route("/api/ps5/syslog/tail", get(ps5_syslog_tail))
        .route("/api/ps5/time/get", get(ps5_time_get_route))
        .route("/api/ps5/time/sync", post(ps5_time_sync_route))
        .route("/api/ps5/time/state/get", get(ps5_time_state_get_route))
        .route("/api/ps5/time/state/set", post(ps5_time_state_set_route))
        .route(
            "/api/ps5/smp-meta/control",
            post(ps5_smp_meta_control_route),
        )
        .route("/api/ps5/smp-meta/stats", get(ps5_smp_meta_stats_route))
        .route("/api/ps5/hw/power", get(ps5_hw_power))
        .route("/api/ps5/hw/storage", get(ps5_hw_storage))
        .route("/api/ps5/proc/list", get(ps5_proc_list))
        .route("/api/ps5/hw/fan-threshold", post(ps5_hw_set_fan_threshold))
        .route("/api/ps5/fs/chmod", post(ps5_fs_chmod))
        .route("/api/ps5/fs/mkdir", post(ps5_fs_mkdir))
        .route("/api/ps5/game-meta", get(ps5_game_meta))
        .route("/api/ps5/game-icon", get(ps5_game_icon))
        .route("/api/ps5/apps/installed", get(ps5_apps_installed))
        .route("/api/ps5/app-icon", get(ps5_app_icon))
        .route("/api/transfer/file", post(transfer_file_handler))
        .route("/api/transfer/dir", post(transfer_dir_handler))
        .route("/api/transfer/zip", post(transfer_zip_handler))
        .route("/api/zip/inspect", post(zip_inspect_handler))
        .route("/api/zip/inspect/stream", post(zip_inspect_stream_handler))
        .route("/api/transfer/7z", post(transfer_7z_handler))
        .route("/api/7z/inspect", post(sevenz_inspect_handler))
        .route(
            "/api/7z/inspect/stream",
            post(sevenz_inspect_stream_handler),
        )
        .route("/api/transfer/rar", post(transfer_rar_handler))
        .route("/api/rar/inspect", post(rar_inspect_handler))
        .route("/api/transfer/file-list", post(transfer_file_list_handler))
        .route("/api/transfer/download", post(transfer_download_handler))
        .route(
            "/api/transfer/download-zip",
            post(transfer_download_zip_handler),
        )
        .route("/api/profile/info", get(profile_info_handler))
        .route("/api/profile/username", post(profile_username_handler))
        .route(
            "/api/profile/local-username",
            post(profile_local_username_handler),
        )
        .route("/api/profile/activate", post(profile_activate_handler))
        .route("/api/profile/clear-slot", post(profile_clear_slot_handler))
        .route("/api/profile/avatar", post(profile_avatar_handler))
        .route(
            "/api/profile/avatar/current",
            get(profile_avatar_current_handler),
        )
        .route(
            "/api/profile/avatar/preview",
            post(profile_avatar_preview_handler),
        )
        .route(
            "/api/transfer/dir-reconcile",
            post(transfer_dir_reconcile_handler),
        )
        // Dry-run reconcile: walks both trees, returns the "what would
        // be sent" stats without starting an upload. Used by the
        // Upload screen's pre-flight diff preview. See
        // transfer_dir_diff_preview_handler.
        .route(
            "/api/transfer/dir-diff-preview",
            post(transfer_dir_diff_preview_handler),
        )
        .route("/api/version", get(engine_version))
        .route("/api/jobs", get(list_jobs))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/jobs/{id}/cancel", post(cancel_job))
        .route("/api/events", get(events_stream))
        .route("/api/engine-logs", get(engine_logs_tail))
        .route("/api/debug/crash", get(debug_crash))
        .with_state(state);

    // SPA fallback: serve the embedded React bundle for every path that doesn't
    // match an explicit /api/* route above.  Only compiled when the `webui`
    // feature is on (Docker / self-hosted image); the regular build keeps the
    // simple `GET /` dashboard handler above.
    #[cfg(feature = "webui")]
    let app = app.fallback(webui::spa_fallback);

    let app = app
        // .pkg install — sessions live in their own state because the
        // HTTP-host serving handler needs Mutex-guarded session lookup
        // independent of the main engine state. Merged at this point
        // so the pkg routes share the same listener + CORS + body limit.
        .merge(pkg_install::router(std::sync::Arc::new(
            pkg_install::PkgInstallState::default(),
        )))
        // Permissive CORS — the only off-loopback route that ever
        // sees a real request is `/pkg-host/*` (PS5 server-side fetch,
        // no browser, no CORS preflight). For loopback Tauri-API hits,
        // the loopback_guard above is the real auth gate; CORS just
        // makes browser dev-tools curl reproducers easier.
        .layer(tower_http::cors::CorsLayer::permissive())
        // Explicit body-size cap. Axum's default DefaultBodyLimit is
        // 2 MiB, which is borderline for a TransferFileListReq carrying
        // tens of thousands of file paths and could silently change
        // with an axum upgrade. Setting it explicitly here documents
        // intent and protects against pathological local input from
        // OOMing the engine. 64 MiB is generous enough for our largest
        // legitimate payload (a 100k-entry file list with mid-length
        // paths is well under 30 MiB encoded JSON) and small enough
        // that a runaway request can't blow up RAM on a 4 GB box.
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        // Request trace — inside the loopback guard (so we don't log rejected
        // LAN probes), wrapping the handlers so it times the full request.
        .layer(middleware::from_fn(log_requests))
        // Loopback-guard middleware MUST be applied last so it ends
        // up the OUTERMOST layer — axum wraps each `.layer()` around
        // the one below it. Pre-2.2.52 fix-round-2 the order was
        // (guard → cors → body-limit), which meant CORS preflight
        // requests from a LAN browser were answered 200 with
        // `Access-Control-Allow-Origin: *` BEFORE the guard saw them
        // — the actual GET still 403'd, but the engine's existence
        // and CORS posture were enumerable from the LAN. With the
        // guard outermost, an off-loopback peer hitting any
        // non-`/pkg-host/*` route is rejected immediately, before
        // CORS / body-limit / handler.
        .layer(middleware::from_fn_with_state(guard_cfg, loopback_guard));

    // Bind `0.0.0.0` so the PS5 can fetch `/pkg-host/*` for fakepkg
    // installs. The loopback-guard middleware (above) gates every
    // other route to peers whose source IP is on the loopback range,
    // preserving the pre-2.2.52 "API is local-only" invariant for
    // everything that isn't the deliberately PS5-facing pkg-host
    // route. Pre-2.2.52 we bound `127.0.0.1`, which kept the API
    // safe from the LAN by accident — but also broke pkg install
    // because the PS5 couldn't reach the same listener it had to
    // download from.
    let bind = cfg.bind.clone();
    // Mirror to stdout (terminal users) AND the engine.log ring
    // (post-mortem diagnosis from the desktop shell once the
    // terminal is closed). Pre-this-fix, the startup line was
    // println!-only and missing from `engine.log`.
    println!("[ps5upload-engine] listening on http://{bind}  (ps5={ps5_addr})");
    crate::log_info!("listening on http://{bind}  (ps5={ps5_addr})");
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            // Mobile / in-process: never exit the process (that would
            // kill the whole app) — surface the bind failure as an Err
            // for the caller to log/retry.
            if !cfg.exit_on_error {
                return Err(anyhow::anyhow!(
                    "in-process engine failed to bind {bind}: {e}"
                ));
            }
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
            // Probe via loopback rather than `bind` (which is `0.0.0.0`
            // and not a routable connect target). If something is bound
            // on this port at all, the loopback form will accept.
            let probe_addr = bind.replacen("0.0.0.0", "127.0.0.1", 1);
            let connectable = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                tokio::net::TcpStream::connect(&probe_addr),
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
                return Ok(());
            }
            eprintln!(
                "[ps5upload-engine] failed to bind {bind}: {e} \
                 (port held by an unresponsive process — kill it or \
                 override with PS5UPLOAD_ENGINE_PORT)",
            );
            std::process::exit(2);
        }
    };
    // Graceful shutdown: when the parent-watcher fires (stdin EOF), the
    // SHUTDOWN OnceCell-guarded Notify wakes this future, axum stops
    // accepting new connections, drains in-flight ones, then returns.
    // The 10-second watchdog in spawn_parent_watcher is the hard ceiling
    // — if a stuck request blocks the drain (rare; only for pkg-host
    // range reads on a dying PS5), we fall through to process::exit(0).
    // `into_make_service_with_connect_info::<SocketAddr>` is what makes
    // the `ConnectInfo<SocketAddr>` extractor work in the loopback-guard
    // middleware. Without this, axum hands handlers a generic ConnectInfo
    // and the middleware would have to fall back to header-based source
    // detection (less reliable, easier to spoof from a hostile LAN peer).
    if let Err(e) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    {
        eprintln!("[ps5upload-engine] axum serve terminated: {e}");
        if cfg.exit_on_error {
            std::process::exit(3);
        }
        return Err(anyhow::anyhow!("axum serve terminated: {e}"));
    }
    Ok(())
}

/// Desktop sidecar entry point. Reads `PS5UPLOAD_ENGINE_PORT` /
/// `PS5_ADDR` from the environment (set by the Tauri shell when it
/// spawns the sidecar), binds `0.0.0.0`, installs the stdin parent-
/// watcher, and uses the historic `process::exit` codes on failure.
/// Behavior preserved verbatim from the former `main`.
/// Install a panic hook that records the panic into the engine log ring (so it
/// reaches the in-app Log tab + the bug bundle, not just stderr) while still
/// running the default hook (stderr print). Desktop sidecar only — the mobile
/// in-process path must not steal the host app's panic hook.
fn install_panic_logger() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let bt = std::backtrace::Backtrace::force_capture();
        // engine_log caps + truncates the message, so a huge backtrace can't
        // blow up the ring.
        engine_log::record("error", format!("PANIC at {loc}: {msg}\n{bt}"));
        default(info);
    }));
}

pub async fn run_cli() {
    install_panic_logger();
    // `PS5UPLOAD_ENGINE_PORT` matches the name the desktop client sets
    // when it spawns the sidecar. Previous generic `ENGINE_PORT` was too
    // easy to collide with other tools.
    let port: u16 = std::env::var("PS5UPLOAD_ENGINE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(19113);
    let ps5_addr = std::env::var("PS5_ADDR").unwrap_or_else(|_| "192.168.137.2:9113".to_string());
    // Extra IPs allowed past the loopback guard (e.g. remote desktop
    // clients reaching a self-hosted engine). Comma-separated; unparseable
    // entries are dropped, unset → empty.
    let allow_ips = parse_allow_ips(&std::env::var("PS5UPLOAD_ALLOW_IP").unwrap_or_default());
    if !allow_ips.is_empty() {
        // The `/api/*` surface has NO authentication — it can install/uninstall
        // titles and read/write/delete files on the PS5. The loopback guard is
        // the only thing in front of it; PS5UPLOAD_ALLOW_IP punches a hole for
        // these IPs. That's fine on a trusted home LAN (the intended use), but
        // an IP allowlist is spoofable and grants any process on those hosts
        // full control — so make the open surface loud, and never expose the
        // engine to an untrusted network or the internet.
        eprintln!(
            "[ps5upload-engine] WARNING: PS5UPLOAD_ALLOW_IP grants UNAUTHENTICATED \
             /api/* access (full PS5 + file control) to {} extra IP(s): {}. \
             Use only on a trusted LAN; never expose this engine to the internet.",
            allow_ips.len(),
            allow_ips
                .iter()
                .map(|ip| ip.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // `run` calls `process::exit` directly on failure here
    // (exit_on_error = true), so the returned Result is only `Ok(())`
    // on normal graceful shutdown.
    let _ = run(EngineConfig {
        bind: format!("0.0.0.0:{port}"),
        ps5_addr,
        parent_watch: true,
        exit_on_error: true,
        allow_ips,
    })
    .await;
}

/// In-process engine entry point for the Tauri **mobile** build, where
/// there is no sidecar binary to spawn. Binds loopback only, installs
/// no parent-watcher, and returns `Err` on failure instead of exiting
/// (an exit would tear down the whole app). The renderer keeps calling
/// `http://127.0.0.1:19113` exactly as on desktop — only the server's
/// host changes from a child process to this task.
pub async fn serve_in_process(bind: &str, ps5_addr: String) -> anyhow::Result<()> {
    run(EngineConfig {
        bind: bind.to_string(),
        ps5_addr,
        parent_watch: false,
        exit_on_error: false,
        allow_ips: Vec::new(),
    })
    .await
}

#[cfg(test)]
mod loopback_guard_tests {
    use super::*;
    use std::net::IpAddr;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn cfg(ips: &[&str]) -> LoopbackGuardConfig {
        LoopbackGuardConfig {
            allowed_ips: ips.iter().map(|s| ip(s)).collect(),
        }
    }

    #[test]
    fn loopback_always_allowed() {
        let c = cfg(&[]);
        assert!(loopback_allows(&c, ip("127.0.0.1"), "/api/jobs"));
        assert!(loopback_allows(&c, ip("::1"), "/api/jobs"));
    }

    #[test]
    fn off_loopback_denied_without_allowlist() {
        let c = cfg(&[]);
        assert!(!loopback_allows(&c, ip("192.168.1.50"), "/api/jobs"));
    }

    #[test]
    fn configured_ips_allowed_others_denied() {
        let c = cfg(&["192.168.1.50", "10.0.0.9"]);
        assert!(loopback_allows(&c, ip("192.168.1.50"), "/api/jobs"));
        assert!(loopback_allows(&c, ip("10.0.0.9"), "/api/jobs"));
        assert!(!loopback_allows(&c, ip("192.168.1.51"), "/api/jobs"));
    }

    #[test]
    fn pkg_host_allowed_from_any_peer() {
        let c = cfg(&[]);
        assert!(loopback_allows(&c, ip("10.0.0.7"), "/pkg-host/abc"));
    }

    #[test]
    fn parse_allow_ips_handles_list_blanks_and_junk() {
        assert_eq!(parse_allow_ips(""), Vec::<IpAddr>::new());
        assert_eq!(
            parse_allow_ips(" 192.168.1.50 , ::1 ,, not-an-ip , 10.0.0.9"),
            vec![ip("192.168.1.50"), ip("::1"), ip("10.0.0.9")]
        );
    }
}

#[cfg(test)]
mod cancel_registry_tests {
    use super::*;

    #[test]
    fn register_returns_flag_and_signal_flips_it() {
        let id = Uuid::new_v4();
        let flag = register_transfer_cancel(id);
        assert!(!flag.load(Ordering::Relaxed), "starts unset");
        assert!(signal_transfer_cancel(id), "found the registered flag");
        assert!(flag.load(Ordering::Relaxed), "signal flipped the flag");
    }

    #[test]
    fn signal_unknown_job_is_false() {
        assert!(!signal_transfer_cancel(Uuid::new_v4()));
    }

    #[test]
    fn register_prunes_finished_entries() {
        // A "finished" transfer is one whose only remaining ref is the
        // registry's (strong_count == 1, i.e. the cfg clone was dropped).
        let finished = Uuid::new_v4();
        drop(register_transfer_cancel(finished)); // we drop our returned Arc → count 1
                                                  // Registering any new job prunes the finished one.
        let live = register_transfer_cancel(Uuid::new_v4());
        let _hold = Arc::clone(&live); // keep the live one's count > 1
        assert!(
            !signal_transfer_cancel(finished),
            "finished entry was pruned on the next register",
        );
    }
}

#[cfg(test)]
mod helpers_tests {
    use super::*;

    #[test]
    fn mgmt_addr_for_swaps_port() {
        assert_eq!(mgmt_addr_for("192.168.1.50:9113"), "192.168.1.50:9114");
        assert_eq!(mgmt_addr_for("10.0.0.1:1234"), "10.0.0.1:9114");
    }

    #[test]
    fn mgmt_addr_for_with_no_port_appends_mgmt() {
        // Defensive — callers shouldn't pass a port-less addr but the
        // helper should produce a valid `:9114` value rather than panic.
        assert_eq!(mgmt_addr_for("192.168.1.50"), "192.168.1.50:9114");
    }

    #[test]
    fn mgmt_addr_for_handles_ipv6_with_brackets() {
        // rsplit on `:` for "[::1]:9113" finds the port-side colon.
        assert_eq!(mgmt_addr_for("[::1]:9113"), "[::1]:9114");
    }

    #[test]
    fn external_pkg_header_classifies_cnt_and_fih() {
        // \x7FCNT stock package with a PS4 content id at offset 0x40.
        let mut head = vec![0u8; 0xA0];
        head[0..4].copy_from_slice(&[0x7F, b'C', b'N', b'T']);
        let cid = b"EP4293-CUSA32097_00-ASTNCPS4SIEE0000";
        head[0x40..0x40 + cid.len()].copy_from_slice(cid);
        let (content_id, title_id, platform) = external_pkg_header(&head);
        assert_eq!(content_id, "EP4293-CUSA32097_00-ASTNCPS4SIEE0000");
        assert_eq!(title_id, "CUSA32097");
        assert_eq!(platform, "ps4");

        // \x7FFIH PS5-native: platform is ps5 from the magic alone; the
        // content id isn't parsed for FIH (Sony's installer reads it).
        let mut fih = vec![0u8; 0xA0];
        fih[0..4].copy_from_slice(&[0x7F, b'F', b'I', b'H']);
        let (cid2, _tid2, plat2) = external_pkg_header(&fih);
        assert_eq!(cid2, "");
        assert_eq!(plat2, "ps5");
    }

    #[test]
    fn external_pkg_header_rejects_short_and_unknown() {
        // Shorter than the 0xA0 header window → all empty, no panic.
        assert_eq!(
            external_pkg_header(&[0u8; 16]),
            (String::new(), String::new(), String::new())
        );
        // Full length but an unrecognized magic → nothing classified.
        let mut head = vec![0u8; 0xA0];
        head[0..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(
            external_pkg_header(&head),
            (String::new(), String::new(), String::new())
        );
    }

    #[test]
    fn mgmt_addr_or_default_uses_default_when_none() {
        assert_eq!(
            mgmt_addr_or_default(None, "192.168.0.1:9113"),
            "192.168.0.1:9114"
        );
    }

    #[test]
    fn parse_or_random_tx_id_round_trip() {
        let hex = "0123456789abcdef0123456789abcdef";
        let bytes = parse_or_random_tx_id(Some(hex)).unwrap();
        assert_eq!(bytes[0], 0x01);
        assert_eq!(bytes[15], 0xef);
    }

    #[test]
    fn parse_or_random_tx_id_rejects_short() {
        assert!(parse_or_random_tx_id(Some("dead")).is_err());
    }

    #[test]
    fn parse_or_random_tx_id_rejects_invalid_hex() {
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert!(parse_or_random_tx_id(Some(bad)).is_err());
    }

    #[test]
    fn parse_or_random_tx_id_none_yields_uuid() {
        let a = parse_or_random_tx_id(None).unwrap();
        let b = parse_or_random_tx_id(None).unwrap();
        assert_ne!(a, b, "two random tx_ids should differ");
    }

    #[test]
    fn hex_val_accepts_both_cases() {
        assert_eq!(hex_val(b'0').unwrap(), 0);
        assert_eq!(hex_val(b'9').unwrap(), 9);
        assert_eq!(hex_val(b'a').unwrap(), 10);
        assert_eq!(hex_val(b'f').unwrap(), 15);
        assert_eq!(hex_val(b'A').unwrap(), 10);
        assert_eq!(hex_val(b'F').unwrap(), 15);
    }

    #[test]
    fn hex_val_rejects_non_hex() {
        assert!(hex_val(b'g').is_err());
        assert!(hex_val(b' ').is_err());
        assert!(hex_val(b'!').is_err());
    }

    #[test]
    fn now_ms_is_close_to_system_time() {
        let n = now_ms();
        assert!(
            n > 1_700_000_000_000,
            "now_ms should be reasonable epoch ms"
        );
    }

    // ── extract_payload_error — Phase B error parser ───────────────────────

    #[test]
    fn extract_payload_error_plain_string_returns_none() {
        let e = anyhow::anyhow!("just a plain error with no json");
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r, None);
        assert_eq!(d, None);
    }

    #[test]
    fn extract_payload_error_finds_both_fields() {
        let e = anyhow::anyhow!(
            "CommitTx rejected (Error): {{\"error\":\"direct_writer_io_error\",\"tx_id\":\"abc\",\"detail\":\"writer thread reported a disk write error mid-stream\"}}"
        );
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r.as_deref(), Some("direct_writer_io_error"));
        assert_eq!(
            d.as_deref(),
            Some("writer thread reported a disk write error mid-stream")
        );
    }

    #[test]
    fn extract_payload_error_finds_only_error_when_detail_absent() {
        let e = anyhow::anyhow!("rejected: {{\"error\":\"fs_delete_path_not_allowed\"}}");
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r.as_deref(), Some("fs_delete_path_not_allowed"));
        assert_eq!(d, None);
    }

    #[test]
    fn extract_payload_error_finds_only_detail_when_error_absent() {
        let e = anyhow::anyhow!("ack body: {{\"detail\":\"freestanding detail\"}}");
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r, None);
        assert_eq!(d.as_deref(), Some("freestanding detail"));
    }

    #[test]
    fn extract_payload_error_walks_chain() {
        let inner = anyhow::anyhow!(
            "payload body: {{\"error\":\"direct_tx_corrupt\",\"detail\":\"shard 4 hash mismatch\"}}"
        );
        let outer = inner.context("transfer_file failed");
        let (r, d) = extract_payload_error(&outer);
        assert_eq!(r.as_deref(), Some("direct_tx_corrupt"));
        assert_eq!(d.as_deref(), Some("shard 4 hash mismatch"));
    }

    #[test]
    fn extract_payload_error_open_brace_without_close_skips() {
        // Defensive — a malformed log line should not crash the parser
        // or accidentally match a partial substring.
        let e = anyhow::anyhow!("error log opening {{ but no close");
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r, None);
        assert_eq!(d, None);
    }

    #[test]
    fn extract_payload_error_unparseable_json_skips() {
        // The `{...}` substring exists but isn't valid JSON. Parser
        // must skip (not crash, not match) and continue down the chain.
        let e = anyhow::anyhow!("garbage {{not valid json at all}}");
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r, None);
        assert_eq!(d, None);
    }

    #[test]
    fn extract_payload_error_ignores_non_string_fields() {
        // Defensive against a future payload variant emitting `error: 42`
        // — we want a clean (None, None) rather than a stringified int.
        let e = anyhow::anyhow!("body: {{\"error\":42,\"detail\":null}}");
        let (r, d) = extract_payload_error(&e);
        assert_eq!(r, None);
        assert_eq!(d, None);
    }

    // ── job_failed_from_err integration ────────────────────────────────────

    #[test]
    fn job_failed_from_err_populates_reason_and_detail() {
        let inner = anyhow::anyhow!(
            "CommitTx rejected: {{\"error\":\"preflight_insufficient_space\",\"detail\":\"/mnt/ext0 short by 28 GiB\"}}"
        );
        let outer = inner.context("transfer pipeline failed at COMMIT");
        let state = job_failed_from_err(1000, 2000, &outer);
        match state {
            JobState::Failed {
                error,
                error_reason,
                error_detail,
                elapsed_ms,
                ..
            } => {
                assert_eq!(
                    error_reason.as_deref(),
                    Some("preflight_insufficient_space")
                );
                assert_eq!(error_detail.as_deref(), Some("/mnt/ext0 short by 28 GiB"));
                assert_eq!(elapsed_ms, 1000);
                // Raw error chain preserved so the UI's "View raw"
                // expander has full context.
                assert!(error.contains("CommitTx rejected"));
            }
            _ => panic!("expected Failed state"),
        }
    }

    #[test]
    fn job_failed_from_err_with_plain_error_leaves_structured_fields_none() {
        let e = anyhow::anyhow!("network blip mid-transfer");
        let state = job_failed_from_err(0, 500, &e);
        match state {
            JobState::Failed {
                error_reason,
                error_detail,
                ..
            } => {
                assert_eq!(error_reason, None);
                assert_eq!(error_detail, None);
            }
            _ => panic!("expected Failed state"),
        }
    }
}
