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
    json_err, now_ms, register_transfer_cancel, set_job, AppState, JobCreated, JobFailOnDropGuard,
    JobStage, JobState,
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
    /// A game folder, or a `.exfat` / `.ffpkg` / `.ffpfsc` mount image.
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
    /// A PlayGo language code (`fr-FR`, …) the package declares as its only one.
    #[serde(default)]
    language: Option<String>,
}

/// The language a build asked for, trimmed; none when blank.
fn language_of(v: Option<String>) -> Option<String> {
    v.map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

/// Where packages go when the caller does not say: the user's Downloads folder, which is
/// where a package is easiest to find afterwards.
pub(crate) fn default_output_dir() -> PathBuf {
    user_home().join("Downloads").join("fpkgs")
}

/// The user's home folder: `HOME` on Unix, `USERPROFILE` (or `HOMEDRIVE`+`HOMEPATH`) on Windows,
/// where `HOME` is normally unset. Only when none of them resolves does it fall back to the
/// temp folder (#364: on Windows that fallback was the *usual* case).
fn user_home() -> PathBuf {
    home_from(|k| std::env::var_os(k))
}

fn home_from(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    let non_empty = |k: &str| get(k).filter(|v| !v.is_empty());
    if let Some(h) = non_empty("HOME").or_else(|| non_empty("USERPROFILE")) {
        return PathBuf::from(h);
    }
    if let (Some(d), Some(p)) = (non_empty("HOMEDRIVE"), non_empty("HOMEPATH")) {
        let mut h = d;
        h.push(p);
        return PathBuf::from(h);
    }
    std::env::temp_dir()
}

pub(crate) fn resolve_engine_path(raw: &str) -> PathBuf {
    let raw = raw.trim();
    // A saved server's path is read through the server, never this machine's disk.
    if raw.starts_with("remote://") || raw.starts_with("ps5://") {
        return PathBuf::from(raw);
    }
    let expanded = if raw == "~" || raw.starts_with("~/") {
        let home = user_home();
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
        Ok::<_, ps5upload_fpkg::Error>(InspectResponse {
            inspection,
            min_firmware,
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

#[derive(Deserialize)]
pub(crate) struct EstimateReq {
    source: String,
    /// Where the package would go: its drive's write speed is part of the time.
    #[serde(default)]
    output_dir: Option<String>,
}

/// POST /api/fpkg/estimate — package size and time at each compression level, from a sample of
/// the game's blocks. Separate from the inspection so a slow sample (a large game on a slow
/// drive) never holds up, or times out, the check itself.
pub(crate) async fn fpkg_estimate_handler(
    State(_): State<AppState>,
    Json(req): Json<EstimateReq>,
) -> impl IntoResponse {
    let source = resolve_engine_path(&req.source);
    let out = output_dir(req.output_dir.as_deref());
    match tokio::task::spawn_blocking(move || build::estimate(&source, Some(&out))).await {
        Ok(Ok(estimates)) => (StatusCode::OK, Json(estimates)).into_response(),
        Ok(Err(error)) => json_err(StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        Err(join) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the estimate task failed: {join}"),
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
                    // Nobody listening: nothing to build.
                    let state = (events_tx.receiver_count() > 0)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
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

    // The file this build writes: a leftover of an interrupted one goes, and this one is
    // marked as being written until the build ends.
    let stem = req
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| req.content_id.clone().filter(|c| !c.trim().is_empty()))
        .or_else(|| inspection.content_id.clone());
    let partial = stem.map(|stem| Writing::new(clear_stale_partial(&out, &stem)));

    let state_for_job = state.clone();
    let request_source = source_path.clone();
    let ticker = JobTicker(ticker.abort_handle());
    tokio::task::spawn_blocking(move || {
        // A panic in the work below must not leave the job "running" forever with its
        // ticker looping: both guards act on any exit, `fail` unless a result was recorded.
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        sweep_output_leftovers(&out);
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
        request.language = language_of(req.language);
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
        drop(partial);
        if outcome.is_err() {
            sweep_output_leftovers(&out);
        }
        let completed_at_ms = now_ms();
        drop(ticker);
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
                        error_console: None,
                    },
                );
            }
        }
        fail.mark_succeeded();
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
    fn a_build_takes_the_language_it_was_asked_for() {
        let req: BuildReq =
            serde_json::from_str(r#"{"source":"/g","language":" fr-FR "}"#).unwrap();
        assert_eq!(language_of(req.language).as_deref(), Some("fr-FR"));
        let req: BuildReq = serde_json::from_str(r#"{"source":"/g","language":""}"#).unwrap();
        assert_eq!(language_of(req.language), None);
        let req: BuildReq = serde_json::from_str(r#"{"source":"/g"}"#).unwrap();
        assert_eq!(language_of(req.language), None);
    }

    #[test]
    fn windows_home_resolves_from_userprofile() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        // Windows: no HOME.
        assert_eq!(
            home_from(env(&[("USERPROFILE", r"C:\Users\len")])),
            PathBuf::from(r"C:\Users\len")
        );
        assert_eq!(
            home_from(env(&[("HOMEDRIVE", "C:"), ("HOMEPATH", r"\Users\len")])),
            PathBuf::from(r"C:\Users\len")
        );
        // An empty HOME does not win.
        assert_eq!(
            home_from(env(&[("HOME", ""), ("USERPROFILE", "/u")])),
            PathBuf::from("/u")
        );
        assert_eq!(home_from(env(&[])), std::env::temp_dir());
    }

    #[test]
    fn expands_home_and_resolves_relative_output_paths() {
        let home = user_home();
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
                    // Nobody listening: nothing to build.
                    let state = (events_tx.receiver_count() > 0)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
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
    let partial_out = PathBuf::from(format!("{}.partial", output.display()));
    let ticker = JobTicker(ticker.abort_handle());
    tokio::task::spawn_blocking(move || {
        // A panic in the work below must not leave the job "running" forever with its
        // ticker looping: both guards act on any exit, `fail` unless a result was recorded.
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        let writing = Writing::new(partial_out);
        sweep_output_leftovers(&out_dir);
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
        drop(writing);
        if outcome.is_err() {
            sweep_output_leftovers(&out_dir);
        }
        let completed_at_ms = now_ms();
        drop(ticker);
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
                        error_console: None,
                    },
                );
            }
        }
        fail.mark_succeeded();
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
pub(crate) struct ExfatBuildReq {
    /// A game folder on this computer (one with `sce_sys` in it).
    source: String,
    /// Where the image goes; the converter's output folder by default.
    #[serde(default)]
    output_dir: Option<String>,
    /// `exfat` (the default), `ffpkg` (UFS2, what ShadowMountPlus recommends) or `ffpfs`
    /// (PFS, experimental in ShadowMountPlus 1.7).
    #[serde(default)]
    format: Option<String>,
    /// Write the image straight into a `.ffpfsc` container (no uncompressed copy on disk).
    #[serde(default)]
    compress: bool,
    /// Pack the game's files into AMPR LZ4 asset packs first (exFAT only, not with `compress`).
    #[serde(default)]
    ampr_lz4: Option<AmprLz4Req>,
}

#[derive(Deserialize, Default)]
pub(crate) struct AmprLz4Req {
    /// 1 (fastest) to 12 (smallest); 9 when absent.
    #[serde(default)]
    level: Option<u8>,
    /// The block size in KiB, a power of two from 16 to 1024; 64 when absent.
    #[serde(default)]
    block_kib: Option<u32>,
    /// A profile in drakmor's `ampr_pack` TOML format, used in place of the built-in one.
    #[serde(default)]
    profile_toml: Option<String>,
}

impl AmprLz4Req {
    /// The packing options, or why they are unusable.
    fn options(&self) -> Result<crate::image_build::Lz4Packs, String> {
        let level = self.level.unwrap_or(9);
        if !(1..=12).contains(&level) {
            return Err(format!("LZ4 level {level} is not 1 to 12"));
        }
        let block = u64::from(self.block_kib.unwrap_or(64)) * 1024;
        let block_shift =
            ps5upload_fpkg::ampr_pack::config::block_shift(block).map_err(|e| e.to_string())?;
        let profile = match self.profile_toml.as_deref() {
            Some(text) if !text.trim().is_empty() => Some(
                ps5upload_fpkg::ampr_pack::Config::from_toml(text, None)
                    .map_err(|e| format!("the LZ4 profile: {e}"))?,
            ),
            _ => None,
        };
        Ok(crate::image_build::Lz4Packs {
            profile,
            level,
            block_shift,
        })
    }
}

/// POST /api/exfat/build — write a game folder as one `.exfat` or `.ffpkg` image ShadowMountPlus
/// mounts (`format`).
/// Runs as a job like a package build: the ticker publishes bytes written, and the job's
/// cancel stops it between chunks. The image is written as `.partial`, read back through the
/// exFAT reader (every file's path and size must match the folder) and only then renamed.
pub(crate) async fn exfat_build_handler(
    State(state): State<AppState>,
    Json(req): Json<ExfatBuildReq>,
) -> impl IntoResponse {
    use ps5upload_fpkg::source::{FolderSource, SourceTree};
    let source = resolve_engine_path(&req.source);
    if !source.is_dir() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a folder", source.display()),
        )
        .into_response();
    }
    let format = match crate::image_build::ImageFormat::parse(req.format.as_deref()) {
        Ok(f) => f,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let packs = match &req.ampr_lz4 {
        None => None,
        Some(_) if format != crate::image_build::ImageFormat::Exfat || req.compress => {
            return json_err(
                StatusCode::BAD_REQUEST,
                "LZ4 asset packs go into a plain exFAT image: choose exFAT and turn off .ffpfsc \
                 compression",
            )
            .into_response()
        }
        Some(r) => match r.options() {
            Ok(p) => Some(p),
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        },
    };
    let out_dir = output_dir(req.output_dir.as_deref());
    let (out_name, inner_name) = crate::image_build::output_names(&source, format, req.compress);
    let output = out_dir.join(out_name);
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
    let scan_source = source.clone();
    let check_packs = packs.is_some();
    let scanned = tokio::task::spawn_blocking(move || {
        let mut tree = FolderSource::open(&scan_source).map_err(|e| e.to_string())?;
        // Refused here, before a job exists, when the game cannot use the packs.
        if check_packs {
            if let Some(why) = ps5upload_fpkg::ampr_pack::image::refusal(&mut tree) {
                return Err(why);
            }
        }
        Ok(tree)
    })
    .await;
    let tree = match scanned {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
    };
    let total: u64 = tree.files().iter().map(|f| f.size).sum();
    if tree.files().is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} has no files in it", source.display()),
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
            total_bytes: total,
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
    // The build's stage (plan, write or compress, verify) and its own progress.
    let current: Arc<std::sync::Mutex<Option<JobStage>>> = Arc::default();
    let tick_stage = current.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut g = jobs.lock().unwrap_or_else(|e| e.into_inner());
            match g.get_mut(&job_id) {
                Some(JobState::Running {
                    bytes_sent, stage, ..
                }) => {
                    *bytes_sent = tick_bytes.load(Ordering::Relaxed);
                    *stage = tick_stage.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    // Nobody listening: nothing to build.
                    let state = (events_tx.receiver_count() > 0)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
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
    let partial_out = PathBuf::from(format!("{}.partial", output.display()));
    let ticker = JobTicker(ticker.abort_handle());
    tokio::task::spawn_blocking(move || {
        // A panic in the work below must not leave the job "running" forever with its
        // ticker looping: both guards act on any exit, `fail` unless a result was recorded.
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        let writing = Writing::new(partial_out);
        let spooling = Writing::new(crate::image_build::lz4_spool(&output));
        sweep_output_leftovers(&out_dir);
        let mut tree = tree;
        // With LZ4 packs the build packs first; the stage list says so.
        let stages: &[&str] = if packs.is_some() {
            &["pack", "plan", "write", "verify"]
        } else {
            &["plan", "write", "verify"]
        };
        let mut report = |id: &str, done: u64, total: u64| {
            if id != "verify" {
                bytes.store(done, Ordering::Relaxed);
            }
            let index = stages
                .iter()
                .position(|s| *s == id || (id == "compress" && *s == "write"))
                .unwrap_or(0) as u32;
            *current.lock().unwrap_or_else(|e| e.into_inner()) = Some(JobStage {
                id: id.to_string(),
                index,
                count: stages.len() as u32,
                done,
                total,
            });
        };
        let outcome = match &packs {
            Some(packs) => crate::image_build::build_packed(
                format,
                &mut tree,
                &output,
                packs,
                &cancel,
                &mut report,
            ),
            None => crate::image_build::build(
                format,
                inner_name.as_deref(),
                &mut tree,
                &output,
                &cancel,
                &mut report,
            ),
        };
        drop(writing);
        drop(spooling);
        if outcome.is_err() {
            sweep_output_leftovers(&out_dir);
        }
        let completed_at_ms = now_ms();
        drop(ticker);
        match outcome {
            Ok(built) => {
                built_packages()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(output.clone());
                crate::engine_log::record(
                    "info",
                    format!(
                        "image: {} -> {} ({} files, {} bytes, {}, read back)",
                        source.display(),
                        output.display(),
                        built.files,
                        built.image_bytes,
                        built.detail
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
                        tx_id_hex: String::new(),
                        bytes_sent: built.image_bytes,
                        dest: output.display().to_string(),
                        files_sent: built.files,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(error) => {
                crate::engine_log::record("warn", format!("image: build failed: {error}"));
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error,
                        error_reason: None,
                        error_detail: None,
                        error_console: None,
                    },
                );
            }
        }
        fail.mark_succeeded();
    });

    (
        StatusCode::ACCEPTED,
        Json(JobCreated {
            job_id: job_id.to_string(),
        }),
    )
        .into_response()
}

/// Stops a job's progress ticker when dropped: when the work ends, a panic included.
struct JobTicker(tokio::task::AbortHandle);

impl Drop for JobTicker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// An unpack's folder while it is still being written: dropped with a path, it removes the
/// folder and its place among the active extracts. Cleared (`None`) once the game is found.
struct Unpacking(Option<PathBuf>);

impl Drop for Unpacking {
    fn drop(&mut self) {
        if let Some(dest) = self.0.take() {
            let _ = std::fs::remove_dir_all(&dest);
            active_extracts()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&dest);
        }
    }
}

/// Package files being written by a build in this engine (`<stem>.pkg.partial`).
fn active_partials() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// A `.pkg.partial` left by a build that is not running any more (the app closed, the engine
/// died) is removed, so the rebuild the queue starts is not refused as "output already exists".
/// One this engine is writing is never touched. Returns the partial's path.
fn clear_stale_partial(out: &Path, stem: &str) -> PathBuf {
    let partial = out.join(format!("{stem}.pkg.partial"));
    let writing = active_partials()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&partial);
    if !writing && partial.is_file() {
        let _ = std::fs::remove_file(&partial);
    }
    partial
}

/// Marks a file as being written by a running build, for as long as it lives: the sweep below
/// leaves it alone.
pub(crate) struct Writing(PathBuf);

impl Writing {
    pub(crate) fn new(path: PathBuf) -> Writing {
        active_partials()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.clone());
        Writing(path)
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        active_partials()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// The in-progress files our builds write: `<name>.<kind>.partial`. Nothing else is touched,
/// since a compression writes next to the user's own image (a browser's `.partial` stays).
fn is_our_partial(name: &str) -> bool {
    [
        ".pkg.partial",
        ".ffpfsc.partial",
        ".ffpfs.partial",
        ".exfat.partial",
        ".ffpkg.partial",
    ]
    .iter()
    .any(|ext| name.to_ascii_lowercase().ends_with(ext))
}

/// Removes a file, trying again for a few seconds while something holds it: on Windows an
/// antivirus or the indexer often has a just-written file open, and one failed attempt left a
/// failed build's whole image on the drive (#432).
fn remove_file_retrying(path: &Path) {
    for attempt in 0..20 {
        match std::fs::remove_file(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == 19 => {
                crate::engine_log::record(
                    "warn",
                    format!("could not remove {}: {e}", path.display()),
                );
            }
            Err(_) => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

/// What failed or interrupted conversions left in `out` and no running one is using: build
/// `.partial` files and archive unpack folders. Run before a conversion writes there, and
/// after one fails (#432: they took up the system drive with nothing to show where).
pub(crate) fn sweep_output_leftovers(out: &Path) {
    remove_stale_extracts(out);
    let Ok(entries) = std::fs::read_dir(out) else {
        return;
    };
    let (stale, spools): (Vec<PathBuf>, Vec<PathBuf>) = {
        let active = active_partials().lock().unwrap_or_else(|e| e.into_inner());
        entries
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                match e.file_type() {
                    Ok(t) if t.is_file() => is_our_partial(&name),
                    // An LZ4 image build's pack volumes, `<name>.exfat.lz4spool/`.
                    Ok(t) if t.is_dir() => name.ends_with(".exfat.lz4spool"),
                    _ => false,
                }
            })
            .map(|e| e.path())
            .filter(|p| !active.contains(p))
            .partition(|p| p.is_file())
    };
    for p in stale {
        remove_file_retrying(&p);
    }
    for p in spools {
        let _ = std::fs::remove_dir_all(&p);
    }
}

/// Prefix of the folders archives are unpacked into, inside the output folder.
const EXTRACT_PREFIX: &str = ".ps5upload-extract-";

/// Unpack folders this engine is still using (a build reads from one until it is cleaned).
fn active_extracts() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Unpacking needs room for the files and, after them, for a package about as large on the
/// same drive; the 1% is the margin the build's own check keeps.
fn extract_shortfall(free: u64, unpacked: u64) -> Option<String> {
    let needed = unpacked.saturating_mul(2).saturating_add(unpacked / 100);
    (free < needed).then(|| {
        format!(
            "only {:.1} GiB free in the output folder; unpacking this archive and building its package needs about {:.1} GiB",
            free as f64 / (1u64 << 30) as f64,
            needed as f64 / (1u64 << 30) as f64,
        )
    })
}

/// The unpack folder `path` is in (it or an ancestor named `.ps5upload-extract-*`), or None:
/// cleanup never deletes anything else.
fn extract_root_of(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| {
            a.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(EXTRACT_PREFIX) && n.len() > EXTRACT_PREFIX.len())
        })
        .map(Path::to_path_buf)
}

/// Unpack folders left in `out` by an earlier run (a crash, a closed app), not in use now.
fn remove_stale_extracts(out: &Path) {
    let Ok(entries) = std::fs::read_dir(out) else {
        return;
    };
    let active = active_extracts().lock().unwrap_or_else(|e| e.into_inner());
    for e in entries.flatten() {
        let p = e.path();
        let stale = e
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(EXTRACT_PREFIX))
            && e.file_type().is_ok_and(|t| t.is_dir())
            && !active.contains(&p);
        if stale {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct ExtractReq {
    /// A `.zip`, `.7z` or `.rar` (the first volume of a multi-part set).
    source: String,
    #[serde(default)]
    output_dir: Option<String>,
    /// RAR only; never logged.
    #[serde(default)]
    password: Option<String>,
}

/// POST /api/fpkg/extract — unpack a game archive into the output folder, as a job. Done's
/// `dest` is the game found inside (a folder or an image), which the build then reads; the
/// unpack folder is removed by `/api/fpkg/extract/cleanup`, on failure, or by a later run.
pub(crate) async fn fpkg_extract_handler(
    State(state): State<AppState>,
    Json(req): Json<ExtractReq>,
) -> impl IntoResponse {
    use ps5upload_core::archive_extract;
    let source = resolve_engine_path(&req.source);
    if archive_extract::archive_kind(&source).is_none() || !source.is_file() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a .zip, .7z or .rar file", source.display()),
        )
        .into_response();
    }
    let out = output_dir(req.output_dir.as_deref());
    if let Err(e) = std::fs::create_dir_all(&out) {
        return json_err(StatusCode::BAD_REQUEST, format!("{}: {e}", out.display()))
            .into_response();
    }
    let password = req.password.filter(|p| !p.is_empty());
    let size = {
        let (source, password, out) = (source.clone(), password.clone(), out.clone());
        tokio::task::spawn_blocking(move || {
            sweep_output_leftovers(&out);
            archive_extract::unpacked_size(&source, password.as_deref())
        })
        .await
    };
    let total = match size {
        Ok(Ok(total)) => total,
        Ok(Err(e)) => return json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
        Err(join) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the archive listing failed: {join}"),
            )
            .into_response()
        }
    };
    if let Some(message) = build::free_bytes(&out).and_then(|free| extract_shortfall(free, total)) {
        return json_err(StatusCode::BAD_REQUEST, message).into_response();
    }

    let job_id = Uuid::new_v4();
    let dest = out.join(format!("{EXTRACT_PREFIX}{job_id}"));
    active_extracts()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(dest.clone());
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
            total_bytes: total,
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
                    // Nobody listening: nothing to build.
                    let state = (events_tx.receiver_count() > 0)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
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
    let ticker = JobTicker(ticker.abort_handle());
    tokio::task::spawn_blocking(move || {
        // A panic in the work below must not leave the job "running" forever with its
        // ticker looping: both guards act on any exit, `fail` unless a result was recorded.
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        // Until the game is found, the folder is this job's to remove (a panic included).
        let mut unpacking = Unpacking(Some(dest.clone()));
        let mut progress = |done: u64, _total: u64| bytes.store(done, Ordering::Relaxed);
        let outcome =
            archive_extract::extract(&source, &dest, password.as_deref(), &mut progress, &cancel)
                .and_then(|()| archive_extract::find_game(&dest));
        let completed_at_ms = now_ms();
        drop(ticker);
        match outcome {
            Ok(game) => {
                unpacking.0 = None;
                crate::engine_log::record(
                    "info",
                    format!(
                        "fpkg: unpacked {} ({} bytes); the game is {}",
                        source.display(),
                        total,
                        game.display()
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
                        tx_id_hex: String::new(),
                        bytes_sent: total,
                        dest: game.display().to_string(),
                        files_sent: 0,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(error) => {
                drop(unpacking);
                crate::engine_log::record("warn", format!("fpkg: unpack failed: {error:#}"));
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: format!("{error:#}"),
                        error_reason: None,
                        error_detail: None,
                        error_console: None,
                    },
                );
            }
        }
        fail.mark_succeeded();
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
pub(crate) struct ExtractCleanupReq {
    /// The game path an unpack reported, or its unpack folder.
    path: String,
}

/// POST /api/fpkg/extract/cleanup — remove an unpack folder once its build is over.
pub(crate) async fn fpkg_extract_cleanup_handler(
    Json(req): Json<ExtractCleanupReq>,
) -> impl IntoResponse {
    let path = resolve_engine_path(&req.path);
    let Some(root) = extract_root_of(&path) else {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not an unpack folder", path.display()),
        )
        .into_response();
    };
    let removed = tokio::task::spawn_blocking(move || {
        active_extracts()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&root);
        match std::fs::remove_dir_all(&root) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", root.display())),
        }
    })
    .await;
    match removed {
        Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
        Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(join) => json_err(StatusCode::INTERNAL_SERVER_ERROR, join.to_string()).into_response(),
    }
}

#[cfg(test)]
mod extract_tests {
    use super::*;

    #[test]
    fn a_leftover_partial_goes_but_one_being_written_stays() {
        let out = std::env::temp_dir().join(format!("ps5upload-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("A.pkg.partial"), b"x").unwrap();
        std::fs::write(out.join("B.pkg.partial"), b"x").unwrap();
        active_partials()
            .lock()
            .unwrap()
            .insert(out.join("B.pkg.partial"));
        clear_stale_partial(&out, "A");
        clear_stale_partial(&out, "B");
        assert!(!out.join("A.pkg.partial").exists());
        assert!(out.join("B.pkg.partial").exists());
        active_partials()
            .lock()
            .unwrap()
            .remove(&out.join("B.pkg.partial"));
    }

    #[test]
    fn space_for_the_files_and_then_the_package() {
        let gib = 1u64 << 30;
        assert!(extract_shortfall(21 * gib, 10 * gib).is_none());
        let m = extract_shortfall(15 * gib, 10 * gib).unwrap();
        assert!(m.contains("15.0 GiB") && m.contains("20.1 GiB"), "{m}");
    }

    #[test]
    fn cleanup_only_reaches_an_unpack_folder() {
        let root = Path::new("/out/.ps5upload-extract-abc");
        assert_eq!(
            extract_root_of(&root.join("My Game/eboot.bin")).as_deref(),
            Some(root)
        );
        assert_eq!(extract_root_of(root).as_deref(), Some(root));
        assert!(extract_root_of(Path::new("/out/games/x")).is_none());
        assert!(extract_root_of(Path::new("/out/.ps5upload-extract-")).is_none());
    }

    #[test]
    fn a_failed_or_interrupted_conversion_leaves_nothing_behind() {
        // #432: a failed image build kept its `.partial` (as large as the image) in the output
        // folder, and an interrupted one was never cleared.
        let out = std::env::temp_dir().join(format!("ps5upload-leftovers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(out.join(format!("{EXTRACT_PREFIX}old"))).unwrap();
        for n in [
            "A.ffpfsc.partial",
            "B.exfat.partial",
            "C.pkg.partial",
            "D.ffpkg.partial",
        ] {
            std::fs::write(out.join(n), b"x").unwrap();
        }
        // Not ours: a browser download and the user's own files stay.
        std::fs::write(out.join("movie.mkv.partial"), b"x").unwrap();
        std::fs::write(out.join("Game.ffpfsc"), b"x").unwrap();
        // One a running build is writing stays too.
        let writing = Writing::new(out.join("D.ffpkg.partial"));
        sweep_output_leftovers(&out);
        let left: std::collections::BTreeSet<String> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            ["D.ffpkg.partial", "Game.ffpfsc", "movie.mkv.partial"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        drop(writing);
        sweep_output_leftovers(&out);
        assert!(!out.join("D.ffpkg.partial").exists());

        // An LZ4 build's pack spool goes too, unless its build is running.
        std::fs::create_dir_all(out.join("E.exfat.lz4spool")).unwrap();
        std::fs::write(out.join("E.exfat.lz4spool/ampr_assets-000.pak"), b"x").unwrap();
        std::fs::create_dir_all(out.join("F.exfat.lz4spool")).unwrap();
        let spooling = Writing::new(out.join("F.exfat.lz4spool"));
        sweep_output_leftovers(&out);
        assert!(!out.join("E.exfat.lz4spool").exists());
        assert!(out.join("F.exfat.lz4spool").exists());
        drop(spooling);
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn lz4_options_are_checked_before_a_job_starts() {
        let ok = AmprLz4Req::default().options().unwrap();
        assert_eq!((ok.level, ok.block_shift), (9, 16));
        assert!(ok.profile.is_none());
        let bad = |r: AmprLz4Req| r.options().err().unwrap();
        assert!(bad(AmprLz4Req {
            level: Some(13),
            ..Default::default()
        })
        .contains("13"));
        assert!(bad(AmprLz4Req {
            block_kib: Some(48),
            ..Default::default()
        })
        .contains("power of two"));
        assert!(bad(AmprLz4Req {
            profile_toml: Some("[pack]\ncompression_level = 0".into()),
            ..Default::default()
        })
        .starts_with("the LZ4 profile"));
        let custom = AmprLz4Req {
            profile_toml: Some("[[rule]]\naction = \"store\"\ninclude = [\"d/**\"]".into()),
            ..Default::default()
        }
        .options()
        .unwrap();
        assert_eq!(custom.profile.unwrap().rules.len(), 1);
    }

    #[test]
    fn stale_unpacks_go_but_active_ones_stay() {
        let out = std::env::temp_dir().join(format!("ps5upload-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let (old, live, keep) = (
            out.join(".ps5upload-extract-old"),
            out.join(".ps5upload-extract-live"),
            out.join("game.pkg"),
        );
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(&keep, b"x").unwrap();
        active_extracts().lock().unwrap().insert(live.clone());
        remove_stale_extracts(&out);
        assert!(!old.exists());
        assert!(live.exists() && keep.exists());
        active_extracts().lock().unwrap().remove(&live);
    }
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

#[cfg(test)]
mod panic_tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A panic in a job's blocking work (as the build, compress, exFAT and unpack jobs run it)
    /// fails the job, stops its ticker and gives up the unpack's folder and active-set entry.
    #[tokio::test]
    async fn a_panicking_job_fails_stops_its_ticker_and_frees_its_folder() {
        let jobs: Arc<Mutex<HashMap<Uuid, JobState>>> = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _rx) = tokio::sync::broadcast::channel(16);
        let job_id = Uuid::new_v4();
        let started_at_ms = now_ms();
        set_job(
            &jobs,
            &events_tx,
            job_id,
            JobState::Running {
                stage: None,
                started_at_ms,
                bytes_sent: 0,
                total_bytes: 1,
                files: Vec::new(),
                skipped_files: 0,
                skipped_bytes: 0,
                files_processing: 0,
                files_finalized: 0,
                files_finalizing_total: 0,
                bytes_finalized: 0,
            },
        );
        let dest = std::env::temp_dir().join(format!("fpkg-panic-{job_id}"));
        std::fs::create_dir_all(&dest).unwrap();
        active_extracts().lock().unwrap().insert(dest.clone());
        let spinning = tokio::spawn(async {
            loop {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let ticker = JobTicker(spinning.abort_handle());
        let (j, e, d) = (jobs.clone(), events_tx.clone(), dest.clone());
        let joined = tokio::task::spawn_blocking(move || {
            let mut fail = JobFailOnDropGuard::new(j, e, job_id, started_at_ms);
            let _unpacking = Unpacking(Some(d));
            let _ticker = ticker;
            if job_id != Uuid::nil() {
                panic!("injected");
            }
            fail.mark_succeeded();
        })
        .await;
        assert!(joined.is_err(), "the work panicked");
        assert!(matches!(
            jobs.lock().unwrap().get(&job_id),
            Some(JobState::Failed { .. })
        ));
        let stopped = spinning.await;
        assert!(
            stopped.unwrap_err().is_cancelled(),
            "the ticker was stopped"
        );
        assert!(!dest.exists());
        assert!(!active_extracts().lock().unwrap().contains(&dest));
    }

    /// A finished unpack keeps its folder.
    #[test]
    fn a_found_game_keeps_its_folder() {
        let dest = std::env::temp_dir().join(format!("fpkg-keep-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dest).unwrap();
        let mut unpacking = Unpacking(Some(dest.clone()));
        unpacking.0 = None;
        drop(unpacking);
        assert!(dest.exists());
        let _ = std::fs::remove_dir_all(&dest);
    }
}
