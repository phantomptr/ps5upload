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

use crate::{json_err, now_ms, register_transfer_cancel, set_job, AppState, JobCreated, JobState};

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
}

/// Where packages go when the caller does not say: the user's Downloads folder, which is
/// where a package is easiest to find afterwards.
pub(crate) fn default_output_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join("Downloads").join("fpkgs")
}

fn output_dir(requested: Option<&str>) -> PathBuf {
    match requested {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
        _ => default_output_dir(),
    }
}

/// POST /api/fpkg/inspect — what the source is, whether it looks convertible, and what it
/// will cost. Synchronous: a walk is fast even for a 286,000-file mount.
pub(crate) async fn fpkg_inspect_handler(
    State(_): State<AppState>,
    Json(req): Json<InspectReq>,
) -> impl IntoResponse {
    let out = output_dir(req.output_dir.as_deref());
    let source = req.source.trim().to_string();
    let result =
        tokio::task::spawn_blocking(move || build::inspect(Path::new(&source), &out)).await;
    match result {
        Ok(Ok(inspection)) => (StatusCode::OK, Json(inspection)).into_response(),
        Ok(Err(error)) => json_err(StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        Err(join) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the inspection task failed: {join}"),
        )
        .into_response(),
    }
}

/// POST /api/fpkg/build — start a conversion. Returns `{ job_id }` immediately.
pub(crate) async fn fpkg_build_handler(
    State(state): State<AppState>,
    Json(req): Json<BuildReq>,
) -> impl IntoResponse {
    let out = output_dir(req.output_dir.as_deref());
    let source = req.source.trim().to_string();
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
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
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
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
            match g.get_mut(&job_id) {
                Some(JobState::Running {
                    bytes_sent,
                    total_bytes,
                    ..
                }) => {
                    *bytes_sent = tick_bytes.load(Ordering::Relaxed);
                    *total_bytes = tick_total.load(Ordering::Relaxed);
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
        let mut control = BuildControl {
            bytes: Some(&mut |done, total_now| {
                bytes.store(done, Ordering::Relaxed);
                total.store(total_now, Ordering::Relaxed);
            }),
            cancel: Some(&cancel),
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
