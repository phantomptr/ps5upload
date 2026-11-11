//! ps5upload-engine — local HTTP service that drives AVA1 transfers.
//!
//! This is a library crate so the engine can be consumed two ways: the
//! desktop sidecar binary (`src/main.rs`) calls `run_cli()`, while the
//! Tauri mobile build links the crate and calls `serve_in_process()` on
//! a background task (Android/iOS have no sidecar-binary spawn model, so
//! the server runs inside the app process). The `windows_subsystem`
//! console suppression lives on the binary (`main.rs`), not here.
//!
//! Listens on 0.0.0.0:19113 by default (set PS5UPLOAD_ENGINE_PORT env var to override). Peers
//! are gated by the `loopback_guard` middleware: loopback and the IPs in PS5UPLOAD_ALLOW_IP
//! (comma-separated, for remote clients) reach everything; any other peer only the few routes
//! `loopback_allows` names. PS5 address defaults to 192.168.137.2 (set PS5_ADDR to override).
//!
//! The API is the route table in `build_app`, plus the sub-routers it merges.

mod activity;
mod ampr_emu_api;
mod ampr_index_api;
mod ampr_trace;
mod api_errors;
mod app_dump_api;
mod ava1_api;
mod browser_payload;
mod bundled_payload;
mod c2c_api;
mod collection;
mod collection_api;
mod collection_check_api;
mod collection_tidy_api;
mod console_backup;
mod console_cleanup_api;
mod console_files_api;
mod console_move;
mod console_payload;
mod console_read;
mod console_shares_api;
mod console_snapshot;
mod console_tasks_api;
mod console_tools_api;
mod consoles_api;
mod convert_source;
mod diag_api;
mod disk_usage;
mod download_relay;
mod elfldr_guard;
mod emulator_saves;
mod engine_log;
mod engine_update;
mod event_bus;
mod event_journal;
mod fakelibs_api;
mod fan_boost;
mod ffmpeg_tool;
mod folder_build;
mod fpkg_api;
mod fpkg_firmware;
mod frame_decode;
mod game_fps_api;
mod games_api;
mod golive_dest;
mod golive_ffmpeg;
mod helper_copies_api;
mod helper_mirror;
mod helper_stop;
mod host_guard;
mod icon_cache;
mod image_browse;
mod image_build;
mod inspect;
mod install;
mod join;
mod join_net;
mod link;
mod link_lz4;
mod local_fs;
mod log_dedup;
mod log_levels;
mod mcp;
mod mesh_api;
mod mesh_drive;
mod mesh_drive_plan;
mod mgmt_route;
mod migrate_6;
mod net_doctor;
mod net_health;
mod openapi;
mod ops;
mod pad_link;
mod payload_updates_api;
mod payloads_api;
mod pkg_install;
mod pkg_sidecar;
mod pldmgr_api;
mod profile_image;
mod ps2_saves;
mod queue_api;
mod remote;
mod remote_download;
mod remote_pkg;
mod rp_diag;
mod rp_golive;
mod rp_keyboard;
mod rp_live_ts;
mod rp_replay;
mod rp_stream_api;
mod save_backup;
mod save_backup_server;
mod save_copy;
mod server_out;
mod settings_api;
mod settings_backup;
mod share;
mod shared_state;
mod state_io;
mod tailnet_api;
mod tailnet_discover;
pub(crate) mod tailnet_engine;
pub(crate) mod tailnet_system;
mod telemetry;
mod vinput_api;
mod wake_login_api;
mod web_pages;
#[cfg(feature = "webui")]
mod webui;
mod win_net;
mod workshop_api;
mod zstd_packs;
mod zstd_runtime;

#[cfg(test)]
mod api_surface_tests;
#[cfg(test)]
mod ava1_only_tests;
#[cfg(test)]
mod event_hooks_tests;
#[cfg(test)]
mod queue_api_tests;

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
        app_launch, app_list_registered, app_register, app_unregister, fs_delete_with_op_id,
        fs_mkdir, fs_mount, fs_op_cancel, fs_op_status, fs_read, fs_read_with_timeout, fs_unmount,
        list_dir, reconcile, DirListing, ListDirOptions, MountResult, ReconcileMode,
        RegisterResult,
    },
    game_meta::{parse_param_json_bytes, parse_param_sfo_bytes},
    hw::{
        drive_sensors, hw_info, hw_power, hw_set_fan_threshold_ex, hw_storage,
        hw_temps_with_clocks, proc_list, DriveSensorList, HwInfo, HwPower, HwStorage, HwTemps,
        ProcList,
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
    volumes::{list_volumes, VolumeList},
};
use tower_http::cors::{AllowOrigin, Any, CorsLayer};

/// The renames a game folder upload needs so each module carries the letter case its
/// `eboot.bin` loads it by. The console's /data is case-sensitive where the exFAT image a
/// folder often came from is not: a Unity dump with `Il2cppUserAssemblies.prx`, loaded as
/// `Il2CppUserAssemblies.prx`, played from the image and crashed a second in from the folder
/// (PPSA22999, PPSA31909). Empty for anything that is not a game folder; never fails an upload.
fn module_case_renames(
    fs: Option<&dyn ps5upload_core::source_fs::SourceFs>,
    root: &std::path::Path,
    requested: &str,
) -> Vec<(String, String)> {
    let eboot = root.join("eboot.bin");
    let is_game = match fs {
        Some(fs) => fs.metadata(&eboot).is_ok_and(|m| !m.is_dir),
        None => eboot.is_file(),
    };
    if !is_game {
        return Vec::new();
    }
    let opened = match fs {
        Some(_) => ps5upload_fpkg::source::open(std::path::Path::new(requested)),
        None => ps5upload_fpkg::source::open(root),
    };
    let Ok(mut tree) = opened else {
        return Vec::new();
    };
    let fixes = ps5upload_fpkg::module_case::tree_fixes(tree.as_mut());
    if !fixes.is_empty() {
        crate::log_info!(
            "transfer_dir: uploading under the eboot's spelling: {}",
            ps5upload_fpkg::module_case::describe(&fixes)
        );
    }
    fixes.into_iter().map(|c| (c.from, c.to)).collect()
}

/// Build a `TransferConfig` for the given address. The transport's own knobs (in-flight
/// window, pack sizes) belong to AVA1's governor, so the retired shard-tuning environment
/// variables are not read. The outbound cap (`PS5UPLOAD_BANDWIDTH_MBPS`) is the one that stays.
fn make_transfer_config(addr: &str) -> TransferConfig {
    let mut cfg = TransferConfig::new(addr);
    cfg.bandwidth_cap_bps = bandwidth_cap_from_env(&|k| std::env::var(k).ok(), process_warned());
    // "Check files on the console after writing" (#783), from the app's shared settings, so
    // every way an upload starts (the app, the queue, a script, an agent) honours it.
    cfg.verify_writes = settings_api::flag("verify_writes");
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
    // At least 1 B/s: a tiny cap must not round to 0, which reads as "no cap".
    Some(((mbps * 1024.0 * 1024.0) as u64).max(1))
}

/// Apply a per-request bandwidth cap to the config. None / 0 / negative
/// = leave the existing cap in place; positive values override.
/// Centralised so all transfer entry points apply the same precedence rule.
fn apply_per_request_bandwidth(cfg: &mut TransferConfig, cap_mbps: Option<f64>) {
    if let Some(n) = cap_mbps {
        if n > 0.0 {
            cfg.bandwidth_cap_bps = Some(((n * 1024.0 * 1024.0) as u64).max(1));
        } else if n == 0.0 {
            // Explicit 0 = "unlimited".
            cfg.bandwidth_cap_bps = None;
        }
    }
}
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{wrappers::ReceiverStream, StreamExt as _};
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
        /// Ordered list of files this job will send. For folder uploads
        /// this is the planned delta (reconcile) or the full tree walk
        /// (plain dir); for single files / file-list uploads, the
        /// requested file(s). The job snapshot (poll, list, events) carries
        /// only its length as `files_count`: a big folder's list is
        /// megabytes and was re-sent on every 500 ms poll. The list itself
        /// is served once by `GET /api/jobs/{id}/files`.
        #[serde(
            rename = "files_count",
            serialize_with = "serialize_len",
            skip_serializing_if = "files_is_empty"
        )]
        files: Arc<Vec<PlannedFile>>,
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
        /// The figures behind a refusal for lack of room, which the client words in the user's
        /// language (`error` and `error_detail` hold the English, for logs and scripts).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_space: Option<ps5upload_ava1::space::SpaceRefusal>,
    },
}

/// Serializes a list as its length (see `JobState::Running::files`).
fn serialize_len<S: serde::Serializer, T>(v: &Arc<Vec<T>>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(v.len() as u64)
}

fn files_is_empty<T>(v: &Arc<Vec<T>>) -> bool {
    v.is_empty()
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
            error_space: None,
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
            error_space: err
                .downcast_ref::<ps5upload_ava1::space::SpaceRefusal>()
                .cloned(),
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
        error_space: None,
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
/// The console's address as the engine uses it: the host only, brackets kept so the pool can add
/// the AVA1 port. One implementation, in core.
pub(crate) use ps5upload_core::events::console_addr;

/// The console a destructive request (delete, move, unregister, power, ...) acts on. It must
/// name one: falling back to the engine's default console could delete files on, or power off,
/// a console the user was not looking at. `None`: answer with [`missing_console_addr`].
fn required_console_addr(addr: Option<String>) -> Option<String> {
    // Map first, then refuse an empty host: ":1234" names no console either.
    let addr = addr
        .as_deref()
        .map(|a| console_addr(&consoles_api::resolve(a)))
        .filter(|a| !a.is_empty())?;
    note_request_console(&addr);
    Some(addr)
}

/// The 400 for a destructive request that names no console.
fn missing_console_addr() -> axum::response::Response {
    json_err(
        StatusCode::BAD_REQUEST,
        "addr is required: say which console this acts on",
    )
    .into_response()
}

fn console_addr_or_default(addr: Option<String>, default_addr: &str) -> String {
    let addr = console_addr(&consoles_api::resolve(
        addr.as_deref().unwrap_or(default_addr),
    ));
    note_request_console(&addr);
    addr
}

tokio::task_local! {
    /// The console the request being handled is about, once its handler has worked that out.
    /// `log_requests` sets the slot up: most calls name their console in a JSON body it never
    /// reads.
    static REQUEST_CONSOLE: std::cell::RefCell<Option<String>>;
}

fn note_request_console(addr: &str) {
    let _ = REQUEST_CONSOLE.try_with(|c| *c.borrow_mut() = Some(addr.to_string()));
}

/// The console a query string names, decoded, under any of the names the API accepts for it.
fn console_from_query(uri: &axum::http::Uri) -> Option<String> {
    let q = Query::<HashMap<String, String>>::try_from_uri(uri).ok()?;
    ["addr", "console", "host", "ps5_addr", "ip"]
        .iter()
        .find_map(|k| q.0.get(*k).filter(|v| !v.is_empty()).cloned())
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
/// Off-loopback peers reach exactly what `loopback_allows` names. `/pkg-host/*` is PS5-facing, so
/// the console can fetch installable bytes; it uses a UUIDv4 token in the URL as the auth gate
/// (~122 bits of entropy, rotated per install — the trust boundary is the local LAN, same as any
/// other on-LAN homebrew installer). `/pad` + `/api/pad/ws` and `/join` + `/api/join/ws` are a
/// phone's and a friend's way in; their sockets check a key themselves.
/// What repeated failures are collapsed by: the route AND the console it was about, so one
/// switched-off console's failures do not hide (or get "recovered" by) another's.
fn failure_key(method: &str, path: &str, console: Option<&str>) -> String {
    match console {
        Some(c) if !c.is_empty() => format!("{method} {path} @{c}"),
        _ => format!("{method} {path}"),
    }
}

/// A route polled about once a second whatever the app is doing, not worth a debug line each.
fn is_constant_poll(path: &str) -> bool {
    matches!(path, "/api/engine-logs" | "/api/activity")
}

/// Log every (allowed) request: method, path, status, duration. Recorded at
/// `debug` so it always lands in engine.log (rotated, crash-survivable) for a
/// complete "what was the engine doing when it hung" trace, but only reaches
/// the renderer's windowed app.jsonl when the user lowers the level to
/// debug/trace. 5xx is bumped to `warn` so failures show at the default level.
async fn log_requests(req: Request, next: Next) -> axum::response::Response {
    let method = req.method().clone();
    let path = api_errors::request_path(&req);
    let query_console = console_from_query(req.uri());
    let start = std::time::Instant::now();
    // The console a call was about, for the event journal: what the handler worked out (a POST
    // names it in its body), else what the query says.
    let (resp, console) = REQUEST_CONSOLE
        .scope(std::cell::RefCell::new(None), async {
            let resp = next.run(req).await;
            (resp, REQUEST_CONSOLE.with(|c| c.borrow().clone()))
        })
        .await;
    let console = console.or(query_console);
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
    // an event worth recording. The app's activity poll is the same, once a
    // second for its whole life: it wore through a busy log in a few hours, and
    // a failure from that morning was gone from a report filed that evening. A
    // failure still gets through: only this debug line is skipped, and the 5xx
    // path below is untouched.
    if !is_constant_poll(&path) {
        // The access log: always kept (engine.log is the crash trace), whatever the level says,
        // in a ring of its own so requests never push other debug lines out of the tail.
        engine_log::record_access(format!("{method} {path} -> {status} ({ms}ms)"));
    }

    // A console that is switched off answers every status poll with 502,
    // once a second, indefinitely. Reported plainly that buries every
    // other line in the log and tells the reader nothing they did not
    // learn from the first one. So repeats collapse: first failure,
    // then quiet, then an occasional reminder, then a recovery line.
    let key = failure_key(method.as_str(), &path, console.as_deref());
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
    {
        use ps5upload_core::events::{emit, Cat, Level};
        match &action {
            log_dedup::LogAction::Warn { .. } => emit(
                Cat::Api,
                Level::Warn,
                "api_failing",
                console.as_deref(),
                format!("{method} {path} -> {status} ({ms}ms)"),
            ),
            log_dedup::LogAction::Recovered { failures } => emit(
                Cat::Api,
                Level::Info,
                "api_recovered",
                console.as_deref(),
                format!("{method} {path} recovered after {failures} failure(s)"),
            ),
            log_dedup::LogAction::Quiet => {}
        }
    }
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
        // A phone as a controller (pad_link.rs): its page and its socket, which checks the
        // QR code's key or waits for the computer to allow it.
        || path == "/pad"
        || path == "/api/pad/ws"
        // A friend joining Play Together (join.rs): the guest page and its socket, which checks
        // the invite or the resume token itself. The rest of /api/join is the host's.
        || path == "/join"
        || path == "/api/join/ws"
        // …and an internet invite's signalling straight to this engine (join_net/local_signal),
        // for this engine's own rooms only: the guest still checks the certificate and the key.
        || path == "/api/join/signal"
        // A friend receiving a Share (share/mod.rs) on the home network: the page and its
        // socket, which serves only what the sender allowed. The rest of /api/share is the
        // sender's.
        || path == "/share"
        || path == "/api/share/ws"
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
    let path = api_errors::request_path(&req);
    if loopback_allows(&cfg, peer.ip(), &path) {
        return next.run(req).await.into_response();
    }
    stderr_quiet(&format!(
        "[ps5upload-engine] refusing request to {path} from {peer}: not loopback and not in \
         PS5UPLOAD_ALLOW_IP — to allow it, set PS5UPLOAD_ALLOW_IP={} (or its subnet, e.g. \
         192.168.1.0/24)",
        peer.ip()
    ));
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
    if host_guard::same_server(origin_scheme, origin_authority, host) {
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

/// `/api/collection/games/<id>/cover`, one id segment: the Collection's read-only cover image.
fn is_collection_cover_path(path: &str) -> bool {
    path.strip_prefix("/api/collection/games/")
        .and_then(|rest| rest.strip_suffix("/cover"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

fn browser_request_allows(headers: &axum::http::HeaderMap, path: &str) -> bool {
    if path.starts_with("/pkg-host/") {
        return true;
    }
    let path = host_guard::strip_base(path);
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
        // The Collection's covers are the same kind of read-only image.
        if cross_site
            && (matches!(path, "/api/ps5/app-icon" | "/api/ps5/game-icon")
                || is_collection_cover_path(path))
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
    // A proxy that rewrites `Host` says what the browser asked for here. A cross-site page cannot
    // set this header without a preflight, and the preflight (without it) is refused above.
    browser_origin_allows(origin, host)
        || host_guard::first_forwarded(headers, "x-forwarded-host")
            .is_some_and(|forwarded| browser_origin_allows(origin, forwarded))
}

/// What to say to a refused browser request: both sides of the comparison, because the usual
/// cause is a reverse proxy that rewrites `Host` (nginx does by default), and nothing else in the
/// answer would point there.
fn cross_site_refusal(headers: &axum::http::HeaderMap) -> String {
    let shown = |name: &str| {
        headers.get(name).map(|v| {
            let text = v.to_str().unwrap_or("(unreadable)");
            text.chars().take(200).collect::<String>()
        })
    };
    let host = shown("host").unwrap_or_else(|| "(none)".to_string());
    match shown("origin") {
        Some(origin) => format!(
            "cross-site browser requests are not allowed: Origin {origin} does not match Host \
             {host}. Behind a reverse proxy, forward the Host header (nginx: proxy_set_header \
             Host $http_host)."
        ),
        None => format!(
            "cross-site browser requests are not allowed: the browser marked this request as \
             cross-site (Host {host})."
        ),
    }
}

async fn browser_origin_guard(req: Request, next: Next) -> impl IntoResponse {
    let path = api_errors::request_path(&req);
    if let Some(refused) = host_guard::refuse_host(req.headers(), &path) {
        return refused;
    }
    if host_guard::is_page_navigation(req.method(), req.headers(), &path)
        || browser_request_allows(req.headers(), &path)
    {
        return next.run(req).await.into_response();
    }
    stderr_quiet(&format!(
        "[ps5upload-engine] refusing cross-site browser request to {path}"
    ));
    (StatusCode::FORBIDDEN, cross_site_refusal(req.headers())).into_response()
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
                // Relative to the source, as the transfer matches: a pattern naming a folder that
                // also occurs above it (`data`, `Games`) must not empty the plan.
                let rel = path.strip_prefix(root).unwrap_or(&path);
                if ps5upload_core::excludes::is_excluded_strings(rel, excludes) {
                    continue;
                }
                if let Ok(m) = entry.metadata() {
                    let size = m.len();
                    total += size;
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
            let rel = path.strip_prefix(root).unwrap_or(&path);
            if ps5upload_core::excludes::is_excluded_strings(rel, excludes) {
                continue;
            }
            let m = fs
                .metadata(&path)
                .map_err(|e| format!("could not read {}: {e}", path.display()))?;
            {
                total += m.len;
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
/// files list the handler stored. The snapshot it broadcasts carries
/// only the list's length (`files_count`), and is serialized under the
/// lock instead of cloning the job, so a tick stays O(1) in the file
/// count.
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
            let still_running = {
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
                        // Nobody listening: skip building the event.
                        // Serialize from the reference (no clone of the
                        // job); `files` serializes as a count. Sent while
                        // the lock is held, so it can never follow the
                        // terminal event `set_job` sends under the same lock.
                        if event_bus::tick_wanted(&events_tx) {
                            if let Some(state) = g.get(&job_id).map(|st| serde_json::json!(st)) {
                                let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": with_live_notes(job_id, state) });
                                let _ = events_tx.send(msg.to_string());
                            }
                        }
                        true
                    }
                    // Job moved to terminal state (Done/Failed) — stop
                    // ticking to avoid writing over the terminal record.
                    _ => false,
                }
            };
            if !still_running {
                break;
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
                        g.get(&job_id).map(|st| serde_json::json!(st))
                    }
                    _ => None,
                }
            };
            let Some(state) = snapshot else { break };
            if event_bus::tick_wanted(&events_tx) {
                let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": with_live_notes(job_id, state) });
                let _ = events_tx.send(msg.to_string());
            }
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
                error_space: None,
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

/// Threads `job_id`'s live notes into `cfg`, with the counter the transfer writes the bytes
/// a resume found already on the console into (served as `resumed_bytes`).
fn attach_live_notes(cfg: &mut TransferConfig, job_id: Uuid) {
    let notes = live_notes_for(job_id);
    cfg.progress_bytes_resumed = Some(notes.resumed_bytes.clone());
    cfg.progress_live = Some(notes);
}

/// Adds a running job's live notes to its snapshot JSON: `phase` (`"skipping"` with
/// `skip_done_bytes` / `skip_total_bytes`), `bottleneck` (AVA1's word), `settling` and
/// `verifying` (the console reads its last large files back, #783). Fields
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
    if n.phase.load(Relaxed) == ps5upload_core::transfer::LIVE_PHASE_MEASURING {
        o.insert("phase".into(), "measuring".into());
        o.insert(
            "measure_done_bytes".into(),
            n.skip_done_bytes.load(Relaxed).into(),
        );
    }
    // A compressed tar is decoded once to list its files before any is sent: how much of the
    // archive file has been read.
    if n.phase.load(Relaxed) == ps5upload_core::transfer::LIVE_PHASE_LISTING {
        o.insert("phase".into(), "listing".into());
        o.insert(
            "listing_done_bytes".into(),
            n.skip_done_bytes.load(Relaxed).into(),
        );
        o.insert(
            "listing_total_bytes".into(),
            n.skip_total_bytes.load(Relaxed).into(),
        );
    }
    // What the console already held when the job first opened (a resume). `bytes_sent`
    // counts it, so a client takes it off for this run's speed and time left.
    let resumed = n.resumed_bytes.load(std::sync::atomic::Ordering::Acquire);
    if resumed > 0 {
        o.insert("resumed_bytes".into(), resumed.into());
    }
    let bn = n.bottleneck.load(Relaxed);
    if bn != 0 {
        o.insert(
            "bottleneck".into(),
            ps5upload_ava1::progress::bottleneck_name(bn).into(),
        );
    }
    match n.route.load(Relaxed) {
        ps5upload_core::transfer::LIVE_ROUTE_DIRECT => {
            o.insert("route".into(), "direct".into());
        }
        ps5upload_core::transfer::LIVE_ROUTE_RELAY => {
            o.insert("route".into(), "relay".into());
            if let Some(why) = n
                .route_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                o.insert("route_reason".into(), why.into());
            }
        }
        _ => {}
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
    // Every byte is on the console and it is reading the last large files back (#783).
    if n.verifying.load(Relaxed) && !n.settling.load(Relaxed) {
        o.insert("verifying".into(), true.into());
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
    merge_pause(job_id, merge_live_notes(notes.as_deref(), v))
}

/// A running Workshop job's pause state: `pausable`, `paused` and `paused_ms` (all its paused
/// time so far, so a client leaves it out of speed and time left). Other jobs are unchanged.
fn merge_pause(job_id: Uuid, mut v: serde_json::Value) -> serde_json::Value {
    let running = v.get("status").and_then(|s| s.as_str()) == Some("running");
    let Some(flag) = running.then(|| cancel_flag(job_id)).flatten() else {
        return v;
    };
    if let (Some(st), Some(o)) = (ps5upload_fpkg::workshop::status(&flag), v.as_object_mut()) {
        o.insert("pausable".into(), true.into());
        o.insert("paused".into(), st.paused.into());
        o.insert("paused_ms".into(), st.paused_ms.into());
    }
    v
}

/// [`register_transfer_cancel`] for a Workshop job (a conversion or an extraction on this
/// computer): the same flag, which `POST /api/jobs/{id}/pause` can also hold (#631).
pub(crate) fn register_workshop_job(job_id: Uuid) -> Arc<AtomicBool> {
    let flag = register_transfer_cancel(job_id);
    ps5upload_fpkg::workshop::register(&flag);
    flag
}

fn cancel_flag(job_id: Uuid) -> Option<Arc<AtomicBool>> {
    cancel_registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&job_id)
        .and_then(Weak::upgrade)
}

/// How asking a job to pause or go on ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobPause {
    Done,
    /// There is no running job by that id.
    NotRunning,
    /// There is, and it is not one that can wait (only a Workshop job's workers can).
    NotPausable,
}

/// Pause or resume a running Workshop job.
pub(crate) fn set_job_paused(job_id: Uuid, paused: bool) -> JobPause {
    match cancel_flag(job_id) {
        None => JobPause::NotRunning,
        Some(f) if ps5upload_fpkg::workshop::set_paused(&f, paused) => JobPause::Done,
        Some(_) => JobPause::NotPausable,
    }
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
                // A paused Workshop job wakes to see it.
                ps5upload_fpkg::workshop::wake(&flag);
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

impl JobState {
    pub(crate) fn started_at_ms(&self) -> u64 {
        match self {
            JobState::Running { started_at_ms, .. }
            | JobState::Done { started_at_ms, .. }
            | JobState::Failed { started_at_ms, .. } => *started_at_ms,
        }
    }
}

/// Update a job's state and broadcast the change over SSE.
pub(crate) fn set_job(
    jobs: &Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: &broadcast::Sender<String>,
    job_id: Uuid,
    state: JobState,
) {
    // Serialized before the move into the map, so the (possibly long) file
    // list is never cloned; it serializes as a count.
    let terminal = matches!(state, JobState::Done { .. } | JobState::Failed { .. });
    let json = serde_json::json!(state);
    {
        let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
        if !g.contains_key(&job_id) && g.len() >= JOBS_MAP_CAP {
            evict_oldest_terminal(&mut g);
        }
        g.insert(job_id, state);
        // Sent under the lock, as the progress ticker does: a ticker's "running" can then
        // never reach a client after this "done". Sent whoever listens: `/api/jobs?since_seq=`
        // numbers a job by its changes.
        {
            let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": with_live_notes(job_id, json.clone()) });
            let _ = events_tx.send(msg.to_string());
        }
    }
    // A job's end at info (what a report needs), every other change at trace (a ticker updates a
    // running job often). Only its state, reason and counters: never the file list.
    let field = |k: &str| json.get(k).cloned().unwrap_or(serde_json::Value::Null);
    if terminal {
        log_at!(
            Info,
            "transfer",
            "job {job_id}: {} after {} ms: reason {} error {} bytes {} files {}",
            field("status"),
            field("elapsed_ms"),
            field("error_reason"),
            field("error"),
            field("bytes_sent"),
            field("files_sent")
        );
    } else {
        log_at!(
            Trace,
            "transfer",
            "job {job_id}: {} bytes {} of {}",
            field("status"),
            field("bytes_sent"),
            field("total_bytes")
        );
    }
    if terminal {
        activity::transfer_ended(&job_id.to_string(), &json);
        telemetry::on_state(job_id, &json);
        if let Some(e) = job_event(&job_id.to_string(), &json) {
            ps5upload_core::events::emit_event(e);
        }
    }
}

/// The journal event for a job that reached a terminal state (bug-report spec §1.2); `None`
/// for any other state.
pub(crate) fn job_event(
    job_id: &str,
    st: &serde_json::Value,
) -> Option<ps5upload_core::events::Event> {
    use ps5upload_core::events::{Cat, Event, Level};
    let console = st.get("error_console").and_then(|v| v.as_str());
    match st.get("status")?.as_str()? {
        "done" => Some(Event::new(
            Cat::Transfer,
            Level::Info,
            "job_done",
            console,
            format!("job {job_id} done"),
            None,
        )),
        "failed" => {
            let err = st
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            Some(Event::new(
                Cat::Transfer,
                Level::Error,
                "job_failed",
                console,
                format!("job {job_id} failed: {err}"),
                Some(serde_json::json!({
                    "reason": st.get("error_reason"),
                    "elapsed_ms": st.get("elapsed_ms"),
                })),
            ))
        }
        _ => None,
    }
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
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
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
    /// The folder was downloaded with `escape_names`: each file goes to the console under the
    /// exact name its kept name stands for (`ps5upload_core::host_names`, #729).
    #[serde(default)]
    unescape_names: bool,
}

/// Upload a `.zip`'s contents, decompressing on the host so files land
/// already extracted on the PS5. Same shape as `TransferDirReq` but the
/// source is an archive path instead of a directory.
#[derive(Deserialize)]
struct TransferZipReq {
    #[serde(alias = "console", alias = "host")]
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
    /// Paces the relay like an upload's cap (the environment's cap otherwise).
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
    /// `auto` (absent), `direct` (never through this computer) or `relay` (never direct); #444.
    #[serde(default)]
    route: Option<String>,
    /// What the sending console dials to reach the receiving one (`host[:port]`, IPv4), in
    /// place of the receiving console's own address: a local forward on the sending console
    /// (ps5-tailscale), or a NAT the consoles see differently from this computer.
    #[serde(default)]
    direct_addr: Option<String>,
    /// #444, a send another device started (its `tx_id`): `take` ends both consoles' halves and
    /// sends again here, resuming from the receiving console's journal; `watch` follows a direct
    /// send still running between the consoles and drives it from here on.
    #[serde(default)]
    adopt: Option<String>,
}

async fn ps5_to_ps5_handler(
    State(state): State<AppState>,
    Json(req): Json<Ps5ToPs5Req>,
) -> impl IntoResponse {
    let adopt = match c2c_api::parse_adopt(req.adopt.as_deref(), req.tx_id.is_some()) {
        Ok(a) => a,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let route_options = {
        let mode = match ps5upload_ava1::c2c::RouteMode::parse(req.route.as_deref()) {
            Ok(m) => m,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        };
        let dial = req
            .direct_addr
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(str::to_string);
        if let Some(d) = &dial {
            if let Err(e) = ps5upload_ava1::c2c::parse_dial(d) {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("the direct route address: {e}"),
                )
                .into_response();
            }
        }
        ps5upload_ava1::c2c::RouteOptions {
            mode,
            dial,
            attach: adopt.as_deref() == Some("watch"),
            lease: None,
        }
    };
    let relay_cap = {
        let mut cfg = make_transfer_config(&req.to);
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        cfg.bandwidth_cap_bps
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
            files: Default::default(),
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
    let route_notes = live.clone();
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
    // What the shared record says about the send while it runs (#444).
    let record_snapshot: ps5upload_ava1::c2c_record::Snapshot = {
        let (notes, progress) = (route_notes.clone(), progress.clone());
        Arc::new(move |r: &mut ps5upload_ava1::c2c_record::SendRecord| {
            r.live_route = match notes.route.load(Ordering::Relaxed) {
                ps5upload_core::transfer::LIVE_ROUTE_DIRECT => Some("direct".into()),
                ps5upload_core::transfer::LIVE_ROUTE_RELAY => Some("relay".into()),
                _ => None,
            };
            r.bytes_done = progress.bytes_sent.load(Ordering::Relaxed);
            r.bytes_total = progress.bytes_total.load(Ordering::Relaxed);
        })
    };
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = (helper_stop::enter(&req.from), helper_stop::enter(&req.to));
        let _stop_guard = TickerStopGuard::new(stop_ticker.clone());
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        // Like every other AVA1 job: no console that cannot be used (not paired, an old
        // helper, nobody listening) starts a relay, and the failure names which one.
        let result = match relay_preflight(&req.from, &req.to, |c| {
            ps5upload_ava1::console::require_ava1(c)
        }) {
            Err(f) => Err(anyhow::Error::from(f)),
            // #433: straight between the consoles when they reach each other, else through
            // this computer; the route (and why) shows on the job while it runs and after.
            // #444: inside the shared record another device finds, watches or takes over.
            Ok(()) => {
                c2c_api::with_record(&req, tx_id, adopt.as_deref(), record_snapshot, |lease| {
                    ps5upload_ava1::c2c::ps5_to_ps5_routed(
                        &req.from,
                        &req.src,
                        &req.to,
                        &req.dest,
                        tx_id,
                        progress.clone(),
                        cancel,
                        relay_cap,
                        &ps5upload_ava1::c2c::RouteOptions {
                            lease,
                            ..route_options.clone()
                        },
                        |route| match route {
                            ps5upload_ava1::c2c::Route::Direct => route_notes.route.store(
                                ps5upload_core::transfer::LIVE_ROUTE_DIRECT,
                                Ordering::Relaxed,
                            ),
                            ps5upload_ava1::c2c::Route::Relay { reason } => {
                                *route_notes
                                    .route_reason
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
                                route_notes.route.store(
                                    ps5upload_core::transfer::LIVE_ROUTE_RELAY,
                                    Ordering::Relaxed,
                                );
                            }
                        },
                    )
                })
            }
        };
        let completed_at_ms = now_ms();
        let state = match result {
            Ok((r, route)) => {
                let (route, reason) = match route {
                    ps5upload_ava1::c2c::Route::Direct => ("direct", None),
                    ps5upload_ava1::c2c::Route::Relay { reason } => ("relay", Some(reason)),
                };
                JobState::Done {
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
                        "route": route, "route_reason": reason,
                    })),
                }
            }
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
    #[serde(alias = "console", alias = "host")]
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

/// `/api/transfer/tar` request: a tar (`.tar`, `.tar.zst`/`.tzst`, `.tar.gz`/`.tgz`,
/// `.tar.xz`/`.txz`, `.tar.bz2`/`.tbz2`/`.tbz`) or a lone `.zst`. The same fields as 7z.
#[derive(Deserialize)]
struct TransferTarReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    archive_path: String,
    #[serde(default)]
    excludes: Vec<String>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

/// `/api/tar/inspect[/stream]` request.
#[derive(Deserialize)]
struct TarInspectReq {
    archive_path: String,
}

/// `/api/transfer/rar` request. Like 7z plus an optional `password` for
/// encrypted archives. (RAR is desktop-only — the phone builds cfg out both
/// the real handler and this struct, so it lives behind the same gate.)
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[derive(Deserialize)]
struct TransferRarReq {
    #[serde(alias = "console", alias = "host")]
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

/// `/api/link/probe` request (R4, #368).
#[derive(Deserialize)]
struct LinkProbeReq {
    url: String,
    #[serde(default)]
    insecure_tls: bool,
}

/// `/api/link/download` request: download a link's file to a console folder.
#[derive(Deserialize)]
struct LinkDownloadReq {
    #[serde(alias = "console", alias = "host")]
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
    /// An `.lz4` link is unpacked on the way (the default); false keeps the `.lz4` as it is.
    #[serde(default = "default_true")]
    unpack: bool,
}

/// `/api/rar/packages` request (every archive kind: the name predates ZIP and 7z).
#[derive(Deserialize)]
struct RarPackagesReq {
    archive_path: String,
    #[serde(default)]
    password: Option<String>,
}

/// `/api/rar/inspect` request. Desktop-only, same gate as the handler.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    tx_id: Option<String>,
    dest_root: String,
    files: Vec<FileListEntryReq>,
    #[serde(default)]
    bandwidth_cap_mbps: Option<f64>,
}

#[derive(Deserialize)]
struct TransferDirReconcileReq {
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

/// Query for the HW_TEMPS endpoint. `extended=1` requests the on-demand
/// telemetry (SoC power / CPU usage / fan duty / product shape); any other
/// value (or absent) is the basic, auto-poll-safe read.
#[derive(Deserialize)]
struct HwTempsQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    extended: Option<u8>,
    /// `clocks=1`: also read the GPU and per-core CPU clocks (`/dev/mp1`). Sent only by the
    /// Hardware screen's explicit "Read sensors" click.
    #[serde(default)]
    clocks: Option<u8>,
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

#[derive(Deserialize)]
struct CleanupReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
}

/// Whether the routes that exist only for tests and benches answer: `PS5UPLOAD_ENGINE_DEBUG=1`.
fn debug_routes_on() -> bool {
    debug_routes_allowed(std::env::var("PS5UPLOAD_ENGINE_DEBUG").ok().as_deref())
}

fn debug_routes_allowed(value: Option<&str>) -> bool {
    value == Some("1")
}

/// What a route that is off answers: the same as one that does not exist.
fn no_such_route() -> axum::response::Response {
    json_err(StatusCode::NOT_FOUND, "no such route").into_response()
}

/// POST /api/ps5/cleanup — asks the PS5 payload to rm -rf a path. For tests and benches only:
/// off unless `PS5UPLOAD_ENGINE_DEBUG=1`.
/// Safe: payload refuses paths outside `/data/ps5upload-{bench,sweep,smoke}/`.
async fn ps5_cleanup(
    State(_state): State<AppState>,
    Json(req): Json<CleanupReq>,
) -> impl IntoResponse {
    if !debug_routes_on() {
        return no_such_route();
    }
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    from: String,
    to: String,
    /// Merge into an existing destination instead of refusing it. Files that
    /// collide are replaced; anything already in the destination that the
    /// source doesn't mention is left alone. Absent = an existing destination
    /// is refused (`fs_move_exists` / `fs_copy_dest_exists`, 409 on a move), so
    /// only a caller that has actually asked the user gets the destructive path.
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
    /// Octal string like "0777". String (not u32) so JSON parsers
    /// don't coerce to decimal and change the meaning.
    mode: String,
    #[serde(default)]
    recursive: bool,
}

/// The registered game running now whose files are at or under `path`, if any. Asks the console
/// only about what runs first, and more only when something does, so a delete with no game
/// running costs one small call. A check that can't be made refuses nothing: the app's own rows
/// check first, and this is the backstop for everything else that deletes.
fn running_title_from(addr: &str, path: &str) -> Option<String> {
    let running: Vec<String> = process_list(addr)
        .ok()?
        .processes
        .into_iter()
        .filter(|p| p.kind == "app" && !p.title_id.is_empty())
        .map(|p| p.title_id)
        .collect();
    if running.is_empty() {
        return None;
    }
    app_list_registered(addr)
        .ok()?
        .apps
        .into_iter()
        .find(|a| {
            running.iter().any(|t| t.eq_ignore_ascii_case(&a.title_id))
                && removes_source(path, &a.src)
        })
        .map(|a| a.title_id)
}

/// Whether deleting `path` takes away a game registered from `source`.
fn removes_source(path: &str, source: &str) -> bool {
    let path = path.trim_end_matches('/');
    let source = source.trim_end_matches('/');
    !path.is_empty()
        && (source == path
            || source
                .strip_prefix(path)
                .is_some_and(|rest| rest.starts_with('/')))
}

async fn ps5_fs_delete(
    State(_state): State<AppState>,
    Json(req): Json<FsPathReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let path = req.path;
    let op_id = req.op_id;
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "fs_delete: addr={addr} path={path} op_id={op_id}"
    );
    let (check_addr, check_path) = (addr.clone(), path.clone());
    if let Ok(Some(title)) =
        tokio::task::spawn_blocking(move || running_title_from(&check_addr, &check_path)).await
    {
        return json_err(
            StatusCode::CONFLICT,
            format!("{title} is running from this path. Close it first."),
        )
        .into_response();
    }
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
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        fs_delete_with_op_id(&addr, &path, op_id, Some(io_timeout))
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
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
                crate::log_at!(
                    Info,
                    "files",
                    "fs_delete cancelled: {path_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
                return json_err(StatusCode::CONFLICT, "cancelled").into_response();
            }
            crate::log_at!(
                Warn,
                "files",
                "fs_delete failed: {path_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

/// POST /api/ps5/fs/unpack `{addr, path, op_id}` — decompress a `.ffpfsc` on the console into
/// the image it holds, beside it (#453). Progress and cancel go through `op-status`/`op-cancel`
/// with the same `op_id`. 200 `{ok, path, size}`; 409 when cancelled; 502 with the helper's
/// cause otherwise (`fs_unpack_no_space: ...`, `fs_unpack_destination_exists`, ...).
async fn ps5_fs_unpack(
    State(_state): State<AppState>,
    Json(req): Json<FsPathReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let (path, op_id) = (req.path, req.op_id);
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "fs_unpack: addr={addr} path={path} op_id={op_id}"
    );
    let path_for_log = path.clone();
    // A 136 GB image inflates for many minutes; the job is followed, not timed out.
    let deadline = std::time::Duration::from_secs(12 * 60 * 60);
    match tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        ps5upload_core::fs_ops::ffpfsc_unpack(&addr, &path, op_id, deadline)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(done) => {
            crate::log_at!(
                Info,
                "files",
                "fs_unpack ok: {path_for_log} -> {} ({} bytes) in {} ms",
                done.path,
                done.size,
                started.elapsed().as_millis()
            );
            Json(serde_json::json!({ "ok": true, "path": done.path, "size": done.size }))
                .into_response()
        }
        Err(e) if e.to_string() == "cancelled" => {
            json_err(StatusCode::CONFLICT, "cancelled").into_response()
        }
        Err(e) => {
            crate::log_at!(Warn, "files", "fs_unpack failed: {path_for_log}: {e:#}");
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

#[derive(Deserialize)]
struct FsExtractReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
    dest: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    merge: bool,
    #[serde(default)]
    replace: bool,
    #[serde(default)]
    op_id: u64,
}

/// POST /api/ps5/fs/extract `{addr, path, dest, password?, merge?, replace?, op_id}` — extract an
/// archive on the console into `dest` there (the helper's extractor payload does the decoding).
/// Progress and cancel go through `op-status`/`op-cancel` with the same `op_id`. 200 `{ok, path,
/// files, dirs, bytes, skipped, unsafe, links}`; 409 when cancelled; 502 with the helper's cause
/// otherwise (`fs_extract_password_required`, `fs_extract_no_space: ...`, ...). The password is
/// never logged.
async fn ps5_fs_extract(
    State(_state): State<AppState>,
    Json(req): Json<FsExtractReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let op_id = req.op_id;
    let x = ps5upload_core::fs_ops::ExtractRequest {
        path: req.path,
        dest: req.dest,
        password: req.password.filter(|p| !p.is_empty()),
        merge: req.merge,
        replace: req.replace,
    };
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "fs_extract: addr={addr} path={} dest={} merge={} replace={} password={} op_id={op_id}",
        x.path,
        x.dest,
        x.merge,
        x.replace,
        x.password.is_some()
    );
    let path_for_log = x.path.clone();
    // A large solid archive decodes for a long time; the job is followed, not timed out.
    let deadline = std::time::Duration::from_secs(12 * 60 * 60);
    match tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        ps5upload_core::fs_ops::archive_extract(&addr, &x, op_id, deadline)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(done) => {
            crate::log_at!(Info, "files", 
                "fs_extract ok: {path_for_log} -> {} ({} files, {} bytes, {} skipped, {} unsafe, {} links) in {} ms",
                done.path,
                done.files,
                done.bytes,
                done.skipped,
                done.unsafe_entries,
                done.links,
                started.elapsed().as_millis()
            );
            Json(serde_json::json!({
                "ok": true,
                "path": done.path,
                "files": done.files,
                "dirs": done.dirs,
                "bytes": done.bytes,
                "skipped": done.skipped,
                "unsafe": done.unsafe_entries,
                "links": done.links,
            }))
            .into_response()
        }
        Err(e) if e.to_string() == "cancelled" => {
            json_err(StatusCode::CONFLICT, "cancelled").into_response()
        }
        Err(e) => {
            crate::log_at!(Warn, "files", "fs_extract failed: {path_for_log}: {e:#}");
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_fs_move(
    State(_state): State<AppState>,
    Json(req): Json<FsMoveReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let from = req.from;
    let to = req.to;
    let started = std::time::Instant::now();
    crate::log_at!(Info, "files", "fs_move: addr={addr} from={from} to={to}");
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
    // One blocking closure for the whole decision (`console_move::run`): the rename is still
    // tried first (a same-drive move is metadata-only). `overwrite` travels with it, so a move
    // that was not allowed to clobber refuses on one drive and across drives alike.
    match tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let console = console_move::Console {
            addr: &addr,
            op_id,
            rename_timeout: io_timeout,
        };
        console_move::run(&console, &from, &to, overwrite)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
                "fs_move ok: {from_for_log} -> {to_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            if e.to_string() == "cancelled" {
                crate::log_at!(
                    Info,
                    "files",
                    "fs_move cancelled: {from_for_log} -> {to_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
                return json_err(StatusCode::CONFLICT, "cancelled").into_response();
            }
            crate::log_at!(
                Warn,
                "files",
                "fs_move failed: {from_for_log} -> {to_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            let msg = format!("{e:#}");
            let status = if console_move::is_exists_refusal(&msg) {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_GATEWAY
            };
            json_err(status, msg).into_response()
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
    State(_state): State<AppState>,
    Json(req): Json<FsMoveReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let from = req.from;
    let to = req.to;
    let started = std::time::Instant::now();
    crate::log_at!(Info, "files", "fs_copy: addr={addr} from={from} to={to}");
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
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        require_console_ready(&addr)?;
        ps5upload_ava1::copy::console_copy(&addr, &from, &to, op_id, false, overwrite)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
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
                crate::log_at!(
                    Info,
                    "files",
                    "fs_copy cancelled: {from_for_log} -> {to_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
                return json_err(StatusCode::CONFLICT, "cancelled").into_response();
            }
            crate::log_at!(
                Warn,
                "files",
                "fs_copy failed: {from_for_log} -> {to_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, msg).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct FsMountReq {
    #[serde(alias = "console", alias = "host")]
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

async fn ps5_fs_mount(Json(req): Json<FsMountReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let image_path = req.image_path;
    let mount_name = req.mount_name;
    let mount_point = req.mount_point;
    let read_only = req.read_only.unwrap_or(false);
    let started = std::time::Instant::now();
    crate::log_at!(Info, "files", 
        "fs_mount: addr={addr} image_path={image_path} mount_name={:?} mount_point={:?} read_only={read_only}",
        mount_name,
        mount_point,
    );
    let image_for_log = image_path.clone();
    let result: Result<MountResult, anyhow::Error> = tokio::task::spawn_blocking(move || {
        let _busy = helper_stop::enter(&addr);
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
            crate::log_at!(
                Info,
                "files",
                "fs_mount ok: {image_for_log} -> {} ({}, {}) in {} ms",
                r.mount_point,
                r.dev_node,
                r.fstype,
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
                "fs_mount failed: {image_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct FsUnmountReq {
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

async fn ps5_app_launch(Json(req): Json<AppLaunchReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let title_id = req.title_id;
    let started = std::time::Instant::now();
    crate::log_at!(Info, "files", "app_launch: addr={addr} title_id={title_id}");
    let title_for_log = title_id.clone();
    match tokio::task::spawn_blocking(move || app_launch(&addr, &title_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
                "app_launch ok: {title_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    src_path: String,
    /// 2.2.26 opt-in: rewrite `<src>/sce_sys/param.json`'s
    /// `applicationDrmType` to `"standard"` before staging. Default
    /// false (don't touch the user's source).
    #[serde(default)]
    patch_drm_type: Option<bool>,
}

async fn ps5_app_register(
    State(_state): State<AppState>,
    Json(req): Json<AppRegisterReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let src_path = req.src_path;
    let patch_drm_type = req.patch_drm_type.unwrap_or(false);
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "app_register: addr={addr} src_path={src_path} patch_drm_type={patch_drm_type}"
    );
    let path_for_log = src_path.clone();
    match tokio::task::spawn_blocking(move || {
        let _busy = helper_stop::enter(&addr);
        app_register(&addr, &src_path, patch_drm_type)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(result) => {
            crate::log_at!(
                Info,
                "files",
                "app_register ok: {path_for_log} -> {} ({}) in {} ms",
                result.title_id,
                result.title_name,
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json::<RegisterResult>(result)).into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
                "app_register failed: {path_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

/// Reverse of `app_register`, and the uninstall of a package install. A
/// Sony refusal is an error (502) from a 7.1.6 helper; from an older one it is
/// `sony_uninstall_refused` in an `ok` reply, which the client must check
/// (#476). `still_present` says the title's folders were still there after
/// Sony accepted (`null` from an older helper).
#[derive(Debug, serde::Deserialize)]
struct AppUnregisterReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

async fn ps5_app_unregister(
    State(_state): State<AppState>,
    Json(req): Json<AppUnregisterReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let title_id = req.title_id;
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "app_unregister: addr={addr} title_id={title_id}"
    );
    let title_for_log = title_id.clone();
    // Uninstalling changes which artwork exists, so drop this console's
    // cached images now rather than letting them age out — the user would
    // otherwise see a cover for a title they just removed.
    match tokio::task::spawn_blocking(move || {
        icon_cache::invalidate_console(&addr);
        app_unregister(&addr, &title_id)
    })
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
            if outcome.sony_refused() && !outcome.refusal_stands() {
                crate::log_at!(
                    Warn,
                    "files",
                    "app_unregister: {title_for_log} — sceAppInstUtilAppUninstall refused with \
                     rc=0x{:08X}, but our registration is gone and the title is no longer listed \
                     ({} ms)",
                    outcome.sony_uninstall_rc,
                    started.elapsed().as_millis()
                );
            } else if outcome.sony_refused() {
                crate::log_at!(
                    Warn,
                    "files",
                    "app_unregister: {title_for_log} — our teardown ok in {} ms, but \
                     sceAppInstUtilAppUninstall REFUSED with rc=0x{:08X}; the title may \
                     remain in Settings → Storage",
                    started.elapsed().as_millis(),
                    outcome.sony_uninstall_rc
                );
            } else if outcome.still_present == Some(true) {
                // WARN so a bug report carries it: the game is still listed (#476).
                crate::log_at!(
                    Warn,
                    "files",
                    "app_unregister: {title_for_log} — Sony accepted in {} ms, but the \
                     title's folders are still on the console",
                    started.elapsed().as_millis()
                );
            } else {
                crate::log_at!(
                    Info,
                    "files",
                    "app_unregister ok: {title_for_log} in {} ms",
                    started.elapsed().as_millis()
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "ok": true,
                    "sony_uninstall_refused": outcome.refusal_stands(),
                    "sony_uninstall_rc": format!("0x{:08X}", outcome.sony_uninstall_rc),
                    "still_present": outcome.still_present,
                })),
            )
                .into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
                "app_unregister failed: {title_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_fs_unmount(
    State(_state): State<AppState>,
    Json(req): Json<FsUnmountReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let mount_point = req.mount_point;
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "fs_unmount: addr={addr} mount_point={mount_point}"
    );
    let mp_for_log = mount_point.clone();
    match tokio::task::spawn_blocking(move || fs_unmount(&addr, &mount_point))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
                "fs_unmount ok: {mp_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
                "fs_unmount failed: {mp_for_log} in {} ms: {e}",
                started.elapsed().as_millis()
            );
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

async fn ps5_fs_chmod(
    State(_state): State<AppState>,
    Json(req): Json<FsChmodReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let path = req.path;
    let mode = req.mode;
    let recursive = req.recursive;
    let started = std::time::Instant::now();
    crate::log_at!(
        Info,
        "files",
        "fs_chmod: addr={addr} path={path} mode={mode} recursive={recursive}"
    );
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
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        ps5upload_core::fs_ops::fs_chmod_with_timeout(&addr, &path, &mode, recursive, io_timeout)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
                "fs_chmod ok: {path_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
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
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

/// Stops a console file operation: this engine's own job if it runs one with that id, else the
/// helper's. False when nothing with that id is running.
async fn cancel_fs_op(addr: String, op_id: u64) -> anyhow::Result<bool> {
    crate::log_at!(Info, "files", "fs_op_cancel: op_id={op_id}");
    if ps5upload_ava1::copy::op_cancel(op_id) {
        return Ok(true);
    }
    tokio::task::spawn_blocking(move || fs_op_cancel(&addr, op_id))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
}

async fn ps5_fs_op_cancel(Json(req): Json<FsOpCancelReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    match cancel_fs_op(addr, req.op_id).await {
        Ok(found) => (
            StatusCode::OK,
            Json(serde_json::json!({ "cancelled": found })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn ps5_fs_mkdir(Json(req): Json<FsPathReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let path = req.path;
    let started = std::time::Instant::now();
    crate::log_at!(Info, "files", "fs_mkdir: addr={addr} path={path}");
    let path_for_log = path.clone();
    match tokio::task::spawn_blocking(move || fs_mkdir(&addr, &path))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r)
    {
        Ok(()) => {
            crate::log_at!(
                Info,
                "files",
                "fs_mkdir ok: {path_for_log} in {} ms",
                started.elapsed().as_millis()
            );
            (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => {
            crate::log_at!(
                Warn,
                "files",
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

/// The fallback read's deadline: with the full read's 30 s, it stays inside the app's 60 s.
const HW_TEMPS_FALLBACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The full read, or when it fails or stalls the basic temperatures with `extended_error` saying
/// why the rest is missing. The helper answers one reply for a read: a single getter that fails or
/// hangs (seen while a game runs) would otherwise hide the temperatures it read fine.
fn hw_temps_with_fallback(
    full: impl FnOnce() -> anyhow::Result<HwTemps>,
    basic: impl FnOnce() -> anyhow::Result<HwTemps>,
    has_more_than_basic: bool,
) -> anyhow::Result<HwTemps> {
    let e = match full() {
        Ok(t) => return Ok(t),
        Err(e) if !has_more_than_basic => return Err(e),
        Err(e) => e,
    };
    let why = format!("{e:#}");
    crate::log_warn!("hw_temps: the full read failed, trying the basic one: {why}");
    match basic() {
        Ok(mut t) => {
            t.extended_error = Some(why);
            Ok(t)
        }
        // The helper is not answering at all: the first error says so best.
        Err(_) => Err(e),
    }
}

async fn ps5_hw_temps(
    State(state): State<AppState>,
    Query(q): Query<HwTempsQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let extended = q.extended.unwrap_or(0) != 0;
    let clocks = q.clocks.unwrap_or(0) != 0;
    let r: Result<HwTemps, anyhow::Error> = tokio::task::spawn_blocking(move || {
        hw_temps_with_fallback(
            || hw_temps_with_clocks(&addr, extended, clocks),
            || ps5upload_core::hw::hw_temps_basic_within(&addr, HW_TEMPS_FALLBACK_TIMEOUT),
            extended || clocks,
        )
    })
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
        Ok(v) => (StatusCode::OK, Json(console_log_reply(&v))).into_response(),
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// Target wall-clock time as unix seconds (UTC). Required when
    /// `use_ntp` is false (or absent): a missing one is a 400, never a
    /// clock set to 1970. Ignored when `use_ntp` is true, so an NTP sync
    /// may omit it entirely.
    #[serde(default)]
    target_unix_seconds: Option<i64>,
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

/// A host name or an IP address, nothing else: the engine sends this server a UDP packet.
fn valid_ntp_server(server: &str) -> bool {
    if server.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    !server.is_empty()
        && server.len() <= 253
        && server.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

async fn ps5_time_sync_route(Json(req): Json<TimeSyncReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };

    // Resolve the target time: either from NTP or from the client-provided value.
    if let Some(server) = req.ntp_server.as_deref().filter(|_| req.use_ntp) {
        if !valid_ntp_server(server) {
            return json_err(
                StatusCode::BAD_REQUEST,
                "ntp_server must be a host name or an IP address",
            )
            .into_response();
        }
    }
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
        match req.target_unix_seconds {
            Some(t) if t > 0 => (t, "client", None),
            _ => {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    "target_unix_seconds is required unless use_ntp is set",
                )
                .into_response();
            }
        }
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

/// SMP-meta worker control. Wraps `smp_meta_control` in spawn_blocking
/// because the underlying Connection is sync TCP. The `addr` field on
/// the request body is optional — falls back to the engine's default
/// PS5 addr — keeping the route shape consistent with the rest of the
/// /api/ps5/* surface.
#[derive(serde::Deserialize)]
struct SmpMetaControlReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(flatten)]
    inner: ps5upload_core::smp_meta::SmpMetaControlRequest,
}

async fn ps5_smp_meta_control_route(Json(req): Json<SmpMetaControlReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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

/// GET /api/ps5/proc/list — a light snapshot of running PS5 processes. The payload walks `allproc`
/// via kernel R/W and returns JSON directly; we pass it through largely untouched after a small
/// reshape into the typed `ProcList` shape. `/api/ps5/process/list` is the detailed one (title ids,
/// memory, kill support).
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
    #[serde(alias = "console", alias = "addr")]
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
/// best-effort: it never delays or fails the status answer.
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

#[derive(Debug, serde::Deserialize)]
struct KlogQuery {
    #[serde(alias = "console", alias = "host")]
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
        Ok(text) => (StatusCode::OK, Json(console_log_reply(&text))).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// A console's kernel log or syslog as a reply: nothing of ours writes a secret there, but
/// everything on the console can, and these go to whoever asks.
fn console_log_reply(text: &str) -> serde_json::Value {
    serde_json::json!({ "text": ps5upload_core::secret_text::without_secrets(text) })
}

/// GET /api/cache/artwork — how much disk the artwork cache is using.
///
/// Exposed because cached artwork outlives the game it came from: the
/// files stay after a title is uninstalled. Small, and on the user's own
/// machine, but it is data that persists past its subject, so it should be
/// visible and removable rather than silently accumulating.
async fn cache_artwork_stats() -> impl IntoResponse {
    // Walks the cache folder: off the async workers.
    let (files, bytes) = tokio::task::spawn_blocking(icon_cache::stats)
        .await
        .unwrap_or((0, 0));
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
    let freed = tokio::task::spawn_blocking(icon_cache::clear)
        .await
        .unwrap_or(0);
    (
        StatusCode::OK,
        Json(serde_json::json!({ "ok": true, "freed_bytes": freed })),
    )
}

#[derive(Deserialize)]
struct AppInfoQueryParams {
    #[serde(alias = "console", alias = "host")]
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
struct HiddenIconsParams {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

/// GET /api/ps5/apps/hidden — the titles whose home-screen icon is hidden (#407), read live
/// from app.db so the list never drifts from the console.
async fn ps5_hidden_icons(
    State(state): State<AppState>,
    Query(q): Query<HiddenIconsParams>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::diagnostics::hidden_icons(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// GET /api/ps5/net/ports — the ports held on the console (Discord ask). The helper binds
/// each port and closes it, so nothing on the console sees a connection. About a second.
async fn ps5_net_ports(
    State(state): State<AppState>,
    Query(q): Query<HiddenIconsParams>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let r = tokio::task::spawn_blocking(move || ps5upload_core::diagnostics::console_ports(&addr))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Deserialize)]
struct UsbEjectReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    mount_point: String,
    #[serde(default)]
    force: bool,
}

/// POST /api/ps5/usb/eject `{addr, mount_point, force}` — unmount a USB drive (`/mnt/usbN`)
/// so it can be pulled. Without `force`, `usb_eject_busy` means something on the console
/// holds it open; `force` unmounts anyway after a sync.
async fn ps5_usb_eject(
    installs: pkg_install::PkgInstallStateHandle,
    req: UsbEjectReq,
) -> axum::response::Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let mp = req.mount_point;
    let force = req.force;
    crate::log_info!("usb_eject: addr={addr} mount_point={mp} force={force}");
    // Our own job writing to the drive holds it open too, and a forced unmount would cut it off:
    // truncated files, and possibly a damaged exFAT volume.
    if helper_stop::is_busy(&addr) || installs.jobs.active_for(&addr) {
        return eject_refused(EJECT_REFUSED_JOBS);
    }
    let r = tokio::task::spawn_blocking(move || {
        // A forced unmount also takes a drive from a game playing from it. The helper can't say
        // which drive a game reads, so any game holds a force. Not being able to ask is no
        // reason to refuse: the helper is what answers the eject as well.
        if force && eject_game_running(&addr) {
            anyhow::bail!(EJECT_REFUSED_GAME);
        }
        ps5upload_core::fs_ops::usb_eject(&addr, &mp, force)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) if e.to_string() == EJECT_REFUSED_GAME => eject_refused(EJECT_REFUSED_GAME),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// An eject refused because this engine runs a transfer, file operation or install there.
const EJECT_REFUSED_JOBS: &str = "usb_eject_refused_jobs";
/// A forced eject refused because a game is running.
const EJECT_REFUSED_GAME: &str = "usb_eject_refused_game";

fn eject_refused(code: &str) -> axum::response::Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": code })),
    )
        .into_response()
}

fn eject_game_running(addr: &str) -> bool {
    process_list(addr)
        .map(|list| list.processes.iter().any(|p| p.kind == "app"))
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct SaveOpenReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// The save image under /user/home; empty or absent only reports what is open.
    #[serde(default)]
    image: String,
}

/// POST /api/ps5/saves/open `{addr, image}` — the helper mounts the save decrypted (copied
/// under /data first) and says where; the Files screen then reads and writes it. One save
/// open at a time; another one closes the first. Empty `image` reports what is open.
async fn ps5_save_open(Json(req): Json<SaveOpenReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let image = req.image;
    crate::log_info!("save_open: addr={addr} image={image}");
    let r = tokio::task::spawn_blocking(move || {
        let _busy = helper_stop::enter(&addr);
        // The open copy is written back over the real image on close, so a game writing the save
        // meanwhile would lose what it wrote. An empty image only reports what is open.
        if let Some(title) = save_backup::title_of_save_image(&image) {
            if save_backup::title_running(&addr, title).map_err(anyhow::Error::msg)? {
                anyhow::bail!("{title} is running on the PS5. Close the game, then open its save.");
            }
        }
        ps5upload_core::saves::save_open(&addr, &image)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/saves/close `{addr}` — unmounts the open save and writes it back.
async fn ps5_save_close(Json(req): Json<SaveOpenReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    crate::log_info!("save_close: addr={addr}");
    let r = tokio::task::spawn_blocking(move || {
        let _busy = helper_stop::enter(&addr);
        ps5upload_core::saves::save_close(&addr)
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
struct SetIconHiddenReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
    hidden: bool,
}

/// POST /api/ps5/apps/hidden `{addr, title_id, hidden}` — hide or show a title's home-screen
/// icon (#407). Only app.db's `visible` flag changes; the title stays installed. A refusal
/// (a system app, a title with no icon, app.db locked) is a 200 with `ok: false` and the
/// helper's reason, so the app can show it.
async fn ps5_set_icon_hidden(Json(req): Json<SetIconHiddenReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::set_icon_hidden(&addr, &req.title_id, req.hidden)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
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
    #[serde(alias = "console", alias = "host")]
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
    State(_state): State<AppState>,
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    pid: i32,
}

/// POST /api/ps5/process/kill — SIGKILL a pid. Body: `{ addr, pid }`.
async fn ps5_process_kill(
    State(_state): State<AppState>,
    Json(req): Json<ProcessKillReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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

// ── Helper stop (#471) ──────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct HelperStopReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

/// POST /api/ps5/helper/stop — Body: `{ addr }`. Asks the console's helper to exit
/// (`node.shutdown`), refused while this engine runs a job against that console.
///
/// 200 `{ "state": "stopped" }` once the helper's port went quiet, or `{ "state": "not_running" }`
/// when nothing answered. 409 `{ "error", "reason": "busy" }` while a job runs; 502
/// `{ "error", "reason": "refused" | "still_running" }` otherwise.
async fn ps5_helper_stop(
    installs: pkg_install::PkgInstallStateHandle,
    req: HelperStopReq,
) -> axum::response::Response {
    // Disruptive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let busy = helper_stop::is_busy(&addr) || installs.jobs.active_for(&addr);
    let r = tokio::task::spawn_blocking(move || helper_stop::stop(&addr, busy)).await;
    match r {
        Ok(Ok(helper_stop::Stopped::Stopped)) => {
            Json(serde_json::json!({ "state": "stopped" })).into_response()
        }
        Ok(Ok(helper_stop::Stopped::NotRunning)) => {
            Json(serde_json::json!({ "state": "not_running" })).into_response()
        }
        Ok(Err(e)) => {
            let code = if e == helper_stop::StopError::Busy {
                StatusCode::CONFLICT
            } else {
                StatusCode::BAD_GATEWAY
            };
            (
                code,
                Json(serde_json::json!({ "error": e.message(), "reason": e.reason() })),
            )
                .into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

// ── Power control + telemetry ───────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct PowerControlReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    /// One of "reboot" | "shutdown" | "standby" | "tick".
    action: String,
}

/// POST /api/ps5/power/control — Body: `{ addr, action }`.
async fn ps5_power_control(
    State(_state): State<AppState>,
    Json(req): Json<PowerControlReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "addr")]
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
    #[serde(alias = "console", alias = "addr")]
    host: String,
    #[serde(default)]
    credential: String,
    /// Both hex, harvested from a pairing. Signing in needs the full session keys, not just the
    /// wake credential; left out, the ones the engine keeps for this console are used.
    #[serde(default)]
    regist_key: String,
    #[serde(default)]
    rp_key: String,
    /// Which of the console's paired PS5 users to sign in (an account id, a name as
    /// `GET /api/consoles` lists it under `rp_users`, or its number there), when the keys are left
    /// out. Left out, the chosen user (the one Play starts as).
    #[serde(default)]
    user: Option<String>,
    /// Someone is there to type the user's login passcode if the console asks: the sign-in is
    /// held and the passcode asked for (wake_login_api). Otherwise (an automatic wake) a
    /// passcode request ends it with the console awake at its sign-in screen.
    #[serde(default)]
    ask_passcode: bool,
}

/// POST /api/ps5/power/wake-login — wake the console AND sign its user in.
///
/// A bare wake lands at user-select; this follows the wake with a Remote
/// Play control session so the console comes up on the user's home screen,
/// the way the official app does. Long-running: it waits for the console to
/// boot before signing in.
async fn ps5_power_wake_login(Json(mut req): Json<PowerWakeLoginReq>) -> impl IntoResponse {
    req.host = consoles_api::resolve(&req.host);
    if req.regist_key.trim().is_empty() && req.rp_key.trim().is_empty() {
        match consoles_api::rp_keys_for(&req.host, req.user.as_deref()) {
            Ok((regist_key, rp_key)) => {
                req.regist_key = regist_key;
                req.rp_key = rp_key;
                // The wake credential goes with the user signed in.
                if req.user.is_some() {
                    req.credential.clear();
                }
            }
            Err(consoles_api::NoKeys::NoSuchUser(names)) => {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "no paired user of this console is {:?}; paired: {}",
                        req.user.as_deref().unwrap_or_default(),
                        names.join(", ")
                    ),
                )
                .into_response();
            }
            Err(consoles_api::NoKeys::NotPaired) => {}
        }
    }
    let host = req.host.clone();
    let cred = req.credential.clone();
    let creds =
        match ps5upload_core::rp_session::SessionCreds::from_hex(&req.regist_key, &req.rp_key) {
            Ok(c) => c,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        };
    let regist_key = req.regist_key.clone();
    let job: wake_login_api::SignInJob = Box::new(move |prompt| {
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
        log_at!(
            Debug,
            "wake",
            "wake-login {host}: {} then waiting for the session port to sign in",
            if credential.trim().is_empty() {
                "no wake credential, not woken,"
            } else {
                "woken,"
            }
        );

        ps5upload_core::rp_session::login_session_when_ready(
            &host,
            &creds,
            std::time::Duration::from_secs(
                std::env::var("PS5UPLOAD_SIGNIN_BUDGET_S")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(90),
            ),
            prompt,
        )
    });
    wake_login_api::start(job, req.ask_passcode).await
}

async fn ps5_power_wake(Json(req): Json<PowerWakeReq>) -> impl IntoResponse {
    let host = consoles_api::resolve(&req.host);
    let from_request = !req.credential.trim().is_empty();
    let cred = if from_request {
        req.credential.clone()
    } else {
        consoles_api::wake_credential(&host).unwrap_or_default()
    };
    // Where the key came from, never the key.
    log_at!(
        Debug,
        "wake",
        "power/wake {host}: credential {}",
        if from_request {
            "from the request"
        } else if cred.is_empty() {
            "missing (none saved for this console)"
        } else {
            "from the saved console"
        }
    );
    // A PS5 on Tailscale whose home address is silent may be asleep at home (wake works) or
    // away from this device's network (it cannot: a resting PS5 is off Tailscale, and a wake is
    // a home-network broadcast). The wake goes out either way; the note says the second plainly.
    let away_note = tailnet_engine::engine()
        .home_silent(&host)
        .then_some(tailnet_engine::WAKE_AWAY_NOTE);
    match tokio::task::spawn_blocking(move || ps5upload_core::ddp::wake(&host, &cred)).await {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "sent": true, "tailnet_note": away_note })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response(),
    }
}

// No Debug: the PIN must not reach a log line by accident.
#[derive(serde::Deserialize)]
struct PowerPairReq {
    /// The payload's management address. Optional — falls back to the
    /// engine's configured console, like every other payload call.
    #[serde(alias = "console")]
    addr: Option<String>,
    /// The console's address. Registration and wake use their own ports,
    /// so this is the bare host, not the management address.
    host: String,
    /// The 8-digit PIN the PS5 shows under Settings › System › Remote Play › Link Device. With
    /// it the engine registers on its own (no helper, any firmware); never logged.
    #[serde(default)]
    pin: Option<String>,
    /// The PSN account id (base64 of its 8 bytes) to pair, with `pin`.
    #[serde(default)]
    account_id: Option<String>,
    /// The paired user's name, for telling a console's paired users apart. Left out, the helper's
    /// name for the user is used when it can be read. Pairing another user adds to the console's
    /// paired users; pairing one again replaces that user's keys.
    #[serde(default)]
    user_name: Option<String>,
}

/// POST /api/ps5/power/pair — pair with a console so it can be woken.
///
/// One call, nothing asked of the user: the payload supplies the PSN
/// account id and mints the PIN, and the engine runs the registration
/// handshake. Returns the wake credential for the client to store.
/// With `pin` (and `account_id`), the PIN the PS5 shows on its Link Device
/// screen is used instead, and the helper is not needed.
///
/// The console has to be awake with the payload running. That is not a
/// limitation worth working around — pairing happens once, and what it
/// buys is the ability to wake the console later, when it is not.
async fn ps5_power_pair(Json(req): Json<PowerPairReq>) -> impl IntoResponse {
    // `host` is the console being paired: it names one even without `addr`.
    let host = consoles_api::resolve(&req.host);
    let Some(addr) = required_console_addr(req.addr.or_else(|| Some(host.clone()))) else {
        return missing_console_addr();
    };
    let manual = req.pin.filter(|p| !p.trim().is_empty());
    let with = if manual.is_some() { "pin" } else { "helper" };
    let tried_host = host.clone();
    crate::log_info!(
        "power_pair: addr={addr} host={host} with={}",
        if manual.is_some() {
            "the PS5's PIN"
        } else {
            "the helper"
        }
    );
    let account_id = req.account_id.unwrap_or_default();
    let user_name = req
        .user_name
        .map(|n| n.trim().chars().take(64).collect::<String>())
        .filter(|n| !n.is_empty());
    let r = tokio::task::spawn_blocking(move || {
        let mut p = match manual {
            Some(pin) => ps5upload_core::rp_regist::pair_with_pin(&host, &account_id, &pin),
            None => ps5upload_core::rp_regist::pair_with_console(&addr, &host),
        }?;
        if let Some(name) = user_name {
            p.user_name = name;
        }
        // The engine keeps the keys too, so a script can start a stream by address alone.
        consoles_api::remember_pairing(&host, &p);
        Ok::<_, anyhow::Error>(p)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    // Kept for GET /api/ps5/rp/pair/attempts and the log, scrubbed of anything like a PIN or key.
    rp_diag::pair_attempt(
        &tried_host,
        with,
        r.as_ref().err().map(|e| format!("{e:#}")).as_deref(),
    );
    match r {
        Ok(p) => (StatusCode::OK, Json(p)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct DdpStatusQuery {
    #[serde(alias = "console", alias = "addr")]
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

// ── Saves / screenshots / videos ────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
struct SavesListQuery {
    #[serde(alias = "console", alias = "host")]
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

/// POST /api/ps5/smp/rescan?addr= — ask ShadowMount+ to scan again now, with its retry counters
/// cleared, so a new image of a game is picked up without deleting the game from the home screen.
async fn ps5_smp_rescan(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let done = tokio::task::spawn_blocking(move || {
        let (api, _) = ps5upload_core::smp_api::SmpApi::probe(&addr).ok_or_else(|| {
            anyhow::anyhow!(
                "ShadowMount+'s API doesn't answer from this computer: it needs ShadowMount+ \
                 1.7 or later running, with api_enabled on and api_bind_address open to the \
                 network in /data/shadowmount/config.ini"
            )
        })?;
        api.rescan()
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match done {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
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
    #[serde(alias = "console", alias = "host")]
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
async fn ps5_smp_checkout_begin(Json(req): Json<SmpCheckoutBeginReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
async fn ps5_smp_checkout_finish(Json(q): Json<AddrQuery>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

async fn ps5_smp_image_rw_begin(
    State(_state): State<AppState>,
    Json(req): Json<SmpImageRwBeginReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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

async fn ps5_smp_image_rw_finish(Json(q): Json<AddrQuery>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
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
    State(_state): State<AppState>,
    Json(req): Json<FsWriteBytesReq>,
) -> impl IntoResponse {
    use base64::Engine as _;
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
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
async fn ps5_peripheral(Json(q): Json<PeripheralReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
    let (action, port) = (q.action, q.port);
    match tokio::task::spawn_blocking(move || {
        ps5upload_core::diagnostics::peripheral_control(&addr, action, port)
    })
    .await
    {
        Ok(Ok(ack)) => Json(serde_json::to_value(ack).unwrap_or_default()).into_response(),
        // The helper refused: not a transport failure, so it says so in `reason`.
        Ok(Err(e)) if format!("{e:#}").contains("PERIPHERAL_CONTROL failed") => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("{e:#}"), "reason": "helper_refused" })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e}")).into_response(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct FanThresholdReq {
    #[serde(alias = "console", alias = "host")]
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
async fn ps5_hw_set_fan_threshold(Json(q): Json<FanThresholdReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
    /// game-icon only: 128, 256 or 384 for a scaled copy (see `icon_thumb_px`).
    #[serde(default)]
    size: Option<u32>,
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
    // A grid's size: the cover scaled down once and cached beside the full one (#450).
    thumb: Option<u32>,
) -> axum::response::Response {
    if let Some(px) = thumb {
        return serve_icon_thumbnail(addr, kind, identity, remote_paths, if_none_match, px).await;
    }
    // A revalidation we can answer from cache costs no console round-trip
    // and no body — this is the cheap path once max-age lapses.
    // Disk reads and writes of the cache run on the blocking pool, not the async workers.
    let cached = {
        let (addr, identity) = (addr.clone(), identity.clone());
        tokio::task::spawn_blocking(move || icon_cache::get(&addr, kind, &identity))
            .await
            .unwrap_or(icon_cache::Cached::Unknown)
    };
    match cached {
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

    // Ok(Some) = found; Ok(None) = every path cleanly absent; Err = at least
    // one path failed for another reason (the console may just be busy).
    // The cache is written in the same blocking task.
    //
    // Only a clean "there is nothing there" is remembered. A transport
    // error must NOT be: a console that was briefly unreachable would
    // otherwise be recorded as having no artwork at all, and every
    // cover would vanish for the length of the negative TTL.
    let result = fetch_icon(addr, kind, identity, remote_paths).await;

    match result {
        Ok(Some(bytes)) => {
            let etag = icon_cache::etag_for(&bytes);
            icon_response(bytes, &etag)
        }
        Ok(None) => (StatusCode::NOT_FOUND, "no icon").into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no icon").into_response(),
    }
}

/// The full cover from the console, written to the cache. Ok(Some) = found; Ok(None) = every
/// path cleanly absent; Err = at least one path failed for another reason (the console may just
/// be busy). The cache is written in the same blocking task.
///
/// Only a clean "there is nothing there" is remembered. A transport error must NOT be: a console
/// that was briefly unreachable would otherwise be recorded as having no artwork at all, and every
/// cover would vanish for the length of the negative TTL.
async fn fetch_icon(
    addr: String,
    kind: &'static str,
    identity: String,
    remote_paths: Vec<String>,
) -> Result<Option<Vec<u8>>, anyhow::Error> {
    tokio::task::spawn_blocking(move || {
        let mut transport_err = None;
        for p in &remote_paths {
            match fs_read(&addr, p, 0, 2 * 1024 * 1024) {
                Ok(bytes) if !bytes.is_empty() => {
                    icon_cache::put(&addr, kind, &identity, &bytes);
                    return Ok(Some(bytes));
                }
                Ok(_) => {}
                Err(e) if icon_cache::is_console_said_no(&e) => {}
                Err(e) => transport_err = Some(e),
            }
        }
        match transport_err {
            Some(e) => Err(e),
            None => {
                icon_cache::put_missing(&addr, kind, &identity);
                Ok(None)
            }
        }
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|inner| inner)
}

/// The sizes a grid may ask an icon route for; any other `size` serves the full cover.
fn icon_thumb_px(size: Option<u32>) -> Option<u32> {
    size.filter(|s| [128, 256, 384].contains(s))
}

/// `bytes` (a cover, usually a 512 px icon0.png of 300-700 KiB) scaled to fit `px`: JPEG when
/// it is opaque, which every PS4/PS5 cover measured was (about 20 KiB at 256 px), PNG when it
/// has transparency. None when it is already that small or does not decode.
fn icon_thumbnail(bytes: &[u8], px: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    if img.width() <= px && img.height() <= px {
        return None;
    }
    let small = img.resize(px, px, image::imageops::FilterType::Triangle);
    let opaque = !small.color().has_alpha() || small.to_rgba8().pixels().all(|p| p.0[3] == 255);
    let mut out = Vec::new();
    if opaque {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
            .encode_image(&small.to_rgb8())
            .ok()?;
    } else {
        small
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .ok()?;
    }
    Some(out)
}

/// [`serve_cached_icon`] for a grid: the cover scaled to `px`, cached under its own key so a
/// shelf of covers costs a few KiB each to send and decode instead of the full PNGs (#450:
/// a macOS Games tab with hundreds of 400 KiB covers stuttered, worse through the data-URL
/// fallback). The full cover is fetched (or read from the cache) once to make it.
async fn serve_icon_thumbnail(
    addr: String,
    kind: &'static str,
    identity: String,
    remote_paths: Vec<String>,
    if_none_match: Option<String>,
    px: u32,
) -> axum::response::Response {
    let tkind = format!("{kind}@{px}");
    let cached = {
        let (addr, identity, tkind) = (addr.clone(), identity.clone(), tkind.clone());
        tokio::task::spawn_blocking(move || icon_cache::get(&addr, &tkind, &identity))
            .await
            .unwrap_or(icon_cache::Cached::Unknown)
    };
    match cached {
        icon_cache::Cached::Hit { bytes, etag } => {
            if icon_cache::etag_matches(if_none_match.as_deref(), &etag) {
                return (StatusCode::NOT_MODIFIED, [(header::ETAG, etag.as_str())]).into_response();
            }
            return icon_response(bytes, &etag);
        }
        icon_cache::Cached::KnownMissing => {
            return (StatusCode::NOT_FOUND, "no icon").into_response();
        }
        icon_cache::Cached::Unknown => {}
    }
    let full = {
        let (a, i) = (addr.clone(), identity.clone());
        let hit = tokio::task::spawn_blocking(move || icon_cache::get(&a, kind, &i))
            .await
            .unwrap_or(icon_cache::Cached::Unknown);
        match hit {
            icon_cache::Cached::Hit { bytes, .. } => Ok(Some(bytes)),
            icon_cache::Cached::KnownMissing => Ok(None),
            icon_cache::Cached::Unknown => {
                fetch_icon(addr.clone(), kind, identity.clone(), remote_paths).await
            }
        }
    };
    match full {
        Ok(Some(full)) => {
            let small = tokio::task::spawn_blocking(move || {
                let small = icon_thumbnail(&full, px).unwrap_or(full);
                icon_cache::put(&addr, &tkind, &identity, &small);
                small
            })
            .await;
            match small {
                Ok(small) => {
                    let etag = icon_cache::etag_for(&small);
                    icon_response(small, &etag)
                }
                Err(_) => (StatusCode::NOT_FOUND, "no icon").into_response(),
            }
        }
        _ => (StatusCode::NOT_FOUND, "no icon").into_response(),
    }
}

fn icon_response(bytes: Vec<u8>, etag: &str) -> axum::response::Response {
    // A thumbnail may be JPEG; a cover from the console is PNG.
    let content_type = if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        "image/png"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
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
    serve_cached_icon(
        addr,
        "game",
        path,
        vec![icon_path],
        inm,
        icon_thumb_px(q.size),
    )
    .await
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
    /// Where this list comes from: read from the console now (`/api/games/installed` is what
    /// saved consoles had when last read; `/api/pkg/installed` is one title's package files).
    source: &'static str,
    titles: Vec<InstalledApp>,
    /// True if app_list_registered() failed (e.g. older payload) — the
    /// UI then can't reliably tag the "registered" group, so it shows
    /// everything under "installed" with a soft note rather than erroring.
    registered_unavailable: bool,
    /// True when the list may be missing titles: a directory listing hit its page size,
    /// or app.db could not be read in full (a failed query, or a helper that could not
    /// deliver every row, #473). The titles shown are real; there may be more.
    incomplete: bool,
}

/// app.db, read at most once per request: a paged read is several calls (#473), and the
/// installed list asks it for names title by title.
struct AppDbOnce<'a> {
    addr: &'a str,
    got: Option<Result<ps5upload_core::diagnostics::AppDbList, String>>,
}

impl<'a> AppDbOnce<'a> {
    fn new(addr: &'a str) -> Self {
        Self { addr, got: None }
    }

    fn get(&mut self) -> &Result<ps5upload_core::diagnostics::AppDbList, String> {
        let addr = self.addr;
        self.got
            .get_or_insert_with(|| appdb_query(addr).map_err(|e| format!("{e:#}")))
    }

    /// app.db's display name for `title_id`, when it has one that is not the id itself.
    fn name_of(&mut self, title_id: &str) -> Option<String> {
        let list = self.get().as_ref().ok()?;
        list.apps
            .iter()
            .filter(|a| a.title_id == title_id)
            .map(|a| a.name.trim())
            .find(|n| !n.is_empty() && *n != title_id)
            .map(str::to_string)
    }
}

/// PS5 title-ids are 4 uppercase letters + 5 digits (PPSA01234, NPXS39041,
/// CUSA00123, …). Used to filter stray non-title entries out of the
/// /user/appmeta listing, and the console's own folders out of save backups.
pub(crate) fn looks_like_title_id(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 9
        && b[..4].iter().all(|c| c.is_ascii_uppercase())
        && b[4..].iter().all(|c| c.is_ascii_digit())
}

/// The title id of the console web UI's home-screen tile. It is a link to the PS5Upload page, not a
/// game, so no PC library listing may show it. Keep in sync with `WEBUI_TILE_TITLE_ID` in
/// payload/include/config.h (a test below checks they match).
pub(crate) const CONSOLE_WEBUI_TILE_TITLE_ID: &str = "PSUP00001";

/// True for the console page's tile, which the PC library hides.
pub(crate) fn is_console_webui_tile(title_id: &str) -> bool {
    title_id
        .trim()
        .eq_ignore_ascii_case(CONSOLE_WEBUI_TILE_TITLE_ID)
}

/// Best-effort title name from `/user/appmeta/<title_id>/`. Tries PS5
/// `param.json` first, then falls back to PS4 `param.sfo`, then queries
/// `app.db` (raw file scan when sqlite is unavailable). Returns None on
/// any failure (no metadata file — common for system apps — bad JSON,
/// bad SFO, path denied); the caller falls back to the bare title_id.
fn appmeta_title_name(addr: &str, title_id: &str, appdb: &mut AppDbOnce<'_>) -> Option<String> {
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
    appdb.name_of(title_id)
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
            || is_console_webui_tile(tid)
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

/// Candidates for titles whose app folder exists but that neither the /user/appmeta scan
/// nor app.db produced (#473), taken from the helper's registered list.
///
/// Only when app.db could not be read in full (`appdb_whole` false): with a complete app.db, a
/// folder it does not list is not an installed title (a failed install, a partial uninstall),
/// and the helper's list holds every /user/app folder, not only titles we registered. System
/// ids (NPXS) are left out, as in [`appdb_installed_additions`]. The caller still checks each
/// candidate for evidence it is a title (a mount.lnk source or a param file). Sorted, so the
/// list does not reshuffle between refreshes.
fn registered_only_additions<'a>(
    registered: impl IntoIterator<Item = &'a str>,
    app_dirs: &std::collections::HashSet<String>,
    seen: &std::collections::HashSet<String>,
    appdb_whole: bool,
) -> Vec<String> {
    if appdb_whole {
        return Vec::new();
    }
    let mut out: Vec<String> = registered
        .into_iter()
        .map(str::trim)
        .filter(|tid| {
            looks_like_title_id(tid)
                && !tid.starts_with("NPXS")
                && app_dirs.contains(*tid)
                && !seen.contains(*tid)
        })
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// True when `/user/app/<id>/sce_sys` holds a param.json or param.sfo: the folder is a title,
/// not the leftovers of a failed install.
fn app_folder_has_param(addr: &str, title_id: &str) -> bool {
    ["param.json", "param.sfo"]
        .iter()
        .any(|f| fs_read(addr, &format!("/user/app/{title_id}/sce_sys/{f}"), 0, 16).is_ok())
}

/// GET /api/ps5/apps/installed?addr=IP:MGMT_PORT — what is on the console now, read from it.
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

            let mut appdb = AppDbOnce::new(&addr);
            // Full installed set: every /user/appmeta/<title_id>/ dir, page by page (the
            // helper clamps a page to 256 entries, so one page is not the whole set).
            let (listing, listing_whole) = list_dir_whole(&addr, "/user/appmeta")?;

            let mut titles: Vec<InstalledApp> = Vec::new();
            for e in listing {
                if !looks_like_title_id(&e.name) || is_console_webui_tile(&e.name) {
                    continue;
                }
                let tid = e.name;
                let system = tid.starts_with("NPXS");
                if let Some(reg) = reg_map.get(&tid) {
                    // Prefer the registered title_name; if it's empty or just
                    // the id (older registrations stored id-as-name), try
                    // param.json for a friendlier label.
                    let name = if reg.title_name.trim().is_empty() || reg.title_name == tid {
                        appmeta_title_name(&addr, &tid, &mut appdb).unwrap_or_else(|| tid.clone())
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
                    let name =
                        appmeta_title_name(&addr, &tid, &mut appdb).unwrap_or_else(|| tid.clone());
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
            // Whether the list below is the console's whole installed set.
            let mut complete = listing_whole;
            let mut app_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
            // Whether the internal drive's listing was read whole: None when it could not be
            // read at all.
            let mut internal_whole: Option<bool> = None;
            for root in ["/user/app", "/mnt/ext0/user/app", "/mnt/ext1/user/app"] {
                if let Ok((entries, whole)) = list_dir_whole(&addr, root) {
                    if root == "/user/app" {
                        internal_whole = Some(whole);
                    }
                    app_dirs.extend(entries.into_iter().map(|e| e.name));
                }
            }
            complete = complete
                && crate::console_snapshot::app_folders_whole(internal_whole, app_dirs.len());
            if !app_dirs.is_empty() {
                // Whether app.db answered in full: only then is a folder it does not list
                // known not to be an installed title.
                let mut appdb_whole = true;
                let rows: Option<Vec<(String, String)>> = match appdb.get() {
                    Ok(list) => {
                        // `err` with no rows: the helper could not read app.db at all.
                        if list.truncated || list.err.is_some() {
                            // Every row the helper delivered is used; the list just may not be whole.
                            crate::log_warn!(
                                "apps/installed: app.db read incomplete ({} rows{})",
                                list.apps.len(),
                                list.err
                                    .as_deref()
                                    .map(|e| format!(": {e}"))
                                    .unwrap_or_default()
                            );
                            complete = false;
                            appdb_whole = false;
                        }
                        Some(
                            list.apps
                                .iter()
                                .map(|a| (a.title_id.clone(), a.name.clone()))
                                .collect(),
                        )
                    }
                    Err(e) => {
                        crate::log_warn!("apps/installed: app.db query failed: {e}");
                        complete = false;
                        appdb_whole = false;
                        None
                    }
                };
                if let Some(rows) = rows {
                    let seen: std::collections::HashSet<String> =
                        titles.iter().map(|t| t.title_id.clone()).collect();
                    for (tid, db_name) in appdb_installed_additions(&rows, &app_dirs, &seen) {
                        let name = match db_name {
                            Some(n) => n,
                            None => appmeta_title_name(&addr, &tid, &mut appdb)
                                .unwrap_or_else(|| tid.clone()),
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
                // When app.db could not be read in full, a title it did not get to still
                // has its folder and its registration (#473): list it if the folder holds a
                // title (our mount.lnk source, or a param file), never a bare leftover folder.
                let seen: std::collections::HashSet<String> =
                    titles.iter().map(|t| t.title_id.clone()).collect();
                for tid in registered_only_additions(
                    reg_map.keys().map(String::as_str),
                    &app_dirs,
                    &seen,
                    appdb_whole,
                ) {
                    let Some(reg) = reg_map.get(&tid) else {
                        continue;
                    };
                    if reg.src.trim().is_empty() && !app_folder_has_param(&addr, &tid) {
                        continue;
                    }
                    let name = if reg.title_name.trim().is_empty() || reg.title_name == tid {
                        appmeta_title_name(&addr, &tid, &mut appdb).unwrap_or_else(|| tid.clone())
                    } else {
                        reg.title_name.clone()
                    };
                    titles.push(InstalledApp {
                        system: false,
                        title_id: tid,
                        title_name: name,
                        origin: "registered".to_string(),
                        image_backed: reg.image_backed,
                        source: reg.src.clone(),
                    });
                }
            }
            // Kept for the game page. System apps are not games.
            {
                let list: Vec<(String, String, Option<String>)> = titles
                    .iter()
                    .filter(|t| !t.system)
                    .map(|t| {
                        let from = (t.origin == "registered" && !t.source.is_empty())
                            .then(|| t.source.clone());
                        (t.title_id.clone(), t.title_name.clone(), from)
                    })
                    .collect();
                let now = crate::console_snapshot::now_unix();
                crate::console_snapshot::with(|snaps| {
                    crate::console_snapshot::merge_full_list(snaps, &addr, &list, complete, now)
                });
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
                source: "console",
                titles,
                registered_unavailable,
                incomplete: !complete,
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
    /// 128, 256 or 384: a scaled copy for a grid. Absent: the full cover.
    #[serde(default)]
    size: Option<u32>,
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
    serve_cached_icon(
        addr,
        "app",
        q.title_id,
        icon_paths,
        inm,
        icon_thumb_px(q.size),
    )
    .await
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

/// Pages a listing is read in: the helper clamps every `fs.list` page to this many entries
/// whatever limit is asked for, so a bigger limit never makes one page the whole folder.
const LIST_DIR_PAGE: u64 = 256;
/// Pages read before a folder is given up as not whole (16k entries).
const LIST_DIR_MAX_PAGES: usize = 64;

/// Every entry of a console folder, page by page, and whether the folder was read whole
/// (`false` when it has more pages than [`LIST_DIR_MAX_PAGES`]). A listing that fails on any
/// page is an error: a busy console must not read as a shorter folder.
fn list_dir_whole(
    addr: &str,
    path: &str,
) -> anyhow::Result<(Vec<ps5upload_core::fs_ops::DirEntry>, bool)> {
    let mut out = Vec::new();
    let mut offset = 0u64;
    for _ in 0..LIST_DIR_MAX_PAGES {
        let page = list_dir(
            addr,
            path,
            ListDirOptions {
                offset,
                limit: LIST_DIR_PAGE,
            },
        )?;
        let n = page.entries.len() as u64;
        out.extend(page.entries);
        if !page.truncated || n == 0 {
            return Ok((out, true));
        }
        offset += n;
    }
    Ok((out, false))
}

pub fn scan_external_pkgs(addr: &str) -> anyhow::Result<Vec<ExternalPkg>> {
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
            // Every page: a folder of more than 256 packages used to stop at the first.
            let entries = match list_dir_whole(addr, &dir) {
                Ok((entries, _)) => entries,
                Err(_) => continue,
            };
            for e in entries {
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
    #[serde(alias = "console", alias = "host")]
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
    let console = addr.clone();
    // `node.status` through the management seam: the typed AVA1 NodeStatus is rebuilt into the
    // legacy JSON (`ucred_elevated` a bool, `prior_instance` only when present). The old
    // transaction fields (runtime_port, shutdown, takeover_requested, active_transactions,
    // last_tx_seq, recovered_transactions) no longer exist; nothing reads them.
    // A short bound per attempt (`STATUS_TIMEOUT`): the helper answers in milliseconds, and a
    // session that went quiet is dropped and replaced rather than waited on (AvaTransport's
    // status path), so a poll answers in seconds even when the console just went away.
    let result = tokio::task::spawn_blocking(move || {
        let body = ps5upload_core::mgmt::call_with(
            &addr,
            ps5upload_core::mgmt::m::NODE_STATUS,
            ps5upload_core::mgmt::m::NODE_STATUS.label,
            b"",
            Some(ps5upload_ava1::mgmt::STATUS_TIMEOUT),
        )?;
        let json: serde_json::Value = serde_json::from_slice(&body)?;
        Ok::<_, anyhow::Error>(json)
    })
    .await;
    // The path the check took, for its connect timings.
    let path = connect_path(&ps5upload_core::route::via(&console));
    ps5upload_ava1::timings::update(&console, |t| t.path = Some(path.to_string()));

    use ps5upload_core::events::{emit, Cat, Level};
    match result {
        Ok(Ok(json)) => {
            // Collapsed by the journal: one line (with a count) per minute of polling.
            emit(
                Cat::Connection,
                Level::Info,
                "status_ok",
                Some(&console),
                "status ok",
            );
            helper_mirror::note_console(&console);
            spawn_console_upgrade_cleanup(console);
            (StatusCode::OK, Json(json)).into_response()
        }
        Ok(Err(e)) => {
            emit(
                Cat::Connection,
                Level::Warn,
                "status_failed",
                Some(&console),
                format!("{e:#}"),
            );
            let power = ps5upload_ava1::console::recent_power(&console);
            (
                StatusCode::BAD_GATEWAY,
                Json(status_failure_body(&e, power)),
            )
                .into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// The connect timings' name for how a console is reached.
fn connect_path(via: &ps5upload_core::route::Via) -> &'static str {
    use ps5upload_core::route::Via;
    match via {
        Via::Lan => "lan",
        Via::TailnetDirect { .. } => "tailnet_direct",
        Via::TailnetRelayed => "tailnet_relayed",
        Via::System => "tailnet_system",
    }
}

/// GET /api/ps5/connect-timings?addr=IP
///
/// The last status check of each console, phase by phase: whether a pooled session was reused
/// or a new one opened (`handshake_ms`: the TCP connect, the AVA1 handshake and the pairing
/// check), whether a verdict remembered from a moment ago answered (`cached_verdict`), the
/// plain TCP connect that looks past such a verdict or checks a quiet session (`tcp_ms`,
/// `tcp_ok`), the `node.status` call (`status_ms`), the helper's `version`, the `path` (`lan`,
/// `tailnet_direct`, `tailnet_relayed`, `tailnet_system`), and whether a session that had gone
/// quiet was dropped (`stale_session_dropped`). Read on demand; nothing is logged. With `addr`,
/// that console only (`{ timing: null }` before its first check); without, `{ consoles: [...] }`,
/// newest first.
async fn ps5_connect_timings(Query(q): Query<AddrQuery>) -> impl IntoResponse {
    match q.addr.filter(|a| !a.trim().is_empty()) {
        Some(addr) => Json(serde_json::json!({
            "timing": ps5upload_ava1::timings::last(&addr),
        })),
        None => Json(serde_json::json!({
            "consoles": ps5upload_ava1::timings::all(),
        })),
    }
}

/// The 502 body of a failed status probe: `{ error, reason?, power? }`. `reason` is the token
/// the error leads with (`console_rest_mode`, `ava1_unreachable`, `helper_not_ava1`,
/// `not_paired`, ...); `power` is what DDP said (`awake` / `rest` / `unreachable`) when the
/// engine asked it, and absent otherwise.
fn status_failure_body(
    e: &anyhow::Error,
    power: Option<ps5upload_core::ddp::Power>,
) -> serde_json::Value {
    let reason = ps5upload_core::mgmt::error_token(e);
    let power = power.or_else(|| {
        (reason.as_deref() == Some(ps5upload_core::mgmt::CONSOLE_REST_MODE))
            .then_some(ps5upload_core::ddp::Power::Rest)
    });
    let mut body = serde_json::json!({ "error": format!("{e:#}") });
    if let Some(r) = reason {
        body["reason"] = r.into();
    }
    if let Some(p) = power {
        body["power"] = p.as_str().into();
    }
    body
}

#[cfg(test)]
mod connect_timings_tests {
    use super::*;
    use ps5upload_core::route::Via;

    #[test]
    fn each_way_to_a_console_has_its_name() {
        assert_eq!(connect_path(&Via::Lan), "lan");
        assert_eq!(
            connect_path(&Via::TailnetDirect {
                latency_ms: Some(3.0)
            }),
            "tailnet_direct"
        );
        assert_eq!(connect_path(&Via::TailnetRelayed), "tailnet_relayed");
        assert_eq!(connect_path(&Via::System), "tailnet_system");
    }

    #[tokio::test]
    async fn the_route_answers_one_console_or_all_without_asking_any() {
        ps5upload_ava1::timings::record(ps5upload_ava1::timings::ConnectTiming {
            console: "10.9.8.7".into(),
            at_ms: 5,
            ok: true,
            status_ms: Some(4),
            ..Default::default()
        });
        let one = ps5_connect_timings(Query(AddrQuery {
            addr: Some("10.9.8.7:9120".into()),
        }))
        .await
        .into_response();
        assert_eq!(one.status(), StatusCode::OK);
        let body = axum::body::to_bytes(one.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["timing"]["status_ms"], 4);
        assert_eq!(v["timing"]["ok"], true);
        let none = ps5_connect_timings(Query(AddrQuery {
            addr: Some("10.9.8.6".into()),
        }))
        .await
        .into_response();
        let body = axum::body::to_bytes(none.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["timing"].is_null(), "{v}");
        let all = ps5_connect_timings(Query(AddrQuery { addr: None }))
            .await
            .into_response();
        let body = axum::body::to_bytes(all.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["consoles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["console"] == "10.9.8.7"));
    }
}

#[cfg(test)]
mod status_failure_body_tests {
    use super::status_failure_body;
    use ps5upload_core::ddp::Power;
    use ps5upload_core::mgmt;

    #[test]
    fn a_console_in_rest_mode_carries_its_reason_and_power() {
        let e = mgmt::console_unusable(
            "NODE_STATUS",
            mgmt::CONSOLE_REST_MODE,
            mgmt::CONSOLE_REST_MODE_MESSAGE,
        );
        let b = status_failure_body(&e, Some(Power::Rest));
        assert_eq!(b["reason"], "console_rest_mode");
        assert_eq!(b["power"], "rest");
        assert!(b["error"]
            .as_str()
            .unwrap()
            .contains("The PS5 is in rest mode"));
        // Named by its reason even when the verdict is no longer cached.
        assert_eq!(status_failure_body(&e, None)["power"], "rest");
    }

    #[test]
    fn unreachable_and_helper_down_keep_their_tokens() {
        let e = mgmt::console_unusable(
            "NODE_STATUS",
            mgmt::AVA1_UNREACHABLE,
            mgmt::CONSOLE_NOT_ANSWERING_MESSAGE,
        );
        let b = status_failure_body(&e, Some(Power::Unreachable));
        assert_eq!(b["reason"], "ava1_unreachable");
        assert_eq!(b["power"], "unreachable");
        let b = status_failure_body(&mgmt::helper_not_ava1("NODE_STATUS"), Some(Power::Awake));
        assert_eq!(b["reason"], "helper_not_ava1");
        assert_eq!(b["power"], "awake");
        // Not probed: no power at all, never a guess.
        let b = status_failure_body(&mgmt::helper_not_ava1("NODE_STATUS"), None);
        assert!(b.get("power").is_none(), "{b}");
        let b = status_failure_body(&anyhow::anyhow!("timed out"), None);
        assert!(b.get("reason").is_none(), "{b}");
        assert_eq!(b["error"], "timed out");
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
        let mut report = ps5upload_core::health::run_health_scan(&addr, env!("CARGO_PKG_VERSION"));
        // The path between this computer and the console, measured from this engine.
        let helper_up = report.checks.iter().any(|c| {
            c.id == "mgmt_reachable" && c.status == ps5upload_core::health::CheckStatus::Pass
        });
        report
            .checks
            .extend(net_health::network_checks(&addr, helper_up));
        report.summary = ps5upload_core::health::summarize(&report.checks);
        report
    })
    .await;
    match result {
        Ok(report) => (StatusCode::OK, Json(report)).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// GET /api/net/doctor?addr=IP
///
/// The facts behind "Diagnose slow transfers": this computer's link toward the console (wired,
/// Wi-Fi, a VPN; its speed and MTU), the console's link from its helper, whether this engine is
/// in a container, and how long a connection to the console takes to open. The client judges
/// them together with the speed test. Blocking work, so it runs off the async runtime.
async fn net_doctor_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    match tokio::task::spawn_blocking(move || net_doctor::gather(&addr)).await {
        Ok(facts) => (StatusCode::OK, Json(facts)).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// GET /api/net/doctor/reply?addr=IP
///
/// Only the connection-open time, so the doctor can time it again while the speed test is
/// sending and see whether the reply time balloons under load.
async fn net_doctor_reply_handler(
    State(state): State<AppState>,
    Query(q): Query<AddrQuery>,
) -> impl IntoResponse {
    let addr = console_addr_or_default(q.addr, &state.default_ps5_addr);
    let host = pkg_install::strip_host_port(&addr);
    match tokio::task::spawn_blocking(move || net_doctor::reply_time(&host, 5)).await {
        Ok((median_ms, samples)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "median_ms": median_ms, "samples": samples })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// Where the speed test's file is written on this computer: the data folder, or the system
/// temp folder when there is none.
fn speed_test_dir() -> std::path::PathBuf {
    remote::store::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("speedtest")
}

/// Smallest and largest speed-test file, in MiB. Small enough to finish quickly on Wi-Fi,
/// large enough at the top that a fast link is measured over seconds, not milliseconds.
const SPEED_TEST_MIN_MIB: u64 = 16;
const SPEED_TEST_MAX_MIB: u64 = 2048;

fn speed_test_size_mib(asked: Option<u64>) -> u64 {
    asked
        .unwrap_or(256)
        .clamp(SPEED_TEST_MIN_MIB, SPEED_TEST_MAX_MIB)
}

/// Writes `mib` MiB that cannot be compressed (a transfer of zeros would measure nothing).
/// A xorshift generator: fast, and no two megabytes alike.
fn write_speed_test_file(path: &std::path::Path, mib: u64) -> std::io::Result<u64> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1);
    let mut block = vec![0u8; 1024 * 1024];
    for _ in 0..mib {
        for chunk in block.as_chunks_mut::<8>().0 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *chunk = x.to_le_bytes();
        }
        f.write_all(&block)?;
    }
    f.flush()?;
    Ok(mib * 1024 * 1024)
}

#[derive(Deserialize)]
struct SpeedTestPrepareReq {
    #[serde(default)]
    size_mib: Option<u64>,
}

/// POST /api/speed-test/prepare {size_mib?}
///
/// Makes the file the speed test sends: the client then uploads it with the ordinary transfer
/// route and downloads it back, so what is measured is what a real copy gets. Returns where
/// the file is and the folder the download should go to.
async fn speed_test_prepare_handler(Json(req): Json<SpeedTestPrepareReq>) -> impl IntoResponse {
    let mib = speed_test_size_mib(req.size_mib);
    let dir = speed_test_dir();
    let path = dir.join("ps5upload-speedtest.bin");
    let back = dir.join("back");
    let (p, b) = (path.clone(), back.clone());
    let r = tokio::task::spawn_blocking(move || {
        // A previous run's files, if it was interrupted.
        let _ = std::fs::remove_dir_all(&b);
        std::fs::create_dir_all(&b)?;
        write_speed_test_file(&p, mib)
    })
    .await;
    match r {
        Ok(Ok(bytes)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "path": path.to_string_lossy(),
                "download_dir": back.to_string_lossy(),
                "bytes": bytes,
            })),
        )
            .into_response(),
        Ok(Err(e)) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "could not write the speed-test file in {}: {e}",
                dir.display()
            ),
        )
        .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// POST /api/speed-test/cleanup: removes the speed test's files from this computer.
async fn speed_test_cleanup_handler() -> impl IntoResponse {
    let dir = speed_test_dir();
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(dir)).await;
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}

#[derive(Deserialize)]
struct HealthFixReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    action: ps5upload_core::health::FixAction,
    /// For `clean_junk`: the files the user was shown. Anything else is left alone, and so is one
    /// of these no longer on the list. Absent: everything on the list at this moment.
    #[serde(default)]
    paths: Option<Vec<String>>,
}

/// POST /api/ps5/health/fix
///
/// Applies one named repair. `action` deserializes into a closed enum,
/// so an unknown or arbitrary value is rejected by serde before any
/// console operation runs.
async fn health_fix_handler(
    installs: pkg_install::PkgInstallStateHandle,
    req: HealthFixReq,
) -> axum::response::Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let action = req.action;
    // A staged install reads its package from a folder this cleans, and a queue can be between
    // two of its items' steps with an old file there.
    if matches!(action, ps5upload_core::health::FixAction::CleanJunk)
        && (helper_stop::is_busy(&addr) || installs.jobs.active_for(&addr))
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "health_fix_busy" })),
        )
            .into_response();
    }
    let paths = req.paths;
    let result = tokio::task::spawn_blocking(move || {
        ps5upload_core::health::apply_fix_of(
            &addr,
            &action,
            paths.as_deref(),
            env!("CARGO_PKG_VERSION"),
        )
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    // A caller-supplied tx_id marks a resume-capable client; the job is reopened by id.
    let caller_supplied_tx_id = req.tx_id.is_some();
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "file");
    telemetry::set_drive(job_id, &req.dest);
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest);
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
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("cannot read source: {}", remote::api::error_with_hint(&e)),
                )
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
            files: files.clone().into(),
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
    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
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
        // The flag registered before the task: the core checks it at every shard boundary.
        cfg.cancel = Some(cancel);
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
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
        publish_transfer_end(&jobs, &events_tx, job_id, started_at_ms, result, 1);
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest_root);
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
            files: Default::default(),
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
    // Registered before the id goes out, not after the walk: the walk is the long part of a
    // big folder, and a cancel that arrives during it used to find no flag (`cancelled:false`)
    // and the whole folder uploaded anyway.
    let cancel = register_transfer_cancel(job_id);

    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
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
                            &anyhow::anyhow!(
                                "cannot read source: {}",
                                remote::api::error_with_hint(&e)
                            ),
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
                    &anyhow::Error::from(ps5upload_ava1::upload::UploadFailure {
                        reason: "source_missing".into(),
                        detail: format!(
                            "source directory not found or not a directory: {}",
                            req.src_dir
                        ),
                    }),
                ),
            );
            fail_guard.mark_succeeded();
            return;
        }

        let renames = module_case_renames(source_fs.as_deref(), &src_path, &req.src_dir);
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
        // A NAS share mounted by the OS reads as local disk: its made-up names are caught
        // here, before a game is copied under them (its own SMB connection is checked in
        // RemoteSourceFs::entries).
        if source_fs.is_none() {
            if let Some(msg) = remote::smb_fs::samba_renamed_message(
                files.iter().map(|f| f.rel_path.as_str()),
                &req.src_dir,
            ) {
                let completed_at_ms = now_ms();
                set_job(
                    &jobs,
                    &events_tx,
                    job_id,
                    job_failed_from_err(
                        started_at_ms,
                        completed_at_ms,
                        &anyhow::Error::from(ps5upload_ava1::upload::UploadFailure {
                            reason: "source_names_made_up".into(),
                            detail: msg,
                        }),
                    ),
                );
                fail_guard.mark_succeeded();
                return;
            }
        }
        let files_sent_count = files.len() as u64;
        crate::log_info!(
            "transfer_dir: job={job_id} walk done in {} ms — files={} bytes={}",
            walk_started.elapsed().as_millis(),
            files_sent_count,
            total_bytes
        );
        // A cancel during the walk ends the job here, as the core would at its first check,
        // without the readiness round trip; one that lands later is caught by the core
        // before anything is sent.
        if cancel.load(Ordering::Relaxed) {
            let completed_at_ms = now_ms();
            set_job(
                &jobs,
                &events_tx,
                job_id,
                job_failed_from_err(
                    started_at_ms,
                    completed_at_ms,
                    &anyhow::anyhow!("transfer_cancelled"),
                ),
            );
            fail_guard.mark_succeeded();
            return;
        }
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
                files: files.into(),
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
        // The flag registered above: the core checks it at every shard boundary.
        cfg.cancel = Some(cancel);
        cfg.excludes = req.excludes;
        cfg.renames = renames;
        cfg.unescape_names = req.unescape_names;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
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
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            result,
            files_sent_count,
        );
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
    // Off the async workers: a stat of a hung network mount parks its thread for the mount's
    // timeout, and every console's requests share these workers.
    let kind = tokio::task::spawn_blocking(move || match std::fs::metadata(&q.path) {
        Ok(md) if md.is_dir() => "folder",
        Ok(md) if md.is_file() => "file",
        Ok(_) => "other",
        Err(_) => "missing",
    })
    .await
    .unwrap_or("missing");
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
                Ok(serde_json::json!({ "ok": true, "result": r, "wrapped_hint": hint }))
            }
            Err(e) => Err(format!("{e:#}")),
        }
    })
    .await;
    match r {
        Ok(Ok(v)) => (StatusCode::OK, Json(v)).into_response(),
        // A folder it could not read. `ok` stays for the client's shared shape.
        Ok(Err(error)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "ok": false, "error": error })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// GET /api/local/game-icon — serve a local game's `sce_sys/icon0.png`.
///
/// The desktop app shows this cover through Tauri's asset protocol, which a
/// browser / self-hosted web UI does not have, so without this the Upload
/// screen shows a blank cover for any game folder on the engine's machine.
/// Deliberately narrow: only a file named `icon0.png` directly inside a
/// `sce_sys` directory, size-capped, and it must really be a PNG.
async fn local_game_icon_handler(Query(q): Query<LocalPathQuery>) -> axum::response::Response {
    const MAX_ICON_BYTES: u64 = 8 * 1024 * 1024;
    let path = std::path::PathBuf::from(q.path);
    if !is_game_icon_path(&path) {
        return json_err(StatusCode::BAD_REQUEST, "not a game icon path").into_response();
    }
    let r = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        use std::io::Read;
        let mut buf = Vec::new();
        std::fs::File::open(&path)?
            .take(MAX_ICON_BYTES + 1)
            .read_to_end(&mut buf)?;
        Ok(buf)
    })
    .await;
    match r {
        Ok(Ok(b)) if b.len() as u64 <= MAX_ICON_BYTES && b.starts_with(b"\x89PNG\r\n\x1a\n") => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "private, max-age=300"),
            ],
            b,
        )
            .into_response(),
        Ok(Ok(_)) => json_err(StatusCode::NOT_FOUND, "icon is not a usable PNG").into_response(),
        Ok(Err(e)) => json_err(StatusCode::NOT_FOUND, format!("icon: {e}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// True only for `<anything>/sce_sys/icon0.png`.
fn is_game_icon_path(p: &std::path::Path) -> bool {
    let name = |x: Option<&std::path::Path>| {
        x.and_then(|x| x.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_owned)
    };
    name(Some(p)).as_deref() == Some("icon0.png") && name(p.parent()).as_deref() == Some("sce_sys")
}

#[cfg(test)]
mod game_icon_path_tests {
    #[test]
    fn a_grid_cover_is_a_small_jpeg_and_transparency_stays_png() {
        let png = |img: image::DynamicImage| {
            let mut out = Vec::new();
            img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
                .unwrap();
            out
        };
        // A 512 px opaque cover with detail, like icon0.png.
        let opaque = image::RgbImage::from_fn(512, 512, |x, y| {
            image::Rgb([(x ^ y) as u8, x as u8, y as u8])
        });
        let full = png(image::DynamicImage::ImageRgb8(opaque));
        let small = super::icon_thumbnail(&full, 256).unwrap();
        assert!(
            small.starts_with(&[0xFF, 0xD8, 0xFF]),
            "opaque covers become JPEG"
        );
        let back = image::load_from_memory(&small).unwrap();
        assert_eq!((back.width(), back.height()), (256, 256));
        assert!(
            small.len() * 4 < full.len(),
            "{} vs {}",
            small.len(),
            full.len()
        );
        // Transparency survives as PNG.
        let clear = image::RgbaImage::from_pixel(512, 512, image::Rgba([0, 0, 0, 0]));
        let t = super::icon_thumbnail(&png(image::DynamicImage::ImageRgba8(clear)), 128).unwrap();
        assert!(t.starts_with(b"\x89PNG"));
        // Already small, or not an image: the caller keeps what it has.
        let tiny = png(image::DynamicImage::ImageRgb8(image::RgbImage::new(64, 64)));
        assert!(super::icon_thumbnail(&tiny, 128).is_none());
        assert!(super::icon_thumbnail(b"not an image", 128).is_none());
        assert_eq!(super::icon_thumb_px(Some(256)), Some(256));
        assert_eq!(super::icon_thumb_px(Some(4096)), None);
    }

    use super::is_game_icon_path;
    use std::path::Path;

    #[test]
    fn accepts_only_sce_sys_icon0() {
        assert!(is_game_icon_path(Path::new(
            "/data/games/G/sce_sys/icon0.png"
        )));
        assert!(!is_game_icon_path(Path::new("/etc/passwd")));
        assert!(!is_game_icon_path(Path::new("/data/games/G/icon0.png")));
        assert!(!is_game_icon_path(Path::new(
            "/data/games/G/sce_sys/param.json"
        )));
        assert!(!is_game_icon_path(Path::new(
            "/data/games/G/sce_sys/icon0.png.bak"
        )));
        assert!(!is_game_icon_path(Path::new("sce_sys")));
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest_root);
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
            files: files.into(),
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

    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // The flag registered before the task: the core checks it at every shard boundary.
        cfg.cancel = Some(cancel);
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
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
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            result,
            files_sent_count,
        );
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

/// Publishes how a transfer job ended. What the console already had and the files actually sent
/// come from the commit ack (a resume or a skip policy, SPEC §11.4; an all-skipped resume is
/// "already up to date"), `planned_files` when the ack has none.
fn publish_transfer_end(
    jobs: &Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: &broadcast::Sender<String>,
    job_id: Uuid,
    started_at_ms: u64,
    result: anyhow::Result<ps5upload_core::transfer::TransferResult>,
    planned_files: u64,
) {
    let completed_at_ms = now_ms();
    let state = match result {
        Ok(r) => {
            let (skipped_files, skipped_bytes, files_sent) =
                ava1_skip_counts(&r.commit_ack_body).unwrap_or((0, 0, planned_files));
            JobState::Done {
                started_at_ms,
                completed_at_ms,
                elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                tx_id_hex: r.tx_id_hex,
                bytes_sent: r.bytes_sent,
                dest: r.dest,
                files_sent,
                skipped_files,
                skipped_bytes,
                commit_ack: serde_json::from_str(&r.commit_ack_body).ok(),
            }
        }
        Err(e) => job_failed_from_err(started_at_ms, completed_at_ms, &e),
    };
    set_job(jobs, events_tx, job_id, state);
}

/// A zip the AVA1 source refused as unusable becomes a typed job failure, so the client shows
/// the archive problem rather than a transport one.
fn zip_failure(e: anyhow::Error) -> anyhow::Error {
    if e.downcast_ref::<ps5upload_ava1::upload::ZipUnsupported>()
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    slot: i32,
    name: String,
}

#[derive(Deserialize)]
struct ProfileLocalUsernameReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    uid: u32,
    name: String,
}

/// The PSN sign-in backup routes: the account slot np-fake-signin matches the user to, and the
/// user (0 or absent = the foreground user).
#[derive(Deserialize)]
struct ProfileNpReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    slot: i32,
    #[serde(default)]
    uid: u32,
}

#[derive(Deserialize)]
struct ProfileActivateReq {
    #[serde(alias = "console", alias = "host")]
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
struct ProfileAvatarReq {
    #[serde(alias = "console", alias = "host")]
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
    #[serde(default)]
    crop: Option<ps5upload_core::profile::CropPosition>,
}

#[derive(Deserialize)]
struct ProfilePreviewReq {
    image_path: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    crop: Option<ps5upload_core::profile::CropPosition>,
}

// ─── Console toast ──────────────────────────────────────────────────────

// ─── User create / delete ───────────────────────────────────────────────

#[derive(Deserialize)]
struct UserCreateReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    name: String,
}

#[derive(Deserialize)]
struct UserDeleteReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    uid: i32,
    #[serde(default)]
    wipe_saves: bool,
}

// ─── Backup & restore ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct BackupSnapshotReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    tag: String,
    path: String,
}

#[derive(Deserialize)]
struct BackupListReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Deserialize)]
struct BackupRestoreReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    tag: String,
    timestamp: i64,
}

#[derive(Deserialize)]
struct BackupDeleteReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    tag: String,
    timestamp: i64,
}

// ── v4.1: Remote Play ─────────────────────────────────────────────────
#[derive(Deserialize)]
struct RemotePlayReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    #[serde(default)]
    manual_account_id: Option<String>,
}

// ── v4.1: Notifications ───────────────────────────────────────────────
#[derive(Deserialize)]
struct NotifListReq {
    #[serde(alias = "console", alias = "host")]
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

async fn profile_username_handler(Json(req): Json<ProfileUsernameReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    Json(req): Json<ProfileLocalUsernameReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    State(_state): State<AppState>,
    Json(req): Json<ProfileActivateReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let slot = req.slot;
    // No id (or a blank one) derives the id from the slot; a given id that does not parse is
    // refused rather than quietly replaced by the derived one.
    let id = match req.id.as_ref() {
        None => None,
        Some(AccountIdInput::Str(s)) if s.trim().is_empty() => None,
        Some(given) => match given.to_u64() {
            Some(id) => Some(id),
            None => {
                return json_err(StatusCode::BAD_REQUEST, "not a valid account id").into_response();
            }
        },
    };
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

/// POST /api/profile/np/state — the user's sign-in state and whether a backup is kept.
async fn profile_np_state_handler(Json(req): Json<ProfileNpReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let (slot, uid) = (req.slot, req.uid);
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_np_state(&addr, slot, uid)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/profile/np/backup — keep what np-fake-signin overwrites, before it runs.
async fn profile_np_backup_handler(Json(req): Json<ProfileNpReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let (slot, uid) = (req.slot, req.uid);
    crate::log_info!("profile_np_backup: addr={addr} slot={slot} uid={uid}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_np_backup(&addr, slot, uid)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(created) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "created": created })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/profile/np/restore — put the kept sign-in state back (undo np-fake-signin).
async fn profile_np_restore_handler(Json(req): Json<ProfileNpReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let uid = req.uid;
    crate::log_info!("profile_np_restore: addr={addr} uid={uid}");
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::profile::profile_np_restore(&addr, uid)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(written) => (
            StatusCode::OK,
            Json(serde_json::json!({ "ok": true, "written": written })),
        )
            .into_response(),
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// POST /api/ps5/users/create — create a new local user account.
async fn user_create_handler(Json(req): Json<UserCreateReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    State(_state): State<AppState>,
    Json(req): Json<UserDeleteReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
async fn backup_snapshot_handler(Json(req): Json<BackupSnapshotReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    State(_state): State<AppState>,
    Json(req): Json<BackupRestoreReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    State(_state): State<AppState>,
    Json(req): Json<BackupDeleteReq>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
async fn remoteplay_request_handler(Json(req): Json<RemotePlayReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    let seen_addr = addr.clone();
    let r = tokio::task::spawn_blocking(move || {
        ps5upload_core::remoteplay::remoteplay_readiness(&addr)
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r);
    match r {
        Ok(result) => {
            // What the next pairing try's record says about the console (rp_diag.rs).
            rp_diag::readiness_seen(&seen_addr, &result);
            (StatusCode::OK, Json(result)).into_response()
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

async fn remoteplay_enable_handler(
    Query(q): Query<AddrQuery>,
    Json(body): Json<RemotePlayEnableBody>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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
        #[serde(alias = "console", alias = "host")]
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
    Query(q): Query<AddrQuery>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(post_addr(q.addr, &body)) else {
        return missing_console_addr();
    };
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
async fn activity_reset_handler(Query(q): Query<AddrQuery>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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
async fn notif_clear_handler(Query(q): Query<AddrQuery>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
}

#[derive(Deserialize)]
struct CheatsGetQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

#[derive(Deserialize)]
struct CheatsDeleteQuery {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

#[derive(Deserialize)]
struct CheatsToggleReq {
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
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

async fn cheats_toggle_handler(Json(req): Json<CheatsToggleReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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

/// POST /api/ps5/cheats/delete `{addr, title_id}`. A POST, like every other call that changes
/// the console: a GET could be issued by any page in a browser without Fetch Metadata.
async fn cheats_delete_handler(
    State(_state): State<AppState>,
    Json(q): Json<CheatsDeleteQuery>,
) -> impl IntoResponse {
    // Destructive: acts only on the console the request names, never the default one.
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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

/// POST /api/ps5/cheats/reload `{addr}` (a POST for the same reason as delete).
async fn cheats_reload_handler(Json(q): Json<CheatsAddrQuery>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(q.addr) else {
        return missing_console_addr();
    };
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

async fn cheats_engine_set_handler(Json(req): Json<CheatsEngineSetReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    repo_id: String,
    filename: String,
    title_id: String,
}

async fn cheats_repos_download_handler(
    Json(req): Json<CheatsRepoDownloadReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
    target_sdk: String,
    /// Opt-in libc.prx symbol swap — see sdk_changer.rs.
    #[serde(default)]
    patch_libc: bool,
}

async fn sdk_patch_handler(
    State(_state): State<AppState>,
    Json(req): Json<SdkPatchReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
}

async fn sdk_restore_handler(
    State(_state): State<AppState>,
    Json(req): Json<SdkRestoreReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    let mode = ps5upload_core::profile::SquareMode::parse(req.mode.as_deref().unwrap_or("crop"))
        .with_crop(req.crop);
    let path = req.image_path;
    let r: Result<Vec<u8>, anyhow::Error> = tokio::task::spawn_blocking(move || {
        let bytes = profile_image::read(&path)?;
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
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    uid: u32,
}

/// The image type of an avatar file by its first bytes: PNG or JPEG, or None (a .dds, say).
fn avatar_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

#[cfg(test)]
mod avatar_mime_tests {
    #[test]
    fn a_jpeg_named_png_is_shown_as_a_jpeg() {
        assert_eq!(
            super::avatar_mime(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(
            super::avatar_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F']),
            Some("image/jpeg")
        );
        assert_eq!(super::avatar_mime(b"DDS |...."), None);
        assert_eq!(super::avatar_mime(b""), None);
    }
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
    let found: Option<(Vec<u8>, &'static str)> = tokio::task::spawn_blocking(move || {
        let dir = format!("/system_data/priv/cache/profile/0x{uid:08X}");
        for name in ["avatar.png", "picture.png"] {
            let path = format!("{dir}/{name}");
            if let Ok(bytes) = ps5upload_core::fs_ops::fs_read(&addr, &path, 0, 2 * 1024 * 1024) {
                // By what the file is, not its name: a PSN avatar is often a JPEG named .png
                // (#718). The cache also holds .dds we can't show.
                if let Some(mime) = avatar_mime(&bytes) {
                    return Some((bytes, mime));
                }
            }
        }
        None
    })
    .await
    .unwrap_or(None);
    let data_url = found.map(|(bytes, mime)| {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        format!("data:{mime};base64,{b64}")
    });
    (
        StatusCode::OK,
        Json(serde_json::json!({ "data_url": data_url })),
    )
        .into_response()
}

async fn profile_avatar_handler(Json(req): Json<ProfileAvatarReq>) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let mode = ps5upload_core::profile::SquareMode::parse(req.mode.as_deref().unwrap_or("crop"))
        .with_crop(req.crop);
    let uid = req.uid.unwrap_or(0);
    let username = req.username;
    let image_path = req.image_path;
    let started = std::time::Instant::now();
    let image_label = if image_path.starts_with("data:") {
        "browser upload"
    } else {
        &image_path
    };
    crate::log_info!("profile_avatar: addr={addr} image={image_label} mode={mode:?} uid={uid}");
    let r = tokio::task::spawn_blocking(move || {
        let bytes = profile_image::read(&image_path)?;
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest_root);
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
            files: files.into(),
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

    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // The flag registered before the task: the core checks it at every shard boundary.
        cfg.cancel = Some(cancel);
        cfg.excludes = req.excludes;
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
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
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            result,
            files_sent_count,
        );
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

// ── tar / .zst handlers ── the 7z handlers' shape. A tar has no index: a plain `.tar` is
//    listed by its headers (instant), a compressed one is decoded once to list it. That listing
//    is cached (`tar_archive::cached_layout`), so the Upload card's inspect pays for it and the
//    upload that follows starts at once. An upload whose archive nobody inspected lists it
//    inside the job (phase "listing", cancellable) and the job's total and files appear then.

/// POST /api/tar/inspect — counts, sizes, entry names and the game of a tar or `.zst`. A
/// compressed tar is decoded to answer, so this can take as long as reading the archive; the
/// `/stream` variant beats while it runs.
async fn tar_inspect_handler(Json(req): Json<TarInspectReq>) -> impl IntoResponse {
    let archive_path = req.archive_path;
    crate::log_info!("tar_inspect: archive={archive_path}");
    let started = std::time::Instant::now();
    let result = tokio::task::spawn_blocking({
        let p = archive_path.clone();
        move || {
            ps5upload_core::tar_archive::inspect_tar_with_progress(
                std::path::Path::new(&p),
                &|_| {},
            )
        }
    })
    .await;
    let elapsed_ms = started.elapsed().as_millis();
    match result {
        Ok(Ok(inspect)) => {
            crate::log_info!(
                "tar_inspect ok: archive={archive_path} files={} unpacked={} elapsed_ms={elapsed_ms}",
                inspect.file_count,
                inspect.total_uncompressed,
            );
            (StatusCode::OK, Json(inspect)).into_response()
        }
        Ok(Err(e)) => {
            crate::log_warn!(
                "tar_inspect failed: archive={archive_path} elapsed_ms={elapsed_ms} err={e:#}"
            );
            json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response()
        }
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// POST /api/tar/inspect/stream — SSE variant; the zip/7z stream's event shapes (progress with
/// `entries_seen` / done / error, plus the heartbeat that keeps a long decode alive).
async fn tar_inspect_stream_handler(Json(req): Json<TarInspectReq>) -> impl IntoResponse {
    let archive_path = req.archive_path;
    crate::log_info!("tar_inspect (stream): archive={archive_path}");
    let started = std::time::Instant::now();
    let (tx, rx) = mpsc::channel::<InspectStreamEvent>(64);
    let path_for_worker = archive_path.clone();
    tokio::task::spawn_blocking(move || {
        let tx_progress = tx.clone();
        let result = ps5upload_core::tar_archive::inspect_tar_with_progress(
            std::path::Path::new(&path_for_worker),
            &move |n| {
                let _ = tx_progress.blocking_send(InspectStreamEvent::Progress(n));
            },
        );
        let elapsed_ms = started.elapsed().as_millis();
        let final_event = match result {
            Ok(inspect) => {
                crate::log_info!(
                    "tar_inspect (stream) ok: archive={path_for_worker} files={} unpacked={} elapsed_ms={elapsed_ms}",
                    inspect.file_count,
                    inspect.total_uncompressed,
                );
                InspectStreamEvent::Done(Box::new(inspect))
            }
            Err(e) => {
                let err = format!("{e:#}");
                crate::log_warn!(
                    "tar_inspect (stream) failed: archive={path_for_worker} elapsed_ms={elapsed_ms} err={err}"
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

/// POST /api/transfer/tar — upload a tar or `.zst`, unpacked. The plan comes from the cached
/// listing when there is one; otherwise the job lists the archive first and fills in its total.
async fn transfer_tar_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferTarReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let caller_supplied_tx_id = req.tx_id.is_some();
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };
    let archive = std::path::PathBuf::from(&req.archive_path);
    if ps5upload_core::tar_archive::stream_kind(&archive).is_none() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!(
                "not a tar or .zst archive (.tar, .tar.zst, .tzst, .tar.gz, .tgz, .tar.xz, .txz, \
                 .tar.bz2, .tbz2, .tbz, .zst): {}",
                req.archive_path
            ),
        )
        .into_response();
    }
    // Off the reactor: the archive may sit on a slow share.
    let planned = {
        let archive = archive.clone();
        let excludes = req.excludes.clone();
        tokio::task::spawn_blocking(move || {
            archive.is_file().then(|| {
                ps5upload_core::tar_archive::cached_layout(&archive)
                    .map(|l| ps5upload_core::tar_archive::tar_plan(&l, &excludes))
                    .unwrap_or_default()
            })
        })
        .await
        .ok()
        .flatten()
    };
    let Some((total_bytes, preview)) = planned else {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("no such archive: {}", req.archive_path),
        )
        .into_response();
    };
    let listed = !preview.is_empty();
    let files: Vec<PlannedFile> = preview
        .into_iter()
        .map(|(rel_path, size)| PlannedFile { rel_path, size })
        .collect();
    let files_sent_count = files.len() as u64;

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "tar");
    telemetry::set_drive(job_id, &req.dest_root);
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest_root);
    let started_at_ms = now_ms();
    crate::log_info!(
        "transfer_tar: job={job_id} addr={addr} archive={} dest_root={} resume={} listed={listed} files={} bytes={total_bytes}",
        req.archive_path,
        req.dest_root,
        caller_supplied_tx_id,
        files_sent_count
    );
    let progress = Arc::new(AtomicU64::new(0));
    let progress_files = Arc::new(AtomicU64::new(0));
    let progress_files_finalized = Arc::new(AtomicU64::new(0));
    let progress_bytes_finalized = Arc::new(AtomicU64::new(0));
    // Filled in by the transfer once it has listed the archive (when the plan had no listing).
    let listed_total = Arc::new(AtomicU64::new(total_bytes));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes,
        dynamic_total_bytes: Some(Arc::clone(&listed_total)),
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
            files: files.into(),
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

    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        cfg.cancel = Some(cancel);
        cfg.excludes = req.excludes;
        cfg.progress_total = Some(Arc::clone(&listed_total));
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        let mut planned_files = files_sent_count;
        // Not listed yet: list it here (the "listing" phase), so the job's file list and total
        // appear before the first byte is sent. The upload then reads the listing from the cache.
        if !listed {
            let listing = ps5upload_core::tar_archive::tar_layout_watched(
                &archive,
                ps5upload_core::tar_archive::TarWatch {
                    cancel: cfg.cancel.as_deref(),
                    live: cfg.progress_live.as_deref(),
                    on_files: None,
                },
            );
            match listing {
                Ok(layout) => {
                    let (total, plan) =
                        ps5upload_core::tar_archive::tar_plan(&layout, &cfg.excludes);
                    crate::log_info!(
                        "transfer_tar listed: job={job_id} codec={} files={} bytes={total} links_left_out={} special_left_out={}",
                        layout.codec.name(),
                        plan.len(),
                        layout.links,
                        layout.special,
                    );
                    planned_files = plan.len() as u64;
                    listed_total.store(total, Ordering::Release);
                    let mut map = jobs.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(JobState::Running {
                        files, total_bytes, ..
                    }) = map.get_mut(&job_id)
                    {
                        *files = Arc::new(
                            plan.into_iter()
                                .map(|(rel_path, size)| PlannedFile { rel_path, size })
                                .collect(),
                        );
                        *total_bytes = total;
                    }
                }
                Err(e) => {
                    crate::log_warn!("transfer_tar listing failed: job={job_id} err={e:#}");
                    let e = ps5upload_ava1::upload::tar_open_error(e, &archive);
                    publish_transfer_end(&jobs, &events_tx, job_id, started_at_ms, Err(e), 0);
                    fail_guard.mark_succeeded();
                    return;
                }
            }
        }
        let result = ps5upload_ava1::upload::upload_tar(&cfg, tx_id, &req.dest_root, &archive);
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            result,
            planned_files,
        );
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
#[cfg(not(any(target_os = "android", target_os = "ios")))]
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

#[cfg(any(target_os = "android", target_os = "ios"))]
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
#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn transfer_rar_handler(
    State(state): State<AppState>,
    Json(req): Json<TransferRarReq>,
) -> impl IntoResponse {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest_root);
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
    // The plan leaves out entries whose headers give no size; the transfer stores the measured
    // total here once it has decoded them.
    let measured_total = Arc::new(AtomicU64::new(total_bytes));
    let ctx = TickerContext {
        started_at_ms,
        total_bytes,
        dynamic_total_bytes: Some(Arc::clone(&measured_total)),
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
            files: files.into(),
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

    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // The flag registered before the task: the core checks it at every shard boundary.
        cfg.cancel = Some(cancel);
        cfg.excludes = req.excludes;
        cfg.progress_total = Some(measured_total);
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
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
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            result,
            files_sent_count,
        );
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

#[cfg(any(target_os = "android", target_os = "ios"))]
async fn transfer_rar_handler() -> impl IntoResponse {
    json_err(
        StatusCode::NOT_IMPLEMENTED,
        "RAR is not supported on this build",
    )
    .into_response()
}

/// POST /api/rar/packages — the `.pkg` entries of a ZIP, 7z or RAR (any folder depth), from
/// the headers alone, so several packages in one archive can be found before anything is
/// unpacked (R6, #370). `PKG_ALLOW` is the same allow-list the unpack passes as `excludes`, so
/// what is listed here is exactly what `/api/transfer/rar` will send. On a phone only RAR is
/// refused: ZIP and 7z are pure Rust and their unpack routes exist there.
async fn rar_packages_handler(Json(req): Json<RarPackagesReq>) -> impl IntoResponse {
    let p = req.archive_path.clone();
    let pw = req.password.clone();
    let r = tokio::task::spawn_blocking(move || archive_package_entries(&p, pw.as_deref())).await;
    match r {
        Ok(Ok(files)) => {
            let mut packages: Vec<serde_json::Value> = files
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

/// The exclude entry that keeps only packages (see `ps5upload_core::excludes`).
const RAR_PKG_ALLOW: &str = "!*.pkg";
/// The same for a package named `.fpkg` (the name a converted package can carry).
const RAR_FPKG_ALLOW: &str = "!*.fpkg";

/// Which archive reader a path's name asks for. A split RAR is recognised by any of its
/// parts' names; a `.7z.001` style split 7z is not supported and is not claimed here.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum ArchiveKind {
    Zip,
    SevenZ,
    Rar,
}

fn archive_kind_of(path: &str) -> Option<ArchiveKind> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else if lower.ends_with(".7z") {
        Some(ArchiveKind::SevenZ)
    } else if lower.ends_with(".rar") {
        Some(ArchiveKind::Rar)
    } else {
        None
    }
}

/// The `.pkg` files inside a ZIP, 7z or RAR (path inside the archive, unpacked size), read
/// from the archive's listing without unpacking anything. `password` only applies to RAR:
/// the ZIP and 7z readers here do not decrypt.
fn archive_package_entries(
    path: &str,
    password: Option<&str>,
) -> anyhow::Result<Vec<(String, u64)>> {
    let allow = [RAR_PKG_ALLOW.to_string(), RAR_FPKG_ALLOW.to_string()];
    let p = std::path::Path::new(path);
    match archive_kind_of(path) {
        Some(ArchiveKind::Zip) => Ok(ps5upload_core::transfer::zip_plan_preview(p, &allow)?.1),
        Some(ArchiveKind::SevenZ) => {
            Ok(ps5upload_core::transfer::sevenz_plan_preview(p, &allow)?.1)
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        Some(ArchiveKind::Rar) => {
            Ok(ps5upload_core::transfer::rar_layout(p, password, &allow)?.files)
        }
        #[cfg(any(target_os = "android", target_os = "ios"))]
        Some(ArchiveKind::Rar) => {
            let _ = password;
            anyhow::bail!("RAR is not supported on this build")
        }
        None => anyhow::bail!("not a .zip, .7z or .rar archive: {path}"),
    }
}

/// POST /api/link/probe — what a link actually serves: a package, some other
/// real file, or something that is not a download (R4, #368). Decided from the response, not
/// the URL's spelling.
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

pub(crate) fn valid_link_url(url: &str) -> bool {
    url.len() <= 4093
        && !url.bytes().any(|b| b < 0x20 || b == 0x7f)
        && !url.contains('#')
        && url.parse::<axum::http::Uri>().ok().is_some_and(|u| {
            matches!(u.scheme_str(), Some("http" | "https"))
                && u.host().is_some_and(|h| !h.is_empty())
        })
}

/// POST /api/link/download — download-only (R4, #368): the engine's ranged
/// fetcher is the AVA1 source, so the link streams to a console folder with nothing staged on
/// this computer. The link is probed AGAIN here and refused unless it is a real file
/// download; the client's earlier probe is a convenience, never the authority.
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
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
    let validators = class.validators.clone();
    // An .lz4 link is unpacked on the way: the console gets the image, not the .lz4.
    let lz4 = class.lz4.clone().filter(|_| req.unpack);
    let packed = total;
    let (default_name, total) = match &lz4 {
        Some(info) => match (info.unpacked_size, &info.unsupported) {
            (Some(n), None) if n > 0 => (info.unpacked_name.clone(), n),
            (_, why) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({
                        "error": why.clone().unwrap_or_else(|| "this .lz4 cannot be unpacked on the way".into()),
                        "reason": "lz4_not_streamable",
                    })),
                )
                    .into_response();
            }
        },
        None => (class.filename.clone(), total),
    };
    let name = req
        .file_name
        .as_deref()
        .map(link::sanitize_name)
        .filter(|n| !n.is_empty())
        .unwrap_or(default_name);
    let dest_path = format!("{dest_dir}/{name}");

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "link_download");
    telemetry::set_drive(job_id, &dest_dir);
    mesh_api::note_job(job_id, &addr, "Upload", &dest_dir);
    let started_at_ms = now_ms();
    // The host only: a link can carry a signed token in its path or query.
    let host = url
        .parse::<axum::http::Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_string))
        .unwrap_or_default();
    crate::log_info!(
        "link_download: job={job_id} addr={addr} host={host} dest={dest_path} bytes={total}{}",
        if lz4.is_some() {
            format!(" lz4_packed={packed}")
        } else {
            String::new()
        }
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
            }]
            .into(),
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

    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        cfg.cancel = Some(cancel);
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        let remote = Arc::new(
            crate::remote_pkg::RemoteSource::new_with_options(url, packed, insecure)
                .with_validators(validators),
        );
        // A plain file is read at any offset (resume continues where the console stopped); an
        // .lz4 is decoded forward on one thread and its decoded bytes are what is sent.
        let (source, seq): (
            Arc<dyn ava1::source::Source>,
            Option<Arc<dyn ava1::seq::SeqSource>>,
        ) = if lz4.is_some() {
            let s = Arc::new(
                link_lz4::Lz4LinkSource::new(remote, packed, name.clone(), total)
                    .pad_short(lz4.as_ref().is_some_and(|i| i.size_from_image)),
            );
            (s.clone(), Some(s))
        } else {
            (
                Arc::new(link::LinkSource::new(remote, name.clone(), total)),
                None,
            )
        };
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
                    opts.flags = ps5upload_ava1::gen::JF_SINGLE_FILE;
                    opts.seq = seq.clone();
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
        publish_transfer_end(&jobs, &events_tx, job_id, started_at_ms, result, 1);
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
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    let tx_id = match parse_or_random_tx_id(req.tx_id.as_deref()) {
        Ok(id) => id,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    let job_id = Uuid::new_v4();
    telemetry::tag(job_id, "file_list");
    telemetry::set_drive(job_id, &req.dest_root);
    mesh_api::note_job(job_id, &addr, "Upload", &req.dest_root);
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
            // A file whose metadata fails leaves the plan AND the transfer list: sent on, the
            // core stats it again and the whole job fails on it.
            let mut kept = Vec::with_capacity(entries.len());
            let mut files: Vec<PlannedFile> = Vec::with_capacity(entries.len());
            for e in entries {
                let size = match std::fs::metadata(&e.src) {
                    Ok(m) => m.len(),
                    Err(err) => {
                        planner_skipped.push((e.src.clone(), err.to_string()));
                        continue;
                    }
                };
                let rel = std::path::Path::new(&e.dest)
                    .strip_prefix(std::path::Path::new(&dest_root))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| e.dest.clone())
                    .replace(std::path::MAIN_SEPARATOR, "/");
                files.push(PlannedFile {
                    rel_path: rel,
                    size,
                });
                kept.push(e);
            }
            (kept, files, planner_skipped)
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
    for (src, err) in &planner_skipped {
        log_warn!("planner: skipped {} (metadata failed: {})", src, err);
    }
    if entries.is_empty() {
        // Nothing readable: no job, the first reason tells the user what is wrong.
        let (src, err) = &planner_skipped[0];
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("no readable file in the list: {src}: {err}"),
        )
        .into_response();
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
            files: files.into(),
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

    // Registered before the task so a cancel that arrives before the transfer starts is kept.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let mut fail_guard =
            JobFailOnDropGuard::new(Arc::clone(&jobs), events_tx.clone(), job_id, started_at_ms);
        if fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr) {
            fail_guard.mark_succeeded();
            return;
        }
        let mut cfg = make_transfer_config(&addr);
        // The flag registered before the task: the core checks it at every shard boundary.
        cfg.cancel = Some(cancel);
        cfg.progress_bytes = Some(Arc::clone(&progress));
        cfg.progress_files = Some(Arc::clone(&progress_files));
        cfg.progress_files_finalized = Some(Arc::clone(&progress_files_finalized));
        cfg.progress_bytes_finalized = Some(Arc::clone(&progress_bytes_finalized));
        attach_live_notes(&mut cfg, job_id);
        apply_per_request_bandwidth(&mut cfg, req.bandwidth_cap_mbps);
        crate::log_info!("transfer_file_list: job={job_id} protocol=ava1");
        // Resume is by job_id (the sender reopens with JobOpen); retries live in the adapter's
        // loop. A list that names several directories becomes one job per directory.
        let result = ps5upload_ava1::upload::upload_list(&cfg, tx_id, &req.dest_root, &entries);
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            result,
            files_sent_count,
        );
        fail_guard.mark_succeeded();
    });

    // The job runs without these files, so the caller is told now: otherwise the run reports
    // success with them simply absent.
    (
        StatusCode::ACCEPTED,
        Json(file_list_created(job_id, &planner_skipped)),
    )
        .into_response()
}

/// A file-list job's answer: its id and the sources it left out because they could not be read.
fn file_list_created(job_id: Uuid, skipped: &[(String, String)]) -> serde_json::Value {
    serde_json::json!({
        "job_id": job_id.to_string(),
        "skipped_unreadable": skipped
            .iter()
            .map(|(path, error)| serde_json::json!({ "path": path, "error": error }))
            .collect::<Vec<_>>(),
    })
}

// ─── Download (PS5 → host) ───────────────────────────────────────────────────

#[derive(Deserialize)]
struct TransferDownloadReq {
    /// The console (a `:port` suffix from an older client is ignored).
    #[serde(alias = "console", alias = "host")]
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
    /// When true, a console name this computer can't hold (`a\b`, `a:b`, `CON`) is written
    /// under its reversible form (`ps5upload_core::host_names`) instead of refused; an upload
    /// with `unescape_names` puts it back exactly. The saves code's downloads (#729).
    #[serde(default)]
    escape_names: bool,
}

/// Where an AVA1 download lands (Task 25).
enum Ava1DownloadTarget {
    /// A tree under this directory: `dest_dir/<basename>`; `true` keeps names this computer
    /// can't hold under their reversible form (`TransferDownloadReq::escape_names`).
    Folder(std::path::PathBuf, bool),
    /// One `.zip` at this path, Stored (resumable) or Deflated (cannot resume).
    Zip(std::path::PathBuf, ps5upload_ava1::download::ZipCompression),
    /// A folder on a saved server (`remote://id/dir`), through this engine (download_relay.rs).
    Server(String),
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
        Ava1DownloadTarget::Folder(dir, _) => dir.join(&basename).to_string_lossy().to_string(),
        Ava1DownloadTarget::Zip(zip, _) => zip.to_string_lossy().to_string(),
        Ava1DownloadTarget::Server(dir) => format!("{}/{basename}", dir.trim_end_matches('/')),
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
            files: Default::default(),
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
    // POST /api/jobs/{id}/cancel flips this; the download stops at its next check and
    // the job ends failed with `transfer_cancelled`, like a cancelled upload.
    let cancel = register_transfer_cancel(job_id);
    tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        let _stop_guard = TickerStopGuard::new(stop_ticker);
        let id = *job_id.as_bytes();
        let ready =
            || !fail_job_unless_console_ready(&jobs, &events_tx, job_id, started_at_ms, &addr);
        run_download_job(
            &jobs,
            &events_tx,
            job_id,
            started_at_ms,
            dest_display,
            &progress_files,
            ready,
            || match &target {
                Ava1DownloadTarget::Folder(dir, false) => ps5upload_ava1::download::to_local(
                    &addr,
                    &src,
                    kind,
                    dir,
                    unsafe_read,
                    id,
                    &counters,
                    Some(cancel),
                ),
                Ava1DownloadTarget::Folder(dir, true) => {
                    ps5upload_ava1::download::to_local_escaped(
                        &addr,
                        &src,
                        kind,
                        dir,
                        unsafe_read,
                        id,
                        &counters,
                        Some(cancel),
                    )
                }
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
                Ava1DownloadTarget::Server(dir) => download_relay::to_server(
                    &addr,
                    &src,
                    kind,
                    dir,
                    unsafe_read,
                    &counters,
                    cancel,
                    &crate::remote::store::data_dir()
                        .unwrap_or_else(std::env::temp_dir)
                        .join("relay"),
                ),
            },
        );
    });
    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

/// Runs a download's transfer and records how the job ended: done, or failed with the
/// transfer's error (`transfer_cancelled` after a cancel). `ready` records its own
/// failure and returns false when the console cannot take the job.
#[allow(clippy::too_many_arguments)]
fn run_download_job(
    jobs: &Arc<Mutex<HashMap<Uuid, JobState>>>,
    events_tx: &broadcast::Sender<String>,
    job_id: Uuid,
    started_at_ms: u64,
    dest_display: String,
    progress_files: &AtomicU64,
    ready: impl FnOnce() -> bool,
    transfer: impl FnOnce() -> anyhow::Result<u64>,
) {
    let mut fail_guard =
        JobFailOnDropGuard::new(Arc::clone(jobs), events_tx.clone(), job_id, started_at_ms);
    if !ready() {
        fail_guard.mark_succeeded();
        return;
    }
    let id = *job_id.as_bytes();
    match transfer() {
        Ok(bytes) => {
            let completed_at_ms = now_ms();
            let files = progress_files.load(Ordering::Relaxed);
            set_job(
                jobs,
                events_tx,
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
                jobs,
                events_tx,
                job_id,
                job_failed_from_err(started_at_ms, completed_at_ms, &e),
            );
        }
    }
    fail_guard.mark_succeeded();
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
    // A folder on a saved server (a NAS share, FTP, SFTP, WebDAV): through this engine.
    if download_relay::is_server_dest(&req.dest_dir) {
        if let Err(e) = crate::remote::path::parse(&req.dest_dir) {
            return json_err(StatusCode::BAD_REQUEST, format!("{e}")).into_response();
        }
        return start_ava1_download(
            &state,
            mgmt_addr,
            req.src_path.clone(),
            kind,
            req_unsafe,
            Ava1DownloadTarget::Server(req.dest_dir.clone()),
        );
    }

    let dest_dir = std::path::PathBuf::from(&req.dest_dir);
    // rsplit on a non-empty string always yields at least one piece,
    // but a panic in a handler kills every console's transfers — fail
    // soft with a 400 rather than expecting the invariant holds.
    let Some(basename) = trimmed_src.rsplit('/').next() else {
        return json_err(StatusCode::BAD_REQUEST, "src_path cannot be empty or '/'")
            .into_response();
    };
    // With `escape_names` the name lands under its reversible form (checked by the download).
    let bad_char = basename.contains('/') || (basename.contains('\\') && !req.escape_names);
    if basename == "." || basename == ".." || bad_char {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!(
                "src_path produces an invalid destination basename ({basename:?}); refusing to download"
            ),
        )
        .into_response();
    }
    if let Some(why) =
        ps5upload_core::transfer::host_unsafe_component(basename).filter(|_| !req.escape_names)
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{basename:?} cannot be saved on this computer: {why}."),
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
        Ava1DownloadTarget::Folder(dest_dir, req.escape_names),
    )
}

#[derive(Deserialize)]
struct TransferDownloadZipReq {
    #[serde(alias = "console", alias = "host")]
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
    /// What to do with a file already at `dest_zip`: `replace` overwrites it (the person chose
    /// that, say in a save dialog). Absent: refuse with a 409, as the link download does.
    #[serde(default)]
    existing: Option<String>,
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
    // A finished download renames over the destination: an archive already there would be gone
    // without a word.
    if req.existing.as_deref() != Some("replace") {
        let at = dest_zip.clone();
        if let Ok(Ok(md)) = tokio::task::spawn_blocking(move || std::fs::metadata(&at)).await {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!("{} already exists ({} bytes).", dest_zip.display(), md.len()),
                    "existing_path": dest_zip.display().to_string(),
                    "existing_bytes": md.len(),
                })),
            )
                .into_response();
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
        unescape_names: false,
    };
    transfer_dir_handler(State(state), Json(dir_req))
        .await
        .into_response()
}

/// GET /api/jobs/{id}: a transfer job, or an install by its own id (`<epoch>-<seq>`, the same
/// status as `/api/pkg/install/status`).
async fn get_job(
    State(state): State<AppState>,
    installs: pkg_install::PkgInstallStateHandle,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if install::is_install_job_id(&id) {
        return match installs.jobs.get(&id) {
            Some(st) => (StatusCode::OK, Json(st)).into_response(),
            None => json_err(StatusCode::NOT_FOUND, "job not found").into_response(),
        };
    }
    let uuid = match id.parse::<Uuid>() {
        Ok(u) => u,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    // Serialized under the lock rather than cloned: the snapshot carries the
    // file list's length only (`files_count`), so this stays cheap at 2 polls/s.
    let job = state
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&uuid)
        .map(|j| serde_json::json!(j));
    match job {
        Some(job) => (StatusCode::OK, Json(with_live_notes(uuid, job))).into_response(),
        None => json_err(StatusCode::NOT_FOUND, "job not found").into_response(),
    }
}

/// GET /api/jobs/{id}/files — a running job's planned file list (`{"files":
/// [{"rel_path","size"}...]}`), fetched once by the UI that shows it. The job
/// snapshot only carries its length. A finished job no longer has one: `[]`.
async fn get_job_files(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let uuid = match id.parse::<Uuid>() {
        Ok(u) => u,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid job id").into_response(),
    };
    // Only the Arc is cloned under the lock; a list of tens of thousands of files is
    // serialized after it is released, so progress ticks are not held up behind it.
    let files: Arc<Vec<PlannedFile>> = match state
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&uuid)
    {
        Some(JobState::Running { files, .. }) => Arc::clone(files),
        Some(_) => Arc::default(),
        None => return json_err(StatusCode::NOT_FOUND, "job not found").into_response(),
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({ "files": &*files })),
    )
        .into_response()
}

/// POST /api/jobs/{id}/cancel — truly stop a running transfer. Flips the job's
/// registered cancel flag; the core aborts at its next shard boundary with
/// `transfer_cancelled` and the partial tx is left interrupted/resumable (same
/// as a dropped connection). Idempotent — cancelling an unknown/finished job is
/// a no-op 200 (`cancelled:false`); the client may race the job ending. An install's id stops
/// that install, as `/api/pkg/install/stop` does.
async fn cancel_job(
    installs: pkg_install::PkgInstallStateHandle,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if install::is_install_job_id(&id) {
        let cancelled = installs.jobs.request_cancel(&id);
        crate::log_info!("cancel_job: install job={id} cancelled={cancelled}");
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "cancelled": cancelled })),
        )
            .into_response();
    }
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
    if !debug_routes_on() {
        return no_such_route();
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
    // Home first, always; a port the console serves to Tailscale goes there only when the
    // console is away (`ps5upload_core::route`). `via` says which path answered.
    let res = tokio::task::spawn_blocking(move || {
        let host = ps5upload_core::events::console_host(&ip);
        match ps5upload_core::route::dial(&host, q.port, false) {
            Some(a) => {
                let (via, _) = tailnet_engine::via_json(&ps5upload_core::route::via(&host));
                let r = ps5upload_core::payload_lifecycle::probe_port(
                    &a.ip().to_string(),
                    a.port(),
                    timeout,
                );
                (r, via)
            }
            None => (
                ps5upload_core::payload_lifecycle::probe_port(&ip, q.port, timeout),
                "lan",
            ),
        }
    })
    .await;
    match res {
        Ok((Ok(()), via)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "open": true, "error": null, "via": via })),
        ),
        Ok((Err(e), via)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "open": false, "error": e, "via": via })),
        ),
        Err(e) => (
            StatusCode::OK,
            Json(serde_json::json!({ "open": false, "error": format!("probe task: {e}") })),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct PortCheckQuery {
    #[serde(alias = "console", alias = "addr", alias = "host")]
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
#[derive(serde::Deserialize)]
struct ToastBody {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title: String,
    subtitle: Option<String>,
    icon: Option<String>,
    action_url: Option<String>,
}

/// `POST /api/ps5/toast`: a notification on the console's screen, over its management port. The web
/// UI's way to it, for "Upload complete" on the TV.
async fn ps5_toast(Json(b): Json<ToastBody>) -> axum::response::Response {
    let Some(addr) = required_console_addr(b.addr) else {
        return missing_console_addr();
    };
    let req = ps5upload_core::app_lifecycle::ToastRequest {
        title: b.title,
        subtitle: b.subtitle.unwrap_or_default(),
        icon: b.icon.unwrap_or_default(),
        action_url: b.action_url.unwrap_or_default(),
    };
    let sent =
        tokio::task::spawn_blocking(move || ps5upload_core::app_lifecycle::toast_send(&addr, &req))
            .await;
    match sent {
        Ok(Ok(ack)) => Json(ack).into_response(),
        Ok(Err(e)) => json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
        Err(e) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn engine_version() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            // Capability flags the UI feature-detects. `rar` is desktop-only
            // (UnRAR is a helper program, which neither phone app has), so the
            // client hides the .rar option when this is false.
            "caps": { "rar": cfg!(not(any(target_os = "android", target_os = "ios"))) },
            // `PS5_ADDR`, when the operator set one: the web UI lists it for a first browser, so
            // the console the setup named is not typed in again.
            "default_console": std::env::var("PS5_ADDR")
                .ok()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty()),
        })),
    )
}

/// GET /api/activity?since=<seq> — running jobs and the endings after `since`, for the app's
/// background watcher (see `activity`).
async fn activity_handler(
    State(state): State<AppState>,
    installs: pkg_install::PkgInstallStateHandle,
    q: activity::SinceQuery,
) -> impl IntoResponse {
    let mut active: Vec<activity::Active> = {
        let jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.iter()
            .filter_map(|(id, s)| match s {
                JobState::Running {
                    started_at_ms,
                    bytes_sent,
                    total_bytes,
                    ..
                } => Some(activity::transfer_active(
                    &id.to_string(),
                    *bytes_sent,
                    *total_bytes,
                    *started_at_ms,
                )),
                _ => None,
            })
            .collect()
    };
    active.extend(installs.jobs.running().iter().map(activity::install_active));
    active.extend(share::activity());
    Json(activity::snapshot(q.since, active))
}

#[derive(Deserialize)]
struct ListJobsQuery {
    /// Only jobs that changed after this event number: a row's `seq`, the
    /// `x-ps5upload-last-seq` header of an earlier answer, or an `id:` from `/api/events`.
    since_seq: Option<u64>,
    /// At most this many rows: the next ones after `since_seq`, else the newest.
    limit: Option<usize>,
}

/// GET /api/jobs?since_seq=&limit= — transfers (`kind: "transfer"`) and installs (`kind: "install"`,
/// whose `job` is the status `/api/pkg/install/status` gives), newest first; with `since_seq`, oldest
/// change first so a poller's cursor never skips a job. `GET /api/jobs/{id}` and its `cancel` take
/// either id. The job map keeps the last 256 finished jobs.
async fn list_jobs(
    State(state): State<AppState>,
    installs: pkg_install::PkgInstallStateHandle,
    Query(q): Query<ListJobsQuery>,
) -> impl IntoResponse {
    let bus = event_bus::bus();
    // Read before the jobs: an event that lands in between is seen again next time, not missed.
    let last_seq = bus.last_seq();
    let jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(u64, u64, serde_json::Value)> = jobs
        .iter()
        .map(|(id, s)| {
            let id = id.to_string();
            let seq = bus.job_seq(&id);
            let row = serde_json::json!({
                "job_id": id,
                "kind": "transfer",
                "status": match s {
                    JobState::Running { .. } => "running",
                    JobState::Done {..} => "done",
                    JobState::Failed {..} => "failed",
                },
                "seq": seq,
                "job": s,
            });
            (seq, s.started_at_ms(), row)
        })
        .collect();
    drop(jobs);
    rows.extend(installs.jobs.all().iter().map(|st| {
        let seq = bus.job_seq(&st.job);
        let mut row = install::job_list_entry(st);
        row["seq"] = seq.into();
        // An install's `started_at` is in seconds.
        (seq, st.started_at.saturating_mul(1000), row)
    }));
    match q.since_seq {
        Some(after) => {
            rows.retain(|r| r.0 > after);
            rows.sort_by_key(|r| r.0);
        }
        None => rows.sort_by_key(|r| std::cmp::Reverse(r.1)),
    }
    rows.truncate(q.limit.unwrap_or(usize::MAX));
    // A truncated page's cursor is its own last row, or the rest would be skipped.
    let cursor = match (q.since_seq, q.limit) {
        (Some(_), Some(limit)) if rows.len() == limit => rows.last().map_or(last_seq, |r| r.0),
        _ => last_seq,
    };
    let summary: Vec<serde_json::Value> = rows.into_iter().map(|r| r.2).collect();
    (
        [("x-ps5upload-last-seq", cursor.to_string())],
        Json(summary),
    )
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

/// Zips the bundle's entries, or says why not.
fn build_bug_bundle(entries: &[BugBundleEntry]) -> Result<Vec<u8>, (StatusCode, String)> {
    use base64::Engine as _;
    use std::io::{Cursor, Write};

    // A bundle is diagnostics, not a file transfer. Cap it so a malformed or
    // hostile caller cannot drive the engine's memory through the roof.
    const MAX_TOTAL: usize = 64 * 1024 * 1024;

    let failed = |why: &str| (StatusCode::INTERNAL_SERVER_ERROR, why.to_string());
    let mut buf = Cursor::new(Vec::<u8>::new());
    let mut total = 0usize;
    let mut zw = zip::ZipWriter::new(&mut buf);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for e in entries {
        if !bundle_path_ok(&e.path) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unsafe entry path: {}", e.path),
            ));
        }
        let bytes: Vec<u8> = if let Some(t) = &e.text {
            t.as_bytes().to_vec()
        } else if let Some(b) = &e.base64 {
            base64::engine::general_purpose::STANDARD
                .decode(b)
                .map_err(|_| {
                    (
                        StatusCode::BAD_REQUEST,
                        format!("entry {} has invalid base64", e.path),
                    )
                })?
        } else {
            Vec::new()
        };
        total = total.saturating_add(bytes.len());
        if total > MAX_TOTAL {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                "bundle too large".to_string(),
            ));
        }
        zw.start_file(e.path.clone(), opts)
            .map_err(|_| failed("zip entry failed"))?;
        zw.write_all(&bytes)
            .map_err(|_| failed("zip write failed"))?;
    }
    zw.finish().map_err(|_| failed("zip finish failed"))?;
    Ok(buf.into_inner())
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
    if req.entries.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "entries is required").into_response();
    }
    let filename = req.filename;
    let entries = req.entries;
    let count = entries.len();
    // Deflating up to 64 MiB takes a while: not on an async worker, which serves every console.
    let built = match tokio::task::spawn_blocking(move || build_bug_bundle(&entries)).await {
        Ok(built) => built,
        Err(e) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("bundle task failed: {e}"),
            )
            .into_response()
        }
    };
    let buf = match built {
        Ok(buf) => buf,
        Err((status, why)) => return json_err(status, why).into_response(),
    };

    // Keep the leaf name simple: it lands in the user's Downloads folder and is
    // echoed into a Content-Disposition header.
    let name = filename
        .as_deref()
        .filter(|n| {
            !n.is_empty()
                && n.len() <= 128
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
        .unwrap_or("ps5upload-bugreport.zip");

    let body = buf;
    crate::log_info!(
        "bug-report bundle: {} entries, {} bytes, name={}",
        count,
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

/// Every connection the engine accepts sends at once (TCP_NODELAY). Without it a small
/// write waits until the peer acknowledges the last one, and a Linux peer that is also
/// sending (the Remote Play page's pad input) holds its acks up to 40 ms: Remote Play frames
/// reached the page ~10-30 ms late on Linux, the "net" figure in its stats. Measured on a
/// Linux loopback: video frames at 60/s with pad input 20/s took 9 ms (median) and 33 ms (95th
/// percentile) to arrive; with this, 0.2 and 0.7 ms. Bulk transfers are written in large
/// chunks, so they lose nothing by it.
fn no_delay(
    l: tokio::net::TcpListener,
) -> axum::serve::TapIo<tokio::net::TcpListener, fn(&mut tokio::net::TcpStream)> {
    use axum::serve::ListenerExt;
    fn set(tcp: &mut tokio::net::TcpStream) {
        let _ = tcp.set_nodelay(true);
    }
    l.tap_io(set as fn(&mut tokio::net::TcpStream))
}

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

/// How long the drain may take once a stop was asked for. Under Docker's 10 seconds before SIGKILL.
const DRAIN_CEILING: std::time::Duration = std::time::Duration::from_secs(8);

/// After a stop was asked for: a response that does not finish in time does not get to keep the
/// process, and the journal is flushed before it goes. Not for an engine inside a phone app, whose
/// process is the app.
fn spawn_drain_ceiling() {
    std::thread::spawn(|| {
        std::thread::sleep(DRAIN_CEILING);
        stderr_quiet("[ps5upload-engine] drain took too long — exiting");
        ps5upload_core::events::emit(
            ps5upload_core::events::Cat::System,
            ps5upload_core::events::Level::Info,
            "engine_stop",
            None,
            "engine stopped with requests still open",
        );
        event_journal::flush_now();
        std::process::exit(0);
    });
}

/// Print to stderr without panicking. `eprintln!` panics when stderr is a closed pipe, which is
/// exactly the state a dead desktop parent leaves behind (it was reading our stderr): the
/// parent-watch thread died on its first message and the engine lived on, holding its port.
fn stderr_quiet(msg: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{msg}");
}

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
    /// `/pkg-host/*` for fakepkg installs). Mobile in-process: the same
    /// (`0.0.0.0:19113`, so the PS5 can fetch packages from the phone); the
    /// renderer uses loopback, and the guard keeps `/api/*` local.
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
    // The port first: everything below touches the data directory and the journal, and an engine
    // that finds a sibling already serving must leave without having touched either.
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
                stderr_quiet(&format!(
                    "[ps5upload-engine] {bind} already bound by another \
                     process (likely a sibling engine); exiting 0",
                ));
                return Ok(());
            }
            stderr_quiet(&format!(
                "[ps5upload-engine] failed to bind {bind}: {e} \
                 (port held by an unresponsive process — kill it or \
                 override with PS5UPLOAD_ENGINE_PORT)",
            ));
            std::process::exit(2);
        }
    };
    // Convert (and the package viewer) read games on saved servers and the console in place.
    convert_source::register();
    // Every management call (hardware, filesystem, apps, ...) goes through one transport
    // seam in the core crate; this registers the AVA1 implementation over the shared pool.
    host_guard::log_settings();
    // A console it cannot serve is a `helper_not_ava1` error, never another protocol.
    ps5upload_ava1::mgmt::install();
    // The engine's own tailnet node (when turned on) and the away routing (tailnet_engine.rs).
    tailnet_engine::boot();
    // Every new session says who this engine is, whatever opened it (Devices showed a phone
    // that had only checked the console's status as an app from before 8.0.0).
    ps5upload_ava1::pool::on_new_session(mesh_api::greet);
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
    // clog!/core_debug! lines, with their level and category. The level setting comes off disk
    // first, so a raise made before a restart is still in force (until it runs out).
    ps5upload_core::log::set_leveled_sink(engine_log::core_sink);
    log_levels::load_saved();
    diag_api::mark_started();
    // The bug-report journal: every crate's events (ps5upload_core::events) land on disk here.
    event_journal::install();
    ps5upload_core::events::emit(
        ps5upload_core::events::Cat::System,
        ps5upload_core::events::Level::Info,
        "engine_start",
        None,
        format!("engine {} started", env!("CARGO_PKG_VERSION")),
    );

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
    // "Use less of this computer" as it was last set (#632).
    workshop_api::load();
    // Play Together's "Invites work" as it was last set.
    join::load_invites();

    // The Collection's automatic refresh (a no-op until a folder is added).
    collection_api::spawn_auto_refresh();
    // "Keep payloads up to date" (a no-op until the user turns it on).
    payload_updates_api::spawn_scheduler();
    // Install sessions live in their own state (served by pkg_install::api_router below); the helper
    // log mirror also reads it, to stay off a console while an install runs.
    let pkg_state = std::sync::Arc::new(pkg_install::PkgInstallState::restored());
    // Automatic save backups wait for an install too.
    save_backup::watch_installs(pkg_state.clone());
    mesh_drive::watch_installs(pkg_state.clone());
    // Copies each connected console's helper log into the event journal.
    {
        let jobs = state.jobs.clone();
        let installs = pkg_state.clone();
        helper_mirror::spawn(std::sync::Arc::new(move || {
            installs.jobs.any_active()
                || jobs
                    .lock()
                    .map(|g| g.values().any(|s| matches!(s, JobState::Running { .. })))
                    .unwrap_or(true)
        }));
    }

    let state_jobs_for_presence = state.jobs.clone();
    let (app, mcp_state) = build_app_and_mcp(
        state,
        pkg_state,
        guard_cfg,
        host_guard::base_path(),
        queue_api::QueueState::from_env(),
    );
    // Mirror to stdout (terminal users) AND the engine.log ring
    // (post-mortem diagnosis from the desktop shell once the
    // terminal is closed). Printed only once the bind has succeeded:
    // it used to come first, so a log showing it proved nothing about
    // whether the engine was actually listening.
    println!("[ps5upload-engine] listening on http://{bind}  (ps5={ps5_addr})");
    crate::log_info!("listening on http://{bind}  (ps5={ps5_addr})");
    // Fan boosts end on time and are undone even if the app closed in between.
    fan_boost::spawn_sweeper();
    // Automatic save backups run here, not in a window: they carry on with no app open.
    save_backup::spawn(
        &bind,
        &std::env::var("PS5UPLOAD_BASE_URL").unwrap_or_default(),
    );
    // Save backups to a network share: on an interval, or when a game is closed (#592).
    save_backup_server::spawn();
    // Mesh Drive folders are kept in sync here too, app open or not.
    mesh_drive::spawn();
    pkg_install::spawn_stale_upload_sweep();
    // What the other devices see of this engine, with no window open to say so.
    mesh_api::spawn_presence(state_jobs_for_presence);
    // Server copies an earlier run left behind (an extract that failed, then the app closed)
    // are removed here: nothing else knows them after a restart.
    tokio::task::spawn_blocking(remote::fetch::sweep_stale_copies);
    tokio::task::spawn_blocking(settings_backup::seal_old_backups);
    // Graceful shutdown: when the parent-watcher fires (stdin EOF), the
    // SHUTDOWN Notify wakes this future, axum stops
    // accepting new connections, drains in-flight ones, then returns.
    // `spawn_drain_ceiling` is the hard ceiling: an open `/api/events`, a
    // browser upload or a `/pkg-host` read would otherwise hold the drain
    // until Docker's SIGKILL, and the stop line below would never be written.
    // `into_make_service_with_connect_info::<SocketAddr>` is what makes
    // the `ConnectInfo<SocketAddr>` extractor work in the loopback-guard
    // middleware. Without this, axum hands handlers a generic ConnectInfo
    // and the middleware would have to fall back to header-based source
    // detection (less reliable, easier to spoof from a hostile LAN peer).
    let served = if cfg.exit_on_error {
        axum::serve(
            no_delay(listener),
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            // An open MCP subscription would hold the drain up for as long as its client stays.
            mcp_state.begin_shutdown();
            spawn_drain_ceiling();
        })
        .await
    } else {
        serve_rebindable(&bind, listener, app, async move {
            shutdown_signal().await;
            mcp_state.begin_shutdown();
        })
        .await
    };
    ps5upload_core::events::emit(
        ps5upload_core::events::Cat::System,
        ps5upload_core::events::Level::Info,
        "engine_stop",
        None,
        "engine stopped",
    );
    event_journal::flush_now();
    if let Err(e) = served {
        stderr_quiet(&format!("[ps5upload-engine] axum serve terminated: {e}"));
        if cfg.exit_on_error {
            std::process::exit(3);
        }
        return Err(anyhow::anyhow!("axum serve terminated: {e}"));
    }
    Ok(())
}

/// The whole HTTP surface: every route and layer, nothing started. `base` is the URL prefix
/// (`PS5UPLOAD_BASE_URL`), empty for the root.
#[cfg(test)]
fn build_app(
    state: AppState,
    pkg_state: pkg_install::PkgInstallStateHandle,
    guard_cfg: LoopbackGuardConfig,
    base: &str,
) -> Router {
    build_app_and_mcp(
        state,
        pkg_state,
        guard_cfg,
        base,
        queue_api::QueueState::from_env(),
    )
    .0
}

/// [`build_app`], and the MCP server in it: an open subscription must be told when the engine stops.
fn build_app_and_mcp(
    state: AppState,
    pkg_state: pkg_install::PkgInstallStateHandle,
    guard_cfg: LoopbackGuardConfig,
    base: &str,
    queue: queue_api::QueueState,
) -> (Router, mcp::McpState) {
    let mcp_events = state.events_tx.clone();
    let ps5_addr = state.default_ps5_addr.clone();
    // Job snapshots onto the numbered feed of `GET /api/events`.
    event_bus::forward_jobs(&state.events_tx);
    let op_registry = std::sync::Arc::new(ops::OpRegistry::default());
    let ops_router = ops::router(op_registry.clone(), state.clone(), pkg_state.clone());
    let activity_installs = pkg_state.clone();
    let jobs_installs = pkg_state.clone();
    let helper_stop_installs = pkg_state.clone();
    let eject_installs = pkg_state.clone();
    let browser_payload_installs = pkg_state.clone();
    let health_fix_installs = pkg_state.clone();
    let cleanup_installs = pkg_state.clone();
    let app = Router::new()
        .route("/", get(ui_handler))
        .route(
            "/api/activity",
            get(
                move |st: State<AppState>, Query(q): Query<activity::SinceQuery>| {
                    activity_handler(st, activity_installs.clone(), q)
                },
            ),
        )
        .route("/api/activity/label", post(activity::label_handler))
        .route("/api/ps5/status", get(ps5_status))
        .route("/api/ps5/connect-timings", get(ps5_connect_timings))
        .route("/api/ps5/port-check", get(ps5_port_check))
        .route("/api/ps5/toast", post(ps5_toast))
        .route("/api/ps5/readiness", get(ps5_readiness))
        .route(
            ps5upload_core::mgmt_proxy::ROUTE,
            post(mgmt_route::mgmt_call_handler),
        )
        .route("/api/ps5/health/scan", get(health_scan_handler))
        .route("/api/net/doctor", get(net_doctor_handler))
        .route("/api/net/doctor/reply", get(net_doctor_reply_handler))
        .route("/api/speed-test/prepare", post(speed_test_prepare_handler))
        .route("/api/speed-test/cleanup", post(speed_test_cleanup_handler))
        .route("/api/ps5/health/junk", get(health_junk_handler))
        .route(
            "/api/ps5/health/fix",
            post(move |Json(req): Json<HealthFixReq>| {
                health_fix_handler(health_fix_installs.clone(), req)
            }),
        )
        .route("/api/ps5/cleanup", post(ps5_cleanup))
        .route("/api/ps5/volumes", get(ps5_volumes))
        .route("/api/ps5/pkg/scan-external", get(ps5_pkg_scan_external))
        .route("/api/ps5/pkg/metadata", get(ps5_pkg_metadata))
        .route("/api/ps5/list-dir", get(ps5_list_dir))
        .route("/api/ps5/disk-usage", post(disk_usage::start))
        .route("/api/ps5/disk-usage/{id}", get(disk_usage::status))
        .route("/api/ps5/disk-usage/{id}/cancel", post(disk_usage::cancel))
        .route("/api/ps5/cleanup/scan", post(console_cleanup_api::scan))
        .route(
            "/api/ps5/tailnet",
            get(tailnet_api::status).post(tailnet_api::op),
        )
        .route(
            "/api/tailnet",
            get(tailnet_api::engine_status).post(tailnet_api::engine_op),
        )
        .route("/api/tailnet/route", get(tailnet_api::route))
        .route("/api/tailnet/check", get(tailnet_api::check))
        .route("/api/tailnet/via", get(tailnet_api::via))
        .route("/api/tailnet/consoles", get(tailnet_discover::consoles))
        .route("/api/tailnet/consoles/add", post(tailnet_discover::add))
        .route(
            "/api/ps5/cleanup/scan/{id}",
            get(console_cleanup_api::scan_status),
        )
        .route(
            "/api/ps5/cleanup/scan/{id}/apply",
            post(
                move |axum::extract::Path(id): axum::extract::Path<u32>,
                      Json(req): Json<console_cleanup_api::ApplyBody>| {
                    console_cleanup_api::apply(cleanup_installs.clone(), id, req)
                },
            ),
        )
        .route(
            "/api/ps5/cleanup/apply/{id}",
            get(console_cleanup_api::apply_status),
        )
        .route("/api/ps5/cleanup/cancel", post(console_cleanup_api::cancel))
        .route("/api/ps5/console-tasks", get(console_tasks_api::list))
        .route(
            "/api/ps5/console-tasks/cancel",
            post(console_tasks_api::cancel),
        )
        .route("/api/ps5/console-shares", get(console_shares_api::list))
        .route(
            "/api/ps5/console-shares/save",
            post(console_shares_api::save),
        )
        .route(
            "/api/ps5/console-shares/remove",
            post(console_shares_api::remove),
        )
        .route("/api/ava1/identity", get(ava1_api::identity_handler))
        .route("/api/ava1/console", get(ava1_api::console_handler))
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
            "/api/ava1/pairing/allow",
            post(ava1_api::pairing_allow_handler),
        )
        .route(
            "/api/ava1/pairing/forget",
            post(ava1_api::pairing_forget_handler),
        )
        .route(
            "/api/ava1/same-console",
            post(ava1_api::same_console_handler),
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
        .route("/api/fpkg/ampr-emu", post(ampr_emu_api::ampr_emu_handler))
        .route(
            "/api/fpkg/ampr-index",
            post(ampr_index_api::ampr_index_handler),
        )
        .route("/api/fpkg/estimate", post(fpkg_api::fpkg_estimate_handler))
        .route("/api/fpkg/extract", post(fpkg_api::fpkg_extract_handler))
        .route(
            "/api/fpkg/extract/cleanup",
            post(fpkg_api::fpkg_extract_cleanup_handler),
        )
        .route("/api/exfat/build", post(fpkg_api::exfat_build_handler))
        .route("/api/folder/build", post(fpkg_api::folder_build_handler))
        .route(
            "/api/image/browse",
            post(image_browse::image_browse_handler),
        )
        .route(
            "/api/collection/settings",
            get(collection_api::get_settings).put(collection_api::put_settings),
        )
        .route("/api/collection/library", get(collection_api::get_library))
        .route("/api/collection/games/{id}", get(collection_api::get_game))
        .route(
            "/api/collection/games/{id}/cover",
            get(collection_api::get_cover),
        )
        .route(
            "/api/collection/scan",
            get(collection_api::get_scan).post(collection_api::post_scan),
        )
        .route(
            "/api/collection/scan/cancel",
            post(collection_api::post_scan_cancel),
        )
        .route("/api/collection/export", get(collection_api::get_export))
        .route("/api/collection/console", get(collection_api::get_console))
        .route(
            "/api/collection/check",
            get(collection_check_api::get_checks).post(collection_check_api::post_check),
        )
        .route(
            "/api/collection/check/cancel",
            post(collection_check_api::post_check_cancel),
        )
        .route("/api/games/installed", get(games_api::list_installed))
        .route("/api/console-backup/scan", post(console_backup::post_scan))
        .route(
            "/api/console-backup/backup",
            post(console_backup::post_backup),
        )
        .route(
            "/api/console-backup/inspect",
            post(console_backup::post_inspect),
        )
        .route(
            "/api/console-backup/restore",
            post(console_backup::post_restore),
        )
        .route(
            "/api/console-backup/status",
            get(console_backup::get_status),
        )
        .route(
            "/api/console-backup/cancel",
            post(console_backup::post_cancel),
        )
        .route(
            "/api/save-backup",
            get(save_backup::get_view).put(save_backup::put_settings),
        )
        .route("/api/save-backup/run", post(save_backup::post_run))
        .route(
            "/api/save-backup/server",
            get(save_backup_server::get_view).put(save_backup_server::put_settings),
        )
        .route(
            "/api/save-backup/server/run",
            post(save_backup_server::post_run),
        )
        .route(
            "/api/save-backup/server/list",
            get(save_backup_server::get_list),
        )
        .route(
            "/api/save-backup/server/restore",
            post(save_backup_server::post_restore),
        )
        .route("/api/save-backup/list", get(save_backup::get_list))
        .route("/api/save-backup/switch", get(emulator_saves::get_switch))
        .route("/api/save-backup/ps2", get(ps2_saves::get_ps2))
        .route("/api/save-backup/restore", post(save_backup::post_restore))
        .route(
            "/api/save-backup/restore-file",
            post(save_backup::post_restore_file),
        )
        .route("/api/save-backup/note", get(save_backup::get_note))
        .route(
            "/api/save-backup/snapshot",
            post(save_backup::post_snapshot),
        )
        .route(
            "/api/save-backup/import-ps4",
            post(save_backup::post_import_ps4),
        )
        .route("/api/save-backup/share", post(save_copy::post_share))
        // A shared save's zip comes as the body, written to disk as it arrives (its own limit).
        .route(
            "/api/save-backup/use-shared",
            post(save_copy::post_use_shared),
        )
        // Another user's save of the same game on the same console (#612).
        .route("/api/save-backup/copy", post(save_copy::post_copy))
        .route("/api/save-backup/now", post(save_backup::post_back_up_now))
        .route("/api/save-backup/latest", get(save_backup::get_latest))
        .route(
            "/api/save-backup/restore-to-user",
            post(save_copy::post_restore_to_user),
        )
        .route(
            "/api/save-backup/copy-console",
            post(save_copy::post_copy_console),
        )
        .route(
            "/api/save-backup/copy-console/targets",
            get(save_copy::get_copy_targets),
        )
        .route(
            "/api/shared/{name}",
            get(shared_state::get_doc).put(shared_state::put_doc),
        )
        .route("/api/shared/{name}/schema", get(shared_state::schema))
        // The console list and settings with names and types, every platform.
        .route(
            "/api/consoles",
            get(consoles_api::list).put(consoles_api::put),
        )
        .route(
            "/api/consoles/settings",
            axum::routing::patch(consoles_api::patch_settings),
        )
        .route(
            "/api/consoles/document",
            get(consoles_api::get_document).put(consoles_api::put_document),
        )
        .route(
            "/api/settings",
            get(settings_api::get).patch(settings_api::patch),
        )
        .route("/api/games/{id}", get(games_api::get_game))
        .route("/api/games/{id}/refresh", post(games_api::refresh))
        .route("/api/console-snapshots/keep", post(games_api::keep))
        .route("/api/console-snapshots/forget", post(games_api::forget))
        .route(
            "/api/collection/trash/preview",
            post(collection_tidy_api::post_trash_preview),
        )
        .route(
            "/api/collection/trash/apply",
            post(collection_tidy_api::post_trash_apply),
        )
        .route(
            "/api/collection/organize/plan",
            post(collection_tidy_api::post_organize_plan),
        )
        .route(
            "/api/collection/organize/plan/start",
            post(collection_tidy_api::post_organize_plan_start),
        )
        .route(
            "/api/collection/organize/plan/status",
            get(collection_tidy_api::get_organize_plan_status),
        )
        .route(
            "/api/collection/organize/plan/cancel",
            post(collection_tidy_api::post_organize_plan_cancel),
        )
        .route(
            "/api/collection/organize/apply",
            post(collection_tidy_api::post_organize_apply),
        )
        .route(
            "/api/collection/organize/runs",
            get(collection_tidy_api::get_organize_runs),
        )
        .route(
            "/api/collection/organize/revert",
            post(collection_tidy_api::post_organize_revert),
        )
        .route("/api/collection/junk", get(collection_tidy_api::get_junk))
        .route(
            "/api/collection/junk/scan",
            post(collection_tidy_api::post_junk_scan),
        )
        .route(
            "/api/collection/junk/cancel",
            post(collection_tidy_api::post_junk_cancel),
        )
        .route(
            "/api/collection/junk/clean",
            post(collection_tidy_api::post_junk_clean),
        )
        .route("/api/collection/macos", get(collection_tidy_api::get_macos))
        .route(
            "/api/collection/macos/finder",
            axum::routing::put(collection_tidy_api::put_macos_finder),
        )
        .route(
            "/api/collection/macos/spotlight",
            post(collection_tidy_api::post_macos_spotlight),
        )
        .route("/api/local/list-dir", get(local_list_dir_handler))
        .route("/api/local/game-icon", get(local_game_icon_handler))
        .route("/api/local/storage-roots", get(local_storage_roots_handler))
        .route("/api/ps5/fs/delete", post(ps5_fs_delete))
        .route("/api/ps5/fs/move", post(ps5_fs_move))
        .route("/api/ps5/fs/unpack", post(ps5_fs_unpack))
        .route("/api/ps5/fs/extract", post(ps5_fs_extract))
        .route("/api/ps5/dump", post(app_dump_api::dump))
        .route("/api/ps5/fs/copy", post(ps5_fs_copy))
        .route("/api/ps5/fs/op-status", get(ps5_fs_op_status))
        .route("/api/ps5/fs/op-cancel", post(ps5_fs_op_cancel))
        .route("/api/ps5/fs/mount", post(ps5_fs_mount))
        .route("/api/ps5/fs/unmount", post(ps5_fs_unmount))
        .route(
            "/api/ps5/usb/eject",
            post(move |Json(req): Json<UsbEjectReq>| ps5_usb_eject(eject_installs.clone(), req)),
        )
        .route("/api/ps5/saves/open", post(ps5_save_open))
        .route("/api/ps5/saves/close", post(ps5_save_close))
        .route("/api/ps5/app/launch", post(ps5_app_launch))
        .route("/api/ps5/app/register", post(ps5_app_register))
        .route("/api/ps5/app/unregister", post(ps5_app_unregister))
        .route("/api/ps5/hw/info", get(ps5_hw_info))
        .route("/api/ps5/hw/temps", get(ps5_hw_temps))
        .route("/api/ps5/syslog/tail", get(ps5_syslog_tail))
        .route("/api/ps5/time/get", get(ps5_time_get_route))
        .route("/api/ps5/time/sync", post(ps5_time_sync_route))
        .route(
            "/api/ps5/smp-meta/control",
            post(ps5_smp_meta_control_route),
        )
        .route("/api/ps5/smp-meta/stats", get(ps5_smp_meta_stats_route))
        .route("/api/ps5/hw/power", get(ps5_hw_power))
        .route("/api/ps5/hw/storage", get(ps5_hw_storage))
        .route("/api/ps5/hw/drive-sensors", get(ps5_hw_drive_sensors))
        // the running game's sampled frame rate, live and captured (#774)
        .route("/api/ps5/hw/fps", get(game_fps_api::fps_live))
        .route(
            "/api/ps5/hw/fps/capture",
            get(game_fps_api::fps_capture_get),
        )
        .route(
            "/api/ps5/hw/fps/capture/start",
            post(game_fps_api::fps_capture_start),
        )
        .route(
            "/api/ps5/hw/fps/capture/stop",
            post(game_fps_api::fps_capture_stop),
        )
        .route(
            "/api/ps5/hw/fps/capture/csv",
            get(game_fps_api::fps_capture_csv),
        )
        .route("/api/ps5/proc/list", get(ps5_proc_list))
        .route("/api/ps5/app/lifecycle", post(ps5_app_lifecycle))
        .route("/api/ps5/klog", get(ps5_klog))
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
        .route(
            "/api/ps5/apps/hidden",
            get(ps5_hidden_icons).post(ps5_set_icon_hidden),
        )
        .route("/api/ps5/focus", get(ps5_focus))
        .route("/api/ps5/net/ports", get(ps5_net_ports))
        // 8.0.0: this engine as one of the devices that use a console (mesh_api.rs).
        .route("/api/mesh/me", get(mesh_api::me_get).post(mesh_api::me_set))
        .route("/api/mesh/state", get(mesh_api::state))
        .route("/api/mesh/activity", post(mesh_api::activity))
        .route("/api/mesh/lock", post(mesh_api::lock))
        .route("/api/mesh/device", post(mesh_api::device))
        .route("/api/mesh/settings", post(mesh_api::settings))
        .route(
            "/api/mesh/doc",
            get(mesh_api::doc_get).post(mesh_api::doc_put),
        )
        .route("/api/mesh/announce", post(mesh_api::announce))
        // 8.0.0 Mesh Drive (mesh_drive.rs).
        .route("/api/drive", get(mesh_drive::list))
        .route("/api/drive/local", get(mesh_drive::local))
        .route("/api/drive/create", post(mesh_drive::create))
        .route("/api/drive/check", post(mesh_drive::check))
        .route("/api/drive/join", post(mesh_drive::join))
        .route("/api/drive/approve", post(mesh_drive::approve))
        .route("/api/drive/password", post(mesh_drive::password))
        .route("/api/drive/link", post(mesh_drive::link))
        .route("/api/drive/unlink", post(mesh_drive::unlink))
        .route("/api/drive/conflicts", get(mesh_drive::conflicts))
        .route("/api/drive/resolve", post(mesh_drive::resolve))
        .route(
            "/api/settings-backup",
            get(settings_backup::list).post(settings_backup::save),
        )
        .route("/api/settings-backup/get", get(settings_backup::get))
        .route("/api/drive/leave", post(mesh_drive::leave))
        .route("/api/drive/delete", post(mesh_drive::delete))
        .route("/api/drive/pause", post(mesh_drive::pause))
        .route("/api/drive/sync", post(mesh_drive::sync_now))
        .route("/api/drive/confirm", post(mesh_drive::confirm))
        .route(
            "/api/ps5/files/prefs",
            get(console_files_api::prefs_get_handler),
        )
        .route(
            "/api/ps5/files/prefs/set",
            post(console_files_api::prefs_set_handler),
        )
        .route(
            "/api/ps5/files/handoff/put",
            post(console_files_api::handoff_put_handler),
        )
        .route(
            "/api/ps5/files/handoff/take",
            post(console_files_api::handoff_take_handler),
        )
        .route("/api/ps5/fs/read-preview", post(ps5_fs_read_preview))
        .route("/api/ps5/process/list", get(ps5_process_list))
        .route("/api/ps5/elfldr/health", get(ps5_elfldr_health))
        .route("/api/ampr/trace", get(ampr_trace::status))
        .route("/api/ampr/trace/start", post(ampr_trace::start))
        .route("/api/ampr/trace/stop", post(ampr_trace::stop))
        .route("/api/ampr/trace/collect", post(ampr_trace::collect))
        .route("/api/ampr/trace/generate", post(ampr_trace::generate))
        .route(
            "/api/ampr/profile",
            get(ampr_trace::profile_get).delete(ampr_trace::profile_delete),
        )
        .route(
            "/api/ps5/helper/replace-copy",
            post(helper_copies_api::replace_copy_handler),
        )
        .route(
            "/api/ps5/helper/restore-previous",
            post(helper_copies_api::restore_previous_handler),
        )
        .route(
            "/api/ps5/helper/store-in-pldmgr",
            post(helper_copies_api::store_in_pldmgr_handler),
        )
        .route("/api/ps5/pldmgr", get(pldmgr_api::overview))
        .route("/api/ps5/pldmgr/launch", post(pldmgr_api::launch))
        .route("/api/ps5/pldmgr/delete", post(pldmgr_api::delete))
        .route("/api/ps5/pldmgr/autoload", post(pldmgr_api::autoload))
        .route("/api/ps5/pldmgr/store", post(pldmgr_api::store))
        .route(
            "/api/ps5/pldmgr/upload",
            post(pldmgr_api::upload).layer(axum::extract::DefaultBodyLimit::max(128 * 1024 * 1024)),
        )
        .route("/api/ps5/elfldr/ensure", post(ps5_elfldr_ensure))
        .route("/api/ps5/process/kill", post(ps5_process_kill))
        .route("/api/ps5/power/control", post(ps5_power_control))
        .route(
            "/api/ps5/helper/stop",
            post(move |Json(req): Json<HelperStopReq>| {
                ps5_helper_stop(helper_stop_installs.clone(), req)
            }),
        )
        .route("/api/ps5/power/telemetry", get(ps5_power_telemetry))
        .route("/api/ps5/power/wake", post(ps5_power_wake))
        .route("/api/ps5/power/wake-login", post(ps5_power_wake_login))
        .route(
            "/api/ps5/power/wake-login/passcode",
            post(wake_login_api::answer),
        )
        // Remote Play stream spike (rp_stream_api).
        .route("/api/ps5/rp/ws", get(rp_stream_api::ws))
        .route("/api/ps5/rp/session", get(rp_stream_api::current))
        .route("/api/ps5/rp/controller", get(rp_stream_api::controller))
        .route(
            "/api/ps5/rp/pair/attempts",
            get(rp_diag::pair_attempts_handler),
        )
        // Remote Play with no app page (rp_control.rs), this computer only.
        .route("/api/ps5/rp/start", post(rp_stream_api::control::start))
        .route("/api/ps5/rp/stop", post(rp_stream_api::control::stop))
        .route(
            "/api/ps5/rp/settings",
            post(rp_stream_api::control::settings),
        )
        .route(
            "/api/ps5/rp/passcode",
            post(rp_stream_api::control::passcode),
        )
        .route(
            "/api/ps5/rp/keyboard",
            post(rp_stream_api::control::keyboard),
        )
        .route("/api/ps5/rp/mic", post(rp_stream_api::control::mic))
        .route("/api/ps5/rp/replay", get(rp_stream_api::replay))
        // Remote Play for a local script or agent (#615), this computer only.
        .route("/api/ps5/rp/frame", get(rp_stream_api::automation::frame))
        .route("/api/ps5/rp/input", post(rp_stream_api::automation::input))
        .route(
            "/api/ps5/rp/picture",
            post(rp_stream_api::automation::picture),
        )
        .route(
            "/api/ps5/rp/input/release",
            post(rp_stream_api::automation::release),
        )
        // The live feed for OBS (#635), this computer only.
        .route(
            "/api/ps5/rp/live.ts",
            get(rp_stream_api::live_feed::live_ts),
        )
        .route(
            "/api/ps5/rp/live/status",
            get(rp_stream_api::live_feed::status),
        )
        // Go live (#635): the Remote Play stream to YouTube, Twitch, Kick, Facebook, custom.
        .route("/api/stream/status", get(rp_golive::status))
        .route("/api/stream/start", post(rp_golive::start))
        .route("/api/stream/stop", post(rp_golive::stop))
        .route("/api/stream/destinations", post(rp_golive::add_destination))
        .route(
            "/api/stream/destinations/{id}",
            post(rp_golive::update_destination).delete(rp_golive::remove_destination),
        )
        .route(
            "/api/stream/tools/ffmpeg/install",
            post(rp_golive::install_ffmpeg),
        )
        .route("/api/ps5/vinput/caps", get(vinput_api::caps))
        .route("/api/ps5/vinput/ws", get(vinput_api::ws))
        .route(
            "/api/ps5/vinput",
            get(vinput_api::http_get).put(vinput_api::http_put),
        )
        .route("/api/ps5/vinput/report", post(vinput_api::http_report))
        .route("/rp-player", get(rp_stream_api::player))
        .route("/pad", get(pad_link::page))
        .route("/api/pad/ws", get(pad_link::ws))
        // Play Together (#641): guests as Players 2-4 (join.rs).
        .route("/join", get(join::page))
        .route("/api/join/ws", get(join::ws))
        .route("/api/join/relay", get(join::relay))
        .route("/api/join/signal", get(join::signal))
        .route("/api/join", get(join::get_state))
        .route("/api/join/open", post(join::open))
        .route("/api/join/close", post(join::close))
        .route("/api/join/invite", post(join::invite))
        .route("/api/join/decide", post(join::decide))
        .route("/api/join/kick", post(join::kick))
        .route("/api/join/playing", post(join::playing))
        .route("/api/join/settings", post(join::set_settings))
        .route("/api/join/profiles", get(join::profiles))
        // Share: an app's things to a friend (share/mod.rs).
        .route("/share", get(share::page))
        .route("/api/share/ws", get(share::ws))
        .route("/api/share", get(share::list).post(share::create))
        .route("/api/share/check", post(share::check))
        .route("/api/share/{id}", get(share::get_one).delete(share::cancel))
        .route("/api/share/{id}/decide", post(share::decide))
        .route("/api/share/receive", post(share::receive_open))
        .route(
            "/api/share/receive/{id}",
            get(share::receive_get).delete(share::receive_cancel),
        )
        .route(
            "/api/share/receive/{id}/accept",
            post(share::receive_accept),
        )
        .route(
            "/api/share/receive/{id}/use-save",
            post(share::receive_use_save),
        )
        .route(
            "/api/share/receive/{id}/data/{item}",
            get(share::receive_data),
        )
        .route(
            "/api/ps5/rp/pad-link",
            get(pad_link::get_status).post(pad_link::set_open),
        )
        .route("/api/ps5/rp/pad-link/decide", post(pad_link::decide))
        .route("/api/ps5/rp/pad-link/kick", post(pad_link::kick))
        .route("/api/ps5/rp/pad-link/control", post(pad_link::control))
        .route(
            "/api/ps5/rp/pad-link/internet",
            post(pad_link::set_internet),
        )
        .route("/api/ps5/rp/pad-relay", get(pad_link::relay))
        .route("/api/ps5/power/ddp-status", get(ps5_power_ddp_status))
        .route("/api/ps5/power/pair", post(ps5_power_pair))
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
            "/api/ps5/remoteplay/cancel",
            post(remoteplay_cancel_handler),
        )
        .route("/api/ps5/notif/list", get(notif_list_handler))
        .route("/api/ps5/notif/clear", post(notif_clear_handler))
        .route("/api/ps5/activity/reset", post(activity_reset_handler))
        .route("/api/local/image/attach", post(local_image_attach))
        .route("/api/local/image/detach", post(local_image_detach))
        .route("/api/local/image/status", get(local_image_status))
        .route("/api/ps5/cheats/list", get(cheats_list_handler))
        .route("/api/ps5/cheats/get", get(cheats_get_handler))
        .route("/api/ps5/cheats/toggle", post(cheats_toggle_handler))
        .route("/api/ps5/cheats/delete", post(cheats_delete_handler))
        .route("/api/ps5/cheats/reload", post(cheats_reload_handler))
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
        .merge(browser_payload::router(browser_payload_installs))
        .merge(payloads_api::router())
        .merge(console_tools_api::router())
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
        .route("/api/ps5/smp/rescan", post(ps5_smp_rescan))
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
        .route(
            "/api/ps5/fan-boost",
            get(fan_boost::get).post(fan_boost::post),
        )
        .route("/api/ps5/peripheral", post(ps5_peripheral))
        .route(
            "/api/ps5/cheats/import",
            post(console_files_api::cheat_import_handler),
        )
        .route(
            "/api/ps5/minion/tile",
            post(console_files_api::minion_tile_handler),
        )
        .route("/api/ps5/fs/stat", post(console_files_api::stat_handler))
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
        .route(
            "/api/transfer/ps5-to-ps5/remote",
            get(c2c_api::remote_handler),
        )
        .route("/api/transfer/ps5-to-ps5/test", post(c2c_api::test_handler))
        .route("/api/local/path-kind", get(local_path_kind_handler))
        .route(
            "/api/local/inspect-folder",
            get(local_inspect_folder_handler),
        )
        .route("/api/zip/inspect", post(zip_inspect_handler))
        .route("/api/zip/inspect/stream", post(zip_inspect_stream_handler))
        .route("/api/transfer/7z", post(transfer_7z_handler))
        .route("/api/7z/inspect", post(sevenz_inspect_handler))
        .route(
            "/api/7z/inspect/stream",
            post(sevenz_inspect_stream_handler),
        )
        .route("/api/transfer/tar", post(transfer_tar_handler))
        .route("/api/tar/inspect", post(tar_inspect_handler))
        .route("/api/tar/inspect/stream", post(tar_inspect_stream_handler))
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
        .route("/api/profile/np/state", post(profile_np_state_handler))
        .route("/api/profile/np/backup", post(profile_np_backup_handler))
        .route("/api/profile/np/restore", post(profile_np_restore_handler))
        .route(
            "/api/profile/avatar",
            post(profile_avatar_handler)
                .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route(
            "/api/profile/avatar/current",
            get(profile_avatar_current_handler),
        )
        .route(
            "/api/profile/avatar/preview",
            post(profile_avatar_preview_handler)
                .layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024)),
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
        .route("/api/engine/update", get(engine_update::check))
        .route(
            "/api/jobs",
            get({
                let installs = jobs_installs.clone();
                move |st: State<AppState>, q: Query<ListJobsQuery>| {
                    list_jobs(st, installs.clone(), q)
                }
            }),
        )
        .route("/api/jobs/summaries", get(telemetry::summaries_handler))
        .route("/api/jobs/{id}/summary", get(telemetry::summary_handler))
        .route("/api/metrics", get(telemetry::metrics_handler))
        .route("/api/bug-report/bundle", post(bug_report_bundle_handler))
        .route(
            "/api/jobs/{id}",
            get({
                let installs = jobs_installs.clone();
                move |st: State<AppState>, id: Path<String>| get_job(st, installs.clone(), id)
            }),
        )
        .route("/api/jobs/{id}/files", get(get_job_files))
        .route(
            "/api/jobs/{id}/cancel",
            post({
                let installs = jobs_installs.clone();
                move |id: Path<String>| cancel_job(installs.clone(), id)
            }),
        )
        .route("/api/jobs/{id}/pause", post(workshop_api::pause_job))
        .route("/api/jobs/{id}/resume", post(workshop_api::resume_job))
        .route("/api/workshop/jobs", get(workshop_api::get_jobs))
        .route(
            "/api/workshop/settings",
            get(workshop_api::get_settings).post(workshop_api::set_settings),
        )
        .route("/api/events", get(event_bus::events_handler))
        .route("/api/engine-logs", get(engine_logs_tail))
        .route(
            "/api/diag/log-level",
            get(diag_api::log_level_get).post(diag_api::log_level_set),
        )
        .route("/api/diag/features", get(diag_api::features))
        .route(
            "/api/logs/level",
            get(log_levels::get_level).put(log_levels::put_level),
        )
        .route("/api/logs/recent", get(log_levels::recent))
        .route("/api/event-journal", get(event_journal::events_handler))
        .route(
            "/api/ps5/helper-log-ftp",
            get(helper_mirror::ftp_log_handler),
        )
        .route("/api/debug/crash", get(debug_crash))
        .route("/api/openapi.json", get(openapi::serve))
        .with_state(state);

    // All browser APIs must be merged before nesting under the URL prefix.
    // Package-host URLs stay at the root because the console fetches them directly.
    let app = app
        .merge(pkg_install::api_router(pkg_state.clone()))
        .merge(remote_download::router(std::sync::Arc::new(
            remote_download::DownloadRegistry::default(),
        )))
        .merge(queue_api::router(queue.clone()))
        .merge(ops_router);

    // Queue items call this same API in-process, as the MCP server does.
    queue.attach(
        app.clone()
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
        pkg_state.clone(),
    );

    // The MCP server drives this same API in-process: it holds a clone of the router as it stands
    // here, without /mcp itself or the reverse-proxy prefix. The body limit and the request log are
    // what the outer layers below give an HTTP client.
    let mcp_state = mcp::McpState::new(
        app.clone()
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
            .layer(middleware::from_fn(log_requests)),
        mcp_events,
        ps5_addr.clone(),
    );
    let app = app.merge(mcp::router(mcp_state.clone()));

    // SPA fallback: serve the embedded React bundle for every path that doesn't
    // match an explicit /api/* route above.  Only compiled when the `webui`
    // feature is on (Docker / self-hosted image); the regular build keeps the
    // simple `GET /` dashboard handler above. An unknown /api/* path is a JSON
    // 404 in every build.
    #[cfg(feature = "webui")]
    let app = app.fallback(webui::spa_fallback);
    #[cfg(not(feature = "webui"))]
    let app = app.fallback(api_errors::fallback);

    let app = if base.is_empty() {
        app
    } else {
        // Create Axum Router using the base URL as a prefix for all routes.
        axum::Router::new()
            .nest(base, app)
            .route(&format!("{base}/"), get(ui_handler))
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
        .allow_headers(Any)
        // The renderer's JSON POST pollers would otherwise send a preflight before every request.
        .max_age(std::time::Duration::from_secs(600));

    let app = app
        .merge(pkg_install::host_router(pkg_state))
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
        // `Prefer: respond-async`: outside the log, so the log shows each route's real answer;
        // inside CORS and the guards, so its 202 is an ordinary answer to an allowed request.
        .layer(middleware::from_fn_with_state(
            op_registry,
            ops::respond_async,
        ))
        // Tauri's renderer performs a few direct fetches to the sidecar for
        // liveness, job polling and streamed resources. Emit CORS headers only
        // for origins accepted by the same narrow trust rule as the guard
        // below. The guard sits outside this layer, so foreign origins are
        // rejected before CORS can answer either a simple request or preflight.
        .layer(cors)
        .layer(middleware::from_fn(web_pages::api_no_store))
        .layer(middleware::from_fn(web_pages::finish_pages))
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
        .layer(middleware::from_fn_with_state(guard_cfg, loopback_guard))
        // Outside even the guards: their refusals are errors in the API's shape too. It also
        // works out the path without the prefix, which the guards above match on.
        .layer(middleware::from_fn_with_state(
            std::sync::Arc::<str>::from(base),
            api_errors::api_surface,
        ))
        // Outside the allowlist it feeds: behind a trusted proxy, the address that counts is the
        // client's.
        .layer(middleware::from_fn(host_guard::apply_trusted_proxy));
    (app, mcp_state)
}

fn rebind_signal() -> &'static tokio::sync::Notify {
    static SIGNAL: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
    SIGNAL.get_or_init(tokio::sync::Notify::new)
}

/// Phones: drop the listening socket and bind a new one, leaving the engine and its running
/// requests alone. iOS can reclaim a suspended app's listening socket, after which nothing
/// connects and `accept` only fails, while the process itself is fine.
pub fn rebind_in_process() {
    rebind_signal().notify_one();
}

/// In-process serving: as `axum::serve`, plus `rebind_in_process`. Connections already open finish
/// on the old server, which has dropped its listener by then.
async fn serve_rebindable<F>(
    bind: &str,
    first: tokio::net::TcpListener,
    app: Router,
    shutdown: F,
) -> std::io::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let (stop_all, _) = tokio::sync::watch::channel(false);
    let stop = stop_all.clone();
    tokio::spawn(async move {
        shutdown.await;
        let _ = stop.send(true);
    });
    let mut listener = first;
    loop {
        let (stop_this, stop_this_rx) = tokio::sync::oneshot::channel::<()>();
        let mut all = stop_all.subscribe();
        let serve = axum::serve(
            no_delay(listener),
            app.clone()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = stop_this_rx => {}
                _ = all.wait_for(|stopped| *stopped) => {}
            }
        });
        let mut serving = tokio::spawn(async move { serve.await });
        tokio::select! {
            ended = &mut serving => {
                return ended.unwrap_or_else(|e| Err(std::io::Error::other(e)));
            }
            _ = rebind_signal().notified() => {
                // The old server keeps draining on its own task.
                let _ = stop_this.send(());
                listener = loop {
                    match tokio::net::TcpListener::bind(bind).await {
                        Ok(l) => break l,
                        Err(e) => {
                            eprintln!("[ps5upload-engine] listening again on {bind}: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    }
                };
            }
        }
    }
}

/// The message a caught panic carried (`catch_unwind`'s payload), for an error text.
pub(crate) fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
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

/// A panic the phone app's own hook caught (client/src-tauri/src/mobile_log.rs), for the in-app
/// Log tab and the bug bundle. The engine's hook above is the desktop sidecar's; on a phone the
/// hook belongs to the host.
pub fn record_host_panic(text: String) {
    engine_log::record_quiet("error", text);
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
    let path = format!("{}/api/jobs", host_guard::base_path());
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let timeout = std::time::Duration::from_secs(3);
    let ok = (|| -> std::io::Result<bool> {
        let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
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

pub use mcp::stdio::run_cli as run_mcp_bridge;

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
mod request_log_tests {
    use super::is_constant_poll;

    #[test]
    fn only_the_constant_polls_stay_out_of_the_debug_log() {
        assert!(is_constant_poll("/api/engine-logs"));
        assert!(is_constant_poll("/api/activity"));
        assert!(!is_constant_poll("/api/ps5/status"));
        assert!(!is_constant_poll("/api/activity/other"));
    }
}

#[cfg(test)]
mod rebind_tests {
    use super::*;
    use std::io::{Read, Write};

    async fn get(addr: std::net::SocketAddr) -> std::io::Result<String> {
        tokio::task::spawn_blocking(move || {
            let mut s = std::net::TcpStream::connect(addr)?;
            s.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
            s.write_all(b"GET /ping HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")?;
            let mut out = String::new();
            s.read_to_string(&mut out)?;
            Ok(out)
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_rebind_makes_a_new_listener_on_the_same_port_and_shutdown_still_ends_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/ping", axum::routing::get(|| async { "pong" }));
        let (quit, quit_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            serve_rebindable(&addr.to_string(), listener, app, async move {
                let _ = quit_rx.await;
            })
            .await
        });
        assert!(get(addr).await.unwrap().contains("pong"));

        rebind_in_process();
        // The switch takes a moment: the same port must answer again.
        let mut answered = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if get(addr).await.is_ok_and(|r| r.contains("pong")) {
                answered = true;
                break;
            }
        }
        assert!(answered, "the port never answered after the rebind");

        let _ = quit.send(());
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .expect("shutdown must still end serving")
            .unwrap()
            .unwrap();
    }
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
            list_dir_error_status("connect 192.0.2.99:9120: timed out"),
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
    fn a_console_log_goes_out_without_a_secret() {
        let reply = console_log_reply("sceSomething key=abc123 url=http://u:hunter2@h/x done");
        let text = reply["text"].as_str().unwrap();
        assert!(
            !text.contains("abc123") && !text.contains("hunter2"),
            "{text}"
        );
        assert!(
            text.contains("sceSomething") && text.contains("done"),
            "{text}"
        );
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
    fn a_phone_reaches_only_the_controller_page_and_socket() {
        let c = cfg(&[]);
        assert!(loopback_allows(&c, ip("192.168.1.30"), "/pad"));
        assert!(loopback_allows(&c, ip("192.168.1.30"), "/api/pad/ws"));
        // Not the link itself, the relay, the session, or anything near the names.
        for p in [
            "/api/ps5/rp/pad-link",
            "/api/ps5/rp/pad-link/decide",
            "/api/ps5/rp/pad-link/internet",
            "/api/ps5/rp/pad-relay",
            "/api/ps5/rp/ws",
            "/api/ps5/rp/frame",
            "/api/ps5/rp/input",
            "/api/ps5/rp/input/release",
            "/api/ps5/rp/picture",
            "/api/ps5/rp/live.ts",
            "/api/ps5/rp/live/status",
            "/api/ps5/rp/start",
            "/api/ps5/rp/stop",
            "/api/ps5/rp/passcode",
            "/api/ps5/rp/keyboard",
            "/api/ps5/rp/mic",
            "/api/ps5/vinput",
            "/api/ps5/vinput/report",
            "/api/consoles",
            "/api/consoles/document",
            "/api/settings",
            "/pad/../api/jobs",
            "/padx",
            "/api/pad/ws/x",
        ] {
            assert!(!loopback_allows(&c, ip("192.168.1.30"), p), "{p}");
        }
    }

    #[test]
    fn a_friend_reaches_only_the_join_page_and_socket() {
        let c = cfg(&[]);
        assert!(loopback_allows(&c, ip("192.168.1.30"), "/join"));
        assert!(loopback_allows(&c, ip("192.168.1.30"), "/api/join/ws"));
        assert!(loopback_allows(&c, ip("100.101.2.3"), "/api/join/signal"));
        // The host's side of Play Together stays on this computer.
        for p in [
            "/api/join",
            "/api/join/open",
            "/api/join/close",
            "/api/join/invite",
            "/api/join/decide",
            "/api/join/kick",
            "/api/join/playing",
            "/api/join/settings",
            "/api/join/relay",
            "/api/join/ws/x",
            "/api/join/wsx",
            "/api/join/signal/x",
            "/join/../api/join/open",
            "/joinx",
            "/join/x",
        ] {
            assert!(!loopback_allows(&c, ip("192.168.1.30"), p), "{p}");
        }
        assert!(loopback_allows(&c, ip("127.0.0.1"), "/api/join/open"));
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
    fn browser_guard_lets_a_link_from_another_site_open_a_page() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "ps5upload.home".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        headers.insert("sec-fetch-mode", "navigate".parse().unwrap());
        headers.insert("sec-fetch-dest", "document".parse().unwrap());
        let get = axum::http::Method::GET;
        assert!(host_guard::is_page_navigation(&get, &headers, "/join"));
        // The guard's other half still refuses this shape, for what is not a page.
        assert!(!host_guard::is_page_navigation(&get, &headers, "/api/jobs"));
        assert!(!browser_request_allows(&headers, "/api/jobs"));
    }

    #[test]
    fn browser_guard_compares_servers_not_strings() {
        // A proxy's Host with the port spelled out, or dropped.
        assert!(browser_origin_allows("https://x", "x:443"));
        assert!(browser_origin_allows("https://x:8443", "x"));
        assert!(!browser_origin_allows("https://evil.example", "x"));
        // A proxy that rewrites Host and says what the browser asked for.
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "192.168.1.10:19113".parse().unwrap());
        headers.insert("origin", "https://ps5upload.home".parse().unwrap());
        assert!(!browser_request_allows(&headers, "/api/jobs"));
        headers.insert("x-forwarded-host", "ps5upload.home, inner".parse().unwrap());
        assert!(browser_request_allows(&headers, "/api/jobs"));
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
        // The Collection's covers: refused before, so a game with no online cover showed its
        // initials and the game page a broken image.
        assert!(browser_request_allows(
            &headers,
            "/api/collection/games/PPSA11386/cover"
        ));
        assert!(!browser_request_allows(
            &headers,
            "/api/collection/games/PPSA11386"
        ));
        assert!(!browser_request_allows(
            &headers,
            "/api/collection/games/a/b/cover"
        ));
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

    /// A folder host serving `dir/host/share` and an engine pool it accepts.
    async fn download_host(dir: &std::path::Path) -> ps5upload_ava1::Pool {
        use ava1::host::FolderHost;
        use ava1::keys::Identity;
        use ava1::peers::PeerStore;
        use ava1::server::{self, ServerCtx};
        use ava1::wire::Message;
        let ava = dir.join("engine");
        let key = Identity::load_or_create(&ava.join("identity"))
            .unwrap()
            .public();
        let mut peers = PeerStore::in_memory();
        peers.add(key, "engine").unwrap();
        let rpc: ava1::server::RpcHandler = Box::new(|method, _| {
            if method == ps5upload_ava1::gen::METHOD_NODE_INFO {
                let info = ps5upload_ava1::gen::NodeInfo {
                    version: "test".into(),
                    platform: "rust".into(),
                    name: "host".into(),
                    firmware: None,
                };
                ava1::session::RpcReply {
                    status: ps5upload_ava1::gen::STATUS_OK,
                    body: info.to_bytes().unwrap(),
                }
            } else {
                ava1::session::RpcReply {
                    status: ps5upload_ava1::gen::ERR_UNKNOWN_METHOD,
                    body: Vec::new(),
                }
            }
        });
        let ctx = ServerCtx::new(Identity::generate().unwrap(), "host", peers, rpc).with_jobs(
            Arc::new(FolderHost {
                root: dir.join("host/share"),
                jobs_dir: dir.join("host/jobs"),
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        tokio::spawn(server::serve(l, Arc::new(ctx)));
        ps5upload_ava1::Pool::new(ava).with_addr(addr)
    }

    /// Runs one download job the way `start_ava1_download` does, cancelling it through
    /// `POST /api/jobs/{id}/cancel` first when `cancel` is set, and returns its final state.
    async fn download_job_end(
        pool: Arc<ps5upload_ava1::Pool>,
        out: std::path::PathBuf,
        cancel: bool,
    ) -> JobState {
        let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::default();
        let (events_tx, _rx) = broadcast::channel(16);
        let job_id = Uuid::new_v4();
        let flag = register_transfer_cancel(job_id);
        if cancel {
            let r = cancel_job(Default::default(), Path(job_id.to_string()))
                .await
                .into_response();
            assert_eq!(r.status(), StatusCode::OK);
        }
        let jobs2 = Arc::clone(&jobs);
        tokio::task::spawn_blocking(move || {
            let files = AtomicU64::new(0);
            run_download_job(
                &jobs2,
                &events_tx,
                job_id,
                now_ms(),
                out.to_string_lossy().into_owned(),
                &files,
                || true,
                || {
                    ps5upload_ava1::download::to_local_in(
                        &pool,
                        "console",
                        "f.bin",
                        DownloadKind::File,
                        &out,
                        false,
                        *job_id.as_bytes(),
                        &ps5upload_ava1::download::Counters::default(),
                        Some(flag),
                    )
                },
            )
        })
        .await
        .unwrap();
        let end = jobs.lock().unwrap().get(&job_id).cloned();
        end.expect("the job recorded how it ended")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_download_job_ends_cancelled() {
        let dir = std::env::temp_dir().join(format!("dl-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("host/share")).unwrap();
        std::fs::write(dir.join("host/share/f.bin"), vec![7u8; 1 << 20]).unwrap();
        let pool = Arc::new(download_host(&dir).await);

        // The same job without a cancel completes, so the cancel below is what stopped it.
        let done = dir.join("done");
        std::fs::create_dir_all(&done).unwrap();
        let end = download_job_end(Arc::clone(&pool), done.clone(), false).await;
        assert!(matches!(end, JobState::Done { .. }), "{end:?}");
        assert_eq!(std::fs::read(done.join("f.bin")).unwrap().len(), 1 << 20);

        let cancelled = dir.join("cancelled");
        std::fs::create_dir_all(&cancelled).unwrap();
        match download_job_end(pool, cancelled.clone(), true).await {
            JobState::Failed { error, .. } => assert_eq!(error, "transfer_cancelled"),
            other => panic!("expected a cancelled job, got {other:?}"),
        }
        assert!(!cancelled.join("f.bin").exists(), "nothing landed");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// A pattern naming a folder that also occurs ABOVE the source (`data`, `Games`) must only
    /// match inside it, as the transfer does: it emptied the plan, and the job then read
    /// "N bytes of 0" while the whole tree was sent.
    #[test]
    fn the_folder_plan_matches_excludes_inside_the_source_only() {
        let base = std::env::temp_dir().join(format!("ps5u-plan-{}", std::process::id()));
        let root = base.join("data").join("Games").join("G");
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("eboot.bin"), b"12345").unwrap();
        std::fs::write(root.join("data").join("a.bin"), b"123").unwrap();
        let excludes = vec!["data".to_string()];

        let (total, files) = walk_plan(&root, &excludes);
        assert_eq!(total, 5);
        assert_eq!(
            files
                .iter()
                .map(|f| f.rel_path.as_str())
                .collect::<Vec<_>>(),
            vec!["eboot.bin"]
        );

        let local = ps5upload_core::source_fs::LocalFs;
        let (total, files) = walk_plan_with(&local, &root, &excludes).unwrap();
        assert_eq!(total, 5);
        assert_eq!(files.len(), 1);

        // Nothing named by the pattern: the whole tree, however the source's own path reads.
        let (total, _) = walk_plan(&root, &["Games".to_string()]);
        assert_eq!(total, 8);

        let inventory = ps5upload_core::fs_ops::walk_local_inventory(&root, &excludes).unwrap();
        assert_eq!(inventory.keys().collect::<Vec<_>>(), vec!["eboot.bin"]);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Every transfer reports a resume the same way: zip, 7z and RAR used to say 0 skipped and the
    /// planned count as sent, whatever the console had.
    #[test]
    fn a_finished_job_reads_what_was_skipped_and_sent_from_the_ack() {
        let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::default();
        let (events_tx, _rx) = broadcast::channel(4);
        let job_id = Uuid::new_v4();
        let result = |ack: &str| {
            Ok(ps5upload_core::transfer::TransferResult {
                tx_id_hex: "00".into(),
                files_sent: 0,
                bytes_sent: 10,
                dest: "/data/x".into(),
                commit_ack_body: ack.into(),
            })
        };
        publish_transfer_end(
            &jobs,
            &events_tx,
            job_id,
            1,
            result(r#"{"skipped_files":3,"skipped_bytes":900,"files_sent":2}"#),
            5,
        );
        let g = jobs.lock().unwrap();
        match g.get(&job_id) {
            Some(JobState::Done {
                files_sent,
                skipped_files,
                skipped_bytes,
                ..
            }) => assert_eq!((*files_sent, *skipped_files, *skipped_bytes), (2, 3, 900)),
            other => panic!("{other:?}"),
        }
        drop(g);
        publish_transfer_end(&jobs, &events_tx, job_id, 1, result("{}"), 5);
        match jobs.lock().unwrap().get(&job_id) {
            Some(JobState::Done {
                files_sent,
                skipped_files,
                ..
            }) => assert_eq!((*files_sent, *skipped_files), (5, 0), "the plan's count"),
            other => panic!("{other:?}"),
        };
    }

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
            unescape_names: false,
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
            unescape_names: false,
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
            unescape_names: false,
        };
        let resp = transfer_dir_handler(State(state), Json(req))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    fn running_with_files(n: usize) -> JobState {
        JobState::Running {
            stage: None,
            started_at_ms: 1,
            bytes_sent: 0,
            total_bytes: 0,
            files: (0..n)
                .map(|i| PlannedFile {
                    rel_path: format!("dir/file{i}.bin"),
                    size: 10,
                })
                .collect::<Vec<_>>()
                .into(),
            skipped_files: 0,
            skipped_bytes: 0,
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
        }
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The 500 ms poll used to carry the whole planned file list (megabytes on a big
    /// folder). It carries the count; the list comes from `/files`, once.
    #[tokio::test]
    async fn a_running_job_snapshot_has_a_file_count_and_files_route_has_the_list() {
        let job_id = Uuid::new_v4();
        let (events_tx, _rx) = broadcast::channel(16);
        let state = AppState {
            jobs: Arc::new(Mutex::new(HashMap::from([(job_id, running_with_files(3))]))),
            default_ps5_addr: "127.0.0.1:1".to_string(),
            events_tx,
        };
        let snap = body_json(
            get_job(
                State(state.clone()),
                Default::default(),
                Path(job_id.to_string()),
            )
            .await
            .into_response(),
        )
        .await;
        assert_eq!(snap["status"], "running");
        assert_eq!(snap["files_count"], 3);
        assert!(snap.get("files").is_none(), "full list in the poll: {snap}");

        let no_paging = Query(ListJobsQuery {
            since_seq: None,
            limit: None,
        });
        let listed = body_json(
            list_jobs(State(state.clone()), Default::default(), no_paging)
                .await
                .into_response(),
        )
        .await;
        assert!(listed[0]["job"].get("files").is_none());
        assert_eq!(listed[0]["job"]["files_count"], 3);

        let files = body_json(
            get_job_files(State(state.clone()), Path(job_id.to_string()))
                .await
                .into_response(),
        )
        .await;
        let list = files["files"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0]["rel_path"], "dir/file0.bin");
        assert_eq!(list[0]["size"], 10);

        let missing = get_job_files(State(state), Path(Uuid::new_v4().to_string()))
            .await
            .into_response();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    /// An event carries the count too, and nothing is built when no one listens.
    /// The progress ticker never broadcasts "running" after the job's terminal event.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_ticker_never_sends_running_after_the_end() {
        for _ in 0..5 {
            let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::new(Mutex::new(HashMap::new()));
            let (events_tx, mut rx) = broadcast::channel(4096);
            let job_id = Uuid::new_v4();
            set_job(&jobs, &events_tx, job_id, running_with_files(0));
            let progress = Arc::new(AtomicU64::new(0));
            let stop = spawn_progress_ticker(
                jobs.clone(),
                events_tx.clone(),
                job_id,
                TickerContext {
                    started_at_ms: 1,
                    total_bytes: 100,
                    dynamic_total_bytes: None,
                    skipped_files: 0,
                    skipped_bytes: 0,
                },
                progress.clone(),
                Arc::default(),
                Arc::default(),
                Arc::default(),
            );
            let bump = progress.clone();
            let bumper = std::thread::spawn(move || {
                for i in 0..2000 {
                    bump.store(i, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_micros(300));
                }
            });
            tokio::time::sleep(Duration::from_millis(410)).await;
            set_job(
                &jobs,
                &events_tx,
                job_id,
                JobState::Failed {
                    started_at_ms: 1,
                    completed_at_ms: 2,
                    elapsed_ms: 1,
                    error: "x".into(),
                    error_reason: None,
                    error_detail: None,
                    error_console: None,
                    error_space: None,
                },
            );
            tokio::time::sleep(Duration::from_millis(450)).await;
            stop.store(true, Ordering::Relaxed);
            bumper.join().unwrap();
            let mut ended = false;
            while let Ok(m) = rx.try_recv() {
                let v: serde_json::Value = serde_json::from_str(&m).unwrap();
                match v["job"]["status"].as_str() {
                    Some("failed") => ended = true,
                    Some("running") => assert!(!ended, "running after the end"),
                    _ => {}
                }
            }
            assert!(ended);
        }
    }

    #[test]
    fn set_job_events_carry_the_count_only() {
        let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, mut rx) = broadcast::channel(16);
        let job_id = Uuid::new_v4();
        set_job(&jobs, &events_tx, job_id, running_with_files(2));
        let msg: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(msg["job"]["files_count"], 2);
        assert!(msg["job"].get("files").is_none());
        drop(rx);
        // No receiver: still stored, nothing to send to.
        set_job(&jobs, &events_tx, job_id, running_with_files(4));
        assert!(matches!(
            jobs.lock().unwrap().get(&job_id),
            Some(JobState::Running { files, .. }) if files.len() == 4
        ));
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
                files: Default::default(),
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
        n.bottleneck.store(ps5upload_ava1::gen::BN_DISK, Relaxed);
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
        assert!(v.get("verifying").is_none());
        // #783: the console reads the last large files back
        n.verifying.store(true, Relaxed);
        let v = merge_live_notes(Some(&n), running.clone());
        assert_eq!(v["verifying"], true);
        n.settling.store(true, Relaxed);
        n.unswept.store(12, Relaxed);
        n.unswept_peak.store(50, Relaxed);
        let v = merge_live_notes(Some(&n), running);
        assert_eq!(v["settling"], true);
        assert!(v.get("verifying").is_none(), "never both lines");
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
    fn a_resumed_running_snapshot_says_what_the_console_already_had() {
        use ps5upload_core::transfer::LiveNotes;
        use std::sync::atomic::Ordering::Relaxed;
        let n = LiveNotes::default();
        let running = serde_json::json!({"status": "running", "bytes_sent": 70});
        // A fresh upload: no note.
        assert!(merge_live_notes(Some(&n), running.clone())
            .get("resumed_bytes")
            .is_none());
        n.resumed_bytes.store(60, Relaxed);
        let v = merge_live_notes(Some(&n), running);
        assert_eq!(v["resumed_bytes"].as_u64(), Some(60));
        let done = serde_json::json!({"status": "done"});
        assert_eq!(merge_live_notes(Some(&n), done.clone()), done);
    }

    /// With two consoles, a request that names none used to act on the default one with a 200.
    #[tokio::test]
    async fn requests_that_change_a_console_must_name_it() {
        macro_rules! refused {
            ($handler:ident, $body:tt) => {{
                let req = serde_json::from_value(serde_json::json!($body)).unwrap();
                let status = $handler(Json(req)).await.into_response().status();
                assert_eq!(status, StatusCode::BAD_REQUEST, stringify!($handler));
            }};
        }
        refused!(ps5_fs_mkdir, { "path": "/data/x" });
        refused!(ps5_fs_mkdir, { "addr": "", "path": "/data/x" });
        refused!(ps5_fs_mkdir, { "addr": ":9021", "path": "/data/x" });
        let eject =
            serde_json::from_value(serde_json::json!({ "mount_point": "/mnt/usb0" })).unwrap();
        let installs = std::sync::Arc::new(pkg_install::PkgInstallState::default());
        assert_eq!(
            ps5_usb_eject(installs, eject).await.status(),
            StatusCode::BAD_REQUEST
        );
        refused!(ps5_save_close, {});
        refused!(ps5_smp_checkout_finish, {});
        refused!(ps5_smp_image_rw_finish, {});
        refused!(user_create_handler, { "name": "x" });
        for status in [
            activity_reset_handler(Query(AddrQuery { addr: None }))
                .await
                .into_response()
                .status(),
            notif_clear_handler(Query(AddrQuery { addr: None }))
                .await
                .into_response()
                .status(),
        ] {
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
    }

    #[test]
    fn ntp_servers_are_names_or_addresses_only() {
        for good in [
            "pool.ntp.org",
            "time.apple.com",
            "ntp",
            "192.168.1.1",
            "fe80::1",
        ] {
            assert!(valid_ntp_server(good), "{good}");
        }
        for bad in [
            "",
            "http://pool.ntp.org",
            "pool.ntp.org/x",
            "pool.ntp.org:123",
            "a b",
            "-bad.example",
            "bad..example",
            "[::1]",
        ] {
            assert!(!valid_ntp_server(bad), "{bad}");
        }
    }

    #[test]
    fn test_only_routes_answer_only_when_debug_is_on() {
        assert!(!debug_routes_allowed(None));
        assert!(!debug_routes_allowed(Some("0")));
        assert!(debug_routes_allowed(Some("1")));
        assert_eq!(no_such_route().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn a_failed_full_sensor_read_still_shows_the_temperatures() {
        let temps = || {
            Ok(HwTemps {
                cpu_temp: 41,
                ..Default::default()
            })
        };
        // The full read works: nothing else runs, nothing is marked.
        let t = hw_temps_with_fallback(temps, || panic!("no second read"), true).unwrap();
        assert_eq!(t.extended_error, None);
        // It fails: the temperatures, and why the rest is missing.
        let t =
            hw_temps_with_fallback(|| Err(anyhow::anyhow!("mgmt timed out")), temps, true).unwrap();
        assert_eq!(t.cpu_temp, 41);
        assert_eq!(t.extended_error.as_deref(), Some("mgmt timed out"));
        // Nothing answers: the first error, not a second one about the fallback.
        let e = hw_temps_with_fallback(
            || Err(anyhow::anyhow!("refused")),
            || Err(anyhow::anyhow!("also refused")),
            true,
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "refused");
        // The basic read was already what was asked for: no point asking twice.
        let e = hw_temps_with_fallback(
            || Err(anyhow::anyhow!("refused")),
            || panic!("no second read"),
            false,
        )
        .unwrap_err();
        assert_eq!(e.to_string(), "refused");
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

    /// A destructive request that names no console is refused, never sent to the default one.
    #[test]
    fn a_destructive_request_must_name_its_console() {
        assert_eq!(
            required_console_addr(Some(" 10.0.0.7:1234 ".into())),
            Some("10.0.0.7".to_string())
        );
        for missing in [
            None,
            Some(String::new()),
            Some("  ".into()),
            Some(":1234".into()),
            Some(" :1234 ".into()),
        ] {
            assert_eq!(required_console_addr(missing), None);
        }
        assert_eq!(missing_console_addr().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_and_power_without_addr_are_400() {
        let (events_tx, _rx) = broadcast::channel(4);
        let state = AppState {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            default_ps5_addr: "127.0.0.1:1".to_string(),
            events_tx,
        };
        let del = ps5_fs_delete(
            State(state.clone()),
            Json(FsPathReq {
                addr: None,
                path: "/data/x".into(),
                op_id: 0,
            }),
        )
        .await
        .into_response();
        assert_eq!(del.status(), StatusCode::BAD_REQUEST);
        let power = ps5_power_control(
            State(state),
            Json(PowerControlReq {
                addr: Some(String::new()),
                action: "reboot".into(),
            }),
        )
        .await
        .into_response();
        assert_eq!(power.status(), StatusCode::BAD_REQUEST);
    }

    /// Failures repeat-collapse per console: one console's outage must not hide another's.
    #[test]
    fn the_failure_key_names_the_console() {
        assert_ne!(
            failure_key("GET", "/api/ps5/status", Some("10.0.0.1")),
            failure_key("GET", "/api/ps5/status", Some("10.0.0.2"))
        );
        assert_eq!(
            failure_key("GET", "/api/jobs", None),
            failure_key("GET", "/api/jobs", Some(""))
        );
    }

    #[test]
    fn a_delete_counts_for_a_game_at_or_inside_the_path_only() {
        let src = "/data/homebrew/Game";
        assert!(removes_source("/data/homebrew/Game", src));
        assert!(removes_source("/data/homebrew/Game/", src));
        assert!(removes_source("/data/homebrew", src));
        assert!(!removes_source("/data/homebrew/Game/file.txt", src));
        assert!(!removes_source("/data/homebrew/Gam", src));
        assert!(!removes_source("/data/homebrew/Game2", src));
        assert!(!removes_source("", src));
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
            files: Default::default(),
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
    use super::{appdb_installed_additions, registered_only_additions};
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

    /// #473: a title past the first 256 app.db rows is still added once the
    /// rows arrive — the window, not the filter, was what hid it.
    #[test]
    fn a_late_sorting_title_past_row_256_is_kept() {
        let mut rows: Vec<(String, String)> = (0..1000)
            .map(|i| (format!("CUSA{i:05}"), format!("Game {i}")))
            .collect();
        rows.insert(0, ("AAAA00001".to_string(), "Early".to_string()));
        rows.push(("ZZZZ99999".to_string(), "Late Title".to_string()));
        let got = appdb_installed_additions(&rows, &set(&["ZZZZ99999"]), &HashSet::new());
        assert_eq!(
            got,
            vec![("ZZZZ99999".to_string(), Some("Late Title".to_string()))]
        );
    }

    /// #473: when app.db could not be read in full, a registered title with its
    /// app folder but no appmeta and no app.db row is a candidate.
    #[test]
    fn an_incomplete_appdb_read_reconciles_registered_titles_with_a_folder() {
        let got = registered_only_additions(
            [
                "PPSA00002",
                "ZZZZ99999",
                "CUSA00001",
                "PPSA00003",
                "NPXS40056",
                "not-an-id",
            ],
            &set(&[
                "ZZZZ99999",
                "CUSA00001",
                "PPSA00002",
                "NPXS40056",
                "not-an-id",
            ]),
            // already listed by the appmeta scan or app.db
            &set(&["CUSA00001"]),
            false,
        );
        // PPSA00003 has no folder; NPXS is a system id, never added this way.
        assert_eq!(got, vec!["PPSA00002".to_string(), "ZZZZ99999".to_string()]);
    }

    /// A complete app.db read that does not list a folder means the folder is not an
    /// installed title (a failed install, a partial uninstall): it is not added.
    #[test]
    fn a_complete_appdb_read_adds_no_folder_it_does_not_list() {
        let got =
            registered_only_additions(["PPSA00002"], &set(&["PPSA00002"]), &HashSet::new(), true);
        assert!(got.is_empty());
    }

    #[test]
    fn drops_the_console_page_tile() {
        let rows = vec![
            ("PSUP00001".to_string(), "PS5Upload".to_string()),
            ("PLDM00001".to_string(), "Payload Manager".to_string()),
        ];
        let got =
            appdb_installed_additions(&rows, &set(&["PSUP00001", "PLDM00001"]), &HashSet::new());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "PLDM00001");
    }

    #[test]
    fn the_tile_id_matches_the_payload_config() {
        let cfg = include_str!("../../../../payload/include/config.h");
        let line = cfg
            .lines()
            .find(|l| l.contains("#define WEBUI_TILE_TITLE_ID"))
            .expect("config.h defines WEBUI_TILE_TITLE_ID");
        assert!(
            line.contains(&format!("\"{}\"", super::CONSOLE_WEBUI_TILE_TITLE_ID)),
            "{line}"
        );
        assert!(super::is_console_webui_tile("PSUP00001"));
        assert!(!super::is_console_webui_tile("PLDM00001"));
    }

    #[test]
    fn skips_titles_the_appmeta_scan_already_found() {
        let rows = vec![("PLDM00001".to_string(), "Payload Manager".to_string())];
        let got = appdb_installed_additions(&rows, &set(&["PLDM00001"]), &set(&["PLDM00001"]));
        assert!(got.is_empty());
    }
}

#[cfg(all(test, not(target_os = "android")))]
mod archive_packages_tests {
    use super::{archive_kind_of, archive_package_entries, ArchiveKind};
    use std::io::Write;

    #[test]
    fn the_archive_kind_follows_the_file_name() {
        assert_eq!(archive_kind_of("/x/Game.ZIP"), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind_of("/x/game.7z"), Some(ArchiveKind::SevenZ));
        assert_eq!(
            archive_kind_of("/x/game.part01.rar"),
            Some(ArchiveKind::Rar)
        );
        assert_eq!(archive_kind_of("/x/game.pkg"), None);
        assert_eq!(archive_kind_of("/x/game.7z.001"), None);
    }

    #[test]
    fn a_zip_lists_only_its_packages_at_any_depth() {
        let dir = std::env::temp_dir().join(format!("ps5upload-arcpkg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("bundle.zip");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&path).expect("create"));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, body) in [
            ("readme.txt", &b"hello"[..]),
            ("game/base.pkg", &b"0123456789"[..]),
            ("game/updates/patch.PKG", &b"abc"[..]),
            ("game/cover.png", &b"png"[..]),
        ] {
            zw.start_file(name, opts).expect("start");
            zw.write_all(body).expect("write");
        }
        zw.finish().expect("finish");

        let mut got = archive_package_entries(path.to_str().expect("utf8"), None).expect("list");
        got.sort();
        assert_eq!(
            got,
            vec![
                ("game/base.pkg".to_string(), 10),
                ("game/updates/patch.PKG".to_string(), 3)
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_no_archive_is_refused_by_name() {
        let err = archive_package_entries("/x/game.pkg", None).expect_err("refused");
        assert!(format!("{err:#}").contains("not a .zip, .7z or .rar"));
    }
}

#[cfg(test)]
mod speed_test_file_tests {
    use super::{speed_test_size_mib, write_speed_test_file};

    #[test]
    fn the_size_is_kept_within_bounds() {
        assert_eq!(speed_test_size_mib(None), 256);
        assert_eq!(speed_test_size_mib(Some(1)), 16);
        assert_eq!(speed_test_size_mib(Some(1_000_000)), 2048);
        assert_eq!(speed_test_size_mib(Some(512)), 512);
    }

    #[test]
    fn the_file_has_the_asked_size_and_does_not_repeat() {
        let dir = std::env::temp_dir().join(format!("ps5upload-speedfile-{}", std::process::id()));
        let path = dir.join("f.bin");
        assert_eq!(
            write_speed_test_file(&path, 2).expect("write"),
            2 * 1024 * 1024
        );
        let data = std::fs::read(&path).expect("read");
        assert_eq!(data.len(), 2 * 1024 * 1024);
        // Not compressible in the cheapest sense: the two megabytes differ, and it is not zeros.
        assert_ne!(data[..1024 * 1024], data[1024 * 1024..]);
        assert!(data.iter().any(|b| *b != 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod module_case_upload_tests {
    use super::module_case_renames;

    #[test]
    fn a_game_folder_uploads_its_modules_under_the_eboot_spelling() {
        let d = std::env::temp_dir().join(format!("ps5u-modcase-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Media/Modules")).unwrap();
        let mut eboot = vec![0u8; 4096];
        eboot.extend_from_slice(
            b"\0%s/Modules/Il2CppUserAssemblies.prx\0Il2cppUserAssemblies.prx\0",
        );
        std::fs::write(d.join("eboot.bin"), &eboot).unwrap();
        std::fs::write(d.join("Media/Modules/Il2cppUserAssemblies.prx"), b"m").unwrap();
        let got = module_case_renames(None, &d, &d.to_string_lossy());
        assert_eq!(
            got,
            vec![(
                "Media/Modules/Il2cppUserAssemblies.prx".to_string(),
                "Media/Modules/Il2CppUserAssemblies.prx".to_string()
            )]
        );
        // Not a game folder: nothing is read, nothing renamed.
        std::fs::remove_file(d.join("eboot.bin")).unwrap();
        assert!(module_case_renames(None, &d, &d.to_string_lossy()).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod avatar_crop_position_tests {
    use super::*;
    #[tokio::test]
    async fn preview_can_frame_the_top_of_a_portrait_instead_of_its_center() {
        use base64::Engine as _;
        // A tiny red/green/blue portrait PNG, supplied by the browser.
        let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAADCAIAAADdv/LVAAAADklEQVR4nGP4zwAEEAIAEfoC/qsosSMAAAAASUVORK5CYII=").unwrap();
        let source = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        );
        let render = |y| {
            serde_json::from_value::<ProfilePreviewReq>(
                serde_json::json!({"image_path":source,"mode":"crop","crop":{"x":50,"y":y}}),
            )
            .unwrap()
        };
        let top = profile_avatar_preview_handler(Json(render(0)))
            .await
            .into_response();
        let center = profile_avatar_preview_handler(Json(render(50)))
            .await
            .into_response();
        assert_eq!(top.status(), StatusCode::OK);
        let top = axum::body::to_bytes(top.into_body(), usize::MAX)
            .await
            .unwrap();
        let center = axum::body::to_bytes(center.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_ne!(top, center, "crop position must change the preview");
    }
}

#[cfg(test)]
mod no_delay_tests {
    use axum::serve::Listener;

    #[tokio::test]
    async fn accepted_connections_send_at_once() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let mut l = super::no_delay(l);
        let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (io, _) = l.accept().await;
        assert!(io.nodelay().unwrap());
    }
}
