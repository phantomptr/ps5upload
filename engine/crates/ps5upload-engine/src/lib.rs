//! ps5upload-engine — local HTTP service that drives AVA1 transfers.
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
//! PS5 address defaults to 192.168.137.2 (set PS5_ADDR to override).
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

mod ava1_api;
mod bundled_payload;
mod console_read;
mod convert_source;
mod elfldr_guard;
mod engine_log;
mod fakelibs_api;
mod fpkg_api;
mod fpkg_firmware;
mod icon_cache;
mod inspect;
mod install;
mod legacy_guard;
mod legacy_helper;
#[cfg(not(target_os = "android"))]
mod link;
mod local_fs;
mod log_dedup;
mod mgmt_route;
mod migrate_6;
mod pkg_install;
mod pkg_sidecar;
mod remote;
mod remote_download;
#[cfg(not(target_os = "android"))]
mod remote_pkg;
mod state_io;
mod telemetry;
#[cfg(feature = "webui")]
mod webui;
mod win_net;

#[cfg(test)]
mod ava1_only_tests;

use axum::http::HeaderMap;
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
use ps5upload_core::{
    app_lifecycle::{app_lifecycle, AppAction},
    cleanup::{cleanup_path, CleanupResult},
    diagnostics::appdb_query,
    diagnostics::{klog_read, net_interfaces},
    download::DownloadKind,
    focus::{focus_probe, FocusProbe},
    fs_ops::{
        app_launch, app_list_registered, app_register, app_unregister, backup_content_databases,
        fs_delete_with_op_id, fs_mkdir, fs_mount, fs_move_with_timeout, fs_op_cancel, fs_op_status,
        fs_read, fs_read_with_timeout, fs_unmount, list_dir, reconcile, DirListing, ListDirOptions,
        MountResult, ReconcileMode, RegisterResult,
    },
    game_meta::{parse_param_json_bytes, parse_param_sfo_bytes},
    hw::{
        drive_sensors, hw_info, hw_power, hw_set_fan_threshold_ex, hw_storage, hw_temps, proc_list,
        DriveSensorList, HwInfo, HwPower, HwStorage, HwTemps, ProcList,
    },
    process_mgr::{process_kill, process_list, ProcessKillAck, ProcessListResult},
    saves::{list_saves, list_screenshots, list_videos, SaveList, ScreenshotList},
    smp::{collect_status as smp_collect_status, SmpStatus},
    sys_time::{
        humanize_err as sys_time_humanize, ps5_time_get, ps5_time_set, PsTime, PsTimeSetResult,
    },
    system_control::{
        power_telemetry, system_control, PowerAction, PowerTelemetry, SystemControlAck,
    },
    transfer::{
        inspect_7z, inspect_zip, sevenz_plan_preview, zip_plan_preview, FileListEntry,
        TransferConfig,
    },
    users::{user_list, UserList},
    volumes::{list_volumes, VolumeList},
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// Build a `TransferConfig` for the given address. The transport's own knobs (in-flight
/// window, pack sizes) belong to AVA1's governor, so the retired shard-tuning environment
/// variables are not read. The outbound cap (`PS5UPLOAD_BANDWIDTH_MBPS`) is the one that stays.
fn make_transfer_config(addr: &str) -> TransferConfig {
    let mut cfg = TransferConfig::new(addr);
    cfg.bandwidth_cap_bps = bandwidth_cap_from_env(&|k| std::env::var(k).ok(), process_warned());
    cfg
}

/// The environment's outbound cap in bytes per second: `PS5UPLOAD_BANDWIDTH_MBPS` (the old
/// name is still read, with a deprecation line). Zero, negative or unparseable = no cap.
fn bandwidth_cap_from_env(
    get: &dyn Fn(&str) -> Option<String>,
    warned: &Mutex<std::collections::HashSet<String>>,
) -> Option<u64> {
    let (v, _) = renamed_env_with(BANDWIDTH_ENV.0, BANDWIDTH_ENV.1, get, warned);
    let mbps = v?.trim().parse::<f64>().ok().filter(|n| *n > 0.0)?;
    Some((mbps * 1024.0 * 1024.0) as u64)
}

/// Apply a per-request bandwidth cap to the config. None / 0 / negative
/// = leave the existing cap in place; positive values override.
/// Centralised so all transfer entry points apply the same precedence rule.
fn apply_per_request_bandwidth(cfg: &mut TransferConfig, cap_mbps: Option<f64>) {
    if let Some(n) = cap_mbps {
        if n > 0.0 {
            cfg.bandwidth_cap_bps = Some((n * 1024.0 * 1024.0) as u64);
        } else if n == 0.0 {
            // Explicit 0 = "unlimited".
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
        Arc, Mutex, OnceLock, Weak,
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

/// Where a staged job stands: the stage's id and place in the sequence, and its own bytes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct JobStage {
    pub id: String,
    pub index: u32,
    pub count: u32,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum JobState {
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
        /// The stage a staged job (an FPKG build) is in; `None` for jobs without stages.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stage: Option<JobStage>,
    },
    Done {
        started_at_ms: u64,
        completed_at_ms: u64,
        elapsed_ms: u64,
        tx_id_hex: String,
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
        /// The console the failure came from, when it names one (a PS5-to-PS5 relay talks
        /// to two). The client opens the pairing dialog for THIS console, not the one it
        /// happens to be watching the job against.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_console: Option<String>,
    },
}

/// Build a `JobState::Failed` from a transfer error, populating the
/// structured reason/detail fields when the payload's error body is
/// parseable. Single call site for every transfer handler's `Err(e)`
/// branch so the structured-field plumbing stays consistent.
fn job_failed_from_err(started_at_ms: u64, completed_at_ms: u64, err: &anyhow::Error) -> JobState {
    // A typed commit refusal must be matched before `extract_payload_error`:
    // the destination is unavailable, and retrying the same job cannot fix it,
    // whether the refusal arrived before or after the data was sent.
    // (C2). The reason is built from the typed `PostCommitKind` (`as_str()` is
    // the crate's single mapping), never parsed out of the Display (C1/A2).
    if let Some(pce) = err.downcast_ref::<ps5upload_ava1::PostCommitError>() {
        log_error!("the console refused to finish the transfer job: {err:#}");
        return JobState::Failed {
            started_at_ms,
            completed_at_ms,
            elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
            error: format!("{err:#}"),
            error_reason: Some(pce.kind.as_str().into()),
            error_detail: Some(pce.detail.clone()), // the console's message
            error_console: None,
        };
    }
    if let Some(cf) = err.downcast_ref::<ps5upload_ava1::upload::ConsoleFailure>() {
        let mut state = job_failed_from_err(
            started_at_ms,
            completed_at_ms,
            &anyhow::Error::from(cf.failure.clone()),
        );
        if let JobState::Failed { error_console, .. } = &mut state {
            *error_console = Some(cf.console.clone());
        }
        return state;
    }
    if let Some(failure) = err.downcast_ref::<ps5upload_ava1::upload::UploadFailure>() {
        log_error!(
            "transfer job failed: {} (reason={})",
            failure.detail,
            failure.reason
        );
        return JobState::Failed {
            started_at_ms,
            completed_at_ms,
            elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
            error: failure.detail.clone(),
            error_reason: Some(failure.reason.clone()),
            error_detail: Some(failure.detail.clone()),
            error_console: None,
        };
    }
    let (reason, detail) = extract_payload_error(err);
    // Central choke point for ALL async transfer-job failures (file, dir,
    // reconcile, download). Logging here means a mid-transfer death — the
    // "upload failed halfway through" case — shows the engine's full error
    // chain in engine.log, instead of the handler logging only the START.
    //
    // `error`, not `warn`: the user's transfer is over and did not succeed.
    // The engine used to have no `error` call sites at all, so every bug
    // bundle reported `error_logs: 0` however badly a transfer had failed,
    // and a maintainer filtering for errors saw an empty list. Advisory
    // warnings ("capacity preflight unavailable", which only skips a check)
    // stay at `warn` — the distinction is whether the user lost work.
    log_error!(
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
        error_console: None,
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
/// The console's address as the engine uses it: the host only. AVA1 has one port, so a `:port`
/// suffix from an older client (`ip:<retired port>`) is ignored. A bracketed IPv6 literal keeps
/// its brackets so the pool can add the AVA1 port.
pub(crate) fn console_addr(addr: &str) -> String {
    let a = addr.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return match rest.find(']') {
            Some(i) => format!("[{}]", &rest[..i]),
            None => a.to_string(),
        };
    }
    match a.split_once(':') {
        // exactly one colon: host:port. More than one is a bare IPv6 literal.
        Some((host, port)) if !port.contains(':') => host.to_string(),
        _ => a.to_string(),
    }
}

fn console_addr_or_default(addr: Option<String>, default_addr: &str) -> String {
    console_addr(addr.as_deref().unwrap_or(default_addr))
}

/// The engine's one startup line about the transport.
fn ava1_startup_line(dir: &str, identity: &str, paired: usize) -> String {
    format!("ava1: dir={dir} identity={identity} paired={paired}")
}

/// A renamed environment variable: the new name wins; the old name is still read, and the first
/// time it is the one that answers, a deprecation line is logged (`true` in the second field).
/// `warned` remembers which old names were already reported.
fn renamed_env_with(
    new: &str,
    old: &str,
    get: &dyn Fn(&str) -> Option<String>,
    warned: &Mutex<std::collections::HashSet<String>>,
) -> (Option<String>, bool) {
    if let Some(v) = get(new) {
        return (Some(v), false);
    }
    let Some(v) = get(old) else {
        return (None, false);
    };
    let first = warned
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(old.to_string());
    if first {
        crate::log_warn!(
            "{old} is deprecated and will stop working in a later release; set {new} instead"
        );
    }
    (Some(v), first)
}

/// Which deprecated names this process has already warned about.
fn process_warned() -> &'static Mutex<std::collections::HashSet<String>> {
    static WARNED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    WARNED.get_or_init(Default::default)
}

/// [`renamed_env_with`] on the process environment.
fn renamed_env(new: &str, old: &str) -> Option<String> {
    renamed_env_with(new, old, &|k| std::env::var(k).ok(), process_warned()).0
}

const ZIP_RAM_THRESHOLD_ENV: (&str, &str) = (
    "PS5UPLOAD_ZIP_RAM_THRESHOLD_MB",
    concat!("FT", "X2_ZIP_RAM_THRESHOLD_MB"),
);
const BANDWIDTH_ENV: (&str, &str) = (
    "PS5UPLOAD_BANDWIDTH_MBPS",
    concat!("FT", "X2_BANDWIDTH_MBPS"),
);
const ARCHIVE_STAGE_ENV: (&str, &str) = (
    "PS5UPLOAD_ARCHIVE_STAGE_MB",
    concat!("FT", "X2_ARCHIVE_STAGE_MB"),
);

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
    // The full per-request trace always goes to the debug log, which is
    // what the crash trace relies on. What varies is whether this also
    // reaches the user at the default level.
    //
    // The log-tail endpoint is the one exception: the log viewer polls it
    // about once a second, so logging it makes the log grow purely from
    // being watched — a self-feeding stream of `GET /api/engine-logs -> 200`
    // that crowds out real traffic and never stops. Reading the log is not
    // an event worth recording. A failure still gets through: only this
    // debug line is skipped, and the 5xx path below is untouched.
    if path != "/api/engine-logs" {
        log_debug!("{method} {path} -> {status} ({ms}ms)");
    }

    // A console that is switched off answers every status poll with 502,
    // once a second, indefinitely. Reported plainly that buries every
    // other line in the log and tells the reader nothing they did not
    // learn from the first one. So repeats collapse: first failure,
    // then quiet, then an occasional reminder, then a recovery line.
    let key = format!("{method} {path}");
    let action = match log_dedup::failure_log().lock() {
        Ok(mut log) => log.observe(&key, status >= 500, std::time::Instant::now()),
        // A poisoned lock must not silence real failures.
        Err(_) => {
            if status >= 500 {
                log_dedup::LogAction::Warn { suppressed: 0 }
            } else {
                log_dedup::LogAction::Quiet
            }
        }
    };
    match action {
        log_dedup::LogAction::Warn { suppressed: 0 } => {
            log_warn!("{method} {path} -> {status} ({ms}ms)");
        }
        log_dedup::LogAction::Warn { suppressed } => {
            log_warn!(
                "{method} {path} -> {status} ({ms}ms) — still failing, \
                 {suppressed} identical repeat(s) not logged"
            );
        }
        log_dedup::LogAction::Recovered { failures } => {
            log_warn!("{method} {path} -> {status} — recovered after {failures} failure(s)");
        }
        log_dedup::LogAction::Quiet => {}
    }
    resp
}

/// Config for `loopback_guard`: extra peers allowed besides loopback.
/// `Arc<[..]>` so the per-request middleware-state clone is a refcount bump,
/// not a Vec copy.
#[derive(Clone)]
struct LoopbackGuardConfig {
    allowed_ips: std::sync::Arc<[AllowRule]>,
}

/// One `PS5UPLOAD_ALLOW_IP` entry: a single address (`192.168.1.20`) or a
/// CIDR range (`192.168.1.0/24`). Ranges exist for self-hosters: a homelab
/// engine is driven from phones and laptops whose DHCP addresses change, and
/// an exact-IP list meant a fresh `403` every time a lease renewed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllowRule {
    addr: std::net::IpAddr,
    prefix: u8,
}

impl AllowRule {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (addr, prefix) = match raw.split_once('/') {
            Some((a, p)) => (
                a.trim().parse::<std::net::IpAddr>().ok()?,
                Some(p.trim().parse::<u8>().ok()?),
            ),
            None => (raw.parse::<std::net::IpAddr>().ok()?, None),
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = prefix.unwrap_or(max);
        (prefix <= max).then_some(Self { addr, prefix })
    }

    fn contains(&self, ip: std::net::IpAddr) -> bool {
        use std::net::IpAddr;
        // A v4 peer reaching a dual-stack socket shows up as ::ffff:a.b.c.d.
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            v4 => v4,
        };
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for AllowRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let max = if self.addr.is_ipv4() { 32 } else { 128 };
        if self.prefix == max {
            write!(f, "{}", self.addr)
        } else {
            write!(f, "{}/{}", self.addr, self.prefix)
        }
    }
}

/// True when `peer` matches any `PS5UPLOAD_ALLOW_IP` rule.
pub(crate) fn allow_rules_contain(rules: &[AllowRule], peer: std::net::IpAddr) -> bool {
    rules.iter().any(|r| r.contains(peer))
}

/// Pure allow/deny decision so it can be unit-tested without a live server.
/// Allow when the path is the PS5-facing `/pkg-host/*`, the peer is on
/// loopback, or the peer matches one of the configured rules.
fn loopback_allows(cfg: &LoopbackGuardConfig, peer: std::net::IpAddr, path: &str) -> bool {
    const OFF_LOOPBACK_ALLOWED: &[&str] = &["/pkg-host/"];
    OFF_LOOPBACK_ALLOWED.iter().any(|p| path.starts_with(p))
        || peer.is_loopback()
        || allow_rules_contain(&cfg.allowed_ips, peer)
}

/// Parse a comma-separated `PS5UPLOAD_ALLOW_IP` value into rules, trimming
/// whitespace and dropping blank/unparseable entries.
fn parse_allow_ips(raw: &str) -> Vec<AllowRule> {
    raw.split(',').filter_map(AllowRule::parse).collect()
}

/// The `PS5UPLOAD_ALLOW_IP` entries that are not blank and do not parse. They
/// used to vanish silently, so a typo looked exactly like "the allowlist is
/// ignored".
fn invalid_allow_ip_entries(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && AllowRule::parse(s).is_none())
        .map(str::to_string)
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
    eprintln!(
        "[ps5upload-engine] refusing request to {path} from {peer}: not loopback and not in \
         PS5UPLOAD_ALLOW_IP — to allow it, set PS5UPLOAD_ALLOW_IP={} (or its subnet, e.g. \
         192.168.1.0/24)",
        peer.ip()
    );
    // Say who was refused and what to set. The bare "loopback only" gave a
    // self-hoster nothing to act on, and behind Docker the address the engine
    // sees is often not the one they would guess.
    (
        StatusCode::FORBIDDEN,
        format!(
            "loopback only: this engine refused {ip}. To allow it, restart the engine with \
             PS5UPLOAD_ALLOW_IP={ip} (or a range such as 192.168.1.0/24). Only do this on a \
             trusted LAN — the API has no password.",
            ip = peer.ip()
        ),
    )
        .into_response()
}

/// Reject browser-initiated cross-site access even when the peer itself is
/// loopback. Without this, a malicious web page can target localhost and use
/// permissive CORS (or a state-changing GET embedded as an image) to control
/// the console through the engine. Native/Tauri proxies and CLI clients do not
/// send browser fetch metadata and remain supported.
fn browser_origin_allows(origin: &str, host: &str) -> bool {
    let Some((origin_scheme, origin_rest)) = origin.split_once("://") else {
        return false;
    };
    let origin_authority = origin_rest.split('/').next().unwrap_or("");
    if origin_authority.eq_ignore_ascii_case(host) {
        return true;
    }
    // Vite development serves the renderer on a different port, but both
    // sides are still loopback. Production web UI is same-origin.
    fn authority_name(authority: &str) -> &str {
        let name = if let Some(bracketed) = authority.strip_prefix('[') {
            bracketed.split(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        name
    }
    fn loopback_name(authority: &str) -> bool {
        let name = authority_name(authority);
        name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1"
    }
    let origin_name = authority_name(origin_authority);
    let tauri_renderer = origin_name.eq_ignore_ascii_case("tauri.localhost")
        || ((origin_scheme.eq_ignore_ascii_case("tauri")
            || origin_scheme.eq_ignore_ascii_case("asset"))
            && origin_name.eq_ignore_ascii_case("localhost"));

    // Tauri's renderer is intentionally cross-origin from its localhost
    // sidecar (`http://tauri.localhost` / `tauri://localhost`). Browsers do
    // not let arbitrary pages forge Origin, so this preserves the native app
    // while foreign web origins remain denied. Vite uses loopback cross-port
    // fetches during development and receives the same narrow exception.
    tauri_renderer || (loopback_name(origin_authority) && loopback_name(host))
}

fn browser_request_allows(headers: &axum::http::HeaderMap, path: &str) -> bool {
    if path.starts_with("/pkg-host/") {
        return true;
    }
    let cross_site = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("cross-site"));
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        // Browser image subresources do not normally send Origin. Tauri's
        // renderer therefore reaches the loopback sidecar with
        // `Sec-Fetch-Site: cross-site`, `Sec-Fetch-Dest: image`, and a trusted
        // Referer. Permit that shape only for the two read-only cover routes;
        // keeping the path + destination checks narrow prevents a foreign
        // page from turning an embedded GET into access to another API route.
        if cross_site
            && matches!(path, "/api/ps5/app-icon" | "/api/ps5/game-icon")
            && headers
                .get("sec-fetch-dest")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("image"))
        {
            return headers
                .get("referer")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|referer| browser_origin_allows(referer, host));
        }
        // Other cross-site navigations and embedded GETs may also omit Origin
        // but still carry Fetch Metadata. Native/CLI clients carry neither.
        return !cross_site;
    };
    browser_origin_allows(origin, host)
}

async fn browser_origin_guard(req: Request, next: Next) -> impl IntoResponse {
    let path = req.uri().path();
    if browser_request_allows(req.headers(), path) {
        return next.run(req).await.into_response();
    }
    eprintln!("[ps5upload-engine] refusing cross-site browser request to {path}");
    (
        StatusCode::FORBIDDEN,
        "cross-site browser requests are not allowed",
    )
        .into_response()
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

/// `walk_plan` over any source (a saved server): one listing per folder, sizes from the listing.
/// A folder that cannot be listed fails the plan — over a network that is a dropped connection
/// or a permission problem, and skipping it would upload an incomplete game and call it done.
fn walk_plan_with(
    fs: &dyn ps5upload_core::source_fs::SourceFs,
    root: &std::path::Path,
    excludes: &[String],
) -> Result<(u64, Vec<PlannedFile>), String> {
    let mut stack = vec![root.to_path_buf()];
    let mut total = 0u64;
    let mut out = Vec::new();
    while let Some(dir) = stack.pop() {
        let children = fs
            .read_dir(&dir)
            .map_err(|e| format!("could not list {}: {e}", dir.display()))?;
        for (path, is_dir) in children {
            if is_dir {
                stack.push(path);
                continue;
            }
            if ps5upload_core::excludes::is_excluded_strings(&path, excludes) {
                continue;
            }
            let m = fs
                .metadata(&path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            {
                total += m.len;
                let rel = path.strip_prefix(root).unwrap_or(&path);
                out.push(PlannedFile {
                    rel_path: rel.to_string_lossy().replace('\\', "/"),
                    size: m.len,
                });
            }
        }
    }
    out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok((total, out))
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
    /// Transfers such as SMB staging discover their total only after an
    /// asynchronous preparation phase. When present, this atomic becomes the
    /// authoritative denominator and prevents a ticker created with total=0
    /// from overwriting the later real total.
    dynamic_total_bytes: Option<Arc<AtomicU64>>,
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
        let mut last_total = u64::MAX;
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
            let total_bytes = ctx
                .dynamic_total_bytes
                .as_ref()
                .map_or(ctx.total_bytes, |total| total.load(Ordering::Acquire));
            let raw_bytes = progress.load(Ordering::Relaxed);
            let bytes_sent = if total_bytes > 0 {
                raw_bytes.min(total_bytes)
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
                && total_bytes == last_total
            {
                continue;
            }
            last_bytes = bytes_sent;
            last_files = files_processing;
            last_ff = files_finalized;
            last_bf = bytes_finalized;
            last_total = total_bytes;
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
                        *t = total_bytes;
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
                    let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": with_live_notes(job_id, serde_json::json!(state)) });
                    let _ = events_tx.send(msg.to_string());
                }
                _ => break,
            }
        }
    });
    stop
}

/// `(skipped files, skipped bytes, files sent)` from an AVA1 commit ack, when it has them.
fn ava1_skip_counts(ack: &str) -> Option<(u64, u64, u64)> {
    let v: serde_json::Value = serde_json::from_str(ack).ok()?;
    Some((
        v["skipped_files"].as_u64()?,
        v["skipped_bytes"].as_u64()?,
        v["files_sent"].as_u64()?,
    ))
}

/// While an AVA1 skip-existing job reads the whole source up front (the `verify` policy),
/// shows it as a "verify" stage with the bytes hashed so far. The upload bar itself stays
/// at 0 meanwhile; the existing Upload screen does not render `stage`, so this is carried
/// in the job snapshot for any client that wants it. Ends when hashing completes or `stop`.
fn spawn_verify_stage(
    jobs: Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: broadcast::Sender<String>,
    job_id: Uuid,
    hashed: Arc<AtomicU64>,
    total: u64,
    stop: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut last = u64::MAX;
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if stop.load(Ordering::Acquire) {
                break;
            }
            let done = hashed.load(Ordering::Relaxed).min(total);
            if done == last {
                continue;
            }
            last = done;
            let finished = done >= total;
            let snapshot = {
                let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
                match g.get_mut(&job_id) {
                    Some(JobState::Running { stage, .. }) => {
                        *stage = (!finished).then(|| JobStage {
                            id: "verify".into(),
                            index: 1,
                            count: 2,
                            done,
                            total,
                        });
                        g.get(&job_id).cloned()
                    }
                    _ => None,
                }
            };
            let Some(state) = snapshot else { break };
            let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": with_live_notes(job_id, serde_json::json!(state)) });
            let _ = events_tx.send(msg.to_string());
            if finished {
                break;
            }
        }
    });
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
                error_console: None,
            },
        );
    }
}

#[derive(Clone)]
pub(crate) struct AppState {
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
/// Self-pruning: the transfer thread holds the only strong `Arc` clone; the
/// registry keeps a `Weak`. When the transfer finishes, the `Arc` drops and
/// the `Weak` upgrades to `None` — so `register` can prune dead entries
/// without the `Arc::strong_count` race (strong_count is not synchronization-
/// safe and could prune a still-running transfer if the count transiently
/// dipped between retain and insert).
fn cancel_registry() -> &'static Mutex<HashMap<Uuid, Weak<AtomicBool>>> {
    static REG: OnceLock<Mutex<HashMap<Uuid, Weak<AtomicBool>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Live notes of running AVA1 jobs, weakly held: the transfer's config owns the `Arc`, so the
/// entry dies with the transfer and a finished job never reports stale notes.
fn live_registry() -> &'static Mutex<HashMap<Uuid, Weak<ps5upload_core::transfer::LiveNotes>>> {
    static REG: OnceLock<Mutex<HashMap<Uuid, Weak<ps5upload_core::transfer::LiveNotes>>>> =
        OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The live notes for `job_id` (to thread into `TransferConfig::progress_live`).
pub(crate) fn live_notes_for(job_id: Uuid) -> Arc<ps5upload_core::transfer::LiveNotes> {
    let notes = Arc::new(ps5upload_core::transfer::LiveNotes::default());
    // Held until the job ends: the telemetry record reads them then (review 009 #4).
    telemetry::hold_notes(job_id, notes.clone());
    let mut g = live_registry().lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|_, v| v.strong_count() > 0);
    g.insert(job_id, Arc::downgrade(&notes));
    notes
}

/// Adds a running job's live notes to its snapshot JSON: `phase` (`"skipping"` with
/// `skip_done_bytes` / `skip_total_bytes`), `bottleneck` (AVA1's word) and `settling`. Fields
/// that do not apply are absent, so the client shows nothing for them. A snapshot that is not
/// `running` is returned as it is.
pub(crate) fn merge_live_notes(
    notes: Option<&ps5upload_core::transfer::LiveNotes>,
    mut v: serde_json::Value,
) -> serde_json::Value {
    use std::sync::atomic::Ordering::Relaxed;
    let (Some(n), Some(o)) = (notes, v.as_object_mut()) else {
        return v;
    };
    if o.get("status").and_then(|s| s.as_str()) != Some("running") {
        return v;
    }
    if n.phase.load(Relaxed) == ps5upload_core::transfer::LIVE_PHASE_SKIPPING {
        o.insert("phase".into(), "skipping".into());
        o.insert(
            "skip_done_bytes".into(),
            n.skip_done_bytes.load(Relaxed).into(),
        );
        o.insert(
            "skip_total_bytes".into(),
            n.skip_total_bytes.load(Relaxed).into(),
        );
    }
    let bn = n.bottleneck.load(Relaxed);
    if bn != 0 {
        o.insert(
            "bottleneck".into(),
            ps5upload_ava1::progress::bottleneck_name(bn).into(),
        );
    }
    if n.settling.load(Relaxed) {
        o.insert("settling".into(), true.into());
        // How many files the console still has to make permanent, and the most it had: the
        // client turns the fall of the first into "N of M" and a time left.
        let left = n.unswept.load(Relaxed);
        let peak = n.unswept_peak.load(Relaxed).max(left);
        o.insert("settle_files_left".into(), left.into());
        o.insert("settle_files_total".into(), peak.into());
    }
    v
}

/// `merge_live_notes` for a job id looked up in the registry.
pub(crate) fn with_live_notes(job_id: Uuid, v: serde_json::Value) -> serde_json::Value {
    let notes = live_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&job_id)
        .and_then(|w| w.upgrade());
    merge_live_notes(notes.as_deref(), v)
}

/// Register a fresh cancel flag for `job_id` and return it to thread into
/// `TransferConfig::cancel`. Prunes flags whose transfer has finished.
pub(crate) fn register_transfer_cancel(job_id: Uuid) -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let mut g = cancel_registry().lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|_, v| v.strong_count() > 0);
    g.insert(job_id, Arc::downgrade(&flag));
    flag
}

/// Flip a job's cancel flag if it's registered (i.e. still running). Returns
/// true if a flag was found. Idempotent.
fn signal_transfer_cancel(job_id: Uuid) -> bool {
    let g = cancel_registry().lock().unwrap_or_else(|e| e.into_inner());
    match g.get(&job_id) {
        Some(weak) => match weak.upgrade() {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        },
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
pub(crate) fn set_job(
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
    if matches!(state, JobState::Done { .. } | JobState::Failed { .. }) {
        telemetry::on_state(job_id, &serde_json::json!(state));
    }
    let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": with_live_notes(job_id, serde_json::json!(state)) });
    let _ = events_tx.send(msg.to_string());
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The synchronous form of [`fail_job_unless_console_ready`] for the routes that answer with
/// an error text instead of a job: the same token and message, `helper_not_ava1: <message>` or
/// `not_paired: <message>`, which the client matches on. Blocking.
fn require_console_ready(addr: &str) -> anyhow::Result<()> {
    ps5upload_ava1::console::require_ava1(addr)
        .map_err(|f| anyhow::anyhow!("{}: {}", f.reason, f.detail))
}

/// Fails the job when the console cannot be used over AVA1: nothing listening or an older
/// helper (`helper_not_ava1`), or not paired yet (`not_paired`). There is no other transport to
/// fall back to, so this runs first, before any preflight. Blocking. `true` = the job was failed.
fn fail_job_unless_console_ready(
    jobs: &Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: &broadcast::Sender<String>,
    job_id: Uuid,
    started_at_ms: u64,
    addr: &str,
) -> bool {
    match ps5upload_ava1::console::require_ava1(addr) {
        Ok(()) => false,
        Err(failure) => {
            let completed_at_ms = now_ms();
            set_job(
                jobs,
                events_tx,
                job_id,
                job_failed_from_err(
                    started_at_ms,
                    completed_at_ms,
                    // Names the console: the client opens THAT console's pairing dialog.
                    &anyhow::Error::from(ps5upload_ava1::upload::ConsoleFailure::on(addr, failure)),
                ),
            );
            true
        }
    }
}

/// Both consoles of a PS5-to-PS5 copy must be usable over AVA1 before any work starts;
/// the first one that is not names itself in the failure. `check` is the readiness probe
/// (the real one is `console::require_ava1`).
fn relay_preflight(
    from: &str,
    to: &str,
    check: impl Fn(&str) -> Result<(), ps5upload_ava1::upload::UploadFailure>,
) -> Result<(), ps5upload_ava1::upload::ConsoleFailure> {
    for console in [from, to] {
        check(console).map_err(|f| ps5upload_ava1::upload::ConsoleFailure::on(console, f))?;
    }
    Ok(())
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
    /// "fast" | "safe": skip files the console already has (the Resume strategy).
    /// Honoured on AVA1 consoles, where the receiver decides; `/api/transfer/dir-reconcile`
    /// sets it when it hands a job to this handler.
    #[serde(default)]
    skip_existing: Option<String>,
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
}

#[derive(Deserialize)]
struct Ps5ToPs5Req {
    from: String,
    src: String,
    to: String,
    dest: String,
    tx_id: Option<String>,
}

async fn ps5_to_ps5_handler(
    State(state): State<AppState>,
    Json(req): Json<Ps5ToPs5Req>,
) -> impl IntoResponse {
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "relay");
    telemetry::set_drive(job_id, &req.dest);
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: 0,
            files: vec![],
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
    let cancel = register_transfer_cancel(job_id);
    let progress = Arc::new(ava1::send::Progress::default());
    let bytes = Arc::new(AtomicU64::new(0));
    let files = Arc::new(AtomicU64::new(0));
    let durable_bytes = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(0));
    let live = live_notes_for(job_id);
    let stop_ticker = spawn_progress_ticker(
        Arc::clone(&jobs),
        events_tx.clone(),
        job_id,
        TickerContext {
            started_at_ms,
            total_bytes: 0,
            dynamic_total_bytes: Some(total.clone()),
            skipped_files: 0,
            skipped_bytes: 0,
        },
        bytes.clone(),
        files.clone(),
        files.clone(),
        durable_bytes.clone(),
    );
    let mirror_stop = stop_ticker.clone();
    let mirror_progress = progress.clone();
    tokio::spawn(async move {
        while !mirror_stop.load(Ordering::Acquire) {
            bytes.store(
                mirror_progress.bytes_sent.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            files.store(
                mirror_progress.files_durable.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            durable_bytes.store(
                mirror_progress.bytes_durable.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            total.store(
                mirror_progress.bytes_total.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            live.bottleneck.store(
                mirror_progress.bottleneck.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    });
    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker.clone());
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        // Like every other AVA1 job: no console that cannot be used (not paired, an old
        // helper, nobody listening) starts a relay, and the failure names which one.
        let result = match relay_preflight(&req.from, &req.to, |c| {
            ps5upload_ava1::console::require_ava1(c)
        }) {
            Err(f) => Err(anyhow::Error::from(f)),
            Ok(()) => ps5upload_ava1::relay::ps5_to_ps5(
                &req.from,
                &req.src,
                &req.to,
                &req.dest,
                tx_id,
                progress.clone(),
                cancel,
            ),
        };
        let completed_at_ms = now_ms();
        let state = match result {
            Ok(r) => JobState::Done {
                started_at_ms,
                completed_at_ms,
                elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                tx_id_hex: ava1::hex::encode(&tx_id),
                bytes_sent: progress.bytes_sent.load(Ordering::Relaxed),
                dest: req.dest,
                files_sent: r.files as u64,
                skipped_files: 0,
                skipped_bytes: 0,
                commit_ack: Some(serde_json::json!({
                    "protocol": "ava1", "files": r.files, "bytes": r.bytes,
                    "resent": r.resent, "max_lanes": r.max_lanes,
                })),
            },
            Err(e) => job_failed_from_err(started_at_ms, completed_at_ms, &e),
        };
        stop_ticker.store(true, Ordering::Release);
        set_job(&jobs, &events_tx, job_id, state);
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

/// `/api/link/probe` request (R4, #368). Desktop-only, same gate as the handler.
#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct LinkProbeReq {
    url: String,
    #[serde(default)]
    insecure_tls: bool,
}

/// `/api/link/download` request: download a link's file to a console folder.
#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct LinkDownloadReq {
    addr: Option<String>,
    tx_id: Option<String>,
    url: String,
    /// Absolute console folder the file lands in.
    dest_dir: String,
    /// Overrides the name the link supplies.
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    insecure_tls: bool,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

/// `/api/rar/packages` request. Desktop-only, same gate as the handler.
#[cfg(not(target_os = "android"))]
#[derive(Deserialize)]
struct RarPackagesReq {
    archive_path: String,
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
    /// "fast" = size and mtime (default), "safe" = content.
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

#[derive(Serialize)]
pub(crate) struct JobCreated {
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

pub(crate) fn json_err(code: StatusCode, msg: impl Into<String>) -> impl IntoResponse {
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
        Err(e) => {
            let msg = format!("{e:#}");
            json_err(list_dir_error_status(&msg), msg).into_response()
        }
    }
}

/// A folder that isn't there is a 404, not a failing console: the payload answered, and said
/// ENOENT (`…_errno_2`). Everything else stays 502. The body is the same either way, which is
/// what the app reads.
fn list_dir_error_status(msg: &str) -> StatusCode {
    // `ends_with`, so errno 20 (ENOTDIR) and friends aren't read as 2.
    if msg.trim_end().ends_with("_errno_2") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_GATEWAY
    }
}

// ─── Local (engine host) filesystem browse ─────────────────────────────────
//
// Browser-mode counterpart to the Tauri desktop app's native file dialog —
// browses the ENGINE's own filesystem (e.g. a Docker container's mounted
// volumes), not the PS5's (see `ps5_list_dir` above) and not the browser's
// own machine (impossible for a remote client). See `local_fs` module doc
// for why this doesn't expand what the engine can already be asked to read
// (the transfer routes already accept any caller-supplied local path).

#[derive(Deserialize)]
struct LocalListDirQuery {
    path: String,
}

/// GET /api/local/list-dir?path=/data/games
async fn local_list_dir_handler(Query(q): Query<LocalListDirQuery>) -> impl IntoResponse {
    let path = q.path;
    let result: Result<Vec<local_fs::LocalEntry>, anyhow::Error> =
        tokio::task::spawn_blocking(move || local_fs::list_dir(&path))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/local/storage-roots
async fn local_storage_roots_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(local_fs::storage_roots())).into_response()
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
    /// Merge into an existing destination instead of refusing it. Files that
    /// collide are replaced; anything already in the destination that the
    /// source doesn't mention is left alone. Absent = the historical
    /// refuse-to-clobber behaviour, so only a caller that has actually asked
    /// the user gets the destructive path.
    #[serde(default)]
    overwrite: bool,
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let from = req.from;
    let to = req.to;
    let started = std::time::Instant::now();
    crate::log_info!("fs_move: addr={addr} from={from} to={to}");
    let from_for_log = from.clone();
    let to_for_log = to.clone();
    // 1-hour deadline: an intra-volume fs_move returns in milliseconds
    // (rename(2) is metadata-only). A CROSS-volume move can't rename (the
    // payload refuses it — a cross-device rename panics this kernel) and
    // returns `fs_move_cross_mount`; the engine then completes it as a console-side
    // copy-then-delete, which can run for minutes on a multi-GiB file. Keep the
    // generous bound so a socket timeout can't fire mid-op.
    let io_timeout = std::time::Duration::from_secs(60 * 60);
    let overwrite = req.overwrite;
    let op_id = if req.op_id != 0 {
        req.op_id
    } else {
        next_fs_op_id()
    };
    // One blocking closure for the whole decision: the rename is still tried first (a
    // same-drive move is metadata-only) and only a cross-mount refusal becomes a console-side
    // move (copy, then delete the source after a verified finish). `overwrite` travels with
    // it, so a move that was not allowed to clobber still refuses.
    match tokio::task::spawn_blocking(move || {
        match fs_move_with_timeout(&addr, &from, &to, Some(io_timeout)) {
            Err(e)
                if {
                    let msg = format!("{e:#}");
                    msg.contains("cross_mount") || msg.contains("EXDEV")
                } =>
            {
                require_console_ready(&addr)?;
                ps5upload_ava1::copy::console_copy(&addr, &from, &to, op_id, true, overwrite)
            }
            other => other,
        }
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
            if e.to_string() == "cancelled" {
                crate::log_info!(
                    "fs_move cancelled: {from_for_log} -> {to_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
                return json_err(StatusCode::CONFLICT, "cancelled").into_response();
            }
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let from = req.from;
    let to = req.to;
    let started = std::time::Instant::now();
    crate::log_info!("fs_copy: addr={addr} from={from} to={to}");
    let from_for_log = from.clone();
    let to_for_log = to.clone();
    // The copy runs on the console as a job (`job.copy`): the engine polls it, so a connection
    // blip does not abort a healthy copy. A non-zero op_id is required for tracking; generate
    // one when the caller didn't supply theirs (they just won't get a % bar).
    let op_id = if req.op_id != 0 {
        req.op_id
    } else {
        next_fs_op_id()
    };
    let overwrite = req.overwrite;
    match tokio::task::spawn_blocking(move || {
        require_console_ready(&addr)?;
        ps5upload_ava1::copy::console_copy(&addr, &from, &to, op_id, false, overwrite)
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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

#[derive(Deserialize)]
struct ContentDbBackupReq {
    addr: Option<String>,
    /// Local directory to write the snapshot into. A timestamped
    /// subdirectory is created underneath.
    dest_dir: String,
}

/// POST /api/ps5/content-db/backup — snapshot `app.db` + `appinfo.db`.
///
/// These two files are the console's record of what is installed, and they
/// can drift from what is actually on disk — a title whose files are gone
/// but whose row survives shows in Settings -> Storage and refuses to
/// delete. Repairing that means editing the databases, so having a
/// known-good copy first is the difference between a recoverable mistake
/// and a broken content index.
async fn ps5_content_db_backup(
    State(state): State<AppState>,
    Json(req): Json<ContentDbBackupReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let stamp = now_ms() / 1000;
    let dest = std::path::PathBuf::from(&req.dest_dir).join(format!("appdb-{stamp}"));
    crate::log_info!("content_db_backup: addr={addr} dest={}", dest.display());
    let dest_for_task = dest.clone();
    match tokio::task::spawn_blocking(move || backup_content_databases(&addr, &dest_for_task))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(paths) => {
            let files: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            crate::log_info!("content_db_backup ok: {} file(s)", files.len());
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "dir": dest, "files": files })),
            )
                .into_response()
        }
        Err(e) => {
            crate::log_warn!("content_db_backup failed: {e:#}");
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_app_unregister(
    State(state): State<AppState>,
    Json(req): Json<AppUnregisterReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let title_id = req.title_id;
    let started = std::time::Instant::now();
    crate::log_info!("app_unregister: addr={addr} title_id={title_id}");
    let title_for_log = title_id.clone();
    // Uninstalling changes which artwork exists, so drop this console's
    // cached images now rather than letting them age out — the user would
    // otherwise see a cover for a title they just removed.
    icon_cache::invalidate_console(&addr);
    match tokio::task::spawn_blocking(move || app_unregister(&addr, &title_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(outcome) => {
            // Our teardown succeeded. Sony's own uninstall is a SEPARATE
            // result and may have been refused — report it instead of
            // flattening both into a bare `ok: true`, which is what let a
            // 0x80B21B02 refusal masquerade as a successful uninstall while
            // the title stayed in Settings → Storage.
            if outcome.sony_refused() {
                crate::log_warn!(
                    "app_unregister: {title_for_log} — our teardown ok in {} ms, but \
                     sceAppInstUtilAppUninstall REFUSED with rc=0x{:08X}; the title may \
                     remain in Settings → Storage",
                    started.elapsed().as_millis(),
                    outcome.sony_uninstall_rc
                );
            } else {
                crate::log_info!(
                    "app_unregister ok: {title_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": true,
                    "sony_uninstall_refused": outcome.sony_refused(),
                    "sony_uninstall_rc": format!("0x{:08X}", outcome.sony_uninstall_rc),
                })),
            )
                .into_response()
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let op_id = q.op_id;
    // An op this engine runs over AVA1 answers from its own registry; any other id falls
    // through to the console.
    if let Some(snap) = ps5upload_ava1::copy::op_snapshot(op_id) {
        return (
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
            .into_response();
    }
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let op_id = req.op_id;
    crate::log_info!("fs_op_cancel: op_id={op_id}");
    if ps5upload_ava1::copy::op_cancel(op_id) {
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "cancelled": true })),
        )
            .into_response();
    }
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    /// Target wall-clock time as unix seconds (UTC). Used when
    /// `use_ntp` is false (or absent). Ignored when `use_ntp` is true,
    /// so an NTP sync may omit it entirely.
    #[serde(default)]
    target_unix_seconds: i64,
    /// When true, the engine queries an NTP server (Cloudflare/Google/
    /// pool.ntp.org) for the current time instead of using the client-
    /// provided `target_unix_seconds`. This avoids PC-clock drift and
    /// is the recommended mode for accurate time sync.
    #[serde(default)]
    use_ntp: bool,
    /// Optional custom NTP server. Defaults to Cloudflare → Google →
    /// pool.ntp.org → Windows (first success wins).
    ntp_server: Option<String>,
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
    /// True when the payload set the clock via `settimeofday` because
    /// the SCE call was unavailable, rejected, or a no-op. Surfaced
    /// because it has a visible consequence: settimeofday moves the
    /// kernel wall clock underneath ShellCore, so Sony's Settings UI
    /// may keep showing the old time until reopened.
    used_fallback: bool,
    /// The epoch the console was actually asked to adopt.
    target_unix: i64,
    /// Where `target_unix` came from — "ntp" or "client".
    source: String,
    /// Which NTP server answered. None when `source` is "client".
    ntp_server: Option<String>,
}

async fn ps5_time_get_route(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);

    // Resolve the target time: either from NTP or from the client-provided value.
    let (target, source, ntp_server) = if req.use_ntp {
        let custom = req.ntp_server.clone();
        let ntp_result = tokio::task::spawn_blocking(move || {
            let servers: Vec<&str> = match &custom {
                Some(s) => vec![s.as_str()],
                None => ps5upload_core::sys_time::DEFAULT_NTP_SERVERS.to_vec(),
            };
            ps5upload_core::sys_time::ntp_query_unix_seconds_with_server(&servers)
        })
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
        match ntp_result {
            Ok((server, ts)) => (ts, "ntp", Some(server)),
            Err(e) => {
                return json_err(StatusCode::BAD_GATEWAY, format!("NTP query failed: {e:#}"))
                    .into_response();
            }
        }
    } else {
        (req.target_unix_seconds, "client", None)
    };

    let target_final = target;
    let r: Result<PsTimeSetResult, anyhow::Error> =
        tokio::task::spawn_blocking(move || ps5_time_set(&addr, target_final))
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
                used_fallback: v.used_fallback,
                target_unix: target,
                source: source.to_string(),
                ntp_server,
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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

/// Drive SMART / temperature sensors. Enumerates `/dev/daN` disks via
/// CAM pass-through (SCSI LOG SENSE page 0x0D) and returns per-drive
/// temp, capacity, ident, and filesystem usage. Also includes fixed-
/// storage summaries (internal SSD + M.2 expansion).
async fn ps5_hw_drive_sensors(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<DriveSensorList, anyhow::Error> =
        tokio::task::spawn_blocking(move || drive_sensors(&addr))
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ProcList, anyhow::Error> = tokio::task::spawn_blocking(move || proc_list(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── Process manager (Processes screen — distinct from PROC_LIST above:
// this is process_mgr's detailed pid/name/title_id/memory/kind view with
// kill support, not hw's lightweight snapshot) ──────────────────────────

/// GET /api/ps5/process/list — detailed process enumeration + kill support.
async fn ps5_process_list(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ProcessListResult, anyhow::Error> =
        tokio::task::spawn_blocking(move || process_list(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct HostQuery {
    host: String,
}

/// GET /api/ps5/elfldr/health?host= — whether the loader on :9021 answers (`healthy`), took
/// the connection and never answered (`stuck`), or isn't there (`absent`).
async fn ps5_elfldr_health(Query(q): Query<HostQuery>) -> impl IntoResponse {
    let health = tokio::task::spawn_blocking(move || {
        elfldr_guard::probe(
            q.host.trim(),
            9021,
            std::time::Duration::from_secs(3),
            std::time::Duration::from_secs(5),
        )
    })
    .await;
    match health {
        Ok(h) => (StatusCode::OK, Json(serde_json::json!({ "health": h }))).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// POST /api/ps5/elfldr/ensure {host} — swap the console's stock elfldr for the patched one
/// (see `elfldr_guard`). Needs the helper up.
async fn ps5_elfldr_ensure(Json(q): Json<HostQuery>) -> impl IntoResponse {
    let r = tokio::task::spawn_blocking(move || elfldr_guard::ensure(q.host.trim())).await;
    match r {
        Ok(Ok(outcome)) => (StatusCode::OK, Json(outcome)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// The first time a console answers with the 6.0 helper, delete the pre-6.0 helper's folders (once per
/// console, remembered on disk; a failed attempt is retried on a later answer). Background and
/// best-effort: it never delays or fails the state answer.
fn spawn_console_upgrade_cleanup(console: String) {
    static RUNNING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let Some(dir) = crate::remote::store::data_dir() else {
        return;
    };
    let host = console_addr(&console);
    if !migrate_6::console_pending(&dir, &host) {
        return;
    }
    {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains(&host) {
            return;
        }
        running.push(host.clone());
    }
    tokio::task::spawn_blocking(move || {
        let done = migrate_6::clean_console(|path| {
            fs_delete_with_op_id(&host, path, 0, Some(std::time::Duration::from_secs(300)))
                .map_err(|e| format!("{e:#}"))
        });
        if done {
            migrate_6::mark_console_done(&dir, &host);
            crate::log_info!("6.0 upgrade: removed the old transfer folders on {host}");
        }
        RUNNING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|h| h != &host);
    });
}

/// GET /api/ps5/helper/state?host= — `{"state": "ava1" | "helper_old" | "starting" | "ava1_failed" | "not_running"}`: whether the
/// console runs an AVA1 helper, an older helper that only speaks the old protocol (the UI offers
/// the one-click update), or nothing (the usual send-payload flow). A TCP-level answer: pairing is
/// a session matter.
async fn ps5_helper_state(Query(q): Query<HostQuery>) -> impl IntoResponse {
    // The client sends `[v6]:port` for an IPv6 console; the probes add their own ports.
    let host = legacy_guard::key(&q.host);
    let console = host.clone();
    let r = tokio::task::spawn_blocking(move || {
        legacy_helper::state(&host, legacy_helper::Ports::default())
    })
    .await;
    if matches!(r, Ok(legacy_helper::AVA1)) {
        spawn_console_upgrade_cleanup(console);
    }
    match r {
        Ok(s) => (StatusCode::OK, Json(serde_json::json!({ "state": s }))).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// POST /api/ps5/helper/replace {host} — replaces an older helper: its shutdown request, a wait for
/// the retired transfer and management ports to close, the stamped helper to :9021 (the trust slot and launch token mean no
/// pairing code), a wait for :9120. Replies `{"state","replaced"}`. Errors carry a stable token at
/// the start of `error`: `legacy_helper_wedged` (409: the old helper did not exit; offer the
/// console restart), `helper_not_running` (409: nothing to replace). Anything else is a 502 with
/// the send failure. A console that already runs AVA1 answers `replaced:false` and is not touched.
async fn ps5_helper_replace(Json(q): Json<HostQuery>) -> impl IntoResponse {
    let host = legacy_guard::key(&q.host);
    let r =
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, (StatusCode, String)> {
            let ports = legacy_helper::Ports::default();
            match legacy_helper::state(&host, ports) {
                legacy_helper::AVA1 => {
                    Ok(serde_json::json!({ "state": "ava1", "replaced": false }))
                }
                legacy_helper::HELPER_OLD => {
                    // One replace per console at a time, and 60 s between restarts.
                    // Read the bundle BEFORE claiming the console: a bundle that cannot be read
                    // must not burn the 60 s cooldown for a replace that never started.
                    let elf = bundled_payload::image_bytes(bundled_payload::Image::Payload)
                        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
                    let _permit = legacy_guard::global()
                        .begin(&host, std::time::Instant::now())
                        .map_err(|t| (StatusCode::CONFLICT, legacy_guard::message(t)))?;
                    let stamped = ava1_api::stamped_helper(&elf);
                    let h2 = host.clone();
                    legacy_helper::replace(
                        &host,
                        ports,
                        legacy_helper::WAIT_CLOSE,
                        legacy_helper::WAIT_AVA1,
                        // Companion: replace() already shut the old helper down; the sender's own
                        // eviction would only repeat the request.
                        move || {
                            ps5upload_core::payload_lifecycle::send_elf_to_loader(
                                &h2,
                                ps5upload_core::payload_lifecycle::PS5_LOADER_PORT,
                                &stamped,
                                ps5upload_core::payload_lifecycle::LoaderImage::Companion,
                            )
                            .map(|_| ())
                        },
                    )
                    .map(|r| {
                        serde_json::json!({
                            "state": if r.ava1_up { "ava1" } else { "starting" },
                            "replaced": true,
                        })
                    })
                    .map_err(|e| match e {
                        legacy_helper::ReplaceError::Wedged => {
                            (StatusCode::CONFLICT, e.to_string())
                        }
                        legacy_helper::ReplaceError::Send(m) => (StatusCode::BAD_GATEWAY, m),
                    })
                }
                other => Err((StatusCode::CONFLICT, legacy_guard::not_replaceable(other))),
            }
        })
        .await;
    match r {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        Ok(Err((code, msg))) => json_err(code, msg).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct KlogQuery {
    addr: Option<String>,
    max_bytes: Option<u32>,
}

/// GET /api/ps5/klog — read buffered kernel log.
///
/// Needed by the browser build's bug report: without it a web-generated
/// report carries no kernel log at all, and the collector treats the missing
/// command as "nothing to collect" rather than an error.
async fn ps5_klog(State(state): State<AppState>, Query(q): Query<KlogQuery>) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    // Same 256 KiB ceiling the desktop command uses.
    let cap = q.max_bytes.unwrap_or(64 * 1024).min(256 * 1024);
    let r = tokio::task::spawn_blocking(move || klog_read(&addr, cap))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(text) => (StatusCode::OK, Json(serde_json::json!({ "text": text }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/cache/artwork — how much disk the artwork cache is using.
///
/// Exposed because cached artwork outlives the game it came from: the
/// files stay after a title is uninstalled. Small, and on the user's own
/// machine, but it is data that persists past its subject, so it should be
/// visible and removable rather than silently accumulating.
async fn cache_artwork_stats() -> impl IntoResponse {
    let (files, bytes) = icon_cache::stats();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "files": files, "bytes": bytes })),
    )
}

/// DELETE /api/cache/artwork — remove every cached image.
///
/// Safe at any time: the cache is an optimisation, so the next render
/// simply reads from the console again.
async fn cache_artwork_clear() -> impl IntoResponse {
    let freed = icon_cache::clear();
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "freed_bytes": freed })),
    )
}

#[derive(Deserialize)]
struct AppInfoQueryParams {
    addr: Option<String>,
    title_id: String,
    /// Comma-separated key filter; omit for every key.
    keys: Option<String>,
}

/// GET /api/ps5/appinfo — per-title rows from appinfo.db.
///
/// appinfo.db is the database behind Settings → Storage, and it can
/// disagree with app.db and with what is actually on disk. Reading it is
/// how you tell those three apart when a title misbehaves.
async fn ps5_appinfo_query(
    State(state): State<AppState>,
    Query(q): Query<AppInfoQueryParams>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let title_id = q.title_id;
    let keys = q.keys;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::appinfo_query(&addr, &title_id, keys.as_deref())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct AppInfoSetReq {
    addr: Option<String>,
    title_id: String,
    key: String,
    val: String,
    /// Where to put the pre-change snapshot of both content databases.
    /// Omit to let the engine choose a directory under its data dir.
    backup_dir: Option<String>,
}

/// POST /api/ps5/appinfo/set — change one appinfo.db value.
///
/// The only route in the engine that writes to a console system database.
/// Both content databases are snapshotted first and the response says
/// where, because the failure mode here is a title whose Settings entry
/// stops rendering and there is otherwise nothing to restore from. If the
/// snapshot cannot be taken the write does not happen at all — an
/// unrecoverable edit is worse than a refused one.
///
/// The payload holds the rest of the preconditions (title not running, the
/// row must already exist, exactly one row changed or roll back).
async fn ps5_appinfo_set(
    State(state): State<AppState>,
    Json(req): Json<AppInfoSetReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let stamp = now_ms() / 1000;
    let backup_dir = match req.backup_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::temp_dir().join(format!("ps5upload-appinfo-{stamp}")),
    };

    let addr2 = addr.clone();
    let backup = tokio::task::spawn_blocking(move || {
        ps5upload_core::fs_ops::backup_content_databases(&addr2, &backup_dir)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);

    let saved = match backup {
        Ok(paths) => paths,
        Err(e) => {
            return json_err(
                StatusCode::BAD_GATEWAY,
                format!("refusing to edit appinfo.db: could not snapshot it first: {e:#}"),
            )
            .into_response();
        }
    };

    let (title_id, key, val) = (req.title_id, req.key, req.val);
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::appinfo_set(&addr, &title_id, &key, &val)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);

    let backup_paths: Vec<String> = saved.iter().map(|p| p.display().to_string()).collect();
    match r {
        Ok(v) if v.ok => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "backup": backup_paths })),
        )
            .into_response(),
        Ok(v) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "error": v.err.unwrap_or_else(|| "appinfo.db update refused".into()),
                "backup": backup_paths,
            })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/net/interfaces — enumerate console network interfaces.
///
/// Same reason as `ps5_klog`: the browser bug report was silently missing it.
async fn ps5_net_interfaces(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || net_interfaces(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct AppLifecycleReq {
    addr: Option<String>,
    action: String,
    app_id: u32,
}

/// POST /api/ps5/app/lifecycle — suspend / resume / kill a running app.
///
/// Added because the BROWSER build had no way to close a game at all: the
/// desktop app reaches this through a Tauri command that calls core
/// directly, with no HTTP route behind it, so "Close game" simply did not
/// exist on the web UI. Same missing-half class as fs_read_preview.
async fn ps5_app_lifecycle(
    State(state): State<AppState>,
    Json(req): Json<AppLifecycleReq>,
) -> impl IntoResponse {
    let action = match req.action.as_str() {
        "suspend" => AppAction::Suspend,
        "resume" => AppAction::Resume,
        "kill" => AppAction::Kill,
        "list" => AppAction::List,
        other => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("unknown app lifecycle action: {other}"),
            )
            .into_response()
        }
    };
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let app_id = req.app_id;
    let r = tokio::task::spawn_blocking(move || app_lifecycle(&addr, action, app_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct FsReadPreviewReq {
    addr: Option<String>,
    path: String,
    max_bytes: Option<u64>,
    /// Where to start reading (default 0). A bug report reads the TAIL of a log that has
    /// outgrown the 256 KiB preview cap: the newest lines are the ones that explain a crash.
    #[serde(default)]
    offset: Option<u64>,
}

/// POST /api/ps5/fs/read-preview — read up to `max_bytes` of a console file,
/// returned base64.
///
/// Exists so the BROWSER build can collect the helper's on-console logs for a
/// bug report. The desktop app reads these through a Tauri command that calls
/// core directly, with no HTTP route — which meant web users' reports silently
/// contained no console logs at all, because the per-file fetch failures are
/// (correctly) swallowed as "file absent" by the collector. Same missing-half
/// bug as fs_write_bytes.
async fn ps5_fs_read_preview(
    State(state): State<AppState>,
    Json(req): Json<FsReadPreviewReq>,
) -> impl IntoResponse {
    // Same 256 KiB ceiling the desktop command enforces, so a caller cannot
    // pull an unbounded file through the engine.
    let cap = req.max_bytes.unwrap_or(256 * 1024).min(256 * 1024);
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let path = req.path.clone();
    let offset = req.offset.unwrap_or(0);
    let r = tokio::task::spawn_blocking(move || {
        fs_read_with_timeout(
            &addr,
            &path,
            offset,
            cap,
            Some(Duration::from_secs(10)),
            false,
        )
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(bytes) => {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            (
                StatusCode::OK,
                Json(serde_json::json!({ "size": bytes.len(), "base64": b64 })),
            )
                .into_response()
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct FakelibManifestReq {
    /// Host directory holding `manifest.json` and `profiles/<titleId>/`.
    path: String,
}

/// GET /api/fakelibs/manifest — read the local fakelib corpus manifest.
///
/// The corpus lives on the USER'S machine, not the console: profiles are
/// gathered from their own games by the app's Backport scan. The desktop
/// build could read it through Tauri, but the browser build cannot touch the
/// filesystem at all, so it goes through the engine — the same missing-half
/// problem as fs_read_preview and fs_write_bytes.
///
/// Read-only, and confined to the single file the caller names a directory
/// for: a caller cannot walk it into an arbitrary read, because only
/// `<path>/manifest.json` is ever opened.
async fn fakelibs_manifest(
    axum::extract::Query(req): axum::extract::Query<FakelibManifestReq>,
) -> impl IntoResponse {
    let manifest = std::path::Path::new(&req.path).join("manifest.json");
    match tokio::task::spawn_blocking(move || std::fs::read_to_string(&manifest)).await {
        Ok(Ok(text)) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(json) => (StatusCode::OK, Json(json)).into_response(),
            Err(e) => json_err(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("manifest.json is not valid JSON: {e}"),
            )
            .into_response(),
        },
        Ok(Err(e)) => json_err(
            StatusCode::NOT_FOUND,
            format!("no fakelib corpus at {}: {e}", req.path),
        )
        .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// GET /api/ps5/focus — which app currently owns the screen.
///
/// Read-only and cheap enough to poll at 1 Hz. The payload answers via
/// dlsym'd `sceSystemServiceGetAppIdOfBigApp` and never ptraces ShellUI.
async fn ps5_focus(State(state): State<AppState>, Query(q): Query<AddrQuery>) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<FocusProbe, anyhow::Error> =
        tokio::task::spawn_blocking(move || focus_probe(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct ProcessKillReq {
    addr: Option<String>,
    pid: i32,
}

/// POST /api/ps5/process/kill — SIGKILL a pid. Body: `{ addr, pid }`.
async fn ps5_process_kill(
    State(state): State<AppState>,
    Json(req): Json<ProcessKillReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let pid = req.pid;
    let r: Result<ProcessKillAck, anyhow::Error> =
        tokio::task::spawn_blocking(move || process_kill(&addr, pid))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── Power control + telemetry ───────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct PowerControlReq {
    addr: Option<String>,
    /// One of "reboot" | "shutdown" | "standby" | "tick".
    action: String,
}

/// POST /api/ps5/power/control — Body: `{ addr, action }`.
async fn ps5_power_control(
    State(state): State<AppState>,
    Json(req): Json<PowerControlReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let action = match req.action.as_str() {
        "reboot" => PowerAction::Reboot,
        "shutdown" => PowerAction::Shutdown,
        "standby" => PowerAction::Standby,
        "tick" => PowerAction::Tick,
        other => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("unknown power action: {other}"),
            )
            .into_response()
        }
    };
    let r: Result<SystemControlAck, anyhow::Error> =
        tokio::task::spawn_blocking(move || system_control(&addr, action))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/power/telemetry — lifetime ICC telemetry (operating
/// seconds, boot cycles, thermal alerts, power-up cause).
#[derive(Debug, serde::Deserialize)]
struct PowerWakeReq {
    /// The console's address. DDP is spoken to the console directly.
    host: String,
    /// `user-credential` captured from the PS Remote Play app. Without it the
    /// console silently ignores the request.
    #[serde(default)]
    credential: String,
}

/// POST /api/ps5/power/wake — wake a console in standby over Sony's DDP.
///
/// Not Wake-on-LAN: a PS5 does not wake from a magic packet (measured — nine
/// datagrams across every plausible port and address did nothing to a sleeping
/// console). This speaks the protocol the Remote Play app uses.
///
/// Fire-and-forget: the console never acknowledges a WAKEUP, so a 200 here
/// means the datagram was sent and nothing more. It also requires "Enable
/// Remote Play" on the console; with that off nothing is listening at all.
#[derive(Debug, serde::Deserialize)]
struct PowerWakeLoginReq {
    host: String,
    #[serde(default)]
    credential: String,
    /// Both hex, harvested from a pairing. Required — signing in needs the
    /// full session keys, not just the wake credential.
    regist_key: String,
    rp_key: String,
}

/// POST /api/ps5/power/wake-login — wake the console AND sign its user in.
///
/// A bare wake lands at user-select; this follows the wake with a Remote
/// Play control session so the console comes up on the user's home screen,
/// the way the official app does. Long-running: it waits for the console to
/// boot before signing in.
async fn ps5_power_wake_login(Json(req): Json<PowerWakeLoginReq>) -> impl IntoResponse {
    let host = req.host.clone();
    let cred = req.credential.clone();
    let creds =
        match ps5upload_core::rp_session::SessionCreds::from_hex(&req.regist_key, &req.rp_key) {
            Ok(c) => c,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        };
    let regist_key = req.regist_key.clone();
    let r = tokio::task::spawn_blocking(move || {
        // The wake credential is the registration key as a number, so derive
        // it from the regist key when the client did not send one — a console
        // with sign-in keys can then always be woken, not just signed in.
        let credential = if cred.trim().is_empty() {
            ps5upload_core::rp_regist::credential_from_regist_key(&regist_key)
                .map(|n| n.to_string())
                .unwrap_or_default()
        } else {
            cred
        };
        // Wake first (harmless if already awake), then wait for the session
        // port and sign in.
        if !credential.trim().is_empty() {
            ps5upload_core::ddp::wake(&host, &credential)?;
        }
        ps5upload_core::rp_session::login_session_when_ready(
            &host,
            &creds,
            std::time::Duration::from_secs(
                std::env::var("PS5UPLOAD_SIGNIN_BUDGET_S")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(90),
            ),
        )
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "signed_in": true })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_power_wake(Json(req): Json<PowerWakeReq>) -> impl IntoResponse {
    let host = req.host.clone();
    let cred = req.credential.clone();
    match tokio::task::spawn_blocking(move || ps5upload_core::ddp::wake(&host, &cred)).await {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "sent": true })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct PowerPairReq {
    /// The payload's management address. Optional — falls back to the
    /// engine's configured console, like every other payload call.
    addr: Option<String>,
    /// The console's address. Registration and wake use their own ports,
    /// so this is the bare host, not the management address.
    host: String,
}

/// POST /api/ps5/power/pair — pair with a console so it can be woken.
///
/// One call, nothing asked of the user: the payload supplies the PSN
/// account id and mints the PIN, and the engine runs the registration
/// handshake. Returns the wake credential for the client to store.
///
/// The console has to be awake with the payload running. That is not a
/// limitation worth working around — pairing happens once, and what it
/// buys is the ability to wake the console later, when it is not.
async fn ps5_power_pair(
    State(state): State<AppState>,
    Json(req): Json<PowerPairReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let host = req.host.clone();
    crate::log_info!("power_pair: addr={addr} host={host}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::rp_regist::pair_with_console(&addr, &host)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(p) => (StatusCode::OK, Json(p)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct DdpStatusQuery {
    host: String,
}

/// GET /api/ps5/power/ddp-status — is the console awake, in standby, or gone?
///
/// Needs no credential and no payload: it answers whether the console is
/// reachable at all, which is exactly the question the app cannot otherwise
/// tell apart from "the helper is not running".
async fn ps5_power_ddp_status(Query(q): Query<DdpStatusQuery>) -> impl IntoResponse {
    let host = q.host.clone();
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::ddp::probe(&host, std::time::Duration::from_secs(3))
    })
    .await;
    match r {
        Ok(Ok(status)) => (StatusCode::OK, Json(status)).into_response(),
        // Not an error the user can act on beyond the hint in the message:
        // a console that is off, or has Remote Play disabled, is silent.
        Ok(Err(e)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "code": 0, "status_text": "", "error": format!("{e:#}") })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

async fn ps5_power_telemetry(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<PowerTelemetry, anyhow::Error> =
        tokio::task::spawn_blocking(move || power_telemetry(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── User accounts ────────────────────────────────────────────────────────

/// GET /api/ps5/users/list — enumerate logged-in user accounts.
async fn ps5_users_list(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<UserList, anyhow::Error> = tokio::task::spawn_blocking(move || user_list(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── Saves / screenshots / videos ────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct SavesListQuery {
    addr: Option<String>,
    #[serde(default)]
    user_id: Option<i32>,
}

/// GET /api/ps5/saves/list?addr=&user_id= — user_id=0 (or absent) lists
/// every user's saves.
async fn ps5_saves_list(
    State(state): State<AppState>,
    Query(q): Query<SavesListQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let uid = q.user_id.unwrap_or(0);
    let r: Result<SaveList, anyhow::Error> =
        tokio::task::spawn_blocking(move || list_saves(&addr, uid))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/screenshots/list?addr=
async fn ps5_screenshots_list(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ScreenshotList, anyhow::Error> =
        tokio::task::spawn_blocking(move || list_screenshots(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/videos/list?addr= — same `{path,size,mtime}` shape as
/// screenshots (payload walks `/user/av_contents/video` instead).
async fn ps5_videos_list(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<ScreenshotList, anyhow::Error> =
        tokio::task::spawn_blocking(move || list_videos(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── ShadowMount+ status ──────────────────────────────────────────────────

/// GET /api/ps5/smp/status?addr= — read-only ShadowMount+ snapshot
/// (installed/running + config/autotune/debug.log + mounted images).
async fn ps5_smp_status(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r: Result<SmpStatus, anyhow::Error> =
        tokio::task::spawn_blocking(move || smp_collect_status(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/smp/checkout — what, if anything, is checked out for editing.
///
/// Returns `{"checkout": null}` when nothing is. The renderer polls this on
/// connect so an edit session interrupted by a crash, a reboot, or just
/// closing the window can be finished rather than silently stranding the
/// image outside ShadowMount+'s scan folders.
async fn ps5_smp_checkout_status(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::smp_checkout::read_state(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(serde_json::json!({ "checkout": v }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct SmpCheckoutBeginReq {
    addr: Option<String>,
    image_path: String,
    mount_point: String,
    #[serde(default)]
    title_id: String,
}

/// POST /api/ps5/smp/checkout/begin — take an image out of ShadowMount+'s
/// scan folders and mount it read-write so its contents can be edited.
///
/// Slow by design: it waits for ShadowMount+'s scan sweep (15 s by default)
/// to notice the source has gone and release its read-only mount. See
/// `ps5upload_core::smp_checkout` for why the image has to be moved rather
/// than simply unmounted.
async fn ps5_smp_checkout_begin(
    State(state): State<AppState>,
    Json(req): Json<SmpCheckoutBeginReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::smp_checkout::begin(&addr, &req.image_path, &req.mount_point, &req.title_id)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok((checkout, mount)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "checkout": checkout, "mount": mount })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/smp/checkout/finish — unmount the edited image, put it back
/// where ShadowMount+ will find it, and clear the journal.
async fn ps5_smp_checkout_finish(
    State(state): State<AppState>,
    Json(q): Json<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::smp_checkout::finish(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(serde_json::json!({ "checkout": v }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_smp_image_rw_status(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::smp_image_rw::read_state(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(serde_json::json!({ "session": v }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct SmpImageRwBeginReq {
    addr: Option<String>,
    title_id: String,
}

async fn ps5_smp_image_rw_begin(
    State(state): State<AppState>,
    Json(req): Json<SmpImageRwBeginReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::smp_image_rw::begin(&addr, &req.title_id)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_smp_image_rw_finish(
    State(state): State<AppState>,
    Json(q): Json<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::smp_image_rw::finish(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct FsWriteBytesReq {
    addr: Option<String>,
    path: String,
    /// Base64 of the file contents. Base64 because this is a JSON body and the
    /// payload's FS_WRITE_BYTES frame takes base64 anyway.
    bytes_b64: String,
    /// Refuse if the path already exists. Default false (overwrite).
    #[serde(default)]
    create_only: bool,
}

/// POST /api/ps5/fs/write-bytes — atomic small-file write (≤256 KB).
///
/// Exists so the engine's browser UI can do what the desktop app already
/// does through the `fs_write_bytes_run` Tauri command. Without it, anything
/// that writes a small config file to the console — notably handing a game to
/// ShadowMount+ by appending to its `manual.lst` — simply failed in the web
/// UI.
///
/// This grants the browser no capability the engine did not already expose:
/// `/api/transfer/file` writes arbitrary bytes to an arbitrary console path,
/// and fs/mkdir, fs/move, fs/copy and fs/delete are all already routed. The
/// payload applies the same `is_path_allowed` check regardless of caller, and
/// the loopback + cross-site guards cover this route like every other one.
async fn ps5_fs_write_bytes(
    State(state): State<AppState>,
    Json(req): Json<FsWriteBytesReq>,
) -> impl IntoResponse {
    use base64::Engine as _;
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let raw = match base64::engine::general_purpose::STANDARD.decode(req.bytes_b64.as_bytes()) {
        Ok(v) => v,
        Err(e) => {
            return json_err(StatusCode::BAD_REQUEST, format!("base64 decode: {e}")).into_response()
        }
    };
    let path = req.path;
    let create_only = req.create_only;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::fs_write_bytes(&addr, &path, &raw, create_only)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|inner| inner);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct PeripheralReq {
    addr: Option<String>,
    action: ps5upload_core::diagnostics::PeripheralAction,
    /// The USB port, beep pattern or brightness level the action takes; 0 otherwise.
    #[serde(default)]
    port: i32,
}

/// POST /api/ps5/peripheral
/// Body: `{ "addr": "IP", "action": "beep", "port": 1 }`
///
/// The disc drive, the USB ports, the beeper and the front light. Replies with the
/// helper's own ack (`ok`, `action`, `code`); a transport failure is a 502.
async fn ps5_peripheral(
    State(state): State<AppState>,
    Json(q): Json<PeripheralReq>,
) -> impl IntoResponse {
    let addr = q.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let (action, port) = (q.action, q.port);
    match tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::peripheral_control(&addr, action, port)
    })
    .await
    {
        Ok(Ok(ack)) => Json(serde_json::to_value(ack).unwrap_or_default()).into_response(),
        // The helper answered ok=false: that is an answer, not a transport failure.
        Ok(Err(e)) if format!("{e:#}").contains("PERIPHERAL_CONTROL failed") => {
            Json(serde_json::json!({ "ok": false, "err": format!("{e:#}") })).into_response()
        }
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct FanThresholdReq {
    addr: Option<String>,
    threshold_c: u8,
    /// Optional fan reapply interval in seconds (1–300). When present,
    /// the payload updates the watcher thread's tick interval AND
    /// persists it to fan_reapply.conf. Absent = leave interval unchanged.
    reapply_sec: Option<u32>,
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let threshold = q.threshold_c;
    let reapply = q.reapply_sec;
    crate::log_info!(
        "hw_set_fan_threshold: addr={addr} threshold_c={threshold} reapply_sec={reapply:?}"
    );
    match tokio::task::spawn_blocking(move || hw_set_fan_threshold_ex(&addr, threshold, reapply))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_info!(
                "hw_set_fan_threshold ok: threshold_c={threshold} reapply_sec={reapply:?}"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "threshold_c": threshold, "reapply_sec": reapply })),
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
/// Reads `sce_sys/param.json` (PS5) or `sce_sys/param.sfo` (PS4 / legacy)
/// via FS_READ on the PS5 and returns the localized title, title-id,
/// content-id, and version fields. Used by the Library screen to upgrade
/// plain folder names into "My Title · PPSA00000" style labels without
/// needing a local copy of the game. Failures (no metadata, bad format,
/// path denied) return 404 so the UI can fall back to the folder name.
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
        // If param.json didn't yield a title, try param.sfo (PS4 games and
        // legacy PS5 homebrew ship SFO, not JSON). Falls through cleanly
        // if the SFO file also doesn't exist.
        let (title, title_id, content_id, content_version, application_category_type) =
            if title.is_none() {
                let sfo_path = format!("{}/sce_sys/param.sfo", path.trim_end_matches('/'));
                if let Ok(sfo_bytes) = fs_read(&addr, &sfo_path, 0, 256 * 1024) {
                    if let Ok(r) = parse_param_sfo_bytes(&sfo_bytes) {
                        (
                            r.title,
                            r.title_id,
                            r.content_id,
                            r.content_version,
                            None, // SFO doesn't expose applicationCategoryType
                        )
                    } else {
                        (
                            title,
                            title_id,
                            content_id,
                            content_version,
                            application_category_type,
                        )
                    }
                } else {
                    (
                        title,
                        title_id,
                        content_id,
                        content_version,
                        application_category_type,
                    )
                }
            } else {
                (
                    title,
                    title_id,
                    content_id,
                    content_version,
                    application_category_type,
                )
            };
        // icon0.png probe — `fs.stat` confirms a non-empty regular file without pulling
        // the image. Errors (path denied, not found) treated as "no icon".
        let icon_path = format!("{}/sce_sys/icon0.png", path.trim_end_matches('/'));
        let has_icon = ps5upload_core::fs_ops::fs_stat(&addr, &icon_path)
            .map(|s| s.kind == "file" && s.size > 0)
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

/// Serve artwork, going to the console only when the cache cannot answer.
///
/// Shared by the two icon routes because the caching rules — revalidation,
/// negative caching, per-console keying — should not be able to drift
/// apart between them.
///
/// `identity` is what distinguishes one image from another *within* a
/// console: a title id, or a folder path.
async fn serve_cached_icon(
    addr: String,
    kind: &'static str,
    identity: String,
    // Tried in order; the first that has bytes wins.
    remote_paths: Vec<String>,
    if_none_match: Option<String>,
) -> axum::response::Response {
    // A revalidation we can answer from cache costs no console round-trip
    // and no body — this is the cheap path once max-age lapses.
    match icon_cache::get(&addr, kind, &identity) {
        icon_cache::Cached::Hit { bytes, etag } => {
            if icon_cache::etag_matches(if_none_match.as_deref(), &etag) {
                return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag.as_str())]).into_response();
            }
            return icon_response(bytes, &etag);
        }
        // The console told us there is no artwork here recently enough to
        // believe. Answering from that saves a round-trip per render for
        // every title that legitimately has none.
        icon_cache::Cached::KnownMissing => {
            return (StatusCode::NOT_FOUND, "no icon").into_response();
        }
        icon_cache::Cached::Unknown => {}
    }

    let read_addr = addr.clone();
    // Ok(Some) = found; Ok(None) = every path cleanly absent; Err = at least
    // one path failed for another reason (the console may just be busy).
    let result: Result<Option<Vec<u8>>, anyhow::Error> = tokio::task::spawn_blocking(move || {
        let mut transport_err = None;
        for p in &remote_paths {
            match fs_read(&read_addr, p, 0, 2 * 1024 * 1024) {
                Ok(bytes) if !bytes.is_empty() => return Ok(Some(bytes)),
                Ok(_) => {}
                Err(e) if icon_cache::is_console_said_no(&e) => {}
                Err(e) => transport_err = Some(e),
            }
        }
        match transport_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|inner| inner);

    match result {
        Ok(Some(bytes)) => {
            icon_cache::put(&addr, kind, &identity, &bytes);
            let etag = icon_cache::etag_for(&bytes);
            icon_response(bytes, &etag)
        }
        // Only a clean "there is nothing there" is remembered. A transport
        // error must NOT be: a console that was briefly unreachable would
        // otherwise be recorded as having no artwork at all, and every
        // cover would vanish for the length of the negative TTL.
        Ok(None) => {
            icon_cache::put_missing(&addr, kind, &identity);
            (StatusCode::NOT_FOUND, "no icon").into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "no icon").into_response(),
    }
}

fn icon_response(bytes: Vec<u8>, etag: &str) -> axum::response::Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            // Icons rarely change once uploaded — a 5-minute cache keeps
            // scrolling smooth across refreshes without sticking forever
            // on a stale file. Past it the ETag makes revalidation cheap.
            (header::CACHE_CONTROL, "private, max-age=300"),
            (header::ETAG, etag),
        ],
        bytes,
    )
        .into_response()
}

/// GET /api/ps5/game-icon?addr=IP:MGMT_PORT&path=/mnt/ext1/homebrew/FooBar
///
/// Streams the folder's `sce_sys/icon0.png` back as `image/png`. The
/// payload caps FS_READ at 2 MiB, comfortably above the largest icon0
/// we've observed (~700 KiB). Failures return 404 so `<img onerror>`
/// handlers can fall back to a placeholder without parsing a JSON body.
async fn ps5_game_icon(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<GameMetaQuery>,
) -> impl IntoResponse {
    if let Err((code, msg)) = validate_meta_path(&q.path) {
        return (code, msg).into_response();
    }
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let path = q.path.trim_end_matches('/').to_string();
    let icon_path = format!("{path}/sce_sys/icon0.png");
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    serve_cached_icon(addr, "game", path, vec![icon_path], inm).await
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
// Why filesystem enumeration and not Sony's app.db: /user/appmeta is where
// the cover art (icon0.png) lives anyway, served by /api/ps5/app-icon below,
// and a directory cannot be locked out from under us the way a database the
// shell holds open can. (The payload *can* read app.db now — it links its
// own SQLite rather than looking for Sony's, which never existed — but the
// filesystem stays the primary source here for that reason.)

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

/// Best-effort title name from `/user/appmeta/<title_id>/`. Tries PS5
/// `param.json` first, then falls back to PS4 `param.sfo`, then queries
/// `app.db` (raw file scan when sqlite is unavailable). Returns None on
/// any failure (no metadata file — common for system apps — bad JSON,
/// bad SFO, path denied); the caller falls back to the bare title_id.
fn appmeta_title_name(addr: &str, title_id: &str) -> Option<String> {
    // /user/appmeta/<id> first; then the app's own sce_sys, which is all a
    // title installed by a homebrew installer has on FW 13.60.
    for dir in [
        format!("/user/appmeta/{title_id}"),
        format!("/user/app/{title_id}/sce_sys"),
    ] {
        // PS5 param.json.
        if let Ok(bytes) = fs_read(addr, &format!("{dir}/param.json"), 0, 256 * 1024) {
            if let Ok(meta) = parse_param_json_bytes(&bytes) {
                if let Some(t) = meta.title.filter(|t| !t.trim().is_empty()) {
                    return Some(t);
                }
            }
        }
        // PS4 / legacy param.sfo.
        if let Ok(bytes) = fs_read(addr, &format!("{dir}/param.sfo"), 0, 256 * 1024) {
            if let Ok(meta) = parse_param_sfo_bytes(&bytes) {
                if let Some(t) = meta.title.filter(|t| !t.trim().is_empty()) {
                    return Some(t);
                }
            }
        }
    }
    // Third fallback: query app.db via the payload's AppDbQuery RPC.
    // On FW where sqlite symbols are unavailable via dlsym (5.10, 9.60),
    // the payload falls back to a raw file scan that reads app.db
    // directly and extracts title names by scanning SQLite B-tree pages.
    // This is the only way to resolve PS4 BC titles (CUSA IDs) whose
    // metadata lives solely in app.db — their appmeta dirs have no
    // param.sfo or param.json.
    if let Ok(appdb) = appdb_query(addr) {
        for app in &appdb.apps {
            if app.title_id == title_id {
                let name = app.name.trim();
                if !name.is_empty() && name != title_id {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// Titles app.db knows are installed that the /user/appmeta scan missed,
/// with app.db's name when it has a usable one.
///
/// Kept only when the app folder exists (`app_dirs`, from /user/app and the
/// extended-storage equivalents): app.db also carries shell entries (NPXS)
/// and preinstalled tiles whose app was never downloaded.
fn appdb_installed_additions(
    rows: &[(String, String)],
    app_dirs: &std::collections::HashSet<String>,
    seen: &std::collections::HashSet<String>,
) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for (id, name) in rows {
        let tid = id.trim();
        if !looks_like_title_id(tid)
            || tid.starts_with("NPXS")
            || seen.contains(tid)
            || !app_dirs.contains(tid)
            || !taken.insert(tid.to_string())
        {
            continue;
        }
        let n = name.trim();
        let name = (!n.is_empty() && n != tid).then(|| n.to_string());
        out.push((tid.to_string(), name));
    }
    out
}

/// GET /api/ps5/apps/installed?addr=IP:MGMT_PORT
async fn ps5_apps_installed(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
            // app.db is the console's own record of what is installed. On FW
            // 13.60 titles put there by homebrew installers (Payload Manager,
            // Shadow Mount+, …) have no /user/appmeta/<id> folder, so the
            // listing above missed every one of them and the Games screen
            // was empty on both test consoles (2026-09-29).
            //
            // app.db alone over-counts, though: it also holds the console's
            // shell entries (NPXS: All Apps, Welcome, Disc Player…) and
            // preinstalled "push resource" tiles like ASTRO's PLAYROOM whose
            // /user/app/<id> was never downloaded. So a title from app.db
            // counts only when its app folder really exists, on internal or
            // extended storage. Best-effort: if app.db or the folders can't
            // be read, the appmeta scan alone is what we had before.
            let mut app_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
            for root in ["/user/app", "/mnt/ext0/user/app", "/mnt/ext1/user/app"] {
                if let Ok(l) = list_dir(
                    &addr,
                    root,
                    ListDirOptions {
                        offset: 0,
                        limit: 512,
                    },
                ) {
                    app_dirs.extend(l.entries.into_iter().map(|e| e.name));
                }
            }
            if !app_dirs.is_empty() {
                match appdb_query(&addr) {
                    Ok(appdb) => {
                        let seen: std::collections::HashSet<String> =
                            titles.iter().map(|t| t.title_id.clone()).collect();
                        let rows: Vec<(String, String)> = appdb
                            .apps
                            .into_iter()
                            .map(|a| (a.title_id, a.name))
                            .collect();
                        for (tid, db_name) in appdb_installed_additions(&rows, &app_dirs, &seen) {
                            let name = match db_name {
                                Some(n) => n,
                                None => {
                                    appmeta_title_name(&addr, &tid).unwrap_or_else(|| tid.clone())
                                }
                            };
                            let (origin, image_backed, source) = match reg_map.get(&tid) {
                                Some(reg) => ("registered", reg.image_backed, reg.src.clone()),
                                None => ("pkg", false, String::new()),
                            };
                            titles.push(InstalledApp {
                                title_id: tid,
                                title_name: name,
                                origin: origin.to_string(),
                                image_backed,
                                source,
                                system: false,
                            });
                        }
                    }
                    Err(e) => crate::log_warn!("apps/installed: app.db query failed: {e:#}"),
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
    headers: HeaderMap,
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    // /user/appmeta/<id> is the usual home, but on FW 13.60 titles
    // installed by homebrew installers have none — only the app's own
    // /user/app/<id>/sce_sys (measured on both consoles, 2026-09-29).
    let icon_paths = vec![
        format!("/user/appmeta/{}/icon0.png", q.title_id),
        format!("/user/app/{}/sce_sys/icon0.png", q.title_id),
        format!("/mnt/ext0/user/app/{}/sce_sys/icon0.png", q.title_id),
    ];
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    serve_cached_icon(addr, "app", q.title_id, icon_paths, inm).await
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
    /// ContentID from the fast outer-header read (empty for FIH until the
    /// embedded CNT is lazily read, or for otherwise unreadable headers).
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

/// Scan connected external/USB drives for installable `.pkg`/`.fpkg` files.
///
/// Walks every real `/mnt/usb*` and `/mnt/ext*` mount depth-first, reading
/// each package's header for content_id + platform. Bounded on depth,
/// directories visited, and packages returned so a multi-thousand-file game
/// drive can't wedge the scan. Errors on individual dirs/files are skipped
/// (best-effort) rather than failing the whole scan.
/// A drive's top-level `ps5upload/` folder holds the package library staged
/// there by the app; those packages are already in the library, so the
/// external scan leaves them out instead of listing them twice.
///
/// An extended-storage drive (`/mnt/ext*`) also keeps the console's installed
/// games under its top-level `user/` (app/patch/addcont) — `app.pkg` and
/// `patch.pkg` there are installed content, not packages to install. Listing
/// them offered to "install" a game's own update over itself (a user report:
/// DOOM's /mnt/ext0/user/patch/CUSA02092/patch.pkg).
fn skip_in_external_scan(drive: &str, depth: u32, name: &str) -> bool {
    depth == 0
        && (name.eq_ignore_ascii_case("ps5upload")
            || (drive.starts_with("/mnt/ext") && name.eq_ignore_ascii_case("user")))
}

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
                let lower = e.name.to_ascii_lowercase();
                let is_package = lower.ends_with(".pkg") || lower.ends_with(".fpkg");
                if e.kind == "dir" {
                    if skip_in_external_scan(&v.path, depth, &e.name) {
                        continue;
                    }
                    if depth + 1 < MAX_DEPTH {
                        stack.push((join(&dir, &e.name), depth + 1));
                    }
                } else if is_package {
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    /// Optional size from the directory listing. When present, sample the
    /// first/last package blocks and return the full artifact fingerprint too.
    #[serde(default)]
    size: Option<u64>,
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let path = q.path;
    let size = q.size.unwrap_or(0);
    let result = tokio::task::spawn_blocking(move || {
        let mut meta = ps5upload_pkg::metadata_from_reader(|off, len| {
            ps5upload_core::fs_ops::fs_read(&addr, &path, off, len).ok()
        })?;
        if size > 0 {
            meta.fingerprint = ps5upload_pkg::package_fingerprint_from_reader(size, |off, len| {
                ps5upload_core::fs_ops::fs_read(&addr, &path, off, len).ok()
            })
            .unwrap_or_default();
        }
        Some(meta)
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let result: Result<VolumeList, anyhow::Error> =
        tokio::task::spawn_blocking(move || list_volumes(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|inner| inner);
    match result {
        Ok(mut v) => {
            // What the app shows as likely to fit: on internal storage less than the free
            // figure, because the console holds back more as data is written.
            for vol in &mut v.volumes {
                vol.likely_fits_bytes = vol.likely_fits_bytes();
            }
            (StatusCode::OK, Json(v)).into_response()
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/status?addr=IP:PORT
async fn ps5_status(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    // `node.status` through the management seam: the typed AVA1 NodeStatus is rebuilt into the
    // legacy JSON (`ucred_elevated` a bool, `prior_instance` only when present). The old
    // transaction fields (runtime_port, shutdown, takeover_requested, active_transactions,
    // last_tx_seq, recovered_transactions) no longer exist; nothing reads them.
    let result = tokio::task::spawn_blocking(move || {
        let body = ps5upload_core::mgmt::call(&addr, ps5upload_core::mgmt::m::NODE_STATUS, b"")
            .map_err(|e| {
                // No AVA1 listener: tell an older helper (Update) from nothing running.
                let host = legacy_guard::key(&addr);
                anyhow::anyhow!(legacy_helper::fold_status_error(
                    format!("{e:#}"),
                    &host,
                    legacy_helper::Ports::default()
                ))
            })?;
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

/// GET /api/ps5/health/scan?addr=IP:MGMT_PORT
///
/// Runs every health check and returns the report. Slow by nature --
/// it makes several round trips to the console -- so it is a blocking
/// task, never called on the async runtime thread.
async fn health_scan_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let result = tokio::task::spawn_blocking(move || {
        ps5upload_core::health::run_health_scan(&addr, env!("CARGO_PKG_VERSION"))
    })
    .await;
    match result {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct HealthFixReq {
    addr: Option<String>,
    action: ps5upload_core::health::FixAction,
}

/// POST /api/ps5/health/fix
///
/// Applies one named repair. `action` deserializes into a closed enum,
/// so an unknown or arbitrary value is rejected by serde before any
/// console operation runs.
async fn health_fix_handler(
    State(state): State<AppState>,
    Json(req): Json<HealthFixReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let action = req.action;
    let result = tokio::task::spawn_blocking(move || {
        ps5upload_core::health::apply_fix(&addr, &action, env!("CARGO_PKG_VERSION"))
    })
    .await;
    match result {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/health/junk?addr=IP:MGMT_PORT
///
/// The exact files `CleanJunk` would delete. Shown to the user before
/// anything is removed -- a cleanup button that does not say what it
/// will destroy is not one users should trust.
async fn health_junk_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let result =
        tokio::task::spawn_blocking(move || ps5upload_core::health::preview_junk(&addr)).await;
    match result {
        Ok(items) => {
            let total: u64 = items.iter().map(|(_, s)| s).sum();
            let files: Vec<_> = items
                .into_iter()
                .map(|(path, size)| serde_json::json!({ "path": path, "size": size }))
                .collect();
            (
                StatusCode::OK,
                Json(serde_json::json!({ "files": files, "total_bytes": total })),
            )
                .into_response()
        }
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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

/// Consecutive console stalls (36 s each) a link download rides out with nothing made
/// durable in between: twelve minutes of a host delivering nothing.
const LINK_STALL_TRIES: u32 = 20;

#[derive(Debug, PartialEq, Eq)]
enum LinkStall {
    Retry,
    Done,
}

/// What a link download does with one attempt's result: a console stall is retried while
/// the host still makes progress (or has not yet used up its tries); anything else, a
/// cancel included, is the job's result.
fn link_stall_verdict<T>(
    r: &anyhow::Result<T>,
    cancelled: bool,
    progressed: bool,
    stalls: &mut u32,
) -> LinkStall {
    let stalled = r.as_ref().err().is_some_and(|e| {
        e.downcast_ref::<ps5upload_ava1::upload::UploadFailure>()
            .is_some_and(|f| f.reason == "ava1_stalled")
    });
    if !stalled || cancelled {
        return LinkStall::Done;
    }
    *stalls = if progressed { 1 } else { *stalls + 1 };
    if *stalls > LINK_STALL_TRIES {
        LinkStall::Done
    } else {
        LinkStall::Retry
    }
}

/// POST /api/transfer/file
async fn transfer_file_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferFileReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    // A caller-supplied tx_id marks a resume-capable client; the job is reopened by id.
    let caller_supplied_tx_id = req.tx_id.is_some();
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "file");
    telemetry::set_drive(job_id, &req.dest);
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
    // A `remote://` source reads through the saved server; anything else is local disk.
    let (source_fs, src_path): (
        Option<Arc<dyn ps5upload_core::source_fs::SourceFs>>,
        std::path::PathBuf,
    ) = if remote::path::is_remote(&req.src) {
        match remote::source_fs::RemoteSourceFs::for_path(&req.src).await {
            Ok((fs, p)) => (Some(fs), p),
            Err(e) => {
                return json_err(StatusCode::BAD_REQUEST, format!("cannot read source: {e}"))
                    .into_response()
            }
        }
    } else {
        (None, std::path::PathBuf::from(&req.src))
    };
    let stat_fs = source_fs.clone();
    let src_for_stat = src_path.clone();
    let total_bytes = match tokio::task::spawn_blocking(move || match stat_fs {
        Some(fs) => fs.metadata(&src_for_stat),
        None => ps5upload_core::source_fs::SourceFs::metadata(
            &ps5upload_core::source_fs::LocalFs,
            &src_for_stat,
        ),
    })
    .await
    {
        Ok(Ok(m)) if m.is_file => m.len,
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
    let src_basename = src_path
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
        dynamic_total_bytes: None,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
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
        // upload_file would leak the ticker task
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
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, bandwidth_cap);

        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
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
        cfg.source_fs = source_fs;
        crate::log_info!("transfer_file: job={job_id} protocol=ava1");
        // Resume is by job_id (the sender reopens with JobOpen); retries live in the
        // adapter's loop.
        let result = ps5upload_ava1::upload::upload_file(&cfg, tx_id, &req.dest, &src_path);
        let files_sent_count: u64 = 1;
        let mut skipped_files_count: u64 = 0;
        let mut skipped_bytes_count: u64 = 0;
        let mut files_sent_count = files_sent_count;
        // An AVA1 job reports what the console already had (skip policies, SPEC §11.4)
        // and the files actually sent; an all-skipped resume is "already up to date".
        if let Some((sf, sb, fs)) = result
            .as_ref()
            .ok()
            .and_then(|r| ava1_skip_counts(&r.commit_ack_body))
        {
            (skipped_files_count, skipped_bytes_count, files_sent_count) = (sf, sb, fs);
        }
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

    let skip_existing = match req.skip_existing.as_deref() {
        None => None,
        Some(m) => match ps5upload_ava1::upload::SkipMode::parse(m) {
            Some(m) => Some(m),
            None => {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("unknown skip_existing mode: {m}"),
                )
                .into_response();
            }
        },
    };

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "dir");
    telemetry::set_drive(job_id, &req.dest_root);
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_dir: job={job_id} addr={addr} src_dir={} dest_root={} resume={} excludes={}",
        req.src_dir,
        req.dest_root,
        caller_supplied_tx_id,
        req.excludes.len()
    );

    // Hand the client its job_id BEFORE walking the source tree.
    //
    // walk_plan does a recursive read_dir+metadata over the whole source
    // (46k-file games are routine). This handler used to await that walk and
    // only then return the id, so the client — which shows "Starting…" until
    // the id arrives — sat inert for the whole walk with no spinner, no file
    // count, and no way to cancel. On a Docker bind-mount (gRPC-FUSE metadata
    // ops are orders of magnitude slower than a local SSD) that is minutes on
    // a real game folder, and it reads as a hang: issue #275, "upload process
    // stuck at starting…".
    //
    // The reconcile route already seeds `Running` with an unknown plan and
    // fills it in from its blocking task; plain dir upload now does the same,
    // so the two folder paths behave alike. The client's Running-with-no-plan
    // interstitial already covers the gap.
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: 0, // unknown until the walk below finishes
            files: vec![],
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

    tokio::task::spawn_blocking(move || {
        // Guard installed first so a panic in the walk can't orphan the job
        // in Running forever (same rationale as the reconcile route).
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);

        // Fail fast on a missing / non-directory source. walk_plan swallows
        // read errors (returns empty), so without this a typo'd path or a
        // permissions problem would start a "Running → 0 bytes" job (or a
        // fake "Done, 0 files") instead of a clear error. This used to be a
        // 400 on the POST; now the id is already out, so it lands as a job
        // failure the client surfaces the same way.
        // A `remote://` source reads through the saved server; anything else is local disk.
        let (source_fs, src_path): (
            Option<Arc<dyn ps5upload_core::source_fs::SourceFs>>,
            std::path::PathBuf,
        ) = if remote::path::is_remote(&req.src_dir) {
            match tokio::runtime::Handle::current()
                .block_on(remote::source_fs::RemoteSourceFs::for_path(&req.src_dir))
            {
                Ok((fs, p)) => (Some(fs), p),
                Err(e) => {
                    let completed_at_ms = now_ms();
                    set_job(
                        &jobs,
                        &events_tx,
                        job_id,
                        job_failed_from_err(
                            started_at_ms,
                            completed_at_ms,
                            &anyhow::anyhow!("cannot read source: {e}"),
                        ),
                    );
                    fail_guard.mark_succeeded();
                    return;
                }
            }
        } else {
            (None, std::path::PathBuf::from(&req.src_dir))
        };
        let is_dir = match &source_fs {
            Some(fs) => fs.metadata(&src_path).map(|m| m.is_dir).unwrap_or(false),
            None => src_path.is_dir(),
        };
        if !is_dir {
            let completed_at_ms = now_ms();
            set_job(
                &jobs,
                &events_tx,
                job_id,
                job_failed_from_err(
                    started_at_ms,
                    completed_at_ms,
                    &anyhow::anyhow!(
                        "source directory not found or not a directory: {}",
                        req.src_dir
                    ),
                ),
            );
            fail_guard.mark_succeeded();
            return;
        }

        let walk_started = std::time::Instant::now();
        let planned = match &source_fs {
            Some(fs) => walk_plan_with(fs.as_ref(), &src_path, &req.excludes),
            None => Ok(walk_plan(&src_path, &req.excludes)),
        };
        let (total_bytes, files) = match planned {
            Ok(p) => p,
            Err(e) => {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    job_failed_from_err(started_at_ms, completed_at_ms, &anyhow::anyhow!(e)),
                );
                fail_guard.mark_succeeded();
                return;
            }
        };
        let files_sent_count = files.len() as u64;
        crate::log_info!(
            "transfer_dir: job={job_id} walk done in {} ms — files={} bytes={}",
            walk_started.elapsed().as_millis(),
            files_sent_count,
            total_bytes
        );
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }

        let progress = Arc::new(AtomicU64::new(0));
        let progress_files = Arc::new(AtomicU64::new(0));
        // P3 / v2.18.0 — apply-phase counters. The engine's
        // send_commit_and_expect_ack reads APPLY_PROGRESS frames from
        // the payload during the commit wait and stores into these.
        let progress_files_finalized = Arc::new(AtomicU64::new(0));
        let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
        let ctx = TickerContext {
            started_at_ms,
            total_bytes,
            dynamic_total_bytes: None,
            skipped_files: 0,
            skipped_bytes: 0,
        };
        // Now that the plan exists, publish it so the UI can switch from the
        // "preparing" interstitial to a real progress bar.
        set_job(
            &jobs,
            &events_tx,
            job_id,
            JobState::Running {
                stage: None,
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
        // See ticker stop-guard rationale at the file-upload spawn site.
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut cfg = make_transfer_config(&addr);
        cfg.source_fs = source_fs;
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        crate::log_info!("transfer_dir: job={job_id} protocol=ava1");
        if skip_existing.is_some() {
            let hashed = Arc::new(AtomicU64::new(0));
            cfg.progress_verify = Some(Arc::clone(&hashed));
            spawn_verify_stage(
                Arc::clone(&jobs),
                events_tx.clone(),
                job_id,
                hashed,
                total_bytes,
                Arc::clone(&_stop_guard.0),
            );
        }
        // Resume is by job_id (the sender reopens with JobOpen); retries live in the
        // adapter's loop.
        let result = match skip_existing {
            // The user's "skip existing" choice: the receiver compares (SPEC §11.4).
            Some(mode) => ps5upload_ava1::upload::upload_dir_skip_existing(
                &cfg,
                tx_id,
                &req.dest_root,
                &src_path,
                mode,
            ),
            None => ps5upload_ava1::upload::upload_dir(&cfg, tx_id, &req.dest_root, &src_path),
        };
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
#[derive(Deserialize)]
struct LocalPathQuery {
    path: String,
}

/// GET /api/local/path-kind — classify a path on the engine's machine.
///
/// The desktop app answers this in-process via a Tauri command. The
/// browser UI has no such thing, so the Upload screen's drag-drop router
/// had nothing to call and threw BrowserUnsupportedError (issue #262).
async fn local_path_kind_handler(Query(q): Query<LocalPathQuery>) -> impl IntoResponse {
    let kind = match std::fs::metadata(&q.path) {
        Ok(md) if md.is_dir() => "folder",
        Ok(md) if md.is_file() => "file",
        Ok(_) => "other",
        Err(_) => "missing",
    };
    (StatusCode::OK, Json(serde_json::json!({ "kind": kind }))).into_response()
}

/// GET /api/local/inspect-folder — preview a game folder on the engine's
/// machine, matching the desktop command's response shape exactly so the
/// renderer needs no branch of its own.
async fn local_inspect_folder_handler(Query(q): Query<LocalPathQuery>) -> impl IntoResponse {
    let path = q.path;
    let r = tokio::task::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        match ps5upload_core::game_meta::inspect_folder(p) {
            Ok(r) => {
                // Only worth hinting when the folder is not itself a game.
                let hint = if r.meta_source == "none" {
                    ps5upload_core::game_meta::wrapped_game_hint(p)
                } else {
                    None
                };
                serde_json::json!({ "ok": true, "result": r, "wrapped_hint": hint })
            }
            Err(e) => serde_json::json!({ "ok": false, "error": format!("{e:#}") }),
        }
    })
    .await;
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": false, "error": format!("join: {e}") })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct BpsInspectReq {
    patch_path: String,
}

#[derive(Deserialize)]
struct BpsApplyReq {
    /// The library to patch, on the engine's filesystem.
    source_path: String,
    /// The `.bps` file to apply.
    patch_path: String,
    /// Where to write the patched result.
    dest_path: String,
}

/// POST /api/bps/inspect — read a BPS patch's header without applying it.
///
/// Lets the UI show what a patch expects before anything is written,
/// which matters because these patches target one exact build of one
/// library.
async fn bps_inspect_handler(Json(req): Json<BpsInspectReq>) -> impl IntoResponse {
    let path = req.patch_path;
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let patch = std::fs::read(&path).map_err(|e| anyhow::anyhow!("read {path}: {e}"))?;
        ps5upload_core::bps::bps_info(&patch)
    })
    .await;
    match r {
        Ok(Ok(info)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "source_size": info.source_size,
                "target_size": info.target_size,
                "metadata": info.metadata,
                "source_crc": format!("{:08x}", info.source_crc),
                "target_crc": format!("{:08x}", info.target_crc),
            })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

/// POST /api/bps/apply — patch a library on the engine's filesystem.
///
/// Backporting needs system libraries from a newer firmware with their
/// unavailable imports patched out; upstream ships those edits as BPS
/// files. Doing it here means a library can be patched on its way to the
/// console instead of through a browser-based patcher.
async fn bps_apply_handler(Json(req): Json<BpsApplyReq>) -> impl IntoResponse {
    let (src, patch_path, dest) = (req.source_path, req.patch_path, req.dest_path);
    crate::log_info!("bps_apply: src={src} patch={patch_path} dest={dest}");
    let dest_for_log = dest.clone();
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<u64> {
        let patch =
            std::fs::read(&patch_path).map_err(|e| anyhow::anyhow!("read {patch_path}: {e}"))?;
        let source = std::fs::read(&src).map_err(|e| anyhow::anyhow!("read {src}: {e}"))?;
        let out = ps5upload_core::bps::bps_apply(&patch, &source)?;
        if let Some(parent) = std::path::Path::new(&dest).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&dest, &out).map_err(|e| anyhow::anyhow!("write {dest}: {e}"))?;
        Ok(out.len() as u64)
    })
    .await;
    match r {
        Ok(Ok(bytes)) => {
            crate::log_info!("bps_apply ok: dest={dest_for_log} bytes={bytes}");
            (
                StatusCode::OK,
                Json(serde_json::json!({ "ok": true, "bytes": bytes, "dest": dest_for_log })),
            )
                .into_response()
        }
        // A checksum mismatch is the expected failure — the patch was
        // built for a different library or a different firmware — so it
        // is a bad request, not a server fault.
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

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
                // tokio task) until the receiver has room. On client
                // disconnect the receiver drops and `blocking_send`
                // returns `Err` immediately — the send is a no-op and
                // the parse continues to completion (the inspect API
                // doesn't support cancellation). The final `Done`/`Error`
                // event below also fails to send, so the worker exits
                // cleanly after parsing finishes.
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

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "zip");
    telemetry::set_drive(job_id, &req.dest_root);
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
        dynamic_total_bytes: None,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
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
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        // Resume is by job_id (the sender reopens with JobOpen); retries live in the
        // adapter's loop. An archive AVA1 cannot read (encryption, an unsupported method, a
        // path the manifest refuses, a damaged directory) is a failure with its own reason:
        // there is no other transport to hand it to.
        let result = ps5upload_ava1::upload::upload_zip(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.zip_path),
        )
        .map_err(zip_failure);
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

/// A zip the AVA1 source refused as unusable becomes a typed job failure, so the client shows
/// the archive problem rather than a transport one.
fn zip_failure(e: anyhow::Error) -> anyhow::Error {
    if e.downcast_ref::<ps5upload_ava1::upload::ZipUnsupported>()
        .is_some()
        || e.downcast_ref::<ps5upload_ava1::upload::ZipTooLarge>()
            .is_some()
    {
        return anyhow::Error::from(ps5upload_ava1::upload::UploadFailure {
            reason: "zip_unsupported".into(),
            detail: format!("{e}"),
        });
    }
    e
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
    /// Accepts a JSON string ("0x1a2b" or decimal) as well as a number.
    ///
    /// A number alone is not safe here: an account id is 64-bit, and a
    /// JavaScript client cannot represent anything above 2^53 exactly — it
    /// would round and silently activate a DIFFERENT id than the user typed.
    /// The string form is what the client sends; the number form stays for
    /// any older caller.
    #[serde(default)]
    id: Option<AccountIdInput>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AccountIdInput {
    Num(u64),
    Str(String),
}

impl AccountIdInput {
    /// None when the text is not a usable id. Zero is rejected: it means "no
    /// account", and clearing a slot is a separate endpoint.
    fn to_u64(&self) -> Option<u64> {
        let v = match self {
            Self::Num(n) => *n,
            Self::Str(s) => {
                let t = s.trim();
                let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    Some(hex) => u64::from_str_radix(hex, 16).ok()?,
                    None => t.parse::<u64>().ok()?,
                };
                parsed
            }
        };
        if v == 0 {
            None
        } else {
            Some(v)
        }
    }
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

// ─── User create / delete ───────────────────────────────────────────────

#[derive(Deserialize)]
struct UserCreateReq {
    addr: Option<String>,
    name: String,
}

#[derive(Deserialize)]
struct UserDeleteReq {
    addr: Option<String>,
    uid: i32,
    #[serde(default)]
    wipe_saves: bool,
}

// ─── Backup & restore ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct BackupSnapshotReq {
    addr: Option<String>,
    tag: String,
    path: String,
}

#[derive(Deserialize)]
struct BackupListReq {
    addr: Option<String>,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Deserialize)]
struct BackupRestoreReq {
    addr: Option<String>,
    tag: String,
    timestamp: i64,
}

#[derive(Deserialize)]
struct BackupDeleteReq {
    addr: Option<String>,
    tag: String,
    timestamp: i64,
}

// ── v4.1: Remote Play ─────────────────────────────────────────────────
#[derive(Deserialize)]
struct RemotePlayReq {
    addr: Option<String>,
    #[serde(default)]
    manual_account_id: Option<String>,
}

// ── v4.1: Fan curve ───────────────────────────────────────────────────
#[derive(Deserialize)]
struct FanCurveSetReq {
    addr: Option<String>,
    points: Vec<ps5upload_core::fan_curve::FanCurvePoint>,
}

// ── v4.1: Notifications ───────────────────────────────────────────────
#[derive(Deserialize)]
struct NotifListReq {
    addr: Option<String>,
    #[serde(default)]
    since_seq: u64,
}

async fn profile_info_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let slot = req.slot;
    let id = req.id.as_ref().and_then(AccountIdInput::to_u64);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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

/// POST /api/ps5/users/create — create a new local user account.
async fn user_create_handler(
    State(state): State<AppState>,
    Json(req): Json<UserCreateReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let name = req.name;
    crate::log_info!("user_create: addr={addr} name={name}");
    let r = tokio::task::spawn_blocking(move || ps5upload_core::users::user_create(&addr, &name))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "uid": result.uid,
                "name": result.name,
            })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/users/delete — delete a local user account.
async fn user_delete_handler(
    State(state): State<AppState>,
    Json(req): Json<UserDeleteReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let uid = req.uid;
    let wipe_saves = req.wipe_saves;
    crate::log_info!("user_delete: addr={addr} uid={uid} wipe_saves={wipe_saves}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::users::user_delete(&addr, uid, wipe_saves)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "uid": uid,
            })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/backup/snapshot — snapshot a file or directory tree.
async fn backup_snapshot_handler(
    State(state): State<AppState>,
    Json(req): Json<BackupSnapshotReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let tag = req.tag;
    let path = req.path;
    crate::log_info!("backup_snapshot: addr={addr} tag={tag} path={path}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::backup::backup_snapshot(&addr, &tag, &path)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "tag": result.tag,
                "timestamp": result.timestamp,
                "files": result.files,
                "bytes": result.bytes,
            })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/backup/list — list snapshots (optionally filtered by tag).
async fn backup_list_handler(
    State(state): State<AppState>,
    Query(req): Query<BackupListReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let tag = req.tag.unwrap_or_default();
    let r = tokio::task::spawn_blocking(move || ps5upload_core::backup::backup_list(&addr, &tag))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(list) => (StatusCode::OK, Json(list)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/backup/restore — restore a snapshot.
async fn backup_restore_handler(
    State(state): State<AppState>,
    Json(req): Json<BackupRestoreReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let tag = req.tag;
    let ts = req.timestamp;
    crate::log_info!("backup_restore: addr={addr} tag={tag} ts={ts}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::backup::backup_restore(&addr, &tag, ts)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "tag": result.tag,
                "restored": result.restored,
            })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/backup/delete — delete a snapshot.
async fn backup_delete_handler(
    State(state): State<AppState>,
    Json(req): Json<BackupDeleteReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let tag = req.tag;
    let ts = req.timestamp;
    let tag_clone = tag.clone();
    crate::log_info!("backup_delete: addr={addr} tag={tag} ts={ts}");
    let r =
        tokio::task::spawn_blocking(move || ps5upload_core::backup::backup_delete(&addr, &tag, ts))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "ok": true,
                "tag": tag_clone,
                "timestamp": ts,
            })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── v4.1: Remote Play handlers ────────────────────────────────────────
async fn remoteplay_request_handler(
    State(state): State<AppState>,
    Json(req): Json<RemotePlayReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let acct = req.manual_account_id;
    crate::log_info!("remoteplay_request: addr={addr}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::remoteplay::remoteplay_request(&addr, acct.as_deref())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(snap) => (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "pin": snap.pin, "account_id": snap.account_id})),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn remoteplay_status_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r =
        tokio::task::spawn_blocking(move || ps5upload_core::remoteplay::remoteplay_status(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(serde::Deserialize)]
struct RemotePlayEnableBody {
    /// "service" (system toggle) or "user" (per-user permission, FW 10.00+).
    scope: String,
}

async fn remoteplay_readiness_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::remoteplay::remoteplay_readiness(&addr)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn remoteplay_enable_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
    Json(body): Json<RemotePlayEnableBody>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    if body.scope != "service" && body.scope != "user" {
        return json_err(
            StatusCode::BAD_REQUEST,
            "scope must be \"service\" or \"user\"".to_string(),
        )
        .into_response();
    }
    let scope = body.scope.clone();
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::remoteplay::remoteplay_enable(&addr, &scope)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn remoteplay_devices_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r =
        tokio::task::spawn_blocking(move || ps5upload_core::remoteplay::remoteplay_devices(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// The console a POST is about: `?addr=` (the desktop app) or a JSON body's `addr` (the browser
/// build, `postJson(..., { addr })`). The query wins when both are given.
///
/// Remote Play cancel read only the query, so a browser cancel went to the default console and
/// failed with "the console did not answer on the AVA1 port in time" while the real console kept
/// its PIN (seen on a Phat at FW 13.60).
fn post_addr(query: Option<String>, body: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    struct AddrBody {
        #[serde(default)]
        addr: Option<String>,
    }
    query.filter(|a| !a.trim().is_empty()).or_else(|| {
        serde_json::from_slice::<AddrBody>(body)
            .ok()
            .and_then(|b| b.addr)
            .filter(|a| !a.trim().is_empty())
    })
}

async fn remoteplay_cancel_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let addr = console_addr_or_default(post_addr(q.addr, &body), &state.default_ps5_addr);
    let r =
        tokio::task::spawn_blocking(move || ps5upload_core::remoteplay::remoteplay_cancel(&addr))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── v4.1: Fan curve handler ───────────────────────────────────────────
async fn fan_curve_set_handler(
    State(state): State<AppState>,
    Json(req): Json<FanCurveSetReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let points = req.points;
    crate::log_info!("fan_curve_set: addr={addr} points={}", points.len());
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::fan_curve::fan_curve_set(&addr, &points)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── v5.1: Fan curve get handler ───────────────────────────────────────
async fn fan_curve_get_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::fan_curve::fan_curve_get(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(points) => (StatusCode::OK, Json(serde_json::json!({"points": points}))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── v4.1: Notifications handler ───────────────────────────────────────
async fn notif_list_handler(
    State(state): State<AppState>,
    Query(req): Query<NotifListReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let since = req.since_seq;
    let r = tokio::task::spawn_blocking(move || ps5upload_core::notif::notif_list(&addr, since))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct LocalImageReq {
    path: Option<String>,
    device: Option<String>,
}

/// POST /api/local/image/attach  { "path": "/path/to.exfat" }
///
/// Attaches the image so the OS mounts it, and returns where. This is a
/// host-side operation -- no console involved.
async fn local_image_attach(Json(req): Json<LocalImageReq>) -> impl IntoResponse {
    let Some(path) = req.path else {
        return json_err(StatusCode::BAD_REQUEST, "path is required").into_response();
    };
    let r = tokio::task::spawn_blocking(move || ps5upload_core::local_image::attach(&path))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(info) => (StatusCode::OK, Json(info)).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

/// POST /api/local/image/detach  { "device": "/dev/disk4" }
async fn local_image_detach(Json(req): Json<LocalImageReq>) -> impl IntoResponse {
    let Some(device) = req.device else {
        return json_err(StatusCode::BAD_REQUEST, "device is required").into_response();
    };
    let r = tokio::task::spawn_blocking(move || ps5upload_core::local_image::detach(&device))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

/// GET /api/local/image/status — what is attached, and whether this
/// platform can attach at all.
async fn local_image_status() -> impl IntoResponse {
    let unsupported = ps5upload_core::local_image::unsupported_reason();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "attached": ps5upload_core::local_image::status(),
            "supported": unsupported.is_none(),
            "unsupported_reason": unsupported,
        })),
    )
        .into_response()
}

/// POST /api/ps5/activity/reset
///
/// Discards ps5upload's recorded play time on the console. This is our
/// own tracking, not the console's own records, which are not writable.
async fn activity_reset_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::activity::activity_reset(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/notif/clear
///
/// Empties the payload's notification ring. These are messages
/// ps5upload put on the console's screen, so this clears everything
/// the Notifications screen can show.
async fn notif_clear_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::notif::notif_clear(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ── v4.2: Cheat engine handlers ──────────────────────────────────────
#[derive(Deserialize)]
struct CheatsAddrQuery {
    addr: Option<String>,
}

#[derive(Deserialize)]
struct CheatsGetQuery {
    addr: Option<String>,
    title_id: String,
}

#[derive(Deserialize)]
struct CheatsDeleteQuery {
    addr: Option<String>,
    title_id: String,
}

#[derive(Deserialize)]
struct CheatsToggleReq {
    addr: Option<String>,
    title_id: String,
    index: i32,
    #[serde(default = "default_true")]
    on: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
struct CheatsEngineSetReq {
    addr: Option<String>,
    enabled: bool,
}

async fn cheats_list_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsAddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::cheats::cheats_list(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn cheats_get_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsGetQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let title_id = q.title_id;
    let r =
        tokio::task::spawn_blocking(move || ps5upload_core::cheats::cheats_get(&addr, &title_id))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn cheats_toggle_handler(
    State(state): State<AppState>,
    Json(req): Json<CheatsToggleReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let title_id = req.title_id;
    let index = req.index;
    let on = req.on;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::cheats::cheats_toggle(&addr, &title_id, index, on)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn cheats_delete_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsDeleteQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let title_id = q.title_id;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::cheats::cheats_delete(&addr, &title_id)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(ok) => (StatusCode::OK, Json(serde_json::json!({ "ok": ok }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn cheats_reload_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsAddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::cheats::cheats_reload(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(ok) => (StatusCode::OK, Json(serde_json::json!({ "ok": ok }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn cheats_status_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsAddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::cheats::cheats_status(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn cheats_engine_set_handler(
    State(state): State<AppState>,
    Json(req): Json<CheatsEngineSetReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let enabled = req.enabled;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::cheats::cheats_engine_set(&addr, enabled)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/* ── Community cheat repo browse + download ─────────────────────── */

#[derive(Deserialize)]
struct CheatsRepoSearchQuery {
    #[serde(default)]
    query: String,
}

async fn cheats_repos_list_handler(State(_state): State<AppState>) -> impl IntoResponse {
    let repos = ps5upload_core::cheats::cheat_repos();
    (StatusCode::OK, Json(repos)).into_response()
}

async fn cheats_repos_search_handler(
    State(_state): State<AppState>,
    Query(q): Query<CheatsRepoSearchQuery>,
) -> impl IntoResponse {
    let r =
        tokio::task::spawn_blocking(move || ps5upload_core::cheats::cheats_repo_search(&q.query))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct CheatsRepoDownloadReq {
    addr: Option<String>,
    repo_id: String,
    filename: String,
    title_id: String,
}

async fn cheats_repos_download_handler(
    State(state): State<AppState>,
    Json(req): Json<CheatsRepoDownloadReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::cheats::cheats_repo_download(
            &addr,
            &req.repo_id,
            &req.filename,
            &req.title_id,
        )
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/* ── Activity tracker ─────────────────────────────────────────────── */

#[derive(Deserialize)]
struct ActivityDbQueryParams {
    addr: Option<String>,
    #[serde(default = "default_db_query")]
    query: String,
}

fn default_db_query() -> String {
    "recently_played".to_string()
}

async fn activity_get_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsAddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::activity::activity_get(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn activity_db_query_handler(
    State(state): State<AppState>,
    Query(q): Query<ActivityDbQueryParams>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let query = q.query;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::activity::activity_db_query(&addr, &query)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/* ── SDK Changer ──────────────────────────────────────────────────── */

async fn sdk_scan_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsAddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::sdk_changer::sdk_scan(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct SdkPatchReq {
    addr: Option<String>,
    title_id: String,
    target_sdk: String,
    /// Opt-in libc.prx symbol swap — see sdk_changer.rs.
    #[serde(default)]
    patch_libc: bool,
}

async fn sdk_patch_handler(
    State(state): State<AppState>,
    Json(req): Json<SdkPatchReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let title_id = req.title_id;
    let target_sdk = req.target_sdk;
    let patch_libc = req.patch_libc;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::sdk_changer::sdk_patch(&addr, &title_id, &target_sdk, patch_libc)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct SdkRestoreReq {
    addr: Option<String>,
    title_id: String,
}

async fn sdk_restore_handler(
    State(state): State<AppState>,
    Json(req): Json<SdkRestoreReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let title_id = req.title_id;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::sdk_changer::sdk_restore(&addr, &title_id)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/* ── TMDB / PlayStation Store metadata ────────────────────────────── */

#[derive(Deserialize)]
struct TmdbFetchReq {
    addr: Option<String>,
    title_id: String,
    #[serde(default)]
    refresh: bool,
    /// Optional region prefix (e.g. "UP9000" for US) to narrow the
    /// PS Store search instead of brute-forcing all 24 known prefixes.
    #[serde(default)]
    region: Option<String>,
}

async fn tmdb_fetch_handler(
    State(state): State<AppState>,
    Query(q): Query<TmdbFetchReq>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let title_id = q.title_id;
    let refresh = q.refresh;
    let region = q.region;
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::tmdb::tmdb_fetch(&addr, &title_id, refresh, region.as_deref())
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/* ── FW Spoof detection ──────────────────────────────────────────── */

async fn fw_spoof_status_handler(
    State(state): State<AppState>,
    Query(q): Query<CheatsAddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::fw_spoof::fw_spoof_status(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
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
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
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
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    telemetry::tag(job_id, "7z");
    telemetry::set_drive(job_id, &req.dest_root);
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
        dynamic_total_bytes: None,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
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
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        // Resume is by job_id (the sender reopens with JobOpen); retries live in the
        // adapter's loop. An archive AVA1 cannot read (encryption, a feature the decoder lacks)
        // is a failure with its own reason: there is no other transport to hand it to.
        let result = ps5upload_ava1::upload::upload_7z(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.archive_path),
        )
        .map_err(|e| {
            if e.downcast_ref::<ps5upload_ava1::upload::SevenzUnsupported>()
                .is_some()
            {
                anyhow::Error::from(ps5upload_ava1::upload::UploadFailure {
                    reason: "7z_unsupported".into(),
                    detail: format!("{e}"),
                })
            } else {
                e
            }
        });
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
    telemetry::tag(job_id, "rar");
    telemetry::set_drive(job_id, &req.dest_root);
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
        dynamic_total_bytes: None,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
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
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        // The password stays in this request for the job's lifetime and is never logged;
        // resume passes reuse it from the RarSource. An archive AVA1 cannot read is a failure
        // with its own reason: there is no other transport to hand it to.
        let result = ps5upload_ava1::upload::upload_rar(
            &cfg,
            tx_id,
            &req.dest_root,
            std::path::Path::new(&req.archive_path),
            req.password.as_deref(),
        )
        .map_err(|e| {
            if e.downcast_ref::<ps5upload_ava1::upload::RarUnsupported>()
                .is_some()
            {
                anyhow::Error::from(ps5upload_ava1::upload::UploadFailure {
                    reason: "rar_unsupported".into(),
                    detail: format!("{e}"),
                })
            } else {
                e
            }
        });
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

/// POST /api/rar/packages — desktop only. The `.pkg` entries of a RAR (any folder depth), from
/// the headers alone, so several packages in one archive can be found before anything is
/// unpacked (R6, #370). `PKG_ALLOW` is the same allow-list the unpack passes as `excludes`, so
/// what is listed here is exactly what `/api/transfer/rar` will send.
#[cfg(not(target_os = "android"))]
async fn rar_packages_handler(Json(req): Json<RarPackagesReq>) -> impl IntoResponse {
    let p = req.archive_path.clone();
    let pw = req.password.clone();
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::transfer::rar_layout(
            std::path::Path::new(&p),
            pw.as_deref(),
            &[RAR_PKG_ALLOW.to_string()],
        )
    })
    .await;
    match r {
        Ok(Ok(layout)) => {
            let mut packages: Vec<serde_json::Value> = layout
                .files
                .iter()
                .map(|(path, size)| serde_json::json!({ "path": path, "size": size }))
                .collect();
            packages.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
            (
                StatusCode::OK,
                Json(serde_json::json!({ "packages": packages, "allow": RAR_PKG_ALLOW })),
            )
                .into_response()
        }
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("rar packages task: {e}"),
        )
        .into_response(),
    }
}

#[cfg(target_os = "android")]
async fn rar_packages_handler() -> impl IntoResponse {
    json_err(
        StatusCode::NOT_IMPLEMENTED,
        "RAR is not supported on this build",
    )
    .into_response()
}

/// The exclude entry that keeps only packages (see `ps5upload_core::excludes`).
#[cfg(not(target_os = "android"))]
const RAR_PKG_ALLOW: &str = "!*.pkg";

/// POST /api/link/probe — desktop only. What a link actually serves: a package, some other
/// real file, or something that is not a download (R4, #368). Decided from the response, not
/// the URL's spelling.
#[cfg(not(target_os = "android"))]
async fn link_probe_handler(Json(req): Json<LinkProbeReq>) -> impl IntoResponse {
    let url = req.url.trim().to_string();
    if !valid_link_url(&url) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "url must be an http(s) link with no fragment or control characters",
        )
        .into_response();
    }
    let insecure = req.insecure_tls;
    match tokio::task::spawn_blocking(move || link::probe(&url, insecure)).await {
        Ok(Ok(c)) => (StatusCode::OK, Json(c)).into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("link probe task: {e}"),
        )
        .into_response(),
    }
}

#[cfg(target_os = "android")]
async fn link_probe_handler() -> impl IntoResponse {
    json_err(
        StatusCode::NOT_IMPLEMENTED,
        "link downloads are not available in the Android build",
    )
    .into_response()
}

#[cfg(not(target_os = "android"))]
fn valid_link_url(url: &str) -> bool {
    url.len() <= 4093
        && !url.bytes().any(|b| b < 0x20 || b == 0x7f)
        && !url.contains('#')
        && url.parse::<axum::http::Uri>().ok().is_some_and(|u| {
            matches!(u.scheme_str(), Some("http" | "https"))
                && u.host().is_some_and(|h| !h.is_empty())
        })
}

/// POST /api/link/download — desktop only. Download-only (R4, #368): the engine's ranged
/// fetcher is the AVA1 source, so the link streams to a console folder with nothing staged on
/// this computer. The link is probed AGAIN here and refused unless it is a real file
/// download; the client's earlier probe is a convenience, never the authority.
#[cfg(not(target_os = "android"))]
async fn link_download_handler(
    State(state): State<AppState>,
    Json(req): Json<LinkDownloadReq>,
) -> impl IntoResponse {
    let url = req.url.trim().to_string();
    if !valid_link_url(&url) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "url must be an http(s) link with no fragment or control characters",
        )
        .into_response();
    }
    let dest_dir = match link::valid_console_dir(&req.dest_dir) {
        Ok(d) => d,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let insecure = req.insecure_tls;
    let probe_url = url.clone();
    let class = match tokio::task::spawn_blocking(move || link::probe(&probe_url, insecure)).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return json_err(StatusCode::BAD_GATEWAY, e).into_response(),
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("link probe task: {e}"),
            )
            .into_response()
        }
    };
    if class.kind == link::LinkKind::Refused {
        let msg = class.message.clone().unwrap_or_default();
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": msg,
                "reason": class.reason,
            })),
        )
            .into_response();
    }
    let Some(total) = class.total_size.filter(|t| *t > 0) else {
        return json_err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the server did not say how big the file is, so it cannot be streamed to the console",
        )
        .into_response();
    };
    if !class.ranges {
        return json_err(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the server does not support partial downloads (HTTP Range), which streaming to the \
             console needs. Download the file on this computer and upload it instead.",
        )
        .into_response();
    }
    let name = req
        .file_name
        .as_deref()
        .map(link::sanitize_name)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| class.filename.clone());
    let dest_path = format!("{dest_dir}/{name}");

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "link_download");
    telemetry::set_drive(job_id, &dest_dir);
    let started_at_ms = now_ms();
    // The host only: a link can carry a signed token in its path or query.
    let host = url
        .parse::<axum::http::Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_string))
        .unwrap_or_default();
    crate::log_info!(
        "link_download: job={job_id} addr={addr} host={host} dest={dest_path} bytes={total}"
    );
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes: total,
        dynamic_total_bytes: None,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: total,
            files: vec![PlannedFile {
                rel_path: name.clone(),
                size: total,
            }],
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
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        let remote = Arc::new(crate::remote_pkg::RemoteSource::new_with_options(
            url, total, insecure,
        ));
        let source = Arc::new(link::LinkSource::new(remote, name.clone(), total));
        let result = ava1::manifest::single(source.as_ref(), &name)
            .map_err(anyhow::Error::from)
            .and_then(|manifest| {
                // `dest` is the full path: JF_SINGLE_FILE writes `<dest>.ava-part` and
                // renames it (the same contract `upload_file_in` follows).
                // The console ends a job whose sender delivers nothing for 36 s
                // (ERR_STALLED). For a file on disk that means a wedged read; for a link
                // it is an ordinary slow host: the first byte is only handed over once a
                // whole download window has arrived. The download keeps running here in
                // the meantime and the console keeps the job, so open it again and carry
                // on, until the host has delivered nothing durable for too long.
                let mut stalls = 0u32;
                let mut durable_seen = progress_bytes_finalized.load(Ordering::Relaxed);
                loop {
                    let mut opts = ava1::send::SendOptions::upload(&dest_path);
                    opts.flags = ava1::gen::JF_SINGLE_FILE;
                    let r = ps5upload_ava1::upload::upload_with(
                        &cfg.addr,
                        tx_id,
                        manifest.clone(),
                        source.clone(),
                        opts,
                        &cfg,
                    );
                    let durable = progress_bytes_finalized.load(Ordering::Relaxed);
                    let cancelled = cfg
                        .cancel
                        .as_ref()
                        .is_some_and(|c| c.load(Ordering::Relaxed));
                    match link_stall_verdict(&r, cancelled, durable > durable_seen, &mut stalls) {
                        LinkStall::Retry => {
                            durable_seen = durable;
                            crate::log_info!(
                                "link_download: job={job_id} the host delivered no data in time (stall {stalls}/{LINK_STALL_TRIES}); reopening the job"
                            );
                            std::thread::sleep(std::time::Duration::from_secs(1));
                        }
                        LinkStall::Done => break r,
                    }
                }
            });
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
                        bytes_sent: r.bytes_sent,
                        dest: r.dest,
                        files_sent: 1,
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
async fn link_download_handler() -> impl IntoResponse {
    json_err(
        StatusCode::NOT_IMPLEMENTED,
        "link downloads are not available in the Android build",
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
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "file_list");
    telemetry::set_drive(job_id, &req.dest_root);
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
        dynamic_total_bytes: None,
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
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
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // Make this transfer cancellable: register a flag the core checks at
        // every shard boundary, flipped by POST /api/jobs/{id}/cancel.
        cfg.cancel = Some(register_transfer_cancel(job_id));
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        cfg.progress_live = Some(live_notes_for(job_id));
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        crate::log_info!("transfer_file_list: job={job_id} protocol=ava1");
        // Resume is by job_id (the sender reopens with JobOpen); retries live in the adapter's
        // loop. A list that names several directories becomes one job per directory.
        let result = ps5upload_ava1::upload::upload_list(&cfg, tx_id, &req.dest_root, &entries);
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
    /// The console (a `:port` suffix from an older client is ignored).
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
    /// When true, bypasses the payload's writable-root allowlist so system
    /// files (/system/, /system_data/, /system_ex/) can be downloaded.
    /// Read-only — the payload ignores this flag for destructive ops.
    #[serde(default)]
    unsafe_read: bool,
}

/// Where an AVA1 download lands (Task 25).
enum Ava1DownloadTarget {
    /// A tree under this directory: `dest_dir/<basename>`.
    Folder(std::path::PathBuf),
    /// One `.zip` at this path, Stored (resumable) or Deflated (cannot resume).
    Zip(std::path::PathBuf, ps5upload_ava1::download::ZipCompression),
}

/// Starts a console -> computer download over AVA1 and answers `ACCEPTED` with the job id.
/// Three things to know:
/// - nothing is enumerated here: the console's own manifest is the source of truth, so the
///   initial `Running` has an empty per-file list (bytes and total are correct; the list
///   returns when the manifest feeds the UI) and there is no skipped-entry report;
/// - the total is unknown until the manifest arrives, so the ticker reads
///   `dynamic_total_bytes`, which the transfer fills in (a zero total is never published
///   as if it were real);
/// - the job id doubles as the AVA1 job id, so the journal, the `JobOpen` and the job
///   record name the same value.
fn start_ava1_download(
    state: &AppState,
    addr: String,
    src: String,
    kind: DownloadKind,
    unsafe_read: bool,
    target: Ava1DownloadTarget,
) -> axum::response::Response {
    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "download");
    let started_at_ms = now_ms();
    let basename = src
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let dest_display = match &target {
        Ava1DownloadTarget::Folder(dir) => dir.join(&basename).to_string_lossy().to_string(),
        Ava1DownloadTarget::Zip(zip, _) => zip.to_string_lossy().to_string(),
    };
    crate::log_info!("transfer_download: job={job_id} protocol=ava1 src={src} dest={dest_display}");
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
    let dynamic_total = Arc::new(AtomicU64::new(0));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes: 0,
        dynamic_total_bytes: Some(Arc::clone(&dynamic_total)),
        skipped_files: 0,
        skipped_bytes: 0,
    };
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: 0,
            files: Vec::new(),
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
    let counters = ps5upload_ava1::download::Counters {
        bytes: Arc::clone(&progress),
        files: Arc::clone(&progress_files),
        files_finalized: Arc::clone(&progress_files_finalized),
        bytes_finalized: Arc::clone(&progress_bytes_finalized),
        total: Some(dynamic_total),
    };
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let id = *job_id.as_bytes();
        let result = match &target {
            Ava1DownloadTarget::Folder(dir) => ps5upload_ava1::download::to_local(
                &addr,
                &src,
                kind,
                dir,
                unsafe_read,
                id,
                &counters,
                Some(cancel),
            ),
            Ava1DownloadTarget::Zip(zip, compression) => ps5upload_ava1::download::to_zip_with(
                &addr,
                &src,
                kind,
                zip,
                unsafe_read,
                *compression,
                id,
                &counters,
                Some(cancel),
            ),
        };
        match result {
            Ok(bytes) => {
                let completed_at_ms = now_ms();
                let files = progress_files.load(Ordering::Relaxed);
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                        bytes_sent: bytes,
                        dest: dest_display,
                        files_sent: files,
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
    let req_unsafe = req.unsafe_read;
    let mgmt_addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    // The console's own manifest is the source of truth: nothing is enumerated here, and the
    // readiness check (helper present, paired) runs on the job's blocking thread.
    start_ava1_download(
        &state,
        mgmt_addr,
        req.src_path.clone(),
        kind,
        req_unsafe,
        Ava1DownloadTarget::Folder(dest_dir),
    )
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
    /// When true, allows reading files outside the normal payload path
    /// allow-list (e.g. /system, /system_data). Read-only. Default false.
    #[serde(default)]
    unsafe_read: bool,
    /// "stored" (default: resumes mid-entry after a dropped connection) or "deflate"
    /// (smaller for text-heavy trees; cannot resume, so a drop restarts the archive).
    #[serde(default)]
    compression: Option<String>,
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
    let mgmt_addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
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
    let zip_compression = match req.compression.as_deref() {
        None | Some("") | Some("stored") => ps5upload_ava1::download::ZipCompression::Stored,
        Some("deflate") => ps5upload_ava1::download::ZipCompression::Deflate,
        Some(other) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                &format!("compression must be \"stored\" or \"deflate\", not {other:?}"),
            )
            .into_response();
        }
    };
    let req_unsafe_zip = req.unsafe_read;
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

    start_ava1_download(
        &state,
        mgmt_addr,
        req.src_path.clone(),
        kind,
        req_unsafe_zip,
        Ava1DownloadTarget::Zip(dest_zip, zip_compression),
    )
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
    let mgmt = console_addr(&addr);
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
/// Resume-friendly directory upload: the console decides what to skip (size and mtime in
/// `fast` mode, content in `safe` mode, SPEC §11.4), so this is the folder upload with the
/// user's skip-existing choice. The job's `total_bytes` and progress reflect what is sent.
/// Remote (NAS) sources go the same way.
///
/// Request body mirrors `TransferDirReq` plus an optional `mode` ("fast"|"safe"; default
/// "fast"). `streams` is accepted for older clients and ignored (AVA1 spreads one job over its
/// own lanes). Response is the same `JobCreated` shape as the other transfer handlers.
async fn transfer_dir_reconcile_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferDirReconcileReq>,
) -> impl IntoResponse {
    let addr = req.addr.unwrap_or_else(|| state.default_ps5_addr.clone());
    let skip = match req.mode.as_deref().unwrap_or("fast") {
        "fast" => "fast",
        "safe" => "safe",
        other => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("unknown reconcile mode: {other}"),
            )
            .into_response();
        }
    };
    let dir_req = TransferDirReq {
        addr: Some(addr),
        tx_id: req.tx_id,
        dest_root: req.dest_root,
        src_dir: req.src_dir,
        excludes: req.excludes,
        bandwidth_cap_mbps: req.bandwidth_cap_mbps,
        skip_existing: Some(skip.to_string()),
    };
    transfer_dir_handler(State(state), Json(dir_req))
        .await
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
        Some(job) => (
            StatusCode::OK,
            Json(with_live_notes(uuid, serde_json::json!(job))),
        )
            .into_response(),
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

/// GET /api/ps5/port-check?ip=&port= — TCP reachability probe.
///
/// Exists so the self-hosted web UI can light up the same connection
/// status dots the desktop client does. A browser cannot open a raw
/// socket, so without this the Connection screen and the first-run
/// reachability step were both dead in a browser session (#300).
///
/// Mirrors the `port_check` Tauri command's shape exactly —
/// `{ open: bool, error: string|null }` — so the two transports stay
/// interchangeable behind `portProbe()`.
async fn ps5_port_check(Query(q): Query<PortCheckQuery>) -> impl IntoResponse {
    let ip = q.ip.trim().to_string();
    if ip.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "open": false, "error": "ip is required" })),
        );
    }
    let timeout = Duration::from_millis(q.timeout_ms.unwrap_or(1500).clamp(100, 10_000));
    let res = tokio::task::spawn_blocking(move || {
        ps5upload_core::payload_lifecycle::probe_port(&ip, q.port, timeout)
    })
    .await;
    match res {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(serde_json::json!({ "open": true, "error": null })),
        ),
        Ok(Err(e)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "open": false, "error": e })),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(serde_json::json!({ "open": false, "error": format!("probe task: {e}") })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct PortCheckQuery {
    ip: String,
    port: u16,
    /// Optional override so a slow LAN can be probed more patiently.
    timeout_ms: Option<u64>,
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

// ─── Bug-report bundling (browser / self-hosted web UI) ──────────────────────

/// One entry to place in the bundle. Exactly one of `text` / `base64` is used;
/// `text` wins when both are present.
#[derive(Deserialize)]
struct BugBundleEntry {
    /// Zip-relative path, e.g. "logs/app.jsonl". Traversal is rejected.
    path: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    base64: Option<String>,
}

#[derive(Deserialize)]
struct BugBundleReq {
    #[serde(default)]
    filename: Option<String>,
    entries: Vec<BugBundleEntry>,
}

/// Reject anything that could escape the archive root or produce a surprising
/// path on extraction. The client builds these names, but the engine is a
/// network service and must not trust its caller.
fn bundle_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && !p.starts_with('\\')
        && !p.contains("..")
        && !p.contains(':')
        && !p.contains('\0')
        && p.split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// POST /api/bug-report/bundle — zip client-supplied entries and return the
/// archive as a download.
///
/// The desktop app builds its bundle in the Tauri shell, which can read the
/// local log files and open a save dialog. A browser can do neither, so the
/// self-hosted web UI had no way to produce a bug report at all — it rendered
/// the whole form and then said "requires the desktop app", which is where a
/// user who has just hit a bug finds out they cannot report it.
///
/// The client already holds everything the bundle needs (its own log ring, the
/// engine log tail over HTTP, the PS5 snapshot and payload logs), so the engine
/// only has to do the two things a browser cannot: build the zip and hand it
/// back as an attachment.
async fn bug_report_bundle_handler(Json(req): Json<BugBundleReq>) -> axum::response::Response {
    use base64::Engine as _;
    use std::io::{Cursor, Write};

    if req.entries.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "entries is required").into_response();
    }
    // A bundle is diagnostics, not a file transfer. Cap it so a malformed or
    // hostile caller cannot drive the engine's memory through the roof.
    const MAX_TOTAL: usize = 64 * 1024 * 1024;

    let mut buf = Cursor::new(Vec::<u8>::new());
    let mut total = 0usize;
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for e in &req.entries {
            if !bundle_path_ok(&e.path) {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    &format!("unsafe entry path: {}", e.path),
                )
                .into_response();
            }
            let bytes: Vec<u8> = if let Some(t) = &e.text {
                t.as_bytes().to_vec()
            } else if let Some(b) = &e.base64 {
                match base64::engine::general_purpose::STANDARD.decode(b) {
                    Ok(v) => v,
                    Err(_) => {
                        return json_err(
                            StatusCode::BAD_REQUEST,
                            format!("entry {} has invalid base64", e.path),
                        )
                        .into_response()
                    }
                }
            } else {
                Vec::new()
            };
            total = total.saturating_add(bytes.len());
            if total > MAX_TOTAL {
                return json_err(StatusCode::PAYLOAD_TOO_LARGE, "bundle too large").into_response();
            }
            if zw.start_file(e.path.clone(), opts).is_err() {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, "zip entry failed")
                    .into_response();
            }
            if zw.write_all(&bytes).is_err() {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, "zip write failed")
                    .into_response();
            }
        }
        if zw.finish().is_err() {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, "zip finish failed")
                .into_response();
        }
    }

    // Keep the leaf name simple: it lands in the user's Downloads folder and is
    // echoed into a Content-Disposition header.
    let name = req
        .filename
        .as_deref()
        .filter(|n| {
            !n.is_empty()
                && n.len() <= 128
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
        .unwrap_or("ps5upload-bugreport.zip");

    let body = buf.into_inner();
    crate::log_info!(
        "bug-report bundle: {} entries, {} bytes, name={}",
        req.entries.len(),
        body.len(),
        name
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/zip"),
    );
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        headers.insert(axum::http::header::CONTENT_DISPOSITION, v);
    }
    (StatusCode::OK, headers, body).into_response()
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
///
/// `notify_one` stores a permit when nothing waits yet, so a parent that dies while the server
/// is still starting (before this future is polled) is not missed; `notify_waiters` dropped it
/// and left only the 10 s hard exit.
static SHUTDOWN: tokio::sync::Notify = tokio::sync::Notify::const_new();

async fn shutdown_signal() {
    let notify = &SHUTDOWN;
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
                        stderr_quiet("[ps5upload-engine] SIGTERM — shutting down");
                    }
                    _ = sigint.recv() => {
                        stderr_quiet("[ps5upload-engine] SIGINT — shutting down");
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
                stderr_quiet("[ps5upload-engine] Ctrl-C — shutting down");
            }
        }
    }
}

fn trigger_shutdown() {
    SHUTDOWN.notify_one();
}

/// Print to stderr without panicking. `eprintln!` panics when stderr is a closed pipe, which is
/// exactly the state a dead desktop parent leaves behind (it was reading our stderr): the
/// parent-watch thread died on its first message and the engine lived on, holding its port.
fn stderr_quiet(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{msg}");
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
        let why = loop {
            match stdin.read(&mut buf) {
                Ok(0) => break "parent process died (stdin EOF)".to_string(),
                Ok(_) => continue,
                Err(e) => break format!("parent-watch stdin read error: {e}"),
            }
        };
        // Shut down first: the parent that read our stderr is usually gone, so nothing
        // printed from here on may be able to stop this thread.
        trigger_shutdown();
        stderr_quiet(&format!(
            "[engine] {why}; draining in-flight requests then exiting"
        ));
        // Belt-and-braces watchdog: if axum's graceful-shutdown drain takes
        // longer than 10 seconds (e.g. a stuck pkg-host range read), hard-exit
        // so we don't keep the port held by a zombie engine after the parent
        // is gone.
        std::thread::sleep(std::time::Duration::from_secs(10));
        stderr_quiet("[engine] graceful shutdown timed out; hard exit");
        std::process::exit(0);
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
    /// Extra peers allowed past the loopback guard (besides loopback), set
    /// from `PS5UPLOAD_ALLOW_IP` (comma-separated IPs or CIDR ranges). Lets
    /// remote desktop clients reach the `/api/*` surface when self-hosting.
    pub allow_ips: Vec<AllowRule>,
}

/// Core server entry. Builds the router, binds, and serves until
/// graceful shutdown. Behavior is identical to the former `main`; the
/// `EngineConfig` flags select desktop-sidecar vs. in-process behavior.
async fn run(cfg: EngineConfig) -> anyhow::Result<()> {
    // Convert (and the package viewer) read games on saved servers and the console in place.
    convert_source::register();
    // Every management call (hardware, filesystem, apps, ...) goes through one transport
    // seam in the core crate; this registers the AVA1 implementation over the shared pool.
    // A console it cannot serve is a `helper_not_ava1` error, never another protocol.
    ps5upload_ava1::mgmt::install();
    // Renamed variables: the old name is read once with a deprecation line. Archives stream
    // (nothing is staged or held back to inflate), so both settings are accepted and reported
    // but change nothing.
    for (new, old) in [ZIP_RAM_THRESHOLD_ENV, ARCHIVE_STAGE_ENV] {
        if renamed_env(new, old).is_some() {
            crate::log_info!("{new} is set; archives stream now, so it has no effect");
        }
    }
    // One-time 6.0 upgrade clean-up of files no 6.x code reads (moved aside, never deleted).
    if let Some(dir) = crate::remote::store::data_dir() {
        let moved = migrate_6::run_host(&dir);
        if !moved.is_empty() {
            crate::log_info!(
                "6.0 upgrade: moved old files nothing reads any more to {}: {}",
                dir.join("legacy-5x").display(),
                moved.join(", ")
            );
        }
    }
    // SPEC.md §14.3: expire old AVA1 job directories at start and then daily.
    ava1_api::spawn_journal_gc();
    // One line at startup: where the AVA1 state lives, which key this engine is, and how many
    // consoles trust it. The per-transfer `protocol=` line stays per transfer.
    let ava = ps5upload_ava1::pool();
    crate::log_info!(
        "{}",
        ava1_startup_line(
            &ava.ava_dir().display().to_string(),
            &ava.identity_prefix(),
            ava.paired_count()
        )
    );
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
        .route("/api/ps5/port-check", get(ps5_port_check))
        .route("/api/ps5/readiness", get(ps5_readiness))
        .route(
            ps5upload_core::mgmt_proxy::ROUTE,
            post(mgmt_route::mgmt_call_handler),
        )
        .route("/api/ps5/health/scan", get(health_scan_handler))
        .route("/api/ps5/health/junk", get(health_junk_handler))
        .route("/api/ps5/health/fix", post(health_fix_handler))
        .route("/api/ps5/cleanup", post(ps5_cleanup))
        .route("/api/ps5/volumes", get(ps5_volumes))
        .route("/api/ps5/pkg/scan-external", get(ps5_pkg_scan_external))
        .route("/api/ps5/pkg/metadata", get(ps5_pkg_metadata))
        .route("/api/ps5/list-dir", get(ps5_list_dir))
        .route("/api/ava1/identity", get(ava1_api::identity_handler))
        .route("/api/ava1/pairing", get(ava1_api::pairing_handler))
        .route(
            "/api/ava1/pairing/confirm",
            post(ava1_api::pairing_confirm_handler),
        )
        .route(
            "/api/ava1/pairing/start",
            post(ava1_api::pairing_start_handler),
        )
        .route(
            "/api/ava1/pairing/cancel",
            post(ava1_api::pairing_cancel_handler),
        )
        .route(
            "/api/ava1/pairing/forget",
            post(ava1_api::pairing_forget_handler),
        )
        .route("/api/game/inspect", post(inspect::inspect_handler))
        .route(
            "/api/game/inspect/image",
            get(inspect::inspect_image_handler),
        )
        .route(
            "/api/game/inspect/files",
            get(inspect::inspect_files_handler),
        )
        .route("/api/fpkg/inspect", post(fpkg_api::fpkg_inspect_handler))
        .route("/api/fpkg/build", post(fpkg_api::fpkg_build_handler))
        .route("/api/fpkg/delete", post(fpkg_api::fpkg_delete_handler))
        .route("/api/fpkg/estimate", post(fpkg_api::fpkg_estimate_handler))
        .route("/api/fpkg/extract", post(fpkg_api::fpkg_extract_handler))
        .route(
            "/api/fpkg/extract/cleanup",
            post(fpkg_api::fpkg_extract_cleanup_handler),
        )
        .route(
            "/api/ffpfsc/compress",
            post(fpkg_api::ffpfsc_compress_handler),
        )
        .route("/api/local/list-dir", get(local_list_dir_handler))
        .route("/api/local/storage-roots", get(local_storage_roots_handler))
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
        .route("/api/ps5/content-db/backup", post(ps5_content_db_backup))
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
        .route("/api/ps5/hw/drive-sensors", get(ps5_hw_drive_sensors))
        .route("/api/ps5/proc/list", get(ps5_proc_list))
        .route("/api/ps5/app/lifecycle", post(ps5_app_lifecycle))
        .route("/api/ps5/klog", get(ps5_klog))
        .route("/api/fakelibs/manifest", get(fakelibs_manifest))
        // The app-managed corpus: the user builds it by importing a pack or
        // scanning a console, and every later backport reuses it.
        .route("/api/fakelibs/corpus", get(fakelibs_api::get_corpus))
        .route("/api/ps5/title-sdk-pair", get(fakelibs_api::title_sdk_pair))
        .route("/api/fakelibs/import", post(fakelibs_api::import))
        // Backport packs: inspect a folder the user downloaded, and take its
        // fakelib/ into the corpus. Path-based — a pack eboot is far too big
        // to push through an upload and straight back out again.
        .route("/api/backport/pack", get(fakelibs_api::inspect_pack))
        .route("/api/backport/pack/import", post(fakelibs_api::import_pack))
        .route("/api/fakelibs/scan", post(fakelibs_api::start_scan))
        .route("/api/fakelibs/scan/{id}", get(fakelibs_api::scan_status))
        .route(
            "/api/fakelibs/set/{id}",
            axum::routing::delete(fakelibs_api::delete_set),
        )
        .route("/api/ps5/net/interfaces", get(ps5_net_interfaces))
        .route(
            "/api/cache/artwork",
            get(cache_artwork_stats).delete(cache_artwork_clear),
        )
        .route("/api/ps5/appinfo", get(ps5_appinfo_query))
        .route("/api/ps5/appinfo/set", post(ps5_appinfo_set))
        .route("/api/ps5/focus", get(ps5_focus))
        .route("/api/ps5/fs/read-preview", post(ps5_fs_read_preview))
        .route("/api/ps5/process/list", get(ps5_process_list))
        .route("/api/ps5/elfldr/health", get(ps5_elfldr_health))
        .route("/api/ps5/elfldr/ensure", post(ps5_elfldr_ensure))
        .route("/api/ps5/helper/state", get(ps5_helper_state))
        .route("/api/ps5/helper/replace", post(ps5_helper_replace))
        .route("/api/ps5/process/kill", post(ps5_process_kill))
        .route("/api/ps5/power/control", post(ps5_power_control))
        .route("/api/ps5/power/telemetry", get(ps5_power_telemetry))
        .route("/api/ps5/power/wake", post(ps5_power_wake))
        .route("/api/ps5/power/wake-login", post(ps5_power_wake_login))
        .route("/api/ps5/power/ddp-status", get(ps5_power_ddp_status))
        .route("/api/ps5/power/pair", post(ps5_power_pair))
        .route("/api/ps5/users/list", get(ps5_users_list))
        .route("/api/ps5/users/create", post(user_create_handler))
        .route("/api/ps5/users/delete", post(user_delete_handler))
        .route("/api/ps5/backup/snapshot", post(backup_snapshot_handler))
        .route("/api/ps5/backup/list", get(backup_list_handler))
        .route("/api/ps5/backup/restore", post(backup_restore_handler))
        .route("/api/ps5/backup/delete", post(backup_delete_handler))
        .route(
            "/api/ps5/remoteplay/request",
            post(remoteplay_request_handler),
        )
        .route("/api/ps5/remoteplay/status", get(remoteplay_status_handler))
        .route(
            "/api/ps5/remoteplay/readiness",
            get(remoteplay_readiness_handler),
        )
        .route(
            "/api/ps5/remoteplay/enable",
            post(remoteplay_enable_handler),
        )
        .route(
            "/api/ps5/remoteplay/devices",
            get(remoteplay_devices_handler),
        )
        .route(
            "/api/ps5/remoteplay/cancel",
            post(remoteplay_cancel_handler),
        )
        .route("/api/ps5/hw/fan-curve", post(fan_curve_set_handler))
        .route("/api/ps5/hw/fan-curve/get", get(fan_curve_get_handler))
        .route("/api/ps5/notif/list", get(notif_list_handler))
        .route("/api/ps5/notif/clear", post(notif_clear_handler))
        .route("/api/ps5/activity/reset", post(activity_reset_handler))
        .route("/api/local/image/attach", post(local_image_attach))
        .route("/api/local/image/detach", post(local_image_detach))
        .route("/api/local/image/status", get(local_image_status))
        .route("/api/ps5/cheats/list", get(cheats_list_handler))
        .route("/api/ps5/cheats/get", get(cheats_get_handler))
        .route("/api/ps5/cheats/toggle", post(cheats_toggle_handler))
        .route("/api/ps5/cheats/delete", get(cheats_delete_handler))
        .route("/api/ps5/cheats/reload", get(cheats_reload_handler))
        .route("/api/ps5/cheats/status", get(cheats_status_handler))
        .route(
            "/api/ps5/cheats/engine-set",
            post(cheats_engine_set_handler),
        )
        .route("/api/ps5/cheats/repos/list", get(cheats_repos_list_handler))
        .route(
            "/api/ps5/cheats/repos/search",
            get(cheats_repos_search_handler),
        )
        .route(
            "/api/ps5/cheats/repos/download",
            post(cheats_repos_download_handler),
        )
        .route("/api/ps5/activity/get", get(activity_get_handler))
        .route("/api/ps5/activity/db-query", get(activity_db_query_handler))
        .route("/api/ps5/sdk/scan", get(sdk_scan_handler))
        .route("/api/ps5/sdk/patch", post(sdk_patch_handler))
        .route("/api/ps5/sdk/restore", post(sdk_restore_handler))
        .route("/api/ps5/tmdb/fetch", get(tmdb_fetch_handler))
        .route("/api/ps5/fw-spoof/status", get(fw_spoof_status_handler))
        .route(
            "/api/remote/connections",
            get(remote::api::list_handler).post(remote::api::add_handler),
        )
        .route(
            "/api/remote/connections/{id}",
            axum::routing::put(remote::api::update_handler).delete(remote::api::delete_handler),
        )
        .route(
            "/api/remote/connections/{id}/test",
            post(remote::api::test_saved_handler),
        )
        .route("/api/remote/test", post(remote::api::test_form_handler))
        .route("/api/remote/list", post(remote::api::list_dir_handler))
        .route(
            "/api/remote/connections/{id}/host-key",
            post(remote::api::host_key_handler),
        )
        .route("/api/remote/fetch", post(remote::api::fetch_handler))
        .route(
            "/api/remote/fetch/cleanup",
            post(remote::api::fetch_cleanup_handler),
        )
        .route(
            "/api/remote/inspect-folder",
            post(remote::api::inspect_folder_handler),
        )
        .route("/api/remote/shares", post(remote::api::shares_form_handler))
        .route(
            "/api/remote/connections/{id}/shares",
            get(remote::api::shares_saved_handler),
        )
        .route("/api/ps5/saves/list", get(ps5_saves_list))
        .route("/api/ps5/screenshots/list", get(ps5_screenshots_list))
        .route("/api/ps5/videos/list", get(ps5_videos_list))
        .route("/api/ps5/smp/status", get(ps5_smp_status))
        .route("/api/ps5/smp/checkout", get(ps5_smp_checkout_status))
        .route("/api/ps5/smp/checkout/begin", post(ps5_smp_checkout_begin))
        .route(
            "/api/ps5/smp/checkout/finish",
            post(ps5_smp_checkout_finish),
        )
        .route("/api/ps5/smp/image-rw", get(ps5_smp_image_rw_status))
        .route("/api/ps5/smp/image-rw/begin", post(ps5_smp_image_rw_begin))
        .route(
            "/api/ps5/smp/image-rw/finish",
            post(ps5_smp_image_rw_finish),
        )
        .route("/api/ps5/hw/fan-threshold", post(ps5_hw_set_fan_threshold))
        .route("/api/ps5/peripheral", post(ps5_peripheral))
        .route("/api/ps5/fs/chmod", post(ps5_fs_chmod))
        .route("/api/ps5/fs/mkdir", post(ps5_fs_mkdir))
        .route("/api/ps5/fs/write-bytes", post(ps5_fs_write_bytes))
        .route("/api/ps5/game-meta", get(ps5_game_meta))
        .route("/api/ps5/game-icon", get(ps5_game_icon))
        .route("/api/ps5/apps/installed", get(ps5_apps_installed))
        .route("/api/ps5/app-icon", get(ps5_app_icon))
        .route("/api/transfer/file", post(transfer_file_handler))
        .route("/api/transfer/dir", post(transfer_dir_handler))
        .route("/api/transfer/zip", post(transfer_zip_handler))
        .route("/api/transfer/ps5-to-ps5", post(ps5_to_ps5_handler))
        .route("/api/local/path-kind", get(local_path_kind_handler))
        .route(
            "/api/local/inspect-folder",
            get(local_inspect_folder_handler),
        )
        .route("/api/bps/inspect", post(bps_inspect_handler))
        .route("/api/bps/apply", post(bps_apply_handler))
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
        .route("/api/rar/packages", post(rar_packages_handler))
        .route("/api/link/probe", post(link_probe_handler))
        .route("/api/link/download", post(link_download_handler))
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
        .route("/api/jobs/summaries", get(telemetry::summaries_handler))
        .route("/api/jobs/{id}/summary", get(telemetry::summary_handler))
        .route("/api/metrics", get(telemetry::metrics_handler))
        .route("/api/bug-report/bundle", post(bug_report_bundle_handler))
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

    let app = if let Ok(base) = std::env::var("PS5UPLOAD_BASE_URL") {
        let base = base.trim_end_matches('/');
        if !base.is_empty() {
            // Format Base URL
            let base = if !base.starts_with('/') {
                format!("/{}", base)
            } else {
                base.to_string()
            };
            // Create Axum Router using the base URL as a prefix for all routes.
            axum::Router::new()
                .nest(&base, app)
                .route(&format!("{}/", base), get(ui_handler))
        } else {
            app
        }
    } else {
        app
    };

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, request| {
            let Some(host) = request
                .headers
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
            else {
                return false;
            };
            origin
                .to_str()
                .ok()
                .is_some_and(|value| browser_origin_allows(value, host))
        }))
        .allow_methods(Any)
        .allow_headers(Any);

    let app = app
        // .pkg install — sessions live in their own state because the
        // HTTP-host serving handler needs Mutex-guarded session lookup
        // independent of the main engine state. Merged at this point
        // so the pkg routes share the same listener + CORS + body limit.
        .merge(pkg_install::router(std::sync::Arc::new(
            pkg_install::PkgInstallState::restored(),
        )))
        // Downloading a link to disk before installing it. Separate state for
        // the same reason as the install sessions: its progress is polled
        // independently of everything else the engine is doing.
        .merge(remote_download::router(std::sync::Arc::new(
            remote_download::DownloadRegistry::default(),
        )))
        // Tauri's renderer performs a few direct fetches to the sidecar for
        // liveness, job polling and streamed resources. Emit CORS headers only
        // for origins accepted by the same narrow trust rule as the guard
        // below. The guard sits outside this layer, so foreign origins are
        // rejected before CORS can answer either a simple request or preflight.
        .layer(cors)
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
        .layer(middleware::from_fn(browser_origin_guard))
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
        // body-limit / handler.
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
    // Mirror to stdout (terminal users) AND the engine.log ring
    // (post-mortem diagnosis from the desktop shell once the
    // terminal is closed). Printed only once the bind has succeeded:
    // it used to come first, so a log showing it proved nothing about
    // whether the engine was actually listening.
    println!("[ps5upload-engine] listening on http://{bind}  (ps5={ps5_addr})");
    crate::log_info!("listening on http://{bind}  (ps5={ps5_addr})");
    // Graceful shutdown: when the parent-watcher fires (stdin EOF), the
    // SHUTDOWN Notify wakes this future, axum stops
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

/// `ps5upload-engine --healthcheck`: exit 0 when this host's engine answers
/// `GET /api/jobs` with 200, 1 otherwise. The container images are `FROM
/// scratch` — no shell, no curl, no wget — so without this Docker and Compose
/// had nothing to run as a HEALTHCHECK. Plain std TCP, no async runtime, so it
/// works from the same binary in a fresh process.
pub fn healthcheck() -> i32 {
    use std::io::{Read, Write};
    let port: u16 = std::env::var("PS5UPLOAD_ENGINE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(19113);
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = std::time::Duration::from_secs(3);
    let ok = (|| -> std::io::Result<bool> {
        let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.write_all(
            format!(
                "GET /api/jobs HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )?;
        let mut head = [0u8; 16];
        let n = stream.read(&mut head)?;
        Ok(head[..n].starts_with(b"HTTP/1.1 200"))
    })()
    .unwrap_or(false);
    if ok {
        0
    } else {
        eprintln!("[ps5upload-engine] healthcheck: engine on {addr} did not answer 200");
        1
    }
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
    let ps5_addr = std::env::var("PS5_ADDR").unwrap_or_else(|_| "192.168.137.2".to_string());
    // Extra IPs allowed past the loopback guard (e.g. remote desktop
    // clients reaching a self-hosted engine). Comma-separated; unparseable
    // entries are dropped, unset → empty.
    // A bridged container gives the PS5 an address it cannot reach, so every
    // stream install fails. Say so at startup, where a self-hoster reads the
    // log, rather than only after the first failed install.
    if pkg_install::bridged_container_without_pkg_host_ip() {
        eprintln!(
            "[ps5upload-engine] WARNING: running in a container with bridge networking and no \
             PS5UPLOAD_PKG_HOST_IP. Stream installs will fail: the PS5 would be told to fetch \
             from the container's internal address. Use host networking (--network host), or \
             set PS5UPLOAD_PKG_HOST_IP to this host's LAN IP."
        );
    }
    let allow_raw = std::env::var("PS5UPLOAD_ALLOW_IP").unwrap_or_default();
    let allow_ips = parse_allow_ips(&allow_raw);
    let invalid = invalid_allow_ip_entries(&allow_raw);
    if !invalid.is_empty() {
        eprintln!(
            "[ps5upload-engine] WARNING: ignoring PS5UPLOAD_ALLOW_IP entries that are not an IP \
             or CIDR range: {}",
            invalid.join(", ")
        );
    }
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
                .map(|rule| rule.to_string())
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
mod list_dir_status_tests {
    use super::*;

    #[test]
    fn a_missing_folder_is_404_and_anything_else_stays_502() {
        let missing = "payload rejected FS_LIST_DIR(/mnt/usb0/ps5upload/pkg_library): fs_list_dir_opendir_errno_2";
        assert_eq!(list_dir_error_status(missing), StatusCode::NOT_FOUND);
        // EACCES, errno 20 (ENOTDIR) and a dead link are not "not there".
        assert_eq!(
            list_dir_error_status("payload rejected FS_LIST_DIR(/x): fs_list_dir_opendir_errno_13"),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            list_dir_error_status("payload rejected FS_LIST_DIR(/x): fs_list_dir_opendir_errno_20"),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            list_dir_error_status("connect 192.168.86.99:9120: timed out"),
            StatusCode::BAD_GATEWAY
        );
    }
}

#[cfg(test)]
mod external_scan_tests {
    use super::skip_in_external_scan;

    #[test]
    fn the_scan_skips_our_own_package_library_on_a_drive() {
        // Packages staged on a USB/M.2 drive live in <drive>/ps5upload/; they
        // are already in the library and must not show again as "external".
        assert!(skip_in_external_scan("/mnt/usb0", 0, "ps5upload"));
        assert!(skip_in_external_scan("/mnt/usb0", 0, "PS5Upload"));
        // Only at the drive's top level: a user folder deeper down is scanned.
        assert!(!skip_in_external_scan("/mnt/usb0", 1, "ps5upload"));
        assert!(!skip_in_external_scan("/mnt/usb0", 0, "games"));
    }

    #[test]
    fn the_scan_skips_installed_games_on_extended_storage() {
        // A user was offered DOOM's installed update,
        // /mnt/ext0/user/patch/CUSA02092/patch.pkg, as a package to install.
        assert!(skip_in_external_scan("/mnt/ext0", 0, "user"));
        assert!(skip_in_external_scan("/mnt/ext1", 0, "user"));
        // A USB stick's own "user" folder is the user's, not the console's.
        assert!(!skip_in_external_scan("/mnt/usb0", 0, "user"));
        // Deeper down it's just a folder name.
        assert!(!skip_in_external_scan("/mnt/ext0", 1, "user"));
    }
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
            allowed_ips: ips.iter().map(|s| AllowRule::parse(s).unwrap()).collect(),
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
        assert!(parse_allow_ips("").is_empty());
        let rules = parse_allow_ips(" 192.168.1.50 , ::1 ,, not-an-ip , 10.0.0.9");
        let shown: Vec<String> = rules.iter().map(|r| r.to_string()).collect();
        assert_eq!(shown, ["192.168.1.50", "::1", "10.0.0.9"]);
        assert_eq!(
            invalid_allow_ip_entries(" 192.168.1.50 , ::1 ,, not-an-ip , 10.0.0.0/33"),
            ["not-an-ip", "10.0.0.0/33"]
        );
    }

    #[test]
    fn a_cidr_range_admits_its_whole_subnet() {
        // A homelab engine is driven from phones and laptops whose DHCP
        // leases change; an exact-IP list meant a new 403 per renewal.
        let c = cfg(&["192.168.1.0/24"]);
        assert!(loopback_allows(&c, ip("192.168.1.7"), "/api/jobs"));
        assert!(loopback_allows(&c, ip("192.168.1.254"), "/api/jobs"));
        assert!(!loopback_allows(&c, ip("192.168.2.7"), "/api/jobs"));
        // A v4 client on a dual-stack socket arrives as ::ffff:a.b.c.d.
        assert!(loopback_allows(&c, ip("::ffff:192.168.1.7"), "/api/jobs"));
        // /0 is everything; a bare IP is /32.
        assert!(loopback_allows(
            &cfg(&["0.0.0.0/0"]),
            ip("8.8.8.8"),
            "/api/jobs"
        ));
        assert_eq!(
            AllowRule::parse("10.0.0.9").unwrap().to_string(),
            "10.0.0.9"
        );
        assert_eq!(
            AllowRule::parse("fd00::/8").unwrap().to_string(),
            "fd00::/8"
        );
        assert!(loopback_allows(
            &cfg(&["fd00::/8"]),
            ip("fd12::1"),
            "/api/jobs"
        ));
    }

    #[test]
    fn browser_guard_rejects_cross_site_fetches() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "127.0.0.1:19113".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!browser_request_allows(&headers, "/api/jobs"));
        assert!(browser_request_allows(
            &headers,
            "/pkg-host/session/file.pkg"
        ));
    }

    #[test]
    fn browser_guard_allows_same_origin_and_loopback_dev() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "nas.local:19113".parse().unwrap());
        headers.insert("origin", "http://nas.local:19113".parse().unwrap());
        assert!(browser_request_allows(&headers, "/api/jobs"));

        headers.insert("host", "127.0.0.1:19113".parse().unwrap());
        headers.insert("origin", "http://localhost:1420".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(browser_request_allows(&headers, "/api/jobs"));
    }

    #[test]
    fn browser_guard_allows_tauri_renderer() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "127.0.0.1:19113".parse().unwrap());
        headers.insert("origin", "http://tauri.localhost".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(browser_request_allows(&headers, "/api/jobs"));

        headers.insert("origin", "tauri://localhost".parse().unwrap());
        assert!(browser_request_allows(&headers, "/api/jobs"));
    }

    #[test]
    fn browser_guard_allows_tauri_cover_images_without_origin() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "127.0.0.1:19113".parse().unwrap());
        headers.insert("referer", "http://tauri.localhost/library".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        headers.insert("sec-fetch-dest", "image".parse().unwrap());

        assert!(browser_request_allows(&headers, "/api/ps5/app-icon"));
        assert!(browser_request_allows(&headers, "/api/ps5/game-icon"));
    }

    #[test]
    fn browser_guard_keeps_originless_cross_site_access_narrow() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "127.0.0.1:19113".parse().unwrap());
        headers.insert("referer", "http://tauri.localhost/library".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        headers.insert("sec-fetch-dest", "image".parse().unwrap());

        assert!(!browser_request_allows(&headers, "/api/jobs"));

        headers.insert("referer", "https://evil.example/".parse().unwrap());
        assert!(!browser_request_allows(&headers, "/api/ps5/app-icon"));

        headers.insert("referer", "http://tauri.localhost/".parse().unwrap());
        headers.insert("sec-fetch-dest", "empty".parse().unwrap());
        assert!(!browser_request_allows(&headers, "/api/ps5/app-icon"));
    }

    #[test]
    fn browser_guard_rejects_foreign_origin() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "127.0.0.1:19113".parse().unwrap());
        headers.insert("origin", "https://evil.example".parse().unwrap());
        assert!(!browser_request_allows(&headers, "/api/ps5/fs/delete"));
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
        // A "finished" transfer is one whose strong Arc has been dropped.
        // The registry holds only a Weak, so after the Arc drops,
        // weak.upgrade() returns None and the entry is pruned on the next
        // register call.
        let finished = Uuid::new_v4();
        drop(register_transfer_cancel(finished)); // we drop our returned Arc
                                                  // Registering any new job prunes the finished one.
        let live = register_transfer_cancel(Uuid::new_v4());
        let _hold = Arc::clone(&live); // keep the live one's Arc alive
        assert!(
            !signal_transfer_cancel(finished),
            "finished entry was pruned on the next register",
        );
    }

    #[test]
    fn signal_for_dead_weak_is_false() {
        // Regression: after the Arc is dropped, signal_transfer_cancel must
        // return false (not panic) because the Weak can no longer upgrade.
        let id = Uuid::new_v4();
        let flag = register_transfer_cancel(id);
        assert!(signal_transfer_cancel(id), "live transfer is signalable");
        drop(flag);
        assert!(
            !signal_transfer_cancel(id),
            "dead weak returns false, not panic",
        );
    }
}

#[cfg(test)]
mod helpers_tests {
    use super::*;

    /// #275 — `POST /api/transfer/dir` used to await the full source walk
    /// before returning the job_id, so the client sat on "Starting…" for the
    /// whole walk (minutes on a Docker bind-mount) with no progress and no
    /// way to cancel. The id must now come back immediately, which also
    /// means a bad source can no longer be a 400: it surfaces as a job
    /// failure instead. This test pins the second half, which is the
    /// observable consequence of the first.
    #[tokio::test]
    async fn transfer_dir_reports_a_bad_source_as_a_job_failure_not_a_400() {
        let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _rx) = broadcast::channel(16);
        let state = AppState {
            jobs: Arc::clone(&jobs),
            default_ps5_addr: "127.0.0.1:1".to_string(),
            events_tx,
        };
        let req = TransferDirReq {
            addr: Some("127.0.0.1:1".to_string()),
            tx_id: None,
            dest_root: "/data/nope".to_string(),
            src_dir: "/definitely/not/a/real/directory/for/tests".to_string(),
            excludes: vec![],
            bandwidth_cap_mbps: None,
            skip_existing: None,
        };

        let resp = transfer_dir_handler(State(state), Json(req))
            .await
            .into_response();
        assert_eq!(
            resp.status(),
            StatusCode::ACCEPTED,
            "the job id must be handed out before the walk, so a missing \
             source can no longer be rejected synchronously"
        );

        // The spawned task marks the job Failed. Poll briefly rather than
        // sleeping a fixed amount so the test isn't timing-fragile.
        let mut failed = None;
        for _ in 0..100 {
            {
                let g = jobs.lock().unwrap();
                if let Some((_, JobState::Failed { error, .. })) =
                    g.iter().next().map(|(k, v)| (*k, v.clone()))
                {
                    failed = Some(error);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let error = failed.expect("job should have been marked Failed");
        assert!(
            error.contains("source directory not found"),
            "unexpected error: {error}"
        );
    }

    /// A folder on a saved server is walked AND sent from the server: the transfer must get
    /// the server path, not the raw `remote://` string (which every server answers NotFound).
    #[tokio::test(flavor = "multi_thread")]
    async fn transfer_dir_sends_a_server_folder_from_the_server() {
        let r = crate::remote::pool::testing::install_global(&[
            ("/g/eboot.bin", b"0123456789"),
            ("/g/sce_sys/param.json", b"{}"),
        ]);
        let id = r
            .store
            .add(
                crate::remote::store::conn("NAS", crate::remote::store::Protocol::Smb),
                crate::remote::store::Secret::None,
            )
            .unwrap()
            .conn
            .id;
        let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _rx) = broadcast::channel(16);
        let state = AppState {
            jobs: Arc::clone(&jobs),
            default_ps5_addr: "127.0.0.1:1".to_string(),
            events_tx,
        };
        let req = TransferDirReq {
            addr: Some("127.0.0.1:1".to_string()),
            tx_id: None,
            dest_root: "/data/x".to_string(),
            src_dir: format!("remote://{id}/g"),
            excludes: vec![],
            bandwidth_cap_mbps: None,
            skip_existing: None,
        };
        let _ = transfer_dir_handler(State(state), Json(req))
            .await
            .into_response();
        let mut failed = None;
        for _ in 0..250 {
            if let Some(JobState::Failed { error, .. }) =
                jobs.lock().unwrap().values().next().cloned()
            {
                failed = Some(error);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // No console answers at 127.0.0.1:1, so the job fails — but at the console, having
        // read the folder, not at the source.
        let error = failed.expect("the job ends (no console to send to)");
        assert!(
            !error.contains("remote://"),
            "read the raw remote path: {error}"
        );
        assert!(
            !error.contains("readdir"),
            "could not list the source: {error}"
        );
    }

    #[tokio::test]
    async fn an_unknown_skip_existing_mode_is_a_bad_request() {
        let (events_tx, _rx) = broadcast::channel(16);
        let state = AppState {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            default_ps5_addr: "127.0.0.1:1".to_string(),
            events_tx,
        };
        let req = TransferDirReq {
            addr: Some("127.0.0.1:1".to_string()),
            tx_id: None,
            dest_root: "/data/x".to_string(),
            src_dir: "/nowhere".to_string(),
            excludes: vec![],
            bandwidth_cap_mbps: None,
            skip_existing: Some("sometimes".to_string()),
        };
        let resp = transfer_dir_handler(State(state), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn an_all_skipped_ack_reports_nothing_sent() {
        let ack =
            r#"{"protocol":"ava1","files":5,"skipped_files":5,"skipped_bytes":900,"files_sent":0}"#;
        assert_eq!(ava1_skip_counts(ack), Some((5, 900, 0)));
        assert_eq!(ava1_skip_counts(r#"{"protocol":"other"}"#), None);
        assert_eq!(ava1_skip_counts("not json"), None);
    }

    #[tokio::test]
    async fn progress_ticker_adopts_a_late_discovered_total() {
        let job_id = Uuid::new_v4();
        let jobs = Arc::new(Mutex::new(HashMap::from([(
            job_id,
            JobState::Running {
                stage: None,
                started_at_ms: 1,
                bytes_sent: 0,
                total_bytes: 0,
                files: vec![],
                skipped_files: 0,
                skipped_bytes: 0,
                files_processing: 0,
                files_finalized: 0,
                files_finalizing_total: 0,
                bytes_finalized: 0,
            },
        )])));
        let (events_tx, _) = broadcast::channel::<String>(8);
        let progress = Arc::new(AtomicU64::new(37));
        let dynamic_total = Arc::new(AtomicU64::new(0));
        let stop = spawn_progress_ticker(
            Arc::clone(&jobs),
            events_tx,
            job_id,
            TickerContext {
                started_at_ms: 1,
                total_bytes: 0,
                dynamic_total_bytes: Some(Arc::clone(&dynamic_total)),
                skipped_files: 0,
                skipped_bytes: 0,
            },
            Arc::clone(&progress),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );

        tokio::time::sleep(Duration::from_millis(260)).await;
        progress.store(0, Ordering::Release);
        dynamic_total.store(100, Ordering::Release);
        tokio::time::sleep(Duration::from_millis(260)).await;

        let guard = jobs.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get(&job_id) {
            Some(JobState::Running {
                bytes_sent,
                total_bytes,
                ..
            }) => {
                assert_eq!(*bytes_sent, 0);
                assert_eq!(*total_bytes, 100);
            }
            _ => panic!("expected running job"),
        }
        drop(guard);
        stop.store(true, Ordering::Release);
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
    fn a_running_snapshot_carries_the_live_notes_and_a_finished_one_does_not() {
        use ps5upload_core::transfer::{LiveNotes, LIVE_PHASE_SKIPPING};
        use std::sync::atomic::Ordering::Relaxed;
        let n = LiveNotes::default();
        let running = serde_json::json!({"status": "running", "bytes_sent": 1});
        // Nothing reported: the snapshot is unchanged (the client shows nothing).
        assert_eq!(merge_live_notes(Some(&n), running.clone()), running);
        n.bottleneck.store(ava1::gen::BN_DISK, Relaxed);
        n.phase.store(LIVE_PHASE_SKIPPING, Relaxed);
        n.skip_done_bytes.store(5, Relaxed);
        n.skip_total_bytes.store(20, Relaxed);
        let v = merge_live_notes(Some(&n), running.clone());
        assert_eq!(v["bottleneck"], "console drive");
        assert_eq!(v["phase"], "skipping");
        assert_eq!(
            (
                v["skip_done_bytes"].as_u64(),
                v["skip_total_bytes"].as_u64()
            ),
            (Some(5), Some(20))
        );
        assert!(
            v.get("settling").is_none(),
            "settling stays absent until the receiver says so"
        );
        assert!(v.get("settle_files_left").is_none());
        n.settling.store(true, Relaxed);
        n.unswept.store(12, Relaxed);
        n.unswept_peak.store(50, Relaxed);
        let v = merge_live_notes(Some(&n), running);
        assert_eq!(v["settling"], true);
        assert_eq!(
            (
                v["settle_files_left"].as_u64(),
                v["settle_files_total"].as_u64()
            ),
            (Some(12), Some(50))
        );
        let done = serde_json::json!({"status": "done"});
        assert_eq!(merge_live_notes(Some(&n), done.clone()), done);
    }

    #[test]
    fn post_addr_reads_the_query_or_the_json_body() {
        // The desktop app sends ?addr=, the browser build a JSON body.
        assert_eq!(
            post_addr(Some("10.0.0.5".into()), b"{}"),
            Some("10.0.0.5".into())
        );
        assert_eq!(
            post_addr(None, br#"{"addr":"10.0.0.7"}"#),
            Some("10.0.0.7".into())
        );
        // both: the query wins
        assert_eq!(
            post_addr(Some("10.0.0.5".into()), br#"{"addr":"10.0.0.7"}"#),
            Some("10.0.0.5".into())
        );
        // nothing usable: the caller falls back to the default console
        assert_eq!(post_addr(None, b""), None);
        assert_eq!(post_addr(None, b"{}"), None);
        assert_eq!(post_addr(None, br#"{"addr":null}"#), None);
        assert_eq!(post_addr(Some("".into()), br#"{"addr":" "}"#), None);
        assert_eq!(post_addr(None, b"not json"), None);
    }

    #[test]
    fn console_addr_or_default_uses_default_when_none() {
        assert_eq!(
            console_addr_or_default(None, "192.168.0.1:9120"),
            "192.168.0.1"
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
    fn a_link_download_rides_out_a_slow_host_but_not_a_dead_one() {
        let stall = || -> anyhow::Result<()> {
            Err(ps5upload_ava1::upload::UploadFailure {
                reason: "ava1_stalled".into(),
                detail: "console refused the transfer (18): progress stalled".into(),
            }
            .into())
        };
        // The host is slow: the console gave up waiting, the download did not.
        let mut n = 0;
        assert_eq!(
            link_stall_verdict(&stall(), false, false, &mut n),
            LinkStall::Retry
        );
        // Progress since the last stall starts the count again.
        n = LINK_STALL_TRIES;
        assert_eq!(
            link_stall_verdict(&stall(), false, true, &mut n),
            LinkStall::Retry
        );
        assert_eq!(n, 1);
        // Nothing durable for every try: the failure is the result.
        n = LINK_STALL_TRIES;
        assert_eq!(
            link_stall_verdict(&stall(), false, false, &mut n),
            LinkStall::Done
        );
        // A cancel, a success and any other failure are never retried.
        let mut n = 0;
        assert_eq!(
            link_stall_verdict(&stall(), true, false, &mut n),
            LinkStall::Done
        );
        assert_eq!(
            link_stall_verdict(&Ok(()), false, false, &mut n),
            LinkStall::Done
        );
        let other: anyhow::Result<()> = Err(anyhow::anyhow!("no route to host"));
        assert_eq!(
            link_stall_verdict(&other, false, false, &mut n),
            LinkStall::Done
        );
        assert_eq!(n, 0);
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

    // ── AVA1 post-commit mapping — the reason pair the client keys on (A1) ────

    #[test]
    fn an_ava1_refusal_reaches_the_job_with_its_reason_and_detail() {
        let e = anyhow::Error::from(ps5upload_ava1::upload::UploadFailure {
            reason: "ava1_no_space".into(),
            detail: "console drive is full".into(),
        });
        match job_failed_from_err(100, 200, &e) {
            JobState::Failed {
                error_reason,
                error_detail,
                ..
            } => {
                assert_eq!(error_reason.as_deref(), Some("ava1_no_space"));
                assert_eq!(error_detail.as_deref(), Some("console drive is full"));
            }
            _ => panic!("expected Failed state"),
        }
    }

    #[test]
    fn a_post_commit_failure_has_its_own_reason() {
        let pce = ps5upload_ava1::PostCommitError {
            kind: ps5upload_ava1::PostCommitKind::Exists,
            detail: "the destination already exists on the console".to_string(),
        };
        let e = anyhow::Error::from(pce);
        let state = job_failed_from_err(1000, 2000, &e);
        match state {
            JobState::Failed {
                error,
                error_reason,
                error_detail,
                ..
            } => {
                assert_eq!(error_reason.as_deref(), Some("ava1_commit_exists"));
                assert_eq!(
                    error_detail.as_deref(),
                    Some("the destination already exists on the console"),
                    "error_detail carries the console's own message"
                );
                // The raw chain is still there for the UI's raw view.
                assert!(error.contains("the console refused to commit the transfer"));
            }
            _ => panic!("expected Failed state"),
        }
    }

    #[test]
    fn a_post_commit_cross_device_failure_has_its_own_reason() {
        // The second half of the pair the client keys on: one test per reason
        // (A1 pins the exact strings in both directions).
        let pce = ps5upload_ava1::PostCommitError {
            kind: ps5upload_ava1::PostCommitKind::CrossDevice,
            detail: "the destination is on another storage device".to_string(),
        };
        let e = anyhow::Error::from(pce);
        let state = job_failed_from_err(1000, 2000, &e);
        match state {
            JobState::Failed { error_reason, .. } => {
                assert_eq!(error_reason.as_deref(), Some("ava1_commit_cross_device"))
            }
            _ => panic!("expected Failed state"),
        }
    }

    #[test]
    fn a_post_commit_error_is_not_retryable() {
        // This pins the typed reason. The adapter's early return on this status
        // is the retry guarantee; this helper does not run the upload loop.
        let pce = ps5upload_ava1::PostCommitError {
            kind: ps5upload_ava1::PostCommitKind::Exists,
            detail: "destination taken".to_string(),
        };
        let e = anyhow::Error::from(pce);
        assert!(
            !ps5upload_core::transfer::is_retryable_transfer_error(&e),
            "a post-commit refusal carries no retryable io::Error"
        );
        // Wrapped the way a handler might pass it down, the classifier
        // must still say no.
        let wrapped = e.context("upload pipeline failed");
        assert!(!ps5upload_core::transfer::is_retryable_transfer_error(
            &wrapped
        ));
    }

    #[test]
    fn the_post_commit_branch_does_not_swallow_payload_json() {
        // Regression: the new typed branch must only fire on the typed
        // error — a payload JSON body that is NOT a PostCommitError still
        // takes the extract_payload_error path.
        let inner = anyhow::anyhow!(
            "CommitTx rejected: {{\"error\":\"preflight_insufficient_space\",\"detail\":\"/mnt/ext0 short by 28 GiB\"}}"
        );
        let state = job_failed_from_err(1000, 2000, &inner);
        match state {
            JobState::Failed {
                error_reason,
                error_detail,
                ..
            } => {
                assert_eq!(
                    error_reason.as_deref(),
                    Some("preflight_insufficient_space")
                );
                assert_eq!(error_detail.as_deref(), Some("/mnt/ext0 short by 28 GiB"));
            }
            _ => panic!("expected Failed state"),
        }
    }
}

#[cfg(test)]
mod cheats_route_tests {
    use super::*;

    #[test]
    fn cheats_toggle_req_deserializes_with_default_on() {
        let json = r#"{"title_id":"CUSA00001","index":0}"#;
        let req: CheatsToggleReq = serde_json::from_str(json).unwrap();
        assert_eq!(req.title_id, "CUSA00001");
        assert_eq!(req.index, 0);
        assert!(req.on, "on should default to true");
    }

    #[test]
    fn cheats_toggle_req_deserializes_explicit_off() {
        let json = r#"{"title_id":"CUSA00002","index":3,"on":false}"#;
        let req: CheatsToggleReq = serde_json::from_str(json).unwrap();
        assert_eq!(req.title_id, "CUSA00002");
        assert_eq!(req.index, 3);
        assert!(!req.on);
    }

    #[test]
    fn cheats_engine_set_req_deserializes() {
        let json = r#"{"enabled":true}"#;
        let req: CheatsEngineSetReq = serde_json::from_str(json).unwrap();
        assert!(req.enabled);
        assert!(req.addr.is_none());
    }
}

#[cfg(test)]
mod bug_bundle_tests {
    use super::bundle_path_ok;

    #[test]
    fn accepts_the_paths_the_bundle_actually_uses() {
        for p in [
            "report.json",
            "README.txt",
            "logs/app.jsonl",
            "logs/engine.log",
            "ps5/klog.txt",
            "ps5/payload-logs/03_ava_events.log",
            "images/shot.png",
        ] {
            assert!(bundle_path_ok(p), "should accept {p}");
        }
    }

    #[test]
    fn rejects_anything_that_could_escape_the_archive() {
        // The client builds these names, but this is a network service: a
        // caller on the LAN must not be able to write outside the zip root or
        // produce a path that surprises the extractor.
        let null_path = format!("with{}null.txt", '\0');
        let cases: Vec<String> = vec![
            String::new(),
            "/etc/passwd".into(),
            "\\windows\\system32".into(),
            "../outside.txt".into(),
            "logs/../../etc/hosts".into(),
            "C:/Users/me/thing.txt".into(),
            "logs//double.txt".into(),
            "logs/./same.txt".into(),
            null_path,
        ];
        for p in &cases {
            assert!(!bundle_path_ok(p), "should reject {p:?}");
        }
    }

    #[test]
    fn rejects_absurdly_long_paths() {
        assert!(!bundle_path_ok(&"a".repeat(201)));
        assert!(bundle_path_ok(&"a".repeat(200)));
    }
}

#[cfg(test)]
mod account_id_input_tests {
    use super::AccountIdInput;

    #[test]
    fn a_full_64_bit_id_survives_as_a_string() {
        // The reason the string form exists: this value is above 2^53, so a
        // JavaScript client sending it as a JSON number would round it and
        // activate a different account than the user typed.
        let big = 0x0123_4567_89ab_cdefu64;
        assert!(big > (1u64 << 53));
        assert_eq!(
            AccountIdInput::Str("0x0123456789abcdef".into()).to_u64(),
            Some(big)
        );
        assert_eq!(AccountIdInput::Str(big.to_string()).to_u64(), Some(big));
        assert_eq!(
            AccountIdInput::Str("0xffffffffffffffff".into()).to_u64(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn accepts_hex_either_case_and_plain_decimal() {
        assert_eq!(AccountIdInput::Str("0x1A2B".into()).to_u64(), Some(0x1a2b));
        assert_eq!(AccountIdInput::Str("0X1a2b".into()).to_u64(), Some(0x1a2b));
        assert_eq!(AccountIdInput::Str("  6789 ".into()).to_u64(), Some(6789));
        assert_eq!(AccountIdInput::Num(6789).to_u64(), Some(6789));
    }

    #[test]
    fn rejects_zero_and_rubbish() {
        // Zero means "no account" — clearing a slot is its own endpoint, so
        // writing zero through activate would be a confusing way to do it.
        assert_eq!(AccountIdInput::Num(0).to_u64(), None);
        assert_eq!(AccountIdInput::Str("0".into()).to_u64(), None);
        assert_eq!(AccountIdInput::Str("0x0".into()).to_u64(), None);
        assert_eq!(AccountIdInput::Str("".into()).to_u64(), None);
        assert_eq!(AccountIdInput::Str("0x".into()).to_u64(), None);
        assert_eq!(AccountIdInput::Str("nonsense".into()).to_u64(), None);
        // Would overflow u64 — must not wrap into a valid-looking id.
        assert_eq!(
            AccountIdInput::Str("0x1ffffffffffffffff".into()).to_u64(),
            None
        );
    }
}

#[cfg(test)]
mod job_stage_tests {
    use super::*;

    fn running(stage: Option<JobStage>) -> JobState {
        JobState::Running {
            started_at_ms: 1,
            bytes_sent: 10,
            total_bytes: 100,
            files: Vec::new(),
            skipped_files: 0,
            skipped_bytes: 0,
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
            stage,
        }
    }

    /// A build job's snapshot names the stage it is in, which the Convert screen lists.
    #[test]
    fn a_running_job_carries_its_stage() {
        let v = serde_json::to_value(running(Some(JobStage {
            id: "compress".into(),
            index: 2,
            count: 5,
            done: 7,
            total: 9,
        })))
        .unwrap();
        assert_eq!(v["stage"]["id"], "compress");
        assert_eq!(v["stage"]["index"], 2);
        assert_eq!(v["stage"]["done"], 7);
        let none = serde_json::to_value(running(None)).unwrap();
        assert!(none.get("stage").is_none());
    }
}

#[cfg(test)]
mod appdb_installed_additions_tests {
    use super::appdb_installed_additions;
    use std::collections::HashSet;

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Measured on FW 13.60 (both consoles): app.db lists homebrew apps that
    /// have no /user/appmeta folder, shell entries, and a preinstalled tile
    /// that was never downloaded. Only the first kind is installed.
    #[test]
    fn keeps_real_apps_drops_shell_entries_and_undownloaded_tiles() {
        let rows = vec![
            ("PLDM00001".to_string(), "Payload Manager".to_string()),
            ("NPXS40056".to_string(), "All Apps".to_string()),
            ("PPSA01325".to_string(), "ASTRO's PLAYROOM".to_string()),
            ("FAKE10101".to_string(), "FAKE10101".to_string()),
            ("PLDM00001".to_string(), "Payload Manager".to_string()),
        ];
        let dirs = set(&["PLDM00001", "FAKE10101", "NPXS40056"]);
        let got = appdb_installed_additions(&rows, &dirs, &HashSet::new());
        assert_eq!(
            got,
            vec![
                ("PLDM00001".to_string(), Some("Payload Manager".to_string())),
                // name equal to the id is not a name
                ("FAKE10101".to_string(), None),
            ]
        );
    }

    #[test]
    fn skips_titles_the_appmeta_scan_already_found() {
        let rows = vec![("PLDM00001".to_string(), "Payload Manager".to_string())];
        let got = appdb_installed_additions(&rows, &set(&["PLDM00001"]), &set(&["PLDM00001"]));
        assert!(got.is_empty());
    }
}
