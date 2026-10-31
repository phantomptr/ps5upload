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
    /// Read the whole package back to verify it. Off (the default): the structure is checked
    /// in full and a seeded sample of the data blocks.
    #[serde(default)]
    full_verify: bool,
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
        // A game folder on a NAS share the OS mounted: names the share made up would become
        // the package's file names (see smb_fs::samba_renamed_message).
        let dir = Path::new(&source);
        if dir.is_dir() {
            let (_, files) = crate::walk_plan(dir, &[]);
            if let Some(msg) = crate::remote::smb_fs::samba_renamed_message(
                files.iter().map(|f| f.rel_path.as_str()),
                &source,
            ) {
                return Err(ps5upload_fpkg::Error::Format(msg));
            }
        }
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

    // The file this build writes: a leftover of an interrupted one goes, and this one is
    // marked as being written until the build ends. Claimed before the job exists, so a
    // second build of the same package is refused rather than started.
    let stem = req
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| req.content_id.clone().filter(|c| !c.trim().is_empty()))
        .or_else(|| inspection.content_id.clone());
    let partial = match stem.map(|stem| out.join(format!("{stem}.pkg.partial"))) {
        Some(path) => match Writing::claim_off_thread(path.clone()).await {
            Some(w) => Some(w),
            None => return already_building(&path),
        },
        None => None,
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
            files: Default::default(),
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
            sample: (!req.full_verify).then(ps5upload_fpkg::sample::fresh_seed),
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
    /// Decode every block to verify the output; by default a seeded sample (and the table).
    #[serde(default)]
    full_verify: bool,
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
    let size = match tokio::fs::metadata(&source).await {
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
    if tokio::fs::try_exists(&output).await.unwrap_or(false) {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", output.display()),
        )
        .into_response();
    }
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", out_dir.display()),
        )
        .into_response();
    }

    let partial_out = PathBuf::from(format!("{}.partial", output.display()));
    let Some(writing) = Writing::claim_off_thread(partial_out.clone()).await else {
        return already_building(&partial_out);
    };

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
            files: Default::default(),
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
        sweep_output_leftovers(&out_dir);
        let mut options = ffpfsc::WrapOptions::default();
        if let Some(level) = req.level {
            options.level = level;
        }
        options.sample = (!req.full_verify).then(ps5upload_fpkg::sample::fresh_seed);
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
                        "ffpfsc: {} -> {} ({} -> {} bytes, {}/{} blocks compressed, verified; elapsed_ms={} level={} workers={} full_verify={})",
                        source.display(),
                        report.output.display(),
                        report.raw_size,
                        report.image_size,
                        report.compressed_blocks,
                        report.blocks,
                        completed_at_ms.saturating_sub(started_at_ms),
                        options.level,
                        std::thread::available_parallelism().map_or(4, |n| n.get()),
                        options.sample.is_none(),
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
    /// How hard a `.ffpfsc` is compressed: `fast`, `balanced` (the default) or `smallest`.
    #[serde(default)]
    ffpfsc_level: Option<String>,
    /// Pack the game's files into AMPR LZ4 asset packs first (exFAT only, not with `compress`).
    #[serde(default)]
    ampr_lz4: Option<AmprLz4Req>,
    /// How much of a file the read-back check reads per call, in KiB (1024 to 65536); 8192
    /// when absent, which measured fastest.
    #[serde(default)]
    verify_chunk_kib: Option<u32>,
    /// Read every byte of the image back to verify it. Off (the default): names, sizes and
    /// folders are checked in full and a seeded sample of the data.
    #[serde(default)]
    full_verify: bool,
    /// What to call the image instead of the source's own name: a game unpacked from an
    /// archive is named after the archive (its unpack folder's name means nothing).
    #[serde(default)]
    name: Option<String>,
    /// Write an edited COPY of the image `source` (#479): its files with these changes, as an
    /// image of the same kind (format and compression are the source's; `format`/`compress`
    /// are ignored), named `<name>-edited` and put next to it unless `name`/`output_dir` say
    /// otherwise. The source image is only read.
    #[serde(default)]
    edits: Vec<ImageEditReq>,
}

/// One change for an edited copy of an image: `{"op":"delete","path":…}` removes a file or
/// folder; `{"op":"put","path":…,"from":…}` puts a file (or a folder's contents) from this
/// computer at `path`, replacing what is there.
#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum ImageEditReq {
    Delete { path: String },
    Put { path: String, from: String },
}

impl ImageEditReq {
    fn edit(&self) -> ps5upload_fpkg::browse::Edit {
        use ps5upload_fpkg::browse::Edit;
        match self {
            Self::Delete { path } => Edit::Delete { path: path.clone() },
            Self::Put { path, from } => Edit::Put {
                path: path.clone(),
                from: resolve_engine_path(from),
            },
        }
    }
}

/// An edited copy's tree, and the image kind it is written as (the source's).
pub(crate) struct EditedImage {
    pub(crate) tree: Box<dyn ps5upload_fpkg::source::SourceTree>,
    pub(crate) format: crate::image_build::ImageFormat,
    pub(crate) compress: bool,
}

/// Opens the image at `source` with `edits` applied, refusing a package (no image writer).
pub(crate) fn open_edited(source: &Path, edits: &[ImageEditReq]) -> Result<EditedImage, String> {
    use crate::image_build::ImageFormat;
    use ps5upload_fpkg::browse::{self, Edited, Filesystem};
    let opened = browse::open(source).map_err(|e| e.to_string())?;
    let format = match opened.filesystem {
        Filesystem::Exfat => ImageFormat::Exfat,
        Filesystem::Ufs2 => ImageFormat::Ffpkg,
        Filesystem::Pfs => ImageFormat::Ffpfs,
        Filesystem::Pkg => {
            return Err(
                "a package cannot be edited here: unpack it to a folder, change that, and \
                 build a new package with Convert"
                    .into(),
            )
        }
    };
    let edits: Vec<_> = edits.iter().map(ImageEditReq::edit).collect();
    let tree = Edited::new(opened.tree, &edits).map_err(|e| e.to_string())?;
    Ok(EditedImage {
        tree: Box::new(tree),
        format,
        compress: opened.compressed,
    })
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
    /// Put ampr_emu 0.4.2.1 into the image's `fakelib/libSceAmpr.sprx` (the game's own kept
    /// as `.orig`) when the game ships none or one that predates asset packs.
    #[serde(default)]
    install_runtime: bool,
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
    Json(mut req): Json<ExfatBuildReq>,
) -> impl IntoResponse {
    let source = resolve_engine_path(&req.source);
    // A game folder on this computer, on a saved server (`remote://`) or on a console
    // (`ps5://`, read through the helper as a package build reads it): the opener below
    // knows them all. A local path has to be a folder; an image or archive is not written
    // as another image from here.
    let remote = ps5upload_fpkg::remote_source::is_remote(&source);
    let image_file = crate::image_build::is_image_file(&source);
    if !remote && !image_file && !tokio::fs::metadata(&source).await.is_ok_and(|m| m.is_dir()) {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a game folder or image", source.display()),
        )
        .into_response();
    }
    // An edited copy (#479): the image on this computer, opened with the changes applied, is
    // written out again as the same kind of image.
    let edited = if req.edits.is_empty() {
        None
    } else {
        if remote || !image_file {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("{} is not a game image on this computer", source.display()),
            )
            .into_response();
        }
        if req.ampr_lz4.is_some() {
            return json_err(
                StatusCode::BAD_REQUEST,
                "an edited copy is written as the image was: LZ4 asset packs are not made here",
            )
            .into_response();
        }
        let (edit_source, edits) = (source.clone(), std::mem::take(&mut req.edits));
        match tokio::task::spawn_blocking(move || open_edited(&edit_source, &edits)).await {
            Ok(Ok(e)) => Some(e),
            Ok(Err(e)) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
            Err(e) => {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
                    .into_response()
            }
        }
    };
    let format = match &edited {
        Some(e) => e.format,
        None => match crate::image_build::ImageFormat::parse(req.format.as_deref()) {
            Ok(f) => f,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        },
    };
    let compress = edited.as_ref().map_or(req.compress, |e| e.compress);
    let level = match ps5upload_fpkg::ffpfsc_block::Level::parse(req.ffpfsc_level.as_deref()) {
        Ok(l) => l,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    let verify_chunk = match crate::image_build::verify_chunk(req.verify_chunk_kib) {
        Ok(c) => c,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    // A fast read-back also covers UFS2's indirect-block seam.
    let mode = ps5upload_fpkg::sample::VerifyMode::from_full(
        req.full_verify,
        format == crate::image_build::ImageFormat::Ffpkg,
    );
    let packs = match &req.ampr_lz4 {
        None => None,
        Some(_) if format != crate::image_build::ImageFormat::Exfat || compress => {
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
    let named = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => {
            if let Some(why) = crate::folder_build::name_problem(n) {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("the image name {n:?} {why}"),
                )
                .into_response();
            }
            PathBuf::from(n)
        }
        // An edited copy is told from its source by name.
        None if edited.is_some() => {
            PathBuf::from(format!("{}-edited", crate::image_build::stem_of(&source)))
        }
        None => source.clone(),
    };
    // An edited copy goes next to its source unless told otherwise.
    let out_dir = match (&edited, req.output_dir.as_deref().map(str::trim)) {
        (Some(_), None | Some("")) => source
            .parent()
            .map_or_else(default_output_dir, Path::to_path_buf),
        _ => output_dir(req.output_dir.as_deref()),
    };
    let (out_name, inner_name) = crate::image_build::output_names(&named, format, compress);
    let output = out_dir.join(out_name);
    if !remote
        && image_file
        && tokio::fs::canonicalize(&output).await.ok()
            == tokio::fs::canonicalize(&source).await.ok()
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            "the image would be written over its own source: pick another format or output folder",
        )
        .into_response();
    }
    if tokio::fs::try_exists(&output).await.unwrap_or(false) {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", output.display()),
        )
        .into_response();
    }
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", out_dir.display()),
        )
        .into_response();
    }
    let scan_source = source.clone();
    let check_packs = packs.is_some();
    let install_runtime = req.ampr_lz4.as_ref().is_some_and(|r| r.install_runtime);
    let edited_tree = edited.map(|e| e.tree);
    let scanned = tokio::task::spawn_blocking(move || {
        // An edited copy keeps the image's own names: only the user's changes are made.
        if let Some(tree) = edited_tree {
            return Ok(tree);
        }
        // A UFS2 image (.ffpkg) is case-sensitive like /data: give modules the eboot's spelling.
        let (mut tree, renamed) =
            ps5upload_fpkg::module_case::open(&scan_source).map_err(|e| e.to_string())?;
        if !renamed.is_empty() {
            crate::log_info!(
                "image build: renamed to the eboot's spelling: {}",
                ps5upload_fpkg::module_case::describe(&renamed)
            );
        }
        if check_packs && install_runtime {
            // The game's own ampr_emu is missing or too old for packs: the image gets the
            // release one, its own kept beside it. Only for a game that reads through AMPR.
            let ready = ps5upload_fpkg::ampr_pack::image::readiness(&mut *tree);
            match ready.as_ref().map(|r| r.kind) {
                Some("runtime") => {
                    let bytes = crate::ampr_trace::release_runtime()?;
                    tree = Box::new(
                        ps5upload_fpkg::ampr_pack::image::RuntimeInstalled::new(tree, bytes)
                            .map_err(|e| e.to_string())?,
                    );
                }
                Some("ok") => {}
                _ => {
                    return Err(ready
                        .and_then(|r| r.refusal)
                        .unwrap_or_else(|| "the game cannot use asset packs".into()))
                }
            }
        }
        // Refused here, before a job exists, when the game cannot use the packs.
        if check_packs {
            if let Some(why) = ps5upload_fpkg::ampr_pack::image::refusal(&mut *tree) {
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

    let partial_out = PathBuf::from(format!("{}.partial", output.display()));
    let Some(writing) = Writing::claim_off_thread(partial_out.clone()).await else {
        return already_building(&partial_out);
    };
    let spool = crate::image_build::lz4_spool(&output);
    let Some(spooling) = Writing::claim_off_thread(spool.clone()).await else {
        return already_building(&spool);
    };

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
            files: Default::default(),
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
                &mut *tree,
                &output,
                packs,
                mode,
                verify_chunk,
                &cancel,
                &mut report,
            ),
            // A libSceAmpr game gets a fresh ampr_emu.index (#449), as a package does.
            None => match crate::image_build::AmprIndexed::wrap(&mut *tree) {
                Ok(Some(mut indexed)) => crate::image_build::build_at(
                    format,
                    inner_name.as_deref(),
                    level,
                    &mut indexed,
                    &output,
                    mode,
                    verify_chunk,
                    &cancel,
                    &mut report,
                ),
                Ok(None) => crate::image_build::build_at(
                    format,
                    inner_name.as_deref(),
                    level,
                    &mut *tree,
                    &output,
                    mode,
                    verify_chunk,
                    &cancel,
                    &mut report,
                ),
                Err(e) => Err(format!("ampr_emu.index: {e}")),
            },
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

#[derive(Deserialize)]
pub(crate) struct FolderBuildReq {
    /// A game image (`.exfat`, `.ffpkg`/`.ufs2`, `.ffpfs`, `.ffpfsc`) or a debug-key `.pkg`,
    /// on this computer, a saved server (`remote://`) or a console (`ps5://`).
    source: String,
    #[serde(default)]
    output_dir: Option<String>,
    /// The folder's name; the source file's name without its extension when absent.
    #[serde(default)]
    name: Option<String>,
    /// Compare every file by SHA-256. Off (the default): every size, and a seeded sample.
    #[serde(default)]
    full_verify: bool,
    /// Save only this file or folder of the image (#479, the image browser's Save), by its
    /// path inside the image: a folder becomes a folder named after it, a file a file. Its
    /// name is the entry's own unless `name` says otherwise.
    #[serde(default)]
    entry: Option<String>,
}

/// What a folder build reads: the whole image, one folder of it, or one file.
enum Unpacked {
    Tree(Box<dyn ps5upload_fpkg::source::SourceTree>),
    File {
        tree: Box<dyn ps5upload_fpkg::source::SourceTree>,
        path: String,
        size: u64,
    },
}

/// The file or folder `entry` (a normalized path inside it) of the image at `source`, read as
/// the image spells it.
fn open_entry(source: &Path, entry: &str) -> Result<Unpacked, String> {
    use ps5upload_fpkg::browse::{Index, Kind, Subtree};
    let tree = ps5upload_fpkg::source::open(source).map_err(|e| e.to_string())?;
    match Index::new(&*tree).kind(entry) {
        Some(Kind::File(size)) => Ok(Unpacked::File {
            tree,
            path: entry.to_string(),
            size,
        }),
        Some(Kind::Dir) => Ok(Unpacked::Tree(Box::new(
            Subtree::new(tree, entry).map_err(|e| e.to_string())?,
        ))),
        None => Err(format!("{entry} is not in {}", source.display())),
    }
}

/// POST /api/folder/build — unpack a game image or a debug-key package into a plain folder
/// (#469). Runs as a job like an image build: stages `write` and `verify`, the folder written
/// as `<name>.partial/` and renamed only once it reads back. Done's `dest` is the folder.
pub(crate) async fn folder_build_handler(
    State(state): State<AppState>,
    Json(req): Json<FolderBuildReq>,
) -> impl IntoResponse {
    use crate::folder_build;
    let source = resolve_engine_path(&req.source);
    let remote = ps5upload_fpkg::remote_source::is_remote(&source);
    if !remote && tokio::fs::metadata(&source).await.is_ok_and(|m| m.is_dir()) {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is already a folder", source.display()),
        )
        .into_response();
    }
    // On a server or console only the name tells: a folder there has no image extension.
    let named_as_image = if remote {
        crate::image_build::has_image_extension(&source)
    } else {
        crate::image_build::is_image_file(&source)
    };
    if !named_as_image {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a game image or package", source.display()),
        )
        .into_response();
    }
    // One file or folder of the image (the browser's Save), or "" / absent for all of it.
    let entry = match req.entry.as_deref() {
        None => None,
        Some(raw) => match ps5upload_fpkg::browse::normalize(raw) {
            Some(p) if p.is_empty() => None,
            Some(p) => Some(p),
            None => {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("{raw:?} is not a path inside the image"),
                )
                .into_response()
            }
        },
    };
    // The source's stem is made safe already; a given name or an entry's own may not be.
    let (name, check) = match req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => (n.to_string(), true),
        None => match &entry {
            Some(e) => (e.rsplit('/').next().unwrap_or(e).to_string(), true),
            None => (crate::image_build::stem_of(&source), false),
        },
    };
    if let Some(why) = check.then(|| folder_build::name_problem(&name)).flatten() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("the folder name {name:?} {why}"),
        )
        .into_response();
    }
    let out_dir = output_dir(req.output_dir.as_deref());
    let target = out_dir.join(&name);
    if tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", target.display()),
        )
        .into_response();
    }
    // A `<name>.partial` that is not one of ours is the user's: never cleared, never used. A
    // saved file's `.partial` is a file, which cannot be told from a user's: it is refused too.
    let partial = folder_build::partial_of(&target);
    if tokio::fs::try_exists(&partial).await.unwrap_or(false)
        && !folder_build::is_our_partial_dir(&partial)
    {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", partial.display()),
        )
        .into_response();
    }
    if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", out_dir.display()),
        )
        .into_response();
    }
    let scan_source = source.clone();
    let scan_entry = entry.clone();
    let scanned = tokio::task::spawn_blocking(move || {
        if let Some(entry) = scan_entry {
            // A part of the image is saved as the image spells it, as the browser listed it.
            return open_entry(&scan_source, &entry);
        }
        // An image ignores letter case and /data does not: a Unity dump whose
        // Il2cppUserAssemblies.prx the eboot loads as Il2CppUserAssemblies.prx plays from the
        // image and crashes a second in from the folder. The folder takes the eboot's spelling.
        let (tree, renamed) =
            ps5upload_fpkg::module_case::open(&scan_source).map_err(|e| e.to_string())?;
        if !renamed.is_empty() {
            crate::log_info!(
                "folder build: renamed to the eboot's spelling: {}",
                ps5upload_fpkg::module_case::describe(&renamed)
            );
        }
        Ok::<_, String>(Unpacked::Tree(tree))
    })
    .await;
    let unpacked = match scanned {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => {
            return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
    };
    let total: u64 = match &unpacked {
        Unpacked::Tree(tree) => {
            if tree.files().is_empty() {
                let what = match &entry {
                    Some(e) => e.clone(),
                    None => source.display().to_string(),
                };
                return json_err(
                    StatusCode::BAD_REQUEST,
                    format!("{what} has no files in it"),
                )
                .into_response();
            }
            tree.files().iter().map(|f| f.size).sum()
        }
        Unpacked::File { size, .. } => *size,
    };
    if let Some(why) =
        build::free_bytes(&out_dir).and_then(|free| folder_build::shortfall(free, total, &out_dir))
    {
        return json_err(StatusCode::BAD_REQUEST, why).into_response();
    }
    let Some(writing) = Writing::claim_off_thread(partial.clone()).await else {
        return already_building(&partial);
    };
    let mode = ps5upload_fpkg::sample::VerifyMode::from_full(req.full_verify, false);

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
            files: Default::default(),
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
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        // A part of an image is saved wherever the user chose: that folder is theirs, and its
        // leftovers are not swept.
        let sweep = entry.is_none();
        if sweep {
            sweep_output_leftovers(&out_dir);
        }
        const STAGES: [&str; 2] = ["write", "verify"];
        let mut report = |id: &str, done: u64, total: u64| {
            if id == "write" {
                bytes.store(done, Ordering::Relaxed);
            }
            let index = STAGES.iter().position(|s| *s == id).unwrap_or(0) as u32;
            *current.lock().unwrap_or_else(|e| e.into_inner()) = Some(JobStage {
                id: id.to_string(),
                index,
                count: STAGES.len() as u32,
                done,
                total,
            });
        };
        let reopen_source = source.clone();
        let reopen_entry = entry.clone();
        // Verification reads the source afresh by the folder's names, so with the same renames
        // (or, for a part of the image, the same part).
        let reopen = move || match &reopen_entry {
            Some(e) => open_entry(&reopen_source, e).map(|u| match u {
                Unpacked::Tree(t) | Unpacked::File { tree: t, .. } => t,
            }),
            None => ps5upload_fpkg::module_case::open(&reopen_source)
                .map(|(t, _)| t)
                .map_err(|e| e.to_string()),
        };
        let outcome = match unpacked {
            Unpacked::Tree(mut tree) => {
                folder_build::build(&mut *tree, &reopen, &target, mode, &cancel, &mut report)
            }
            Unpacked::File {
                mut tree,
                path,
                size,
            } => folder_build::extract_file(
                &mut *tree,
                &path,
                size,
                &reopen,
                &target,
                &cancel,
                &mut report,
            ),
        };
        drop(writing);
        if outcome.is_err() && sweep {
            sweep_output_leftovers(&out_dir);
        }
        let completed_at_ms = now_ms();
        drop(ticker);
        match outcome {
            Ok(built) => {
                crate::engine_log::record(
                    "info",
                    format!(
                        "folder: {} -> {} ({}, read back)",
                        source.display(),
                        target.display(),
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
                        bytes_sent: built.bytes,
                        dest: target.display().to_string(),
                        files_sent: built.files,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(error) => {
                crate::engine_log::record("warn", format!("folder: build failed: {error}"));
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
            let _ = ps5upload_core::archive_extract::remove_path_retrying(&dest);
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

/// Marks a file as being written by a running build, for as long as it lives: the sweep below
/// leaves it alone, and a second build of the same output is refused.
pub(crate) struct Writing(PathBuf);

impl Writing {
    /// Claims `path` for one build. `None` when another running build already writes it: two
    /// builds of one output would wreck each other's file. A leftover at `path` from a build
    /// that is not running any more (the app closed, the engine died) is removed, so the
    /// rebuild the queue starts is not refused as "output already exists".
    pub(crate) fn claim(path: PathBuf) -> Option<Writing> {
        let mut active = active_partials().lock().unwrap_or_else(|e| e.into_inner());
        if active.contains(&path) {
            return None;
        }
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(&path);
        } else if path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
        active.insert(path.clone());
        Some(Writing(path))
    }

    /// [`Writing::claim`] on a blocking thread: clearing a leftover can mean removing a
    /// whole pack spool.
    pub(crate) async fn claim_off_thread(path: PathBuf) -> Option<Writing> {
        tokio::task::spawn_blocking(move || Writing::claim(path))
            .await
            .ok()
            .flatten()
    }
}

/// The 409 for a build whose output another running build is writing.
fn already_building(path: &Path) -> axum::response::Response {
    json_err(
        StatusCode::CONFLICT,
        format!("{} is already being built", path.display()),
    )
    .into_response()
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

/// A `<name>.partial` with `<name>.partial.json` next to it is a link download in progress
/// (see `remote_download`), kept so it can resume.
fn has_download_sidecar(partial: &Path) -> bool {
    let mut name = partial.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    partial.with_file_name(name).is_file()
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
                    // A link download writes `<name>.pkg.partial` with a `.partial.json`
                    // resume sidecar beside it; that one is resumable and not ours to sweep.
                    Ok(t) if t.is_file() => {
                        is_our_partial(&name) && !has_download_sidecar(&e.path())
                    }
                    // An LZ4 image build's pack volumes, `<name>.exfat.lz4spool/`, and a
                    // folder build's unfinished folder (marked, so a user's own is kept).
                    Ok(t) if t.is_dir() => {
                        name.ends_with(".exfat.lz4spool")
                            || crate::folder_build::is_our_partial_dir(&e.path())
                    }
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
            let _ = ps5upload_core::archive_extract::remove_path_retrying(&p);
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
            files: Default::default(),
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
        match ps5upload_core::archive_extract::remove_path_retrying(&root) {
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

    /// An unpack that fails half way (here a damaged second entry, after the first was written)
    /// leaves nothing on this computer: the files it wrote are closed and the folder removed.
    #[test]
    fn a_failed_unpack_leaves_no_files_behind() {
        use std::io::Write;
        let base = std::env::temp_dir().join(format!("fpkg-failed-unpack-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let zip_path = base.join("game.zip");
        {
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("game/eboot.bin", opts).unwrap();
            zw.write_all(&[b'A'; 4096]).unwrap();
            zw.start_file("game/data.bin", opts).unwrap();
            zw.write_all(&[b'B'; 4096]).unwrap();
            zw.finish().unwrap();
        }
        // Damage the second entry's bytes: its CRC check fails mid-unpack.
        let mut bytes = std::fs::read(&zip_path).unwrap();
        let at = bytes.windows(64).position(|w| w == [b'B'; 64]).unwrap();
        bytes[at + 10] = b'X';
        std::fs::write(&zip_path, bytes).unwrap();

        let dest = base.join(format!("{EXTRACT_PREFIX}test"));
        active_extracts().lock().unwrap().insert(dest.clone());
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let outcome = {
            let _unpacking = Unpacking(Some(dest.clone()));
            let r = ps5upload_core::archive_extract::extract(
                &zip_path,
                &dest,
                None,
                &mut |_, _| {},
                &cancel,
            );
            assert!(
                dest.join("game/eboot.bin").exists(),
                "the first entry was written"
            );
            r
        };
        assert!(outcome.is_err(), "the damaged entry must fail the unpack");
        assert!(!dest.exists(), "the failed unpack's files were left behind");
        assert!(!active_extracts().lock().unwrap().contains(&dest));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_leftover_partial_goes_but_a_second_build_of_the_same_output_is_refused() {
        let out = std::env::temp_dir().join(format!("ps5upload-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("A.pkg.partial"), b"x").unwrap();
        let first = Writing::claim(out.join("A.pkg.partial")).unwrap();
        assert!(!out.join("A.pkg.partial").exists(), "the leftover goes");
        std::fs::write(out.join("A.pkg.partial"), b"being written").unwrap();
        assert!(Writing::claim(out.join("A.pkg.partial")).is_none());
        assert_eq!(
            std::fs::read(out.join("A.pkg.partial")).unwrap(),
            b"being written"
        );
        assert_eq!(
            already_building(&out.join("A.pkg.partial")).status(),
            StatusCode::CONFLICT
        );
        drop(first);
        assert!(Writing::claim(out.join("A.pkg.partial")).is_some());
        let _ = std::fs::remove_dir_all(&out);
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
        std::fs::remove_file(out.join("D.ffpkg.partial")).unwrap();
        let writing = Writing::claim(out.join("D.ffpkg.partial")).unwrap();
        std::fs::write(out.join("D.ffpkg.partial"), b"x").unwrap();
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
        let spooling = Writing::claim(out.join("F.exfat.lz4spool")).unwrap();
        std::fs::create_dir_all(out.join("F.exfat.lz4spool")).unwrap();
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
                files: Default::default(),
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
