//! The FPKG converter's HTTP surface: look at a source, then build it in the background.
//!
//! Building a game takes minutes to hours, so it runs as a job on the same infrastructure
//! transfers use: the handler returns a job id, a 200 ms ticker publishes byte progress,
//! and `/api/jobs/{id}/cancel` stops it (the flag the build checks per block).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use ps5upload_fpkg::build::{self, BuildControl, BuildRequest};

use crate::{
    json_err, now_ms, register_transfer_cancel, set_job, AppState, JobCreated, JobStage, JobState,
};

/// A build's stages by index, as `build::Stage::index` numbers them.
const STAGES: [build::Stage; 5] = [
    build::Stage::Check,
    build::Stage::Plan,
    build::Stage::Compress,
    build::Stage::Write,
    build::Stage::Verify,
];

#[derive(Deserialize)]
pub(crate) struct InspectReq {
    /// A game folder, or a `.exfat` / `.ffpkg` mount image.
    source: String,
    /// Where the package would go; defaults to `~/Downloads/fpkgs`. Used only to report
    /// the room left.
    #[serde(default)]
    output_dir: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct BuildReq {
    source: String,
    #[serde(default)]
    output_dir: Option<String>,
    /// Overrides the content id `param.json` declares.
    #[serde(default)]
    content_id: Option<String>,
    /// Output file stem; the content id by default.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    passcode: Option<String>,
    /// Rewrites `requiredSystemSoftwareVersion` (a BCD hex word, e.g. `0x0510000000000000` for
    /// 5.10). A console older than the declared minimum refuses the install with `0x80a3000d`.
    #[serde(default)]
    firmware: Option<String>,
    /// `fast`, `balanced` (the default) or `smallest`: how hard the Kraken encoder works.
    #[serde(default)]
    compression: Option<String>,
}

/// Where packages go when the caller does not say: the user's Downloads folder, which is
/// where a package is easiest to find afterwards.
pub(crate) fn default_output_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Downloads").join("fpkgs")
}

fn resolve_engine_path(raw: &str) -> PathBuf {
    let raw = raw.trim();
    let expanded = if raw == "~" || raw.starts_with("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        if raw == "~" {
            home
        } else {
            home.join(&raw[2..])
        }
    } else {
        PathBuf::from(raw)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(expanded)
    }
}

fn output_dir(requested: Option<&str>) -> PathBuf {
    match requested {
        Some(dir) if !dir.trim().is_empty() => resolve_engine_path(dir),
        _ => default_output_dir(),
    }
}

/// What `/api/fpkg/inspect` answers: the inspection, plus the lowest firmware the game can
/// run on when its modules say so (see `fpkg_firmware`).
#[derive(serde::Serialize)]
struct InspectResponse {
    #[serde(flatten)]
    inspection: build::Inspection,
    min_firmware: Option<String>,
    /// Size and time at each compression level; absent when the sample could not be read.
    estimates: Option<build::Estimates>,
}

fn min_firmware_of(source: &Path, declared: Option<&str>) -> Option<String> {
    let mut tree = ps5upload_fpkg::source::open(source).ok()?;
    crate::fpkg_firmware::min_firmware(tree.as_mut(), declared)
}

/// POST /api/fpkg/inspect — what the source is, whether it looks convertible, and what it
/// will cost. Synchronous: a walk is fast even for a 286,000-file mount.
pub(crate) async fn fpkg_inspect_handler(
    State(_): State<AppState>,
    Json(req): Json<InspectReq>,
) -> impl IntoResponse {
    let out = output_dir(req.output_dir.as_deref());
    let source = resolve_engine_path(&req.source)
        .to_string_lossy()
        .into_owned();
    let result = tokio::task::spawn_blocking(move || {
        let inspection = build::inspect(Path::new(&source), &out)?;
        let min_firmware =
            min_firmware_of(Path::new(&source), inspection.required_firmware.as_deref());
        let estimates = build::estimate(Path::new(&source)).ok();
        Ok::<_, ps5upload_fpkg::Error>(InspectResponse {
            inspection,
            min_firmware,
            estimates,
        })
    })
    .await;
    match result {
        Ok(Ok(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(Err(error)) => json_err(StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        Err(join) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the inspection task failed: {join}"),
        )
        .into_response(),
    }
}

/// Packages this engine process built: the only files `/api/fpkg/delete` will remove.
fn built_packages() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Remove a package this engine built, and forget it.
fn delete_built(path: &Path) -> Result<(), String> {
    let mut set = built_packages().lock().unwrap_or_else(|e| e.into_inner());
    if !set.contains(path) {
        return Err("not a package this app built in this session".into());
    }
    std::fs::remove_file(path).map_err(|e| e.to_string())?;
    set.remove(path);
    Ok(())
}

#[derive(Deserialize)]
pub(crate) struct DeleteReq {
    path: String,
}

/// POST /api/fpkg/delete — remove a package this engine built (the Convert screen's Delete
/// package). Anything else is refused.
pub(crate) async fn fpkg_delete_handler(
    State(_): State<AppState>,
    Json(req): Json<DeleteReq>,
) -> impl IntoResponse {
    match delete_built(&resolve_engine_path(&req.path)) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// POST /api/fpkg/build — start a conversion. Returns `{ job_id }` immediately.
pub(crate) async fn fpkg_build_handler(
    State(state): State<AppState>,
    Json(req): Json<BuildReq>,
) -> impl IntoResponse {
    let out = output_dir(req.output_dir.as_deref());
    let source = resolve_engine_path(&req.source)
        .to_string_lossy()
        .into_owned();
    let source_path = PathBuf::from(&source);

    // Look first: a source that cannot convert should fail now, with the reason, rather
    // than as a job the user has to poll to see fail.
    let inspection = {
        let out = out.clone();
        let source_path = source_path.clone();
        match tokio::task::spawn_blocking(move || build::inspect(&source_path, &out)).await {
            Ok(Ok(inspection)) => inspection,
            Ok(Err(error)) => {
                return json_err(StatusCode::BAD_REQUEST, error.to_string()).into_response()
            }
            Err(join) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("the inspection task failed: {join}"),
                )
                .into_response()
            }
        }
    };

    let job_id = Uuid::new_v4();
    let cancel = register_transfer_cancel(job_id);
    let bytes = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(inspection.planned_size));
    // The stage the build is in, by index (`u64::MAX` before the first).
    let stage = Arc::new(AtomicU64::new(u64::MAX));
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: inspection.planned_size,
            files: Vec::new(),
            skipped_files: 0,
            skipped_bytes: 0,
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
        },
    );

    // The ticker: the build's own counters, published on the same cadence as a transfer's.
    let jobs = state.jobs.clone();
    let events_tx = state.events_tx.clone();
    let tick_bytes = bytes.clone();
    let tick_total = total.clone();
    let tick_stage = stage.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
            match g.get_mut(&job_id) {
                Some(JobState::Running {
                    bytes_sent,
                    total_bytes,
                    stage,
                    ..
                }) => {
                    *bytes_sent = tick_bytes.load(Ordering::Relaxed);
                    *total_bytes = tick_total.load(Ordering::Relaxed);
                    let at = tick_stage.load(Ordering::Relaxed);
                    *stage = STAGES.get(at as usize).map(|s| JobStage {
                        id: s.id().to_string(),
                        index: s.index(),
                        count: build::STAGE_COUNT,
                        done: *bytes_sent,
                        total: *total_bytes,
                    });
                    let state = g.get(&job_id).cloned();
                    drop(g);
                    if let Some(state) = state {
                        let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": state });
                        let _ = events_tx.send(msg.to_string());
                    }
                }
                // Done or Failed: stop, so a terminal record is never overwritten.
                _ => break,
            }
        }
    });

    let state_for_job = state.clone();
    let request_source = source_path.clone();
    tokio::task::spawn_blocking(move || {
        let mut request = BuildRequest::new(&request_source, &out);
        request.content_id = req.content_id.filter(|id| !id.trim().is_empty());
        request.file_name = req.name.filter(|name| !name.trim().is_empty());
        if let Some(passcode) = req.passcode.filter(|p| !p.is_empty()) {
            request.passcode = passcode;
        }
        request.firmware = req.firmware.filter(|v| !v.trim().is_empty());
        // Without an explicit one, the package declares the firmware the game runs on.
        if request.firmware.is_none() {
            request.firmware =
                min_firmware_of(&request_source, inspection.required_firmware.as_deref());
        }
        if let Some(level) = req.compression.as_deref().and_then(|v| v.parse().ok()) {
            request.level = level;
        }
        // Each stage reports its own bytes; a new one starts from nothing.
        let mut on_stage = |s: build::Stage| {
            bytes.store(0, Ordering::Relaxed);
            total.store(0, Ordering::Relaxed);
            stage.store(u64::from(s.index()), Ordering::Relaxed);
        };
        let mut control = BuildControl {
            bytes: Some(&mut |done, total_now| {
                bytes.store(done, Ordering::Relaxed);
                total.store(total_now, Ordering::Relaxed);
            }),
            cancel: Some(&cancel),
            stage: Some(&mut on_stage),
        };
        // The build's phase lines go to the engine log, where the Log tab shows them.
        let mut phase = |line: &str| {
            crate::engine_log::record("info", format!("fpkg: {line}"));
        };
        let outcome = build::build_controlled(&request, &mut phase, &mut control);
        let completed_at_ms = now_ms();
        ticker.abort();
        match outcome {
            Ok(report) => {
                built_packages()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(report.path.clone());
                let warnings = report.warnings.len();
                crate::engine_log::record(
                    "info",
                    format!(
                        "fpkg: built {} ({} bytes, {} warnings)",
                        report.path.display(),
                        report.size,
                        warnings
                    ),
                );
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: report.content_id.clone(),
                        shards_sent: 0,
                        bytes_sent: report.size,
                        dest: report.path.display().to_string(),
                        files_sent: inspection.files as u64,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(error) => {
                crate::engine_log::record("warn", format!("fpkg: build failed: {error}"));
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: error.to_string(),
                        error_reason: None,
                        error_detail: None,
                    },
                );
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn expands_home_and_resolves_relative_output_paths() {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        assert_eq!(
            output_dir(Some("~/Downloads/fpkg")),
            home.join("Downloads/fpkg")
        );
        assert_eq!(
            output_dir(Some("generated/fpkg")),
            std::env::current_dir().unwrap().join("generated/fpkg")
        );
        let absolute = std::env::current_dir().unwrap().join("fpkg");
        assert_eq!(output_dir(Some(absolute.to_str().unwrap())), absolute);
    }
}

#[derive(Deserialize)]
pub(crate) struct CompressReq {
    /// A game image: `.exfat` (most compatible) or `.ffpkg`.
    source: String,
    /// Where the `.ffpfsc` goes; the source's own folder by default.
    #[serde(default)]
    output_dir: Option<String>,
    /// zlib level 1–9; the library default when absent.
    #[serde(default)]
    level: Option<u32>,
}

/// POST /api/ffpfsc/compress — compress a game image into a `.ffpfsc` for ShadowMountPlus.
/// Runs as a job like a package build: the ticker publishes bytes read, and the job's cancel
/// stops it between blocks. The output is written as `.partial`, read back and matched
/// against the source, and only then renamed.
pub(crate) async fn ffpfsc_compress_handler(
    State(state): State<AppState>,
    Json(req): Json<CompressReq>,
) -> impl IntoResponse {
    use ps5upload_fpkg::ffpfsc;
    let source = resolve_engine_path(&req.source);
    let size = match std::fs::metadata(&source) {
        Ok(m) if m.is_file() && m.len() > 0 => m.len(),
        Ok(_) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("{} is not a game image file", source.display()),
            )
            .into_response()
        }
        Err(e) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("{}: {e}", source.display()),
            )
            .into_response()
        }
    };
    let out_dir = match req.output_dir.as_deref() {
        Some(dir) if !dir.trim().is_empty() => resolve_engine_path(dir),
        _ => source
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(default_output_dir),
    };
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let output = out_dir.join(format!("{stem}.ffpfsc"));
    if output.exists() {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", output.display()),
        )
        .into_response();
    }
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", out_dir.display()),
        )
        .into_response();
    }

    let job_id = Uuid::new_v4();
    let cancel = register_transfer_cancel(job_id);
    let bytes = Arc::new(AtomicU64::new(0));
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: size,
            files: Vec::new(),
            skipped_files: 0,
            skipped_bytes: 0,
            files_processing: 0,
            files_finalized: 0,
            files_finalizing_total: 0,
            bytes_finalized: 0,
        },
    );
    let jobs = state.jobs.clone();
    let events_tx = state.events_tx.clone();
    let tick_bytes = bytes.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
            match g.get_mut(&job_id) {
                Some(JobState::Running { bytes_sent, .. }) => {
                    *bytes_sent = tick_bytes.load(Ordering::Relaxed);
                    let state = g.get(&job_id).cloned();
                    drop(g);
                    if let Some(state) = state {
                        let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": state });
                        let _ = events_tx.send(msg.to_string());
                    }
                }
                _ => break,
            }
        }
    });

    let state_for_job = state.clone();
    tokio::task::spawn_blocking(move || {
        let mut options = ffpfsc::WrapOptions::default();
        if let Some(level) = req.level {
            options.level = level;
        }
        let mut progress = |done: u64, _total: u64| bytes.store(done, Ordering::Relaxed);
        let mut control = ffpfsc::Control {
            progress: Some(&mut progress),
            cancel: Some(&cancel),
        };
        let outcome = ffpfsc::wrap(&source, &output, &options, &mut control);
        let completed_at_ms = now_ms();
        ticker.abort();
        match outcome {
            Ok(report) => {
                crate::engine_log::record(
                    "info",
                    format!(
                        "ffpfsc: {} -> {} ({} -> {} bytes, {}/{} blocks compressed, verified)",
                        source.display(),
                        report.output.display(),
                        report.raw_size,
                        report.image_size,
                        report.compressed_blocks,
                        report.blocks
                    ),
                );
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        tx_id_hex: report.inner_name.clone(),
                        shards_sent: 0,
                        bytes_sent: report.image_size,
                        dest: report.output.display().to_string(),
                        files_sent: 1,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(error) => {
                crate::engine_log::record("warn", format!("ffpfsc: compression failed: {error}"));
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: error.to_string(),
                        error_reason: None,
                        error_detail: None,
                    },
                );
            }
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod delete_tests {
    use super::*;

    /// Delete package removes only what this engine built, once.
    #[test]
    fn only_a_package_this_engine_built_can_be_deleted() {
        let dir = std::env::temp_dir().join(format!("fpkg-del-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ours = dir.join("ours.pkg");
        let theirs = dir.join("theirs.pkg");
        std::fs::write(&ours, b"x").unwrap();
        std::fs::write(&theirs, b"x").unwrap();
        built_packages().lock().unwrap().insert(ours.clone());
        assert!(delete_built(&theirs).is_err());
        assert!(theirs.exists());
        assert!(delete_built(&ours).is_ok());
        assert!(!ours.exists());
        // Deleted once, forgotten: a second delete is refused rather than hitting a new file.
        std::fs::write(&ours, b"x").unwrap();
        assert!(delete_built(&ours).is_err());
        assert!(ours.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
