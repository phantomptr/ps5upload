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
    json_err, now_ms, register_workshop_job, set_job, AppState, JobCreated, JobFailOnDropGuard,
    JobStage, JobState,
};

/// How long a build request waits for its source to be read (and checked) before it answers.
/// Under the app's 60 s request limit, with room to spare.
const SCAN_ANSWER_WITHIN: Duration = Duration::from_secs(20);

/// Runs `rest`, a build's scan of its source and everything up to its job's start, and answers
/// as it does when it is over within [`SCAN_ANSWER_WITHIN`]: a refusal is still a refusal before
/// any job. A scan that takes longer (a solid `.7z` or `.rar` decoded up to the game's param
/// file, a slow server) used to hold the request until the app gave up on it, while the build
/// started anyway with nobody told; Stop could not reach it either. Now the job is listed at its
/// `check` stage and the request answers with its id: what `rest` ends with becomes the job's
/// (its own start, or a failure with the refusal's words), and Stop sets the flag the scan reads.
async fn answer_or_hand_off(
    state: &AppState,
    job_id: Uuid,
    rest: impl std::future::Future<Output = axum::response::Response> + Send + 'static,
) -> axum::response::Response {
    answer_or_hand_off_within(state, job_id, rest, SCAN_ANSWER_WITHIN).await
}

async fn answer_or_hand_off_within(
    state: &AppState,
    job_id: Uuid,
    rest: impl std::future::Future<Output = axum::response::Response> + Send + 'static,
    within: Duration,
) -> axum::response::Response {
    let mut task = tokio::spawn(rest);
    let failed = |e: tokio::task::JoinError| {
        json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the build stopped unexpectedly: {e}"),
        )
        .into_response()
    };
    let Ok(joined) = tokio::time::timeout(within, &mut task).await else {
        let started_at_ms = now_ms();
        {
            // Only while `rest` has not listed the job itself, which it may have done a moment
            // ago: under the lock, so its own state is never overwritten by this one.
            let mut jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
            if let std::collections::hash_map::Entry::Vacant(slot) = jobs.entry(job_id) {
                let st = slot.insert(JobState::Running {
                    stage: Some(JobStage {
                        id: "check".into(),
                        index: 0,
                        count: 1,
                        done: 0,
                        total: 0,
                    }),
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
                });
                let st = serde_json::json!(st);
                drop(jobs);
                let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": st });
                let _ = state.events_tx.send(msg.to_string());
            }
        }
        let (jobs, events_tx) = (state.jobs.clone(), state.events_tx.clone());
        tokio::spawn(async move {
            let answer = task.await.unwrap_or_else(failed);
            if answer.status().is_success() {
                return;
            }
            let body = axum::body::to_bytes(answer.into_body(), 1 << 20)
                .await
                .unwrap_or_default();
            let error = serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
            let completed_at_ms = now_ms();
            set_job(
                &jobs,
                &events_tx,
                job_id,
                JobState::Failed {
                    started_at_ms,
                    completed_at_ms,
                    elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                    error,
                    error_reason: None,
                    error_detail: None,
                    error_console: None,
                    error_space: None,
                },
            );
        });
        return (
            StatusCode::ACCEPTED,
            Json(JobCreated {
                job_id: job_id.to_string(),
            }),
        )
            .into_response();
    };
    joined.unwrap_or_else(failed)
}

/// A job's overall bytes out of `job_total`, for its `bytes_sent`: each of the `of` stages it goes
/// through an equal share, those before `at` done in full and this one by its own `done` of
/// `total`. Never past `job_total`. The stage's own count is the job's `stage`, which the Convert
/// screen draws; this one is what a job list's bar (Tasks, the API, MCP) shows, so it must not
/// fall back to 0 at each stage or pass its total.
pub(crate) fn overall_progress(job_total: u64, at: u32, of: u32, done: u64, total: u64) -> u64 {
    if of == 0 {
        return 0;
    }
    let within = if total == 0 {
        0.0
    } else {
        done.min(total) as f64 / total as f64
    };
    let share = (f64::from(at.min(of)) + within) / f64::from(of);
    ((job_total as f64 * share) as u64).min(job_total)
}

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
    /// A PS4 update to look at merged into the game `source` (see `ps5upload_fpkg::ps4_merge`):
    /// the answer is the merged game's, or why the two don't go together.
    #[serde(default)]
    update: Option<String>,
}

pub(crate) use ps5upload_convert_core::refusal::{patch_refusal, ps4_refusal};

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
    /// When the package's name is taken in the output folder, name it `<stem> (2).pkg` (and so
    /// on) instead of refusing: Convert's "Save each next to its source", where the folder is
    /// the user's own and may hold anything.
    #[serde(default)]
    keep_both: bool,
    /// A PS4 update to merge into the PS4 game `source` first (`ps5upload_fpkg::ps4_merge`):
    /// the package is the game at the update's version.
    #[serde(default)]
    update: Option<String>,
    /// For a game in LZ4 or zstd asset packs: package it as plain files, as an image build's
    /// `unpack_packs` writes them. Every packed file goes in under its own name, decoded as the
    /// package is written (no copy on disk), the pack volumes and their indexes are left out,
    /// and the package's `ampr_emu.index` lists the files it carries. A libSceAmpr game whose
    /// emulator is ampr-emu-ng, an old ampr_emu or unknown gets the one `ampr_emu_build` names
    /// (`add_ampr_emu` off leaves it). Refused (400) for a game that is not in packs. Without
    /// it a packed game is packaged as it is, packs and all.
    #[serde(default)]
    unpack_packs: bool,
    /// With `unpack_packs`: replace or add the game's AMPR emulator as an image build does.
    /// On unless turned off.
    #[serde(default = "default_add_ampr_emu")]
    add_ampr_emu: bool,
    /// With `unpack_packs`: which emulator, `ampr_emu` (the default) or `ampr-emu-ng`.
    #[serde(default)]
    ampr_emu_build: Option<String>,
    /// With `unpack_packs`: leave the AMPR emulator out of the package (every
    /// `libSceAmpr.sprx` in `fakelib` and `fakelib2`, and a kept `.orig` copy), so the game
    /// reads its plain files through the console's own libSceAmpr. `add_ampr_emu` and
    /// `ampr_emu_build` then do nothing. A libSceAmpr game still gets a fresh `ampr_emu.index`.
    /// Off by default: one backported game played from a package without its emulator on FW
    /// 5.10, which is not every game on every firmware. Refused (400) without `unpack_packs`:
    /// packs are read only through the emulator.
    #[serde(default)]
    leave_out_ampr_emu: bool,
}

/// Why a package build's `unpack_packs` / `leave_out_ampr_emu` can't go ahead for a game in
/// `packs` (the inspection's, `None` for a plain game), in a sentence; `None` when it can.
fn package_unpack_refusal(
    unpack: bool,
    leave_out_emulator: bool,
    packs: Option<&str>,
) -> Option<&'static str> {
    if leave_out_emulator && !unpack {
        return Some(
            "leaving out the AMPR emulator goes with unpacking the asset packs: a game in packs \
             reads them only through its emulator",
        );
    }
    if unpack && packs.is_none() {
        return Some(
            "this game is not in asset packs (it has no ampr_assets.index or ampr_zstd.index \
             pack set): there is nothing to unpack",
        );
    }
    None
}

/// The language a build asked for, trimmed; none when blank.
fn language_of(v: Option<String>) -> Option<String> {
    v.map(|l| l.trim().to_string()).filter(|l| !l.is_empty())
}

/// Where packages go when the caller does not say: the user's Downloads folder, which is
/// where a package is easiest to find afterwards.
/// Run a Workshop job's work off the async runtime, on a thread of its own that "use less of
/// this computer" can lower (#632) without touching the shared blocking pool.
/// The job's thread is inside the engine's runtime, as a blocking task's is: a source on a
/// saved server opened there (a folder build's read-back reopens it) reaches the server.
pub(crate) fn spawn_workshop_job(work: impl FnOnce() + Send + 'static) {
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        ps5upload_fpkg::workshop::on_job_thread(move || {
            let _runtime = handle.enter();
            work()
        })
    });
}

pub(crate) fn default_output_dir() -> PathBuf {
    user_home().join("Downloads").join("fpkgs")
}

/// The user's home folder: `HOME` on Unix, `USERPROFILE` (or `HOMEDRIVE`+`HOMEPATH`) on Windows,
/// where `HOME` is normally unset. Only when none of them resolves does it fall back to the
/// temp folder (#364: on Windows that fallback was the *usual* case).
pub(crate) fn user_home() -> PathBuf {
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

pub(crate) fn output_dir(requested: Option<&str>) -> PathBuf {
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
    /// Present when the game's `fakelib`/`fakelib2` holds backport libraries: what an image or
    /// folder build's `remove_backport` would leave out, and why it can't when it can't.
    #[serde(skip_serializing_if = "Option::is_none")]
    backport: Option<crate::remove_backport::BackportReport>,
    /// Why an image or folder build could not move this source to the Trash afterwards
    /// (`delete_source`), whatever it makes: it is on a PS5 or a server, or this engine has no
    /// Trash. Absent when it could.
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_source_refusal: Option<String>,
}

fn min_firmware_of(source: &Path, declared: Option<&str>) -> Option<String> {
    let mut tree = ps5upload_fpkg::source::open(source).ok()?;
    crate::fpkg_firmware::min_firmware(tree.as_mut(), declared)
}

/// What the source's fakelib holds of a backport, when it holds any.
fn backport_of(source: &Path) -> Option<crate::remove_backport::BackportReport> {
    let mut tree = ps5upload_fpkg::source::open(source).ok()?;
    crate::remove_backport::report(tree.as_mut())
}

/// POST /api/fpkg/inspect — what the source is, whether it looks convertible, and what it
/// will cost. Synchronous: a walk is fast even for a 286,000-file mount.
/// Whether a game folder carries its title's parameters: sce_sys/param.json (PS5) or
/// sce_sys/param.sfo (PS4).
async fn has_title_param(dir: &Path) -> bool {
    for name in ["param.json", "param.sfo"] {
        if tokio::fs::metadata(dir.join("sce_sys").join(name))
            .await
            .is_ok_and(|m| m.is_file())
        {
            return true;
        }
    }
    false
}

/// Why a folder without its title's parameters can't be converted, and the usual cause.
fn missing_param_message(dir: &Path, package: bool) -> String {
    let what = if package {
        "sce_sys/param.json"
    } else {
        "sce_sys/param.json (or a PS4 game's param.sfo)"
    };
    format!(
        "{} has no {what}, so it is not a complete game dump. Pick the game's own folder, the \
         one with eboot.bin and sce_sys in it; if that is this folder, the dump is missing files.",
        dir.display()
    )
}

pub(crate) async fn fpkg_inspect_handler(
    State(_): State<AppState>,
    Json(req): Json<InspectReq>,
) -> impl IntoResponse {
    let out = output_dir(req.output_dir.as_deref());
    let source = resolve_engine_path(&req.source)
        .to_string_lossy()
        .into_owned();
    let update = update_path(req.update.as_deref());
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
        let inspection = build::inspect_with_update(Path::new(&source), update.as_deref(), &out)?;
        let min_firmware =
            min_firmware_of(Path::new(&source), inspection.required_firmware.as_deref());
        let backport = backport_of(Path::new(&source));
        Ok::<_, ps5upload_fpkg::Error>(InspectResponse {
            inspection,
            min_firmware,
            backport,
            delete_source_refusal: crate::source_removal::source_refusal(Path::new(&source)),
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

/// POST /api/fpkg/build — start a conversion. Returns `{ job_id }` immediately. A PS5 game
/// becomes a PS5 package; a PS4 game (a folder, an image or a PS4 package; with `update`, merged
/// with its update) becomes a PS4 fake package, read back and compared file by file.
pub(crate) async fn fpkg_build_handler(
    State(state): State<AppState>,
    Json(req): Json<BuildReq>,
) -> impl IntoResponse {
    let out = output_dir(req.output_dir.as_deref());
    let source = resolve_engine_path(&req.source)
        .to_string_lossy()
        .into_owned();
    let source_path = PathBuf::from(&source);
    // Registered as reading its source from before the inspection reads it to the job's end:
    // an in-place edit of it can't start meanwhile, nor this while one runs.
    let reading = match crate::image_edit_in_place::Reading::take(&source_path) {
        Ok(r) => r,
        Err(why) => return json_err(StatusCode::CONFLICT, why).into_response(),
    };

    // Look first: a source that cannot convert should fail now, with the reason, rather
    // than as a job the user has to poll to see fail.
    let update = req
        .update
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .map(resolve_engine_path);
    let inspection = {
        let out = out.clone();
        let source_path = source_path.clone();
        let update = update.clone();
        match tokio::task::spawn_blocking(move || {
            // An image an in-place edit was interrupted in is settled before it is read (here
            // and by the build after).
            settle_local_source(&source_path).map_err(ps5upload_fpkg::Error::Format)?;
            if let Some(why) = patch_refusal(&source_path) {
                return Err(ps5upload_fpkg::Error::Format(why));
            }
            build::inspect_with_update(&source_path, update.as_deref(), &out)
        })
        .await
        {
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

    if let Some(why) =
        package_unpack_refusal(req.unpack_packs, req.leave_out_ampr_emu, inspection.packs)
    {
        return json_err(StatusCode::BAD_REQUEST, why).into_response();
    }
    // A packed game read as plain files: decoded as the package is written, its emulator
    // replaced or left out as asked.
    let reader = req.unpack_packs.then(|| {
        build::SourceReader(if req.leave_out_ampr_emu {
            open_unpacked_without_emulator()
        } else {
            open_unpacked(req.add_ampr_emu, req.ampr_emu_build.clone())
        })
    });

    // A PS4 game (or one with its update merged in) becomes a PS4 package; without param.json
    // there is no PS5 title to package (#596).
    if inspection.ps4_game || inspection.ps4_kind.is_some() || update.is_some() {
        return ps4_package_build(state, req, out, source_path, update, inspection, reading).await;
    }
    if inspection
        .checks
        .iter()
        .any(|c| c.name == "param.json present" && !c.ok)
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            missing_param_message(&source_path, true),
        )
        .into_response();
    }

    // The file this build writes: a leftover of an interrupted one goes, and this one is
    // marked as being written until the build ends. Claimed before the job exists, so a
    // second build of the same package is refused rather than started.
    let stem = req
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| req.content_id.clone().filter(|c| !c.trim().is_empty()))
        // The game's own id names the file only when it is a plain name (never `../x`).
        .or_else(|| {
            inspection
                .content_id
                .clone()
                .filter(|c| build::content_id_is_plain(c))
        });
    let stem = match stem {
        Some(s) if req.keep_both => Some(
            crate::folder_build::keep_both_stem(&s, |c| {
                crate::folder_build::name_in_use(&out.join(format!("{c}.pkg")))
            })
            .unwrap_or(s),
        ),
        other => other,
    };
    let chosen_name = stem.clone().filter(|_| req.keep_both);
    let partial = match stem.map(|stem| out.join(format!("{stem}.pkg.partial"))) {
        Some(path) => match claim_off_thread(path.clone()).await {
            Some(w) => Some(w),
            None => return already_building(&path),
        },
        None => None,
    };

    let job_id = Uuid::new_v4();
    crate::workshop_api::note_run(job_id, "package", &source_path);
    let scaffold = Scaffold::of(&source_path);
    let cancel = register_workshop_job(job_id);
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
    let job_total = inspection.planned_size.max(1);
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
                    // The stage's own count goes in `stage` (the Convert screen draws each
                    // stage); the job's is the whole build's, so it never goes back.
                    let (done, of) = (
                        tick_bytes.load(Ordering::Relaxed),
                        tick_total.load(Ordering::Relaxed),
                    );
                    let at = tick_stage.load(Ordering::Relaxed);
                    *stage = STAGES.get(at as usize).map(|s| JobStage {
                        id: s.id().to_string(),
                        index: s.index(),
                        count: build::STAGE_COUNT,
                        done,
                        total: of,
                    });
                    if let Some(s) = STAGES.get(at as usize) {
                        let overall =
                            overall_progress(job_total, s.index(), build::STAGE_COUNT, done, of);
                        *bytes_sent = (*bytes_sent).max(overall);
                    }
                    *total_bytes = job_total;
                    // Nobody listening: nothing to build.
                    let state = crate::event_bus::tick_wanted(&events_tx)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
                    drop(g);
                    if let Some(state) = state {
                        let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": crate::with_live_notes(job_id, state) });
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
    spawn_workshop_job(move || {
        // Released last: an in-place edit may claim the source once this job is over.
        let _reading = reading;
        // First in, last out: gone only after everything reading it has been dropped.
        let _scaffold = scaffold;
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
        request.file_name = chosen_name.or(req.name.filter(|name| !name.trim().is_empty()));
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
        request.reader = reader;
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
                        error_space: None,
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

/// A PS4 game, or a PS4 game with its update merged in, as a PS4 fake package
/// (`ps5upload_fpkg::ps4_pkg_write`), then read back with our PS4 reader and compared file by
/// file. Its files go in as they are: no case fixes and no signing, which are a PS5 package's.
async fn ps4_package_build(
    state: AppState,
    req: BuildReq,
    out: PathBuf,
    source_path: PathBuf,
    update: Option<PathBuf>,
    inspection: build::Inspection,
    reading: crate::image_edit_in_place::Reading,
) -> axum::response::Response {
    use ps5upload_fpkg::ps4_pkg::Ps4Kind;
    use ps5upload_fpkg::ps4_pkg_write as ps4w;
    use ps5upload_fpkg::source::SourceTree;

    let refuse = |why: String| json_err(StatusCode::BAD_REQUEST, why).into_response();
    if update.is_none() {
        match inspection.ps4_kind {
            Some(Ps4Kind::Patch) => {
                return refuse(format!(
                    "{} is a PS4 update, and only a game becomes a PS4 package: choose its game \
                     as the source and this as the update to merge in, and the package is the \
                     game at the update's version",
                    source_path.display()
                ))
            }
            Some(Ps4Kind::Addon) => {
                return refuse(format!(
                    "{} is a PS4 add-on, and only a PS4 game becomes a PS4 package here",
                    source_path.display()
                ))
            }
            _ => {}
        }
    }
    let opened = {
        let (source_path, update) = (source_path.clone(), update.clone());
        tokio::task::spawn_blocking(move || -> Result<Box<dyn SourceTree>, String> {
            let mut tree: Box<dyn SourceTree> = match &update {
                Some(u) => Box::new(
                    ps5upload_fpkg::ps4_merge::open(&source_path, u).map_err(|e| e.to_string())?,
                ),
                None => ps5upload_fpkg::source::open(&source_path).map_err(|e| e.to_string())?,
            };
            match ps4w::refusal(tree.as_mut()) {
                Some(why) => Err(format!("{}: {why}", source_path.display())),
                None => Ok(tree),
            }
        })
        .await
    };
    let tree = match opened {
        Ok(Ok(tree)) => tree,
        Ok(Err(why)) => return refuse(why),
        Err(join) => {
            return json_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("the source could not be opened: {join}"),
            )
            .into_response()
        }
    };

    // `<content id>.pkg` unless named; a name already taken is a new name or a refusal, never
    // an overwrite (the source itself may be that file).
    let stem = req
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        // The param.sfo's id names the file only when it is a plain name (never `../x`).
        .or_else(|| {
            inspection
                .content_id
                .clone()
                .filter(|c| build::content_id_is_plain(c))
        })
        .or_else(|| {
            source_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "ps4-game".to_string());
    let stem = if req.keep_both {
        crate::folder_build::keep_both_stem(&stem, |c| {
            crate::folder_build::name_in_use(&out.join(format!("{c}.pkg")))
        })
        .unwrap_or(stem)
    } else {
        stem
    };
    let final_path = out.join(format!("{stem}.pkg"));
    if final_path.exists() {
        return json_err(
            StatusCode::CONFLICT,
            format!(
                "output already exists: {}; choose another output folder or name",
                final_path.display()
            ),
        )
        .into_response();
    }
    let partial_path = out.join(format!("{stem}.pkg.partial"));
    let partial = match claim_off_thread(partial_path.clone()).await {
        Some(w) => w,
        None => return already_building(&partial_path),
    };

    let job_id = Uuid::new_v4();
    crate::workshop_api::note_run(job_id, "package", &source_path);
    let scaffold = Scaffold::of(&source_path);
    let cancel = register_workshop_job(job_id);
    let bytes = Arc::new(AtomicU64::new(0));
    let total = Arc::new(AtomicU64::new(inspection.bytes));
    let stage = Arc::new(AtomicU64::new(u64::from(build::Stage::Write.index())));
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        JobState::Running {
            stage: None,
            started_at_ms,
            bytes_sent: 0,
            total_bytes: inspection.bytes,
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
    // Writing, then reading back: the job's progress is the two stages together.
    let job_total = inspection.bytes.max(1);
    let (tick_bytes, tick_total, tick_stage) = (bytes.clone(), total.clone(), stage.clone());
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
                    // The stage's own count goes in `stage` (the Convert screen draws each
                    // stage); the job's is the whole build's, so it never goes back.
                    let (done, of) = (
                        tick_bytes.load(Ordering::Relaxed),
                        tick_total.load(Ordering::Relaxed),
                    );
                    let at = tick_stage.load(Ordering::Relaxed);
                    *stage = STAGES.get(at as usize).map(|s| JobStage {
                        id: s.id().to_string(),
                        index: s.index(),
                        count: build::STAGE_COUNT,
                        done,
                        total: of,
                    });
                    if let Some(s) = STAGES.get(at as usize) {
                        let overall = overall_progress(
                            job_total,
                            s.index().saturating_sub(build::Stage::Write.index()),
                            2,
                            done,
                            of,
                        );
                        *bytes_sent = (*bytes_sent).max(overall);
                    }
                    *total_bytes = job_total;
                    let state = crate::event_bus::tick_wanted(&events_tx)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
                    drop(g);
                    if let Some(state) = state {
                        let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": crate::with_live_notes(job_id, state) });
                        let _ = events_tx.send(msg.to_string());
                    }
                }
                _ => break,
            }
        }
    });

    let state_for_job = state.clone();
    let ticker = JobTicker(ticker.abort_handle());
    let mut tree = tree;
    spawn_workshop_job(move || {
        // Released last: an in-place edit may claim the source once this job is over.
        let _reading = reading;
        let _scaffold = scaffold;
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        sweep_output_leftovers(&out);
        let outcome = (|| -> Result<ps4w::Written, String> {
            std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&partial_path)
                .map_err(|e| format!("{}: {e}", partial_path.display()))?;
            // Writing, then hashing the image for the container's digests: one stage.
            let image_bytes = std::cell::Cell::new(0u64);
            let written = ps4w::write(
                tree.as_mut(),
                &mut file,
                &ps4w::Options::default(),
                &mut |step, done, of| {
                    let (done, of) = match step {
                        ps4w::Step::Write => {
                            image_bytes.set(of);
                            (done, of.saturating_mul(2))
                        }
                        ps4w::Step::Hash => (image_bytes.get() + done, image_bytes.get() + of),
                    };
                    bytes.store(done, Ordering::Relaxed);
                    total.store(of, Ordering::Relaxed);
                },
                &cancel,
            )
            .map_err(|e| e.to_string())?;
            drop(file);
            stage.store(u64::from(build::Stage::Verify.index()), Ordering::Relaxed);
            bytes.store(0, Ordering::Relaxed);
            ps4w::read_back(
                &partial_path,
                &written,
                &mut |done, of| {
                    bytes.store(done, Ordering::Relaxed);
                    total.store(of, Ordering::Relaxed);
                },
                &cancel,
            )
            .map_err(|e| e.to_string())?;
            if final_path.exists() {
                return Err(format!(
                    "output already exists: {}; choose another output folder or name",
                    final_path.display()
                ));
            }
            // Never over a file that appeared since the check above (another program's, a
            // second build's): the system's no-replace rename where there is one.
            let mut kept = false;
            ps5upload_convert_core::safe_write::name_finished(
                &partial_path,
                &final_path,
                &mut kept,
            )?;
            Ok(written)
        })();
        drop(partial);
        if outcome.is_err() {
            sweep_output_leftovers(&out);
        }
        let completed_at_ms = now_ms();
        drop(ticker);
        match outcome {
            Ok(written) => {
                built_packages()
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(final_path.clone());
                crate::engine_log::record(
                    "info",
                    format!(
                        "fpkg: built PS4 package {} ({} bytes, {} files read back the same; \
                         licence {}, keystone {})",
                        final_path.display(),
                        written.size,
                        written.files.len(),
                        if written.made_licence { "made" } else { "kept" },
                        if written.made_keystone {
                            "made"
                        } else {
                            "kept"
                        },
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
                        tx_id_hex: written.content_id.clone(),
                        bytes_sent: written.size,
                        dest: final_path.display().to_string(),
                        files_sent: written.files.len() as u64,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: None,
                    },
                );
            }
            Err(error) => {
                crate::engine_log::record("warn", format!("fpkg: PS4 package failed: {error}"));
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
                        error_space: None,
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
pub(crate) struct ExfatBuildReq {
    /// A game folder (one with `sce_sys` in it) or image, on this computer, a saved server
    /// (`remote://`) or a console (`ps5://`); or a `.zip`, `.7z` or `.rar` on this computer (a
    /// multi-part RAR set by its first part) holding one game folder, read straight out of it
    /// with nothing unpacked. An archive that has to be unpacked first (a game image inside it,
    /// or an `update` to merge) is refused (400) with `archive_needs_unpack` at the start of
    /// the reason: `POST /api/fpkg/extract` then unpacks it.
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
    /// Pack the game's files into AMPR LZ4 asset packs first, in any image, plain or (with
    /// `compress`) inside a `.ffpfsc`.
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
    /// With `edits`: change the image `source` itself instead of writing an edited copy. Only
    /// for an uncompressed `.exfat`, `.ffpkg` or `.ffpfs` on this computer, not in asset packs,
    /// and only when every change fits the image's free space (it never grows): otherwise
    /// refused (400) with the reason, before anything is written. An undo journal
    /// (`<image>.forge-undo`) is written beside it first; every changed file is read back and
    /// the filesystem checked, and any failure restores the image. `POST /api/image/edit/check`
    /// says beforehand whether it would be accepted.
    #[serde(default)]
    in_place: bool,
    /// When the image's name is taken in the output folder, add ` (2)` (and so on) instead of
    /// refusing (see `BuildReq::keep_both`).
    #[serde(default)]
    keep_both: bool,
    /// An edited copy of the image `source` whose one change is a regenerated
    /// `ampr_emu.index` (Workshop's "Rebuild AMPR index", #621): written like an edited copy,
    /// refused for a compressed image or a game that does not read through libSceAmpr.
    #[serde(default)]
    rebuild_ampr_index: bool,
    /// Add ampr_emu to a game that reads through libSceAmpr and ships no emulator: without one
    /// it fails to start from an image. On unless turned off.
    #[serde(default = "default_add_ampr_emu")]
    add_ampr_emu: bool,
    /// Which emulator: `ampr_emu` (the default) or `ampr-emu-ng`.
    #[serde(default)]
    ampr_emu_build: Option<String>,
    /// Pack the game's files into zstd asset packs for ampr-emu-ng, which goes in the image: any
    /// format, plain or (with `compress`) inside a `.ffpfsc`; not with LZ4 packs. A game in LZ4
    /// packs is decoded on the way.
    #[serde(default)]
    ampr_zstd: Option<AmprZstdReq>,
    /// Where the image goes: `{"kind":"local"}` (the default: `output_dir` on this computer) or
    /// `{"kind":"console","addr":…,"dir":…}`: straight into that PS5's folder `dir` over AVA1,
    /// sent as it is written, so the image is never stored on this computer. The job's `dest` is
    /// then the image's path on the PS5, and the read-back runs there. For `.exfat`, `.ffpkg`
    /// and `.ffpfs` images, plain or in a `.ffpfsc` (`compress`: compressed once to measure it,
    /// then again as it is sent, so it takes about twice the compression time), with or
    /// without `ampr_lz4` or `ampr_zstd` packs (LZ4 packs are measured, then each volume is made
    /// again as it is sent, so nothing is stored here; should a volume not come out as measured,
    /// the packs are made into the engine's data folder first and removed once the image is
    /// there); edited copies are refused for it.
    #[serde(default)]
    destination: Option<ImageDestinationReq>,
    /// A PS4 update (a package, image or folder) merged into the PS4 game `source`: the image
    /// is the game at the update's version (`ps5upload_fpkg::ps4_merge`). Not with `edits`.
    #[serde(default)]
    update: Option<String>,
    /// Leave the backport libraries in the game's `fakelib`/`fakelib2` out of the image, the
    /// emulators there kept (the inspection's `backport` lists both). Refused, with the reason,
    /// when they can't go: the backport lowered `eboot.bin`'s SDK (nothing here restores it),
    /// or the game stays in asset packs whose own list names them. A game with no backport
    /// library is converted as it is. Not with `edits`.
    #[serde(default)]
    remove_backport: bool,
    /// Once the image is written, read back in full and given its name, move `source` to this
    /// computer's Trash or Recycle Bin (never deleted for good). Only for a file or folder on
    /// this computer made into an image on this computer; refused, with the reason, for a
    /// source on a PS5 or a server, an output inside the source (or the source inside the
    /// output), an engine with no Trash (Docker, a headless Linux), `edits`, `update` and a
    /// `destination` elsewhere. The whole image is read back whatever `full_verify` says.
    /// The source stays when it changed during the run or another job reads it; the finished
    /// job's `commit_ack` says which: `source_removed` and `source_note`.
    #[serde(default)]
    delete_source: bool,
    /// For a game in LZ4 or zstd asset packs: write it as plain files again. Every packed file
    /// goes in under its own name, the pack volumes and their indexes are left out, a fresh
    /// `ampr_emu.index` lists the files, and a libSceAmpr game whose emulator is ampr-emu-ng,
    /// an old ampr_emu or unknown gets the one `ampr_emu_build` names (`add_ampr_emu` off
    /// leaves it). Refused for a game that is not packed, and with `ampr_lz4`, `ampr_zstd` or
    /// `edits`. Without it a packed game is written as it is, packs and all.
    #[serde(default)]
    unpack_packs: bool,
    /// The password of a `.rar` `source` (an archive read in place); never logged or kept.
    #[serde(default)]
    archive_password: Option<String>,
}

/// Where an image build puts the image.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ImageDestinationReq {
    /// This computer: `output_dir`.
    Local,
    /// A PS5: `addr` (its address) and `dir` (an absolute folder on it, made when missing). A
    /// dropped connection goes on where it stopped, and so does the same build started again.
    Console { addr: String, dir: String },
    /// A saved server (`source_id`, a connection under Connections: SMB, NFS, FTP, SFTP or
    /// WebDAV) and `dir`, a folder on it (made when missing). Written there as `<name>.partial`,
    /// read back there and only then renamed, never over anything; the job's `dest` is then
    /// `remote://<source_id>/<dir>/<name>`. On SMB, NFS and SFTP a write that was cut off goes
    /// on from what the server holds.
    Server { source_id: String, dir: String },
}

/// Why an image build cannot go straight to a server, in a sentence for the user; `None` when
/// it can. The same images as straight to the console: `.exfat`, `.ffpkg` and `.ffpfs`, plain
/// or in a `.ffpfsc`, with or without LZ4 or zstd asset packs.
pub(crate) fn server_refusal(edited: bool) -> Option<&'static str> {
    edited.then_some(
        "an edited copy of an image is written next to it on this computer, not straight to a \
         server",
    )
}

/// A request's server destination, checked: the saved connection must exist and `dir` must be
/// a path on it.
fn server_dest(source_id: &str, dir: &str) -> Result<crate::server_out::ServerDest, String> {
    if dir.trim().is_empty() {
        return Err("choose a folder on the server".into());
    }
    crate::server_out::ServerDest::new(source_id.trim(), dir)
        .map_err(|e| crate::server_out::said(&e))
}

/// Why an image build cannot go straight to the console, in a sentence for the user; `None`
/// when it can: `.exfat`, `.ffpkg` and `.ffpfs`, plain or in a `.ffpfsc` (compressed once to
/// measure it, then again as it is sent), with or without LZ4 or zstd asset packs (as
/// ps5-dump-forge writes packs into an image on another device).
pub(crate) fn console_refusal(edited: bool) -> Option<&'static str> {
    edited.then_some(
        "an edited copy of an image is written next to it on this computer, not straight to the \
         PS5",
    )
}

/// How an image build ended: on this computer, or streamed to a console.
enum Ended {
    Local(Result<crate::image_build::ImageBuilt, String>),
    Console(anyhow::Result<crate::image_build::Streamed>),
    Server(Result<crate::image_build::ToServer, String>),
}

/// The job of an image build straight to the server `dest`, the image called `name` in its
/// folder: planned with the time an unfinished earlier try pinned (so it goes on from the
/// server's partial file), streamed, read back there and renamed
/// (`image_build::stream_to_server`).
#[allow(clippy::too_many_arguments)]
fn to_server_job(
    dest: &crate::server_out::ServerDest,
    name: &str,
    format: crate::image_build::ImageFormat,
    ffpfsc: Option<crate::image_build::FfpfscOut>,
    zstd_level: Option<(u8, crate::zstd_runtime::RuntimeChoice)>,
    zstd_rules: Option<&crate::zstd_packs::Rules>,
    runtime: &mut Option<crate::zstd_runtime::PackRuntime>,
    lz4: Option<(&crate::image_build::Lz4Packs, &Path)>,
    tree: &mut dyn ps5upload_fpkg::source::SourceTree,
    mode: ps5upload_fpkg::sample::VerifyMode,
    verify_chunk: u64,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> Result<crate::image_build::ToServer, String> {
    let data = crate::remote::store::data_dir();
    let path = dest.path_of(name);
    let stamp = crate::image_build::server_stamp(data.as_deref(), &dest.id, &path);
    let target = crate::image_build::ServerImage {
        dest,
        name: name.to_string(),
        data,
    };
    match zstd_level {
        Some((level, choice)) => {
            let r = crate::zstd_runtime::pack_runtime(choice)?;
            crate::log_at!(
                Info,
                "convert",
                "zstd packs: {} goes into {} on {}",
                r.describe(),
                path,
                dest.label
            );
            crate::zstd_packs::stream_to_server(
                format,
                ffpfsc,
                level,
                zstd_rules,
                runtime.insert(r),
                tree,
                stamp,
                &target,
                mode,
                verify_chunk,
                cancel,
                stage,
            )
        }
        // LZ4 packs: made here, streamed, read back there through the packs.
        None if lz4.is_some() => {
            let Some((packs, spool)) = lz4 else {
                return Err("no LZ4 packs".into());
            };
            crate::image_build::stream_lz4(
                packs,
                tree,
                stamp,
                spool,
                &path,
                mode,
                cancel,
                stage,
                &mut |t, salt, stage| {
                    crate::image_build::stream_to_server(
                        format,
                        ffpfsc,
                        stamp,
                        salt,
                        t,
                        &target,
                        mode,
                        verify_chunk,
                        cancel,
                        stage,
                    )
                    .map_err(anyhow::Error::msg)
                },
                &|| {
                    let files = dest.files().map_err(|e| crate::server_out::said(&e))?;
                    ps5upload_fpkg::remote_source::open_remote(files, &path)
                        .map_err(|e| e.to_string())
                },
                &|| match dest.fs() {
                    Ok(fs) => dest.remove(&*fs, &path),
                    Err(e) => format!(
                        "removing it from {} failed too ({e}): delete {path} by hand",
                        dest.label
                    ),
                },
                &dest.label,
            )
            .map(|(mut s, line)| {
                s.built.detail.push_str(&line);
                s
            })
            .map_err(|e| format!("{e:#}"))
        }
        // A libSceAmpr game gets a fresh ampr_emu.index (#449), as a local build does.
        None => match crate::image_build::AmprIndexed::wrap_at(&mut *tree, stamp) {
            Ok(Some(mut indexed)) => crate::image_build::stream_to_server(
                format,
                ffpfsc,
                stamp,
                "",
                &mut indexed,
                &target,
                mode,
                verify_chunk,
                cancel,
                stage,
            ),
            Ok(None) => crate::image_build::stream_to_server(
                format,
                ffpfsc,
                stamp,
                "",
                tree,
                &target,
                mode,
                verify_chunk,
                cancel,
                stage,
            ),
            Err(e) => Err(format!("ampr_emu.index: {e}")),
        },
    }
}

/// The job of an image build straight to the console at `addr`, the image at `path` there: the
/// console must answer over AVA1, the name must be free, then the image is planned, its size
/// checked against the drive's room, streamed and read back (`image_build::stream_to_console`).
#[allow(clippy::too_many_arguments)]
fn to_console_job(
    addr: &str,
    path: &str,
    format: crate::image_build::ImageFormat,
    ffpfsc: Option<crate::image_build::FfpfscOut>,
    zstd_level: Option<(u8, crate::zstd_runtime::RuntimeChoice)>,
    zstd_rules: Option<&crate::zstd_packs::Rules>,
    runtime: &mut Option<crate::zstd_runtime::PackRuntime>,
    lz4: Option<(&crate::image_build::Lz4Packs, &Path)>,
    tree: &mut dyn ps5upload_fpkg::source::SourceTree,
    mode: ps5upload_fpkg::sample::VerifyMode,
    verify_chunk: u64,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    job_id: Uuid,
    stage: &mut dyn FnMut(&str, u64, u64),
) -> anyhow::Result<crate::image_build::Streamed> {
    use ps5upload_core::fs_ops;
    // None starts while the helper there is being stopped or removed.
    let Some(_busy) = crate::helper_stop::try_enter(addr) else {
        anyhow::bail!("{}", crate::helper_stop::STOPPING_MESSAGE);
    };
    ps5upload_ava1::console::require_ava1(addr)
        .map_err(|f| anyhow::Error::from(ps5upload_ava1::upload::ConsoleFailure::on(addr, f)))?;
    match fs_ops::fs_stat(addr, path) {
        Ok(_) => anyhow::bail!(
            "{path} already exists on the PS5: delete it there or give the image another name"
        ),
        Err(e) if fs_ops::is_not_found(&format!("{e:#}")) => {}
        Err(e) => return Err(e.context(format!("looking for {path} on the PS5"))),
    }
    let dir = path.rsplit_once('/').map_or("/", |(d, _)| d);
    if let Err(e) = fs_ops::fs_mkdir(addr, dir) {
        crate::log_at!(Warn, "convert", "to console: making {dir}: {e:#}");
    }
    let room = ps5upload_ava1::space::volumes_probe()(addr, dir);
    let mut cfg = crate::make_transfer_config(addr);
    cfg.cancel = Some(cancel.clone());
    cfg.progress_bytes = Some(Arc::new(AtomicU64::new(0)));
    cfg.progress_bytes_finalized = Some(Arc::new(AtomicU64::new(0)));
    crate::attach_live_notes(&mut cfg, job_id);
    let send = |m: ava1::manifest::Manifest,
                src: Arc<dyn ava1::source::Source>,
                seq: Arc<dyn ava1::seq::SeqSource>,
                job: [u8; 16]| {
        let mut opts = ava1::send::SendOptions::upload(path);
        opts.flags = ps5upload_ava1::gen::JF_SINGLE_FILE;
        ps5upload_ava1::upload::upload_with_seq_in(
            ps5upload_ava1::pool(),
            addr,
            job,
            m,
            src,
            Some(seq),
            opts,
            &cfg,
        )
    };
    let remove = |p: &str| fs_ops::fs_delete(addr, p);
    let target = crate::image_build::ConsoleTarget {
        console: addr.to_string(),
        path: path.to_string(),
        send: &send,
        files: Arc::new(crate::console_read::ConsoleFiles {
            addr: addr.to_string(),
            label: format!("the PS5 at {addr}"),
        }),
        remove: &remove,
        room,
    };
    let data = crate::remote::store::data_dir();
    let stamp = crate::image_build::console_stamp(data.as_deref(), addr, path);
    let streamed = match zstd_level {
        Some((level, choice)) => {
            let r = crate::zstd_runtime::pack_runtime(choice).map_err(anyhow::Error::msg)?;
            crate::log_at!(
                Info,
                "convert",
                "zstd packs: {} goes into {path} on the PS5",
                r.describe()
            );
            crate::zstd_packs::stream_to_console(
                format,
                ffpfsc,
                level,
                zstd_rules,
                runtime.insert(r),
                tree,
                stamp,
                &target,
                mode,
                verify_chunk,
                cancel,
                stage,
            )
        }
        // LZ4 packs: made here, streamed, read back on the PS5 through the packs.
        None if lz4.is_some() => match lz4 {
            Some((packs, spool)) => crate::image_build::stream_lz4(
                packs,
                tree,
                stamp,
                spool,
                path,
                mode,
                cancel,
                stage,
                &mut |t, salt, stage| {
                    crate::image_build::stream_to_console(
                        format,
                        ffpfsc,
                        Some(stamp),
                        salt,
                        t,
                        &target,
                        mode,
                        verify_chunk,
                        cancel,
                        stage,
                    )
                },
                &|| {
                    ps5upload_fpkg::remote_source::open_remote(target.files.clone(), &target.path)
                        .map_err(|e| e.to_string())
                },
                &|| match (target.remove)(&target.path) {
                    Ok(()) => "it was removed from the PS5".to_string(),
                    Err(r) => {
                        format!("removing it from the PS5 failed too ({r:#}): delete it by hand")
                    }
                },
                "the PS5",
            )
            .map(|(mut s, line)| {
                s.built.detail.push_str(&line);
                s
            }),
            None => Err(anyhow::anyhow!("no LZ4 packs")),
        },
        // A libSceAmpr game gets a fresh ampr_emu.index (#449), as a local build does.
        None => match crate::image_build::AmprIndexed::wrap_at(&mut *tree, stamp) {
            Ok(Some(mut indexed)) => crate::image_build::stream_to_console(
                format,
                ffpfsc,
                Some(stamp),
                "",
                &mut indexed,
                &target,
                mode,
                verify_chunk,
                cancel,
                stage,
            ),
            Ok(None) => crate::image_build::stream_to_console(
                format,
                ffpfsc,
                Some(stamp),
                "",
                tree,
                &target,
                mode,
                verify_chunk,
                cancel,
                stage,
            ),
            Err(e) => Err(anyhow::anyhow!("ampr_emu.index: {e}")),
        },
    };
    if streamed.is_ok() {
        crate::image_build::forget_console_stamp(data.as_deref(), addr, path);
    }
    streamed
}

/// A request's `update`, resolved; `None` when absent or blank.
fn update_path(update: Option<&str>) -> Option<PathBuf> {
    update
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(resolve_engine_path)
}

#[derive(Debug, Deserialize)]
pub(crate) struct AmprZstdReq {
    /// 1 to 22; 5 when absent.
    #[serde(default)]
    level: Option<u8>,
    /// Which ampr-emu-ng goes in: `latest` (the default: the newest GitHub release, used only
    /// when its download matches the SHA-256 GitHub publishes for it, else the tested one) or
    /// `pinned` (the tested release). The job's `commit_ack.ampr_runtime` says which went in.
    #[serde(default)]
    runtime: crate::zstd_runtime::RuntimeChoice,
    /// A profile in drakmor's `ampr_pack` TOML format (what `ampr_lz4.profile_toml` takes)
    /// deciding which files stay loose: a file is packed unless its last matching `[[rule]]`,
    /// or `[pack] default_action` when none matches (`loose` unless set), says `loose`. System
    /// files and empty files stay loose whatever it says. Its block sizes, `hot`, `layout`,
    /// groups and LZ4 settings mean nothing to zstd packs and are ignored. Absent or blank:
    /// every file but the system files is packed.
    #[serde(default)]
    profile_toml: Option<String>,
}

impl AmprZstdReq {
    /// The level, or why it is unusable.
    fn level(&self) -> Result<u8, String> {
        let level = self.level.unwrap_or(5);
        if (1..=22).contains(&level) {
            Ok(level)
        } else {
            Err(format!("zstd level {level} is not 1 to 22"))
        }
    }

    /// The profile's rules, `None` without one, or why it is unusable.
    fn rules(&self) -> Result<Option<Arc<crate::zstd_packs::Rules>>, String> {
        match self.profile_toml.as_deref() {
            Some(text) if !text.trim().is_empty() => {
                ps5upload_fpkg::ampr_pack::Config::from_toml(text, None)
                    .map(|c| Some(Arc::new(c)))
                    .map_err(|e| format!("the pack profile: {e}"))
            }
            _ => Ok(None),
        }
    }
}

/// What a zstd pack job ends with in its `commit_ack`: the emulator that went into the game.
fn runtime_ack(runtime: &crate::zstd_runtime::PackRuntime) -> serde_json::Value {
    serde_json::json!({ "ampr_runtime": runtime })
}

fn default_add_ampr_emu() -> bool {
    true
}

/// Opens a game with its asset packs decoded (`unpack_packs`), for a build and for the
/// read-back that opens the source again: both get the same emulator and the same index.
type OpenUnpacked = Arc<
    dyn Fn(
            Box<dyn ps5upload_fpkg::source::SourceTree>,
        ) -> Result<Box<dyn ps5upload_fpkg::source::SourceTree>, String>
        + Send
        + Sync,
>;

/// [`OpenUnpacked`] for a request: the emulator `build` names (`add` off: the game's own
/// stays), fetched at most once, and one time on every record of the new index.
fn open_unpacked(add: bool, build: Option<String>) -> OpenUnpacked {
    let mtime = now_ms() as i64 / 1000;
    let fetched: std::sync::OnceLock<Result<Vec<u8>, String>> = std::sync::OnceLock::new();
    Arc::new(move |tree| {
        let fetch = || {
            fetched
                .get_or_init(|| crate::ampr_trace::emulator(build.as_deref()))
                .clone()
        };
        let emulator = add.then_some(crate::unpack_packs::Emulator {
            ng: build.as_deref() == Some("ampr-emu-ng"),
            fetch: &fetch,
        });
        crate::unpack_packs::open(tree, emulator.as_ref(), mtime)
    })
}

/// [`OpenUnpacked`] for a package that leaves the AMPR emulator out (`leave_out_ampr_emu`):
/// nothing is fetched.
fn open_unpacked_without_emulator() -> OpenUnpacked {
    let mtime = now_ms() as i64 / 1000;
    Arc::new(move |tree| crate::unpack_packs::open_without_emulator(tree, mtime))
}

/// One change for an edited copy of an image: `{"op":"delete","path":…}` removes a file or
/// folder; `{"op":"put","path":…,"from":…}` puts a file (or a folder's contents) from this
/// computer at `path`, replacing what is there; `{"op":"rename","from":…,"to":…}` renames or
/// moves a file or folder to a path nothing is at yet; `{"op":"mkdir","path":…}` makes a new
/// empty folder. They are applied in order, each seeing what the ones before it did.
#[derive(Deserialize, Debug)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum ImageEditReq {
    Delete { path: String },
    Put { path: String, from: String },
    Rename { from: String, to: String },
    Mkdir { path: String },
}

impl ImageEditReq {
    pub(crate) fn edit(&self) -> ps5upload_fpkg::browse::Edit {
        use ps5upload_fpkg::browse::Edit;
        match self {
            Self::Delete { path } => Edit::Delete { path: path.clone() },
            Self::Put { path, from } => Edit::Put {
                path: path.clone(),
                from: resolve_engine_path(from),
            },
            // Both ends of a rename are paths inside the image, not on this computer.
            Self::Rename { from, to } => Edit::Rename {
                from: from.clone(),
                to: to.clone(),
            },
            Self::Mkdir { path } => Edit::Mkdir { path: path.clone() },
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
    // An interrupted in-place edit's journal is settled first.
    crate::image_edit_in_place::before_open(source)?;
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
    let tree = Edited::for_filesystem(opened.tree, &edits, opened.filesystem)
        .map_err(|e| e.to_string())?;
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

/// Before a job reads `source`: a file on this computer an interrupted in-place edit left an
/// undo journal beside is settled first (`image_edit_in_place::before_open`), so the job never
/// reads a part-edited image. Nothing to do for a folder or a source elsewhere.
fn settle_local_source(source: &Path) -> Result<(), String> {
    if ps5upload_fpkg::remote_source::is_remote(source) || !source.is_file() {
        return Ok(());
    }
    crate::image_edit_in_place::before_open(source).map(drop)
}

/// A build's `delete_source`, checked before the job exists: the plan, or the status and reason
/// to refuse the request with. The check walks a source folder, so it runs off the request's
/// thread.
async fn plan_source_removal(
    source: &Path,
    output: &Path,
) -> Result<crate::source_removal::Plan, (StatusCode, String)> {
    let (source, output) = (source.to_path_buf(), output.to_path_buf());
    match tokio::task::spawn_blocking(move || crate::source_removal::plan(&source, &output)).await {
        Ok(Ok(plan)) => Ok(plan),
        Ok(Err(why)) => Err((StatusCode::BAD_REQUEST, why)),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))),
    }
}

/// Whether another Workshop job that is still running reads `source`, something in it, or a
/// folder it is in. This job (`job_id`) is not counted.
fn source_in_use(state: &AppState, job_id: Uuid, source: &Path) -> bool {
    let me = job_id.to_string();
    let jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
    crate::workshop_api::live_runs(|id| matches!(jobs.get(&id), Some(JobState::Running { .. })))
        .iter()
        .any(|run| {
            let other = Path::new(&run.source);
            run.job_id != me && (other.starts_with(source) || source.starts_with(other))
        })
}

/// A finished, verified build's planned source removal, done and written to the engine log
/// (`what` names the build there). `notes` are what the source's reader said it left out (a
/// patch package's files left to its base game, protected entries it could not decrypt, an
/// archive's files beside the game folder): any keeps the source, which is more than what was
/// made from it.
fn remove_source(
    plan: &crate::source_removal::Plan,
    output: &Path,
    state: &AppState,
    job_id: Uuid,
    what: &str,
    notes: &[String],
) -> crate::source_removal::Outcome {
    let busy = source_in_use(state, job_id, plan.source());
    let outcome = crate::source_removal::finish(plan, output, notes, busy);
    crate::engine_log::record(
        if outcome.removed { "info" } else { "warn" },
        format!("{what}: {}", outcome.note),
    );
    outcome
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
    if req.in_place && req.edits.is_empty() {
        return json_err(
            StatusCode::BAD_REQUEST,
            "an in-place edit needs changes (edits)",
        )
        .into_response();
    }
    // The build reads its source from here (an edited copy's open below included) to its end:
    // registered first, so an in-place edit of it can't start meanwhile, nor this while one
    // runs. An in-place edit claims the image itself instead (`image_edit_in_place::start`).
    let reading = if req.in_place {
        None
    } else {
        match crate::image_edit_in_place::Reading::take(&source) {
            Ok(r) => Some(r),
            Err(why) => return json_err(StatusCode::CONFLICT, why).into_response(),
        }
    };
    // A game folder on this computer, on a saved server (`remote://`) or on a console
    // (`ps5://`, read through the helper as a package build reads it): the opener below
    // knows them all. A local path has to be a folder, an image, or a `.zip`/`.7z`/`.rar`
    // whose game folder is read in place (`archive_tree`).
    let remote = ps5upload_fpkg::remote_source::is_remote(&source);
    let image_file = crate::image_build::is_image_file(&source);
    let archive =
        crate::archive_tree::reads_in_place(&source).then(|| crate::archive_tree::ArchiveSource {
            password: req.archive_password.take().filter(|p| !p.is_empty()),
            cancel: Default::default(),
        });
    if !remote
        && !image_file
        && archive.is_none()
        && !tokio::fs::metadata(&source).await.is_ok_and(|m| m.is_dir())
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a game folder or image", source.display()),
        )
        .into_response();
    }
    // A folder on this computer has to be a game: param.json (PS5) or param.sfo (PS4) in its
    // sce_sys (#596). Otherwise any folder became an image that mounts but never launches.
    if !remote && !image_file && archive.is_none() && !has_title_param(&source).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            missing_param_message(&source, false),
        )
        .into_response();
    }
    let update = update_path(req.update.as_deref());
    // An update is merged into a game read by path: an archive's game is unpacked for it.
    if archive.is_some() && update.is_some() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!(
                "{}: an update is merged into a game unpacked from its archive first",
                crate::archive_tree::NEEDS_UNPACK
            ),
        )
        .into_response();
    }
    // An edited copy (#479): the image on this computer, opened with the changes applied, is
    // written out again as the same kind of image.
    let edited = if req.edits.is_empty() && !req.rebuild_ampr_index {
        None
    } else {
        if update.is_some() {
            return json_err(
                StatusCode::BAD_REQUEST,
                "an update is merged into a game being converted, not into an edited copy",
            )
            .into_response();
        }
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
        if req.remove_backport {
            return json_err(
                StatusCode::BAD_REQUEST,
                "an edited copy does not remove the backport: delete its files instead",
            )
            .into_response();
        }
        // The changes go into the image itself (image_edit_in_place.rs), or are refused there
        // with the reason (the UI then offers the edited copy).
        if req.in_place {
            if req.rebuild_ampr_index || req.destination.is_some() || req.delete_source {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    "an in-place edit takes only the changes (and full_verify)",
                )
                .into_response();
            }
            let edits = req.edits.iter().map(ImageEditReq::edit).collect();
            return crate::image_edit_in_place::start(state, source, edits, req.full_verify).await;
        }
        let (edit_source, edits) = (source.clone(), std::mem::take(&mut req.edits));
        let index_only = req.rebuild_ampr_index && edits.is_empty();
        match tokio::task::spawn_blocking(move || {
            if index_only {
                crate::ampr_index_api::open_for_index(&edit_source)
            } else {
                open_edited(&edit_source, &edits)
            }
        })
        .await
        {
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
    // A source that goes afterwards is replaced by this image: every byte of it is read back.
    let mode = ps5upload_fpkg::sample::VerifyMode::from_full(
        req.full_verify || req.delete_source,
        format == crate::image_build::ImageFormat::Ffpkg,
    );
    let packs = match &req.ampr_lz4 {
        None => None,
        Some(r) => match r.options() {
            Ok(p) => Some(p),
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        },
    };
    let zstd_level =
        match &req.ampr_zstd {
            None => None,
            Some(_) if packs.is_some() || edited.is_some() => return json_err(
                StatusCode::BAD_REQUEST,
                "zstd asset packs are one choice: not with LZ4 packs, and not for an edited copy",
            )
            .into_response(),
            Some(r) => match r.level() {
                Ok(level) => Some((level, r.runtime)),
                Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
            },
        };
    if req.unpack_packs && (packs.is_some() || zstd_level.is_some() || edited.is_some()) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "unpacking the asset packs writes plain files: not with LZ4 or zstd packs, and not \
             for an edited copy",
        )
        .into_response();
    }
    let unpack = req
        .unpack_packs
        .then(|| open_unpacked(req.add_ampr_emu, req.ampr_emu_build.clone()));
    // A zstd pack profile: which files stay loose. Refused here when it does not parse.
    let zstd_rules = match req.ampr_zstd.as_ref().map(AmprZstdReq::rules) {
        Some(Ok(r)) => r,
        Some(Err(e)) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        None => None,
    };
    // Straight to a console: (its address, the folder there). Straight to a server: the folder
    // there.
    let mut to_server: Option<crate::server_out::ServerDest> = None;
    let to_console = match req.destination.take() {
        None | Some(ImageDestinationReq::Local) => None,
        Some(ImageDestinationReq::Server { source_id, dir }) => {
            if let Some(why) = server_refusal(edited.is_some()) {
                return json_err(StatusCode::BAD_REQUEST, why).into_response();
            }
            match server_dest(&source_id, &dir) {
                Ok(d) => to_server = Some(d),
                Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
            }
            None
        }
        Some(ImageDestinationReq::Console { addr, dir }) => {
            if let Some(why) = console_refusal(edited.is_some()) {
                return json_err(StatusCode::BAD_REQUEST, why).into_response();
            }
            let dir = match crate::link::valid_console_dir(&dir) {
                Ok(d) => d,
                Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
            };
            let Some(addr) = crate::required_console_addr(Some(addr)) else {
                return crate::missing_console_addr();
            };
            Some((addr, dir))
        }
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
        // An archive's image is named after the archive, without `.zip` or `.partN.rar`.
        None if archive.is_some() => PathBuf::from(crate::archive_tree::stem(&source)),
        None => source.clone(),
    };
    // An edited copy goes next to its source unless told otherwise.
    let out_dir = match (&edited, req.output_dir.as_deref().map(str::trim)) {
        (Some(_), None | Some("")) => source
            .parent()
            .map_or_else(default_output_dir, Path::to_path_buf),
        _ => output_dir(req.output_dir.as_deref()),
    };
    // Straight to a console or a server, nothing is written on this computer.
    let straight = to_console.is_some() || to_server.is_some();
    // On a console or a server, a name already taken is refused when the job starts (it is
    // asked there).
    let named = if req.keep_both && !straight {
        let taken = |stem: &str| {
            let (name, _) = crate::image_build::output_names(Path::new(stem), format, compress);
            crate::folder_build::name_in_use(&out_dir.join(name))
        };
        crate::folder_build::keep_both_stem(&crate::image_build::stem_of(&named), taken)
            .map_or(named, PathBuf::from)
    } else {
        named
    };
    let (out_name, inner_name) = crate::image_build::output_names(&named, format, compress);
    // On a console: (its address, the image's path there).
    let to_console = to_console.map(|(addr, dir)| (addr, format!("{dir}/{out_name}")));
    let output = out_dir.join(&out_name);
    if !straight
        && !remote
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
    if !straight && tokio::fs::try_exists(&output).await.unwrap_or(false) {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", output.display()),
        )
        .into_response();
    }
    if straight {
        // Nothing is written on this computer.
    } else if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", out_dir.display()),
        )
        .into_response();
    }
    // "Move the source to the Trash once the output is verified": refused here, with the
    // reason, when it could not be honoured, and what the source is now is noted before
    // anything reads it.
    let removal = if req.delete_source {
        let why = if edited.is_some() {
            Some("an edited copy never removes the image it was made from")
        } else if straight {
            Some(
                "the image goes straight to a PS5 or a server, and a source is only removed \
                 for an image this computer keeps",
            )
        } else if update.is_some() {
            Some("with an update merged in there are two sources: remove them yourself")
        } else if archive.is_some() && crate::archive_tree::multi_part(&source) {
            Some("the archive is one part of a multi-part set: remove its parts yourself")
        } else {
            None
        };
        if let Some(why) = why {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("the source can't be moved to the Trash: {why}"),
            )
            .into_response();
        }
        match plan_source_removal(&source, &output).await {
            Ok(plan) => Some(plan),
            Err((status, why)) => return json_err(status, why).into_response(),
        }
    } else {
        None
    };
    // The job's id and Stop exist before the source is read: a solid archive's scan can decode
    // gigabytes, and a scan that outlasts `SCAN_ANSWER_WITHIN` goes on as the job (see
    // `answer_or_hand_off`), stopped by Stop like the rest of it.
    let job_id = Uuid::new_v4();
    // Straight to a console the job can be cancelled, not paused: a writer held for minutes
    // would leave the console's job without data until it gives up (AVA1's stall limit).
    let cancel = if straight {
        crate::register_transfer_cancel(job_id)
    } else {
        register_workshop_job(job_id)
    };
    // A read waiting on the archive (a long skip through solid data) stops with the job.
    if let Some(a) = &archive {
        a.cancel.set(cancel.clone());
    }
    let handoff_state = state.clone();
    let rest = async move {
        let scan_source = source.clone();
        let check_packs = packs.is_some();
        let install_runtime = req.ampr_lz4.as_ref().is_some_and(|r| r.install_runtime);
        let add_ampr_emu = req.add_ampr_emu;
        let ampr_emu_build = req.ampr_emu_build.clone();
        let edited_tree = edited.map(|e| e.tree);
        let scan_update = update.clone();
        let remove_backport = req.remove_backport;
        let scan_archive = archive.clone();
        // What the source's reader left out (`SourceTree::notes`): it keeps a source that goes.
        let source_notes: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let scan_notes = source_notes.clone();
        let scanned = tokio::task::spawn_blocking(move || {
            // An edited copy keeps the image's own names: only the user's changes are made.
            if let Some(tree) = edited_tree {
                return Ok(tree);
            }
            if let Some(why) = patch_refusal(&scan_source) {
                return Err(why);
            }
            // An image an in-place edit was interrupted in is settled before it is read.
            if image_file && !remote {
                crate::image_edit_in_place::before_open(&scan_source)?;
            }
            // A UFS2 image (.ffpkg) is case-sensitive like /data: give modules the eboot's spelling.
            // A PS4 update goes into its game here (the merge checks they go together).
            let (mut tree, renamed, signing) =
                open_game(&scan_source, scan_update.as_deref(), scan_archive.as_ref())?;
            if scan_archive.is_some() {
                crate::log_at!(Info, "convert", "image build: {}", tree.describe());
            }
            *scan_notes.lock().unwrap_or_else(|e| e.into_inner()) = tree.notes();
            match &scan_update {
                Some(_) => crate::log_at!(Info, "convert", "image build: {}", tree.describe()),
                None => {
                    if let Some(why) = ps4_refusal(&scan_source, &mut *tree, true) {
                        return Err(why);
                    }
                }
            }
            if !renamed.is_empty() {
                crate::log_at!(
                    Info,
                    "convert",
                    "image build: renamed to the eboot's spelling: {}",
                    ps5upload_fpkg::module_case::describe(&renamed)
                );
            }
            if let Some(line) = signing.describe() {
                crate::log_at!(Info, "convert", "image build: {line}");
            }
            if let Some(open) = &unpack {
                // The packs decoded: the image holds the plain game.
                tree = open(tree)?;
            }
            if zstd_level.is_some() {
                // A packed game goes straight into the new packs.
                tree = crate::zstd_packs::readable_for_packing(tree)?;
            } else if check_packs && ps5_dump_forge_zstd::reader::detect(&*tree) {
                // The other way: the zstd packs decoded, for LZ4 packs to be made from them.
                tree = Box::new(
                    ps5_dump_forge_zstd::reader::unpack(tree)
                        .map_err(|e| format!("reading the game's zstd packs: {e}"))?,
                );
            }
            // After any packs were decoded (their own list names the backport's files) and before
            // an emulator or new packs are added to what is left.
            if remove_backport {
                let libs = crate::remove_backport::removable(&mut *tree)?;
                tree = crate::remove_backport::without(tree, &libs);
            }
            if !check_packs && zstd_level.is_none() && add_ampr_emu {
                // Some libSceAmpr games with no emulator fail to start from an image, and
                // ShadowMount+ doesn't add one. Packs have their own choice below.
                if let Some(module) =
                    ps5upload_fpkg::ampr_pack::image::emulator_needed_at(&mut *tree)
                {
                    // A game written in the packs it is in gets the emulator that reads them,
                    // whichever was asked for (#828).
                    let packs = crate::remove_backport::packed(&mut *tree);
                    let build =
                        crate::ampr_trace::emulator_for_packs(packs, ampr_emu_build.as_deref());
                    if build.unwrap_or("ampr_emu")
                        != ampr_emu_build.as_deref().unwrap_or("ampr_emu")
                    {
                        crate::log_at!(
                            Info,
                            "convert",
                            "image build: the game stays in {} packs, so it gets {}",
                            packs.unwrap_or_default(),
                            build.unwrap_or("ampr_emu")
                        );
                    }
                    let bytes = crate::ampr_trace::emulator(build).map_err(|e| {
                        format!(
                            "this game may need an AMPR emulator to start from an image, and it \
                         could not be fetched: {e}. Connect to the internet, or turn off \
                         \"Add the AMPR emulator\" in Convert's options"
                        )
                    })?;
                    tree = Box::new(
                        ps5upload_fpkg::ampr_pack::image::RuntimeInstalled::new_at(
                            tree, bytes, &module,
                        )
                        .map_err(|e| e.to_string())?,
                    );
                    crate::log_at!(
                        Info,
                        "convert",
                        "image build: {} added as {module}",
                        build.unwrap_or("ampr_emu")
                    );
                }
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
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
                    .into_response()
            }
        };
        // Stopped while the source was read (a scan that does not read through the archive
        // cannot be interrupted): nothing starts.
        if cancel.load(Ordering::Relaxed) {
            return json_err(StatusCode::BAD_REQUEST, "build cancelled").into_response();
        }
        let total: u64 = tree.files().iter().map(|f| f.size).sum();
        if tree.files().is_empty() {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("{} has no files in it", source.display()),
            )
            .into_response();
        }
        // An uncompressed image is at least as large as the game: refused before the first write
        // rather than filling the drive and failing near the end. A compressed one or LZ4 packs may
        // well fit in less, so the screen warns and it goes ahead.
        // Straight to a console, the console's own room is checked once the image's size is planned.
        let partial_out = PathBuf::from(format!("{}.partial", output.display()));
        if !straight && !compress && packs.is_none() && zstd_level.is_none() {
            // What a build that stopped part way left is room too: the build goes on from it or
            // clears it. Counted as taken, a retry of a 100 GB image stopped at 60 GB was refused
            // for the 60 GB its own partial held, every time, until that file was deleted by hand.
            let held = leftover_bytes(&partial_out);
            if let Some(why) = build::free_bytes(&out_dir).and_then(|free| {
                crate::folder_build::shortfall(free.saturating_add(held), total, &out_dir)
            }) {
                return json_err(StatusCode::BAD_REQUEST, why).into_response();
            }
        }

        // Two builds into one console path would wreck each other's job, as two into one file would.
        let claimed = match (&to_console, &to_server) {
            (Some((addr, path)), _) => PathBuf::from(format!("ps5://{addr}{path}")),
            (None, Some(d)) => PathBuf::from(d.url(&d.path_of(&out_name))),
            (None, None) => partial_out.clone(),
        };
        let Some(writing) = claim_off_thread(claimed.clone()).await else {
            return already_building(&claimed);
        };
        // LZ4 packs are spooled beside the image. Straight to a console or a server they are made
        // as they are sent; the engine's data folder (one spool per destination) is the fallback.
        let spool = if straight {
            crate::image_build::stream_spool(&claimed)
        } else {
            crate::image_build::lz4_spool(&output)
        };
        let spooling = match straight && packs.is_none() {
            true => None,
            false => {
                let Some(s) = claim_off_thread(spool.clone()).await else {
                    return already_building(&spool);
                };
                Some(s)
            }
        };

        crate::workshop_api::note_run(job_id, "image", &source);
        let scaffold = Scaffold::of(&source);
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
                        let state = crate::event_bus::tick_wanted(&events_tx)
                            .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                            .flatten();
                        drop(g);
                        if let Some(state) = state {
                            let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": crate::with_live_notes(job_id, state) });
                            let _ = events_tx.send(msg.to_string());
                        }
                    }
                    _ => break,
                }
            }
        });

        let state_for_job = state.clone();
        let ticker = JobTicker(ticker.abort_handle());
        spawn_workshop_job(move || {
            // Released last: an in-place edit may claim the source once this job is over.
            let _reading = reading;
            // First in, last out: gone only after everything reading it has been dropped.
            let _scaffold = scaffold;
            // A panic in the work below must not leave the job "running" forever with its
            // ticker looping: both guards act on any exit, `fail` unless a result was recorded.
            let mut fail = JobFailOnDropGuard::new(
                state_for_job.jobs.clone(),
                state_for_job.events_tx.clone(),
                job_id,
                started_at_ms,
            );
            if !straight {
                sweep_output_leftovers(&out_dir);
            } else if packs.is_some() {
                // What killed builds left in the data folder's spools, none of them running.
                if let Some(dir) = spool.parent() {
                    sweep_output_leftovers(dir);
                }
            }
            let mut tree = tree;
            // With LZ4 packs the build packs first; the stage list says so. A `.ffpfsc` straight to
            // a console or a server is compressed once to measure it before it is written.
            let measured = straight && compress;
            let stages: &[&str] = match (packs.is_some() || zstd_level.is_some(), measured) {
                (true, true) => &["pack", "plan", "compress", "write", "verify"],
                (true, false) => &["pack", "plan", "write", "verify"],
                (false, true) => &["plan", "compress", "write", "verify"],
                (false, false) => &["plan", "write", "verify"],
            };
            // The job's bytes are the whole build's (see `overall_progress`); the stage's own are in
            // its `stage`.
            let job_total = total;
            let mut report = |id: &str, done: u64, total: u64| {
                let index = stages
                    .iter()
                    .position(|s| *s == id || (id == "compress" && *s == "write"))
                    .unwrap_or(0) as u32;
                bytes.fetch_max(
                    overall_progress(job_total, index, stages.len() as u32, done, total),
                    Ordering::Relaxed,
                );
                *current.lock().unwrap_or_else(|e| e.into_inner()) = Some(JobStage {
                    id: id.to_string(),
                    index,
                    count: stages.len() as u32,
                    done,
                    total,
                });
            };
            // A PS4 game's image needs the param.json ShadowMount+ reads, which only PS5 games ship.
            let mut ps4 = ps5upload_fpkg::ps4_param::Ps4Params::wrap(&mut *tree);
            let source_tree: Result<&mut dyn ps5upload_fpkg::source::SourceTree, String> =
                match ps4.as_mut() {
                    Ok(Some(p)) => Ok(p),
                    Ok(None) => Ok(&mut *tree),
                    Err(e) => Err(format!("param.sfo: {e}")),
                };
            // zstd packs: the emulator that goes in, fetched here (off the request) and recorded.
            let mut runtime = None;
            // Straight to a console or a server in a `.ffpfsc`: the image's name inside it.
            let ffpfsc = inner_name
                .as_deref()
                .map(|inner| crate::image_build::FfpfscOut { inner, level });
            // Straight to a console: its own job, failing with the transfer's reasons.
            let ended = match (source_tree, &to_console) {
                (Ok(tree), None) if to_server.is_some() => Ended::Server(match &to_server {
                    Some(dest) => to_server_job(
                        dest,
                        &out_name,
                        format,
                        ffpfsc,
                        zstd_level,
                        zstd_rules.as_deref(),
                        &mut runtime,
                        packs.as_ref().map(|p| (p, spool.as_path())),
                        tree,
                        mode,
                        verify_chunk,
                        &cancel,
                        &mut report,
                    ),
                    None => Err("no server".into()),
                }),
                (Err(e), None) if to_server.is_some() => Ended::Server(Err(e)),
                (Ok(tree), Some((addr, path))) => Ended::Console(to_console_job(
                    addr,
                    path,
                    format,
                    ffpfsc,
                    zstd_level,
                    zstd_rules.as_deref(),
                    &mut runtime,
                    packs.as_ref().map(|p| (p, spool.as_path())),
                    tree,
                    mode,
                    verify_chunk,
                    &cancel,
                    job_id,
                    &mut report,
                )),
                (Err(e), Some(_)) => Ended::Console(Err(anyhow::anyhow!(e))),
                (source_tree, None) => Ended::Local(match (source_tree, &packs) {
                    (Err(e), _) => Err(e),
                    (Ok(tree), None) if zstd_level.is_some() => {
                        // The .ffpfsc's own level, before the zstd level takes the name.
                        let level_ffpfsc = level;
                        let (level, choice) = zstd_level.unwrap_or_default();
                        crate::zstd_runtime::pack_runtime(choice).and_then(|r| {
                            crate::log_at!(
                                Info,
                                "convert",
                                "zstd packs: {} goes into {}",
                                r.describe(),
                                output.display()
                            );
                            crate::zstd_packs::build(
                                format,
                                inner_name.as_deref(),
                                level_ffpfsc,
                                level,
                                zstd_rules.as_deref(),
                                runtime.insert(r),
                                tree,
                                &output,
                                mode,
                                verify_chunk,
                                &cancel,
                                &mut report,
                            )
                        })
                    }
                    (Ok(tree), Some(packs)) => crate::image_build::build_packed(
                        format,
                        inner_name.as_deref(),
                        level,
                        tree,
                        &output,
                        packs,
                        mode,
                        verify_chunk,
                        &cancel,
                        &mut report,
                    ),
                    // A libSceAmpr game gets a fresh ampr_emu.index (#449), as a package does.
                    // Any image without packs can go on after a crash: same files, same bytes, as long as
                    // the regenerated index and a PFS image keep their time.
                    (Ok(tree), None) => {
                        let stamp = crate::image_build::index_stamp(&partial_out);
                        match crate::image_build::AmprIndexed::wrap_at(&mut *tree, stamp) {
                            Ok(Some(mut indexed)) => crate::image_build::build_resumable(
                                format,
                                inner_name.as_deref(),
                                level,
                                stamp,
                                &mut indexed,
                                &output,
                                mode,
                                verify_chunk,
                                &cancel,
                                &mut report,
                            ),
                            Ok(None) => crate::image_build::build_resumable(
                                format,
                                inner_name.as_deref(),
                                level,
                                stamp,
                                tree,
                                &output,
                                mode,
                                verify_chunk,
                                &cancel,
                                &mut report,
                            ),
                            Err(e) => Err(format!("ampr_emu.index: {e}")),
                        }
                    }
                }),
            };
            drop(writing);
            drop(spooling);
            if matches!(ended, Ended::Local(Err(_))) {
                sweep_output_leftovers(&out_dir);
            }
            let completed_at_ms = now_ms();
            drop(ticker);
            let outcome = match ended {
                Ended::Local(outcome) => outcome,
                Ended::Server(written) => {
                    let url = to_server
                        .as_ref()
                        .map_or_else(String::new, |d| d.url(&d.path_of(&out_name)));
                    let state = match written {
                        Ok(s) => {
                            crate::engine_log::record(
                            "info",
                            format!(
                                "image: {} -> {url} ({} files, {} bytes, {}, read back on the server){}",
                                source.display(),
                                s.built.files,
                                s.built.image_bytes,
                                s.built.detail,
                                runtime
                                    .as_ref()
                                    .map(|r| format!("; emulator {}", r.describe()))
                                    .unwrap_or_default()
                            ),
                        );
                            let mut ack = serde_json::json!({
                                "to_server": true,
                                "resumed_from": s.resumed_from,
                            });
                            if let (Some(o), Some(r)) = (ack.as_object_mut(), runtime.as_ref()) {
                                o.insert("ampr_runtime".into(), serde_json::json!(r));
                            }
                            JobState::Done {
                                started_at_ms,
                                completed_at_ms,
                                elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                                tx_id_hex: String::new(),
                                bytes_sent: s.built.image_bytes,
                                dest: url,
                                files_sent: s.built.files,
                                skipped_files: 0,
                                skipped_bytes: 0,
                                commit_ack: Some(ack),
                            }
                        }
                        Err(error) => {
                            crate::engine_log::record(
                                "warn",
                                format!("image: to the server failed: {error}"),
                            );
                            JobState::Failed {
                                started_at_ms,
                                completed_at_ms,
                                elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                                error,
                                error_reason: None,
                                error_detail: None,
                                error_console: None,
                                error_space: None,
                            }
                        }
                    };
                    set_job(&state_for_job.jobs, &state_for_job.events_tx, job_id, state);
                    fail.mark_succeeded();
                    return;
                }
                Ended::Console(streamed) => {
                    let path = to_console.as_ref().map_or("", |(_, p)| p.as_str());
                    let state = match streamed {
                        Ok(s) => {
                            crate::engine_log::record(
                            "info",
                            format!(
                                "image: {} -> PS5 {path} ({} files, {} bytes, {}, read back on the PS5){}",
                                source.display(),
                                s.built.files,
                                s.built.image_bytes,
                                s.built.detail,
                                runtime
                                    .as_ref()
                                    .map(|r| format!("; emulator {}", r.describe()))
                                    .unwrap_or_default()
                            ),
                        );
                            let mut ack: serde_json::Value =
                                serde_json::from_str(&s.transfer.commit_ack_body)
                                    .unwrap_or_else(|_| serde_json::json!({}));
                            if let (Some(o), Some(r)) = (ack.as_object_mut(), runtime.as_ref()) {
                                o.insert("ampr_runtime".into(), serde_json::json!(r));
                            }
                            if let Some(o) = ack.as_object_mut() {
                                o.insert("to_console".into(), true.into());
                                o.insert("passes".into(), s.passes.into());
                            }
                            JobState::Done {
                                started_at_ms,
                                completed_at_ms,
                                elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                                tx_id_hex: s.transfer.tx_id_hex,
                                bytes_sent: s.built.image_bytes,
                                dest: path.to_string(),
                                files_sent: s.built.files,
                                skipped_files: 0,
                                skipped_bytes: 0,
                                commit_ack: Some(ack),
                            }
                        }
                        Err(e) => {
                            crate::engine_log::record(
                                "warn",
                                format!("image: to the PS5 failed: {e:#}"),
                            );
                            crate::job_failed_from_err(started_at_ms, completed_at_ms, &e)
                        }
                    };
                    set_job(&state_for_job.jobs, &state_for_job.events_tx, job_id, state);
                    fail.mark_succeeded();
                    return;
                }
            };
            match outcome {
                Ok(built) => {
                    built_packages()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(output.clone());
                    let removal = match &removal {
                        Some(plan) => {
                            // The source's own files are closed before it is moved away. (The
                            // PS4 wrapper above only borrowed the tree, and is done with it.)
                            drop(tree);
                            let notes = std::mem::take(
                                &mut *source_notes.lock().unwrap_or_else(|e| e.into_inner()),
                            );
                            Some(remove_source(
                                plan,
                                &output,
                                &state_for_job,
                                job_id,
                                "image",
                                &notes,
                            ))
                        }
                        None => None,
                    };
                    crate::engine_log::record(
                        "info",
                        format!(
                            "image: {} -> {} ({} files, {} bytes, {}, read back){}",
                            source.display(),
                            output.display(),
                            built.files,
                            built.image_bytes,
                            built.detail,
                            runtime
                                .as_ref()
                                .map(|r| format!("; emulator {}", r.describe()))
                                .unwrap_or_default()
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
                            commit_ack: crate::source_removal::ack(
                                runtime.as_ref().map(runtime_ack),
                                removal.as_ref(),
                            ),
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
                            error_space: None,
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
    };
    answer_or_hand_off(&handoff_state, job_id, rest).await
}

#[derive(Deserialize)]
pub(crate) struct FolderBuildReq {
    /// A game image (`.exfat`, `.ffpkg`/`.ufs2`, `.ffpfs`, `.ffpfsc`) or a debug-key `.pkg`,
    /// on this computer, a saved server (`remote://`) or a console (`ps5://`); or a `.zip`,
    /// `.7z` or `.rar` on this computer holding one game folder, read in place as an image
    /// build reads it (and refused with `archive_needs_unpack` the same way).
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
    /// When the folder's name is taken in the output folder, add ` (2)` (and so on) instead of
    /// refusing (see `BuildReq::keep_both`).
    #[serde(default)]
    keep_both: bool,
    /// Write the game with its files in zstd asset packs for ampr-emu-ng, as an image build's
    /// `ampr_zstd` does, as a plain folder: the packs, the files that stay loose, and the
    /// emulator in `fakelib/libSceAmpr.sprx`. The source may then also be a game folder (on
    /// this computer, a saved server or a console). Not with `entry`.
    #[serde(default)]
    ampr_zstd: Option<AmprZstdReq>,
    /// Write the game with its files in AMPR LZ4 asset packs, as an image build's `ampr_lz4`
    /// does, as a plain folder: the pack volumes, their manifest, the files that stay loose and
    /// a fresh `ampr_emu.index`. The volumes are packed into `<folder>.lz4spool/` beside it
    /// first (for a folder on a server, into the engine's data folder), so packing needs that
    /// room too until the folder is written. The source may then also be a game folder. Not
    /// with `ampr_zstd`, `unpack_packs` or `entry`.
    #[serde(default)]
    ampr_lz4: Option<AmprLz4Req>,
    /// A PS4 update merged into the PS4 game `source`, as an image build's `update`; the game
    /// may then also be a folder. Not with `entry`.
    #[serde(default)]
    update: Option<String>,
    /// Where the folder goes: `{"kind":"local"}` (the default: `output_dir` on this computer)
    /// or `{"kind":"server","source_id":…,"dir":…}`: a folder on a saved server, every file
    /// written under a temporary name and renamed, the files a game starts from (`eboot.bin`,
    /// `sce_sys/param.json`) last and only once the folder reads back there. Not with `entry`;
    /// a console is not a destination here.
    #[serde(default)]
    destination: Option<ImageDestinationReq>,
    /// Leave the game's backport libraries out of the folder, as an image build's
    /// `remove_backport` does, with the same refusals. Not with `entry`.
    #[serde(default)]
    remove_backport: bool,
    /// Once the folder is written, read back in full and given its name, move `source` to this
    /// computer's Trash, as an image build's `delete_source` does and with the same refusals.
    /// Not with `entry`, `update` or a `destination` elsewhere.
    #[serde(default)]
    delete_source: bool,
    /// For a game in LZ4 or zstd asset packs: write it as plain files again, as an image
    /// build's `unpack_packs` does. The source may then also be a game folder. Not with
    /// `ampr_zstd` or `entry`.
    #[serde(default)]
    unpack_packs: bool,
    /// With `unpack_packs`: replace or add the game's AMPR emulator as an image build does.
    /// On unless turned off.
    #[serde(default = "default_add_ampr_emu")]
    add_ampr_emu: bool,
    /// With `unpack_packs`: which emulator, `ampr_emu` (the default) or `ampr-emu-ng`.
    #[serde(default)]
    ampr_emu_build: Option<String>,
    /// The password of a `.rar` `source` (an archive read in place); never logged or kept.
    #[serde(default)]
    archive_password: Option<String>,
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

/// `path` with links resolved, for a folder that may not exist yet: its nearest existing parent
/// resolved, the rest as given.
async fn resolved(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = tokio::fs::canonicalize(at).await {
            return Some(rest.iter().rev().fold(real, |p, c| p.join(c)));
        }
        rest.push(at.file_name()?.to_os_string());
        at = at.parent()?;
    }
}

/// The game at `source` with modules in the eboot's spelling and unsigned executables signed
/// (`module_case::open_with_update`), as every image or folder build and its read-back open it;
/// for an archive read in place (`archive`), the game folder inside it (an update is never
/// merged into one: that is refused before).
type OpenedGame = (
    Box<dyn ps5upload_fpkg::source::SourceTree>,
    Vec<ps5upload_fpkg::module_case::CaseFix>,
    ps5upload_fpkg::fself::Signing,
);

fn open_game(
    source: &Path,
    update: Option<&Path>,
    archive: Option<&crate::archive_tree::ArchiveSource>,
) -> Result<OpenedGame, String> {
    match archive {
        Some(a) => {
            let tree = crate::archive_tree::open(source, a.password.as_deref(), &a.cancel)?;
            Ok(ps5upload_fpkg::module_case::wrap(tree))
        }
        None => {
            ps5upload_fpkg::module_case::open_with_update(source, update).map_err(|e| e.to_string())
        }
    }
}

/// The game at `source` (a folder, image or package anywhere, or an archive read in place)
/// read for zstd packs: modules in the eboot's spelling, as an image or folder build reads it,
/// then [`crate::zstd_packs::readable_for_packing`]. A packed folder build's read-back opens it
/// the same way.
fn open_for_packing(
    source: &Path,
    update: Option<&Path>,
    archive: Option<&crate::archive_tree::ArchiveSource>,
) -> Result<(Box<dyn ps5upload_fpkg::source::SourceTree>, Vec<String>), String> {
    let (tree, renamed, signing) = open_game(source, update, archive)?;
    let notes = tree.notes();
    if !renamed.is_empty() {
        crate::log_at!(
            Info,
            "convert",
            "packed folder build: renamed to the eboot's spelling: {}",
            ps5upload_fpkg::module_case::describe(&renamed)
        );
    }
    if let Some(line) = signing.describe() {
        crate::log_at!(Info, "convert", "packed folder build: {line}");
    }
    crate::zstd_packs::readable_for_packing(tree).map(|t| (t, notes))
}

/// The game at `source` read for LZ4 packs, as an image build reads it: modules in the eboot's
/// spelling, and a game in zstd packs decoded so the LZ4 packs are made of its files. A packed
/// folder build's read-back opens it the same way.
fn open_for_lz4(
    source: &Path,
    update: Option<&Path>,
    archive: Option<&crate::archive_tree::ArchiveSource>,
) -> Result<(Box<dyn ps5upload_fpkg::source::SourceTree>, Vec<String>), String> {
    let (tree, renamed, signing) = open_game(source, update, archive)?;
    let notes = tree.notes();
    if !renamed.is_empty() {
        crate::log_at!(
            Info,
            "convert",
            "lz4 folder build: renamed to the eboot's spelling: {}",
            ps5upload_fpkg::module_case::describe(&renamed)
        );
    }
    if let Some(line) = signing.describe() {
        crate::log_at!(Info, "convert", "lz4 folder build: {line}");
    }
    if ps5_dump_forge_zstd::reader::detect(&*tree) {
        let unpacked: Box<dyn ps5upload_fpkg::source::SourceTree> = Box::new(
            ps5_dump_forge_zstd::reader::unpack(tree)
                .map_err(|e| format!("reading the game's zstd packs: {e}"))?,
        );
        return Ok((unpacked, notes));
    }
    Ok((tree, notes))
}

/// `tree` ready for LZ4 packs, or why it can't have them: the release ampr_emu put in when the
/// game's own is missing or too old and `install_runtime` asks for it (its own kept beside it),
/// then the game's emulator checked for pack support.
fn lz4_ready(
    mut tree: Box<dyn ps5upload_fpkg::source::SourceTree>,
    install_runtime: bool,
) -> Result<Box<dyn ps5upload_fpkg::source::SourceTree>, String> {
    use ps5upload_fpkg::ampr_pack::image as img;
    if install_runtime {
        let ready = img::readiness(&mut *tree);
        match ready.as_ref().map(|r| r.kind) {
            Some("runtime") => {
                let bytes = crate::ampr_trace::release_runtime()?;
                tree =
                    Box::new(img::RuntimeInstalled::new(tree, bytes).map_err(|e| e.to_string())?);
            }
            Some("ok") => {}
            _ => {
                return Err(ready
                    .and_then(|r| r.refusal)
                    .unwrap_or_else(|| "the game cannot use asset packs".into()))
            }
        }
    }
    match img::refusal(&mut *tree) {
        Some(why) => Err(why),
        None => Ok(tree),
    }
}

/// POST /api/folder/build — unpack a game image or a debug-key package into a plain folder
/// (#469). Runs as a job like an image build: stages `write` and `verify`, the folder written
/// as `<name>.partial/` and renamed only once it reads back. Done's `dest` is the folder. With
/// `ampr_zstd` the game's files go into zstd packs (stage `pack` first), from an image,
/// package or game folder; `commit_ack.ampr_runtime` names the emulator that went in. With
/// `ampr_lz4` they go into LZ4 packs the same way, read by the game's own ampr_emu.
pub(crate) async fn folder_build_handler(
    State(state): State<AppState>,
    Json(mut req): Json<FolderBuildReq>,
) -> impl IntoResponse {
    use crate::folder_build;
    let source = resolve_engine_path(&req.source);
    // Registered as reading its source from before its first read to the job's end: an
    // in-place edit of it can't start meanwhile, nor this while one runs.
    let reading = match crate::image_edit_in_place::Reading::take(&source) {
        Ok(r) => r,
        Err(why) => return json_err(StatusCode::CONFLICT, why).into_response(),
    };
    // Straight to a server: the folder there.
    let to_server = match req.destination.take() {
        None | Some(ImageDestinationReq::Local) => None,
        Some(ImageDestinationReq::Console { .. }) => {
            return json_err(
                StatusCode::BAD_REQUEST,
                "a game folder is saved to this computer or a server, not straight to the PS5:                  upload it once it is made",
            )
            .into_response()
        }
        Some(ImageDestinationReq::Server { source_id, dir }) => {
            if req
                .entry
                .as_deref()
                .is_some_and(|e| !e.trim().trim_matches('/').is_empty())
            {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    "one file or folder of an image is saved to this computer, not straight to a                      server",
                )
                .into_response();
            }
            match server_dest(&source_id, &dir) {
                Ok(d) => Some(d),
                Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
            }
        }
    };
    let remote = ps5upload_fpkg::remote_source::is_remote(&source);
    // A `.zip`, `.7z` or `.rar` on this computer: its game folder read in place.
    let archive =
        crate::archive_tree::reads_in_place(&source).then(|| crate::archive_tree::ArchiveSource {
            password: req.archive_password.take().filter(|p| !p.is_empty()),
            cancel: Default::default(),
        });
    if archive.is_some()
        && req
            .entry
            .as_deref()
            .is_some_and(|e| !e.trim().trim_matches('/').is_empty())
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            "one file or folder is saved from an image, not from an archive",
        )
        .into_response();
    }
    let zstd = match &req.ampr_zstd {
        None => None,
        Some(_)
            if req
                .entry
                .as_deref()
                .is_some_and(|e| !e.trim().trim_matches('/').is_empty()) =>
        {
            return json_err(
                StatusCode::BAD_REQUEST,
                "zstd packs are made of a whole game, not one file or folder of it",
            )
            .into_response()
        }
        Some(r) => match r.level() {
            Ok(level) => Some((level, r.runtime)),
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        },
    };
    let zstd_rules = match req.ampr_zstd.as_ref().map(AmprZstdReq::rules) {
        Some(Ok(r)) => r,
        Some(Err(e)) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        None => None,
    };
    let has_entry = req
        .entry
        .as_deref()
        .is_some_and(|e| !e.trim().trim_matches('/').is_empty());
    let lz4 = match &req.ampr_lz4 {
        None => None,
        Some(_) if has_entry => {
            return json_err(
                StatusCode::BAD_REQUEST,
                "LZ4 packs are made of a whole game, not one file or folder of it",
            )
            .into_response()
        }
        Some(_) if zstd.is_some() || req.unpack_packs => {
            return json_err(
                StatusCode::BAD_REQUEST,
                "LZ4 packs are one choice: not with zstd packs or unpacking the asset packs",
            )
            .into_response()
        }
        Some(r) => match r.options() {
            Ok(p) => Some(p),
            Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
        },
    };
    let install_runtime = req.ampr_lz4.as_ref().is_some_and(|r| r.install_runtime);
    if req.unpack_packs && (zstd.is_some() || has_entry) {
        return json_err(
            StatusCode::BAD_REQUEST,
            "unpacking the asset packs writes the whole game as plain files: not with zstd \
             packs, and not for one file or folder of it",
        )
        .into_response();
    }
    let unpack = req
        .unpack_packs
        .then(|| open_unpacked(req.add_ampr_emu, req.ampr_emu_build.clone()));
    let update = update_path(req.update.as_deref());
    // An update is merged into a game read by path: an archive's game is unpacked for it.
    if archive.is_some() && update.is_some() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!(
                "{}: an update is merged into a game unpacked from its archive first",
                crate::archive_tree::NEEDS_UNPACK
            ),
        )
        .into_response();
    }
    if update.is_some()
        && req
            .entry
            .as_deref()
            .is_some_and(|e| !e.trim().trim_matches('/').is_empty())
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            "an update is merged into a whole game, not one file or folder of it",
        )
        .into_response();
    }
    if req.remove_backport
        && req
            .entry
            .as_deref()
            .is_some_and(|e| !e.trim().trim_matches('/').is_empty())
    {
        return json_err(
            StatusCode::BAD_REQUEST,
            "the backport is removed from a whole game, not one file or folder of it",
        )
        .into_response();
    }
    let remove_backport = req.remove_backport;
    let local_dir = !remote && tokio::fs::metadata(&source).await.is_ok_and(|m| m.is_dir());
    // A game folder with an update merged in is a new folder, and so is one unpacked.
    if local_dir && zstd.is_none() && lz4.is_none() && update.is_none() && unpack.is_none() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is already a folder", source.display()),
        )
        .into_response();
    }
    // A game folder on this computer has to be a game, as for an image (#596).
    if local_dir && !has_title_param(&source).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            missing_param_message(&source, false),
        )
        .into_response();
    }
    // On a server or console only the name tells: a folder there has no image extension.
    let named_as_image = if remote {
        crate::image_build::has_image_extension(&source)
    } else {
        crate::image_build::is_image_file(&source)
    };
    // Packs are made of a game folder anywhere too, and so is a game with an update merged in
    // or its packs unpacked.
    if !named_as_image
        && archive.is_none()
        && !((zstd.is_some() || lz4.is_some() || update.is_some() || unpack.is_some())
            && (local_dir || remote))
    {
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
            // An archive's folder is named after the archive, without `.zip` or `.partN.rar`.
            None if archive.is_some() => (crate::archive_tree::stem(&source), false),
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
    // On a server, a name already taken is refused when the job starts (it is asked there).
    let name = if req.keep_both && to_server.is_none() {
        crate::folder_build::keep_both_stem(&name, |c| {
            crate::folder_build::name_in_use(&out_dir.join(c))
        })
        .unwrap_or(name)
    } else {
        name
    };
    let target = out_dir.join(&name);
    if to_server.is_none() && tokio::fs::try_exists(&target).await.unwrap_or(false) {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", target.display()),
        )
        .into_response();
    }
    // A `<name>.partial` that is not one of ours is the user's: never cleared, never used. A
    // saved file's `.partial` is a file, which cannot be told from a user's: it is refused too.
    let partial = folder_build::partial_of(&target);
    if to_server.is_none()
        && tokio::fs::try_exists(&partial).await.unwrap_or(false)
        && !folder_build::is_our_partial_dir(&partial)
    {
        return json_err(
            StatusCode::CONFLICT,
            format!("{} already exists", partial.display()),
        )
        .into_response();
    }
    // A game folder's packs never go inside it: the read-back would list them as the game's.
    if local_dir && to_server.is_none() {
        if let (Ok(src), Some(out)) = (
            tokio::fs::canonicalize(&source).await,
            resolved(&out_dir).await,
        ) {
            if out.starts_with(&src) {
                return json_err(
                    StatusCode::BAD_REQUEST,
                    "the new folder would be written inside the game folder: pick another \
                     output folder",
                )
                .into_response();
            }
        }
    }
    if to_server.is_some() {
        // Nothing is written on this computer.
    } else if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{}: {e}", out_dir.display()),
        )
        .into_response();
    }
    // As for an image: refused here when the source could not go afterwards.
    let removal = if req.delete_source {
        let why = if entry.is_some() {
            Some("saving one file or folder of an image does not remove the image")
        } else if archive.is_some() && crate::archive_tree::multi_part(&source) {
            Some("the archive is one part of a multi-part set: remove its parts yourself")
        } else if to_server.is_some() {
            Some(
                "the folder goes straight to a server, and a source is only removed for a \
                 folder this computer keeps",
            )
        } else if update.is_some() {
            Some("with an update merged in there are two sources: remove them yourself")
        } else {
            None
        };
        if let Some(why) = why {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("the source can't be moved to the Trash: {why}"),
            )
            .into_response();
        }
        match plan_source_removal(&source, &target).await {
            Ok(plan) => Some(plan),
            Err((status, why)) => return json_err(status, why).into_response(),
        }
    } else {
        None
    };
    // As for an image: the job's id and Stop exist before the source is read, and a long scan
    // goes on as the job (`answer_or_hand_off`).
    let job_id = Uuid::new_v4();
    // To a server the job can be cancelled, not paused: a write held for minutes would leave
    // the server's connection idle until it gives up.
    let cancel = if to_server.is_some() {
        crate::register_transfer_cancel(job_id)
    } else {
        register_workshop_job(job_id)
    };
    // A read waiting on the archive (a long skip through solid data) stops with the job; the
    // read-back's own opening of it shares the link.
    if let Some(a) = &archive {
        a.cancel.set(cancel.clone());
    }
    let handoff_state = state.clone();
    let rest = async move {
        let scan_source = source.clone();
        let scan_entry = entry.clone();
        let scan_update = update.clone();
        let packing = zstd.is_some();
        let lz4_packing = lz4.is_some();
        // The backport libraries left out, found once: the read-back leaves the same ones out.
        let left_out: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let scan_left_out = left_out.clone();
        let scan_unpack = unpack.clone();
        let scan_archive = archive.clone();
        // What the source's reader left out (`SourceTree::notes`): it keeps a source that goes.
        let source_notes: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        let scan_notes = source_notes.clone();
        let scanned = tokio::task::spawn_blocking(move || {
            // An image an in-place edit was interrupted in is settled before it is read.
            settle_local_source(&scan_source)?;
            let noted = |tree: &dyn ps5upload_fpkg::source::SourceTree| {
                *scan_notes.lock().unwrap_or_else(|e| e.into_inner()) = tree.notes();
            };
            if let Some(entry) = scan_entry {
                // A part of the image is saved as the image spells it, as the browser listed it.
                return open_entry(&scan_source, &entry);
            }
            // `tree` (packs already decoded when new ones are made) without its backport.
            let strip = |mut tree: Box<dyn ps5upload_fpkg::source::SourceTree>| {
                if !remove_backport {
                    return Ok(tree);
                }
                let libs = crate::remove_backport::removable(&mut *tree)?;
                let tree = crate::remove_backport::without(tree, &libs);
                *scan_left_out.lock().unwrap_or_else(|e| e.into_inner()) = libs;
                Ok::<_, String>(tree)
            };
            if packing {
                // Packs are made of a whole game.
                if let Some(why) = patch_refusal(&scan_source) {
                    return Err(why);
                }
                let (mut tree, notes) =
                    open_for_packing(&scan_source, scan_update.as_deref(), scan_archive.as_ref())?;
                *scan_notes.lock().unwrap_or_else(|e| e.into_inner()) = notes;
                if scan_update.is_none() {
                    if let Some(why) = ps4_refusal(&scan_source, &mut *tree, false) {
                        return Err(why);
                    }
                }
                return Ok(Unpacked::Tree(strip(tree)?));
            }
            if lz4_packing {
                // LZ4 packs too are made of a whole game, with its own ampr_emu (or the release one).
                if let Some(why) = patch_refusal(&scan_source) {
                    return Err(why);
                }
                let (mut tree, notes) =
                    open_for_lz4(&scan_source, scan_update.as_deref(), scan_archive.as_ref())?;
                *scan_notes.lock().unwrap_or_else(|e| e.into_inner()) = notes;
                if scan_update.is_none() {
                    if let Some(why) = ps4_refusal(&scan_source, &mut *tree, false) {
                        return Err(why);
                    }
                }
                return Ok(Unpacked::Tree(lz4_ready(strip(tree)?, install_runtime)?));
            }
            // An image ignores letter case and /data does not: a Unity dump whose
            // Il2cppUserAssemblies.prx the eboot loads as Il2CppUserAssemblies.prx plays from the
            // image and crashes a second in from the folder. The folder takes the eboot's spelling.
            // A PS4 update goes into its game here (the merge checks they go together).
            let (mut tree, renamed, signing) =
                open_game(&scan_source, scan_update.as_deref(), scan_archive.as_ref())?;
            if scan_archive.is_some() {
                crate::log_at!(Info, "convert", "folder build: {}", tree.describe());
            }
            match &scan_update {
                Some(_) => crate::log_at!(Info, "convert", "folder build: {}", tree.describe()),
                None => {
                    if let Some(why) = ps4_refusal(&scan_source, &mut *tree, false) {
                        return Err(why);
                    }
                }
            }
            if !renamed.is_empty() {
                crate::log_at!(
                    Info,
                    "convert",
                    "folder build: renamed to the eboot's spelling: {}",
                    ps5upload_fpkg::module_case::describe(&renamed)
                );
            }
            if let Some(line) = signing.describe() {
                crate::log_at!(Info, "convert", "folder build: {line}");
            }
            // A patch package unpacks to the files it carries whole; the log says what it left.
            for note in tree.notes() {
                crate::log_at!(Info, "convert", "folder build: {note}");
            }
            noted(&*tree);
            if let Some(open) = &scan_unpack {
                // The packs decoded: the folder holds the plain game.
                tree = open(tree)?;
            }
            Ok::<_, String>(Unpacked::Tree(strip(tree)?))
        })
        .await;
        let unpacked = match scanned {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
            Err(e) => {
                return json_err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
                    .into_response()
            }
        };
        // Stopped while the source was read (a scan that does not read through the archive
        // cannot be interrupted): nothing starts.
        if cancel.load(Ordering::Relaxed) {
            return json_err(StatusCode::BAD_REQUEST, "build cancelled").into_response();
        }
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
        // Packs may well fit in less, as for an image: no refusal before the first write. On a
        // server, the server's own room is asked once the job starts.
        if let Some(why) = build::free_bytes(&out_dir)
            .and_then(|free| crate::folder_build::shortfall(free, total, &out_dir))
            .filter(|_| zstd.is_none() && lz4.is_none() && to_server.is_none())
        {
            return json_err(StatusCode::BAD_REQUEST, why).into_response();
        }
        // Two builds into one server folder would wreck each other, as two into one folder here.
        let claimed = match &to_server {
            Some(d) => PathBuf::from(d.url(&d.path_of(&name))),
            None => partial.clone(),
        };
        let Some(writing) = claim_off_thread(claimed.clone()).await else {
            return already_building(&claimed);
        };
        // LZ4 packs are spooled beside the folder first, or, for a folder on a server, in the
        // engine's data folder.
        let spool = match &to_server {
            Some(_) => crate::image_build::stream_spool(&claimed),
            None => crate::image_build::lz4_spool(&target),
        };
        let spooling = match lz4.is_some() {
            false => None,
            true => {
                let Some(s) = claim_off_thread(spool.clone()).await else {
                    return already_building(&spool);
                };
                Some(s)
            }
        };
        // A source that goes afterwards is replaced by this folder: every file is compared in full.
        let mode = ps5upload_fpkg::sample::VerifyMode::from_full(
            req.full_verify || req.delete_source,
            false,
        );

        crate::workshop_api::note_run(job_id, "folder", &source);
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
                        let state = crate::event_bus::tick_wanted(&events_tx)
                            .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                            .flatten();
                        drop(g);
                        if let Some(state) = state {
                            let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": crate::with_live_notes(job_id, state) });
                            let _ = events_tx.send(msg.to_string());
                        }
                    }
                    _ => break,
                }
            }
        });

        let state_for_job = state.clone();
        let ticker = JobTicker(ticker.abort_handle());
        spawn_workshop_job(move || {
            // Released last: an in-place edit may claim the source once this job is over.
            let _reading = reading;
            let mut fail = JobFailOnDropGuard::new(
                state_for_job.jobs.clone(),
                state_for_job.events_tx.clone(),
                job_id,
                started_at_ms,
            );
            // A part of an image is saved wherever the user chose: that folder is theirs, and its
            // leftovers are not swept.
            let sweep = entry.is_none() && to_server.is_none();
            if sweep {
                sweep_output_leftovers(&out_dir);
            } else if lz4.is_some() {
                // What killed builds left in the data folder's spools, none of them running.
                if let Some(dir) = spool.parent() {
                    sweep_output_leftovers(dir);
                }
            }
            // Packs are made first: the stage list says so.
            let stages: &[&str] = if zstd.is_some() || lz4.is_some() {
                &["pack", "write", "verify"]
            } else {
                &["write", "verify"]
            };
            // The job's bytes are the whole build's (see `overall_progress`); the stage's own are in
            // its `stage`.
            let job_total = total;
            let mut report = |id: &str, done: u64, total: u64| {
                let index = stages.iter().position(|s| *s == id).unwrap_or(0) as u32;
                bytes.fetch_max(
                    overall_progress(job_total, index, stages.len() as u32, done, total),
                    Ordering::Relaxed,
                );
                *current.lock().unwrap_or_else(|e| e.into_inner()) = Some(JobStage {
                    id: id.to_string(),
                    index,
                    count: stages.len() as u32,
                    done,
                    total,
                });
            };
            let reopen_source = source.clone();
            let reopen_entry = entry.clone();
            let reopen_update = update.clone();
            // An archive is opened again the same way (its password kept in memory for that).
            let reopen_archive = archive.clone();
            // Verification reads the source afresh by the folder's names, so with the same renames
            // (or, for a part of the image, the same part; for packs, decoded the same way; with an
            // update, merged the same way).
            let left_out = std::mem::take(&mut *left_out.lock().unwrap_or_else(|e| e.into_inner()));
            let reopen = move || match &reopen_entry {
                Some(e) => open_entry(&reopen_source, e).map(|u| match u {
                    Unpacked::Tree(t) | Unpacked::File { tree: t, .. } => t,
                }),
                None if lz4_packing => open_for_lz4(
                    &reopen_source,
                    reopen_update.as_deref(),
                    reopen_archive.as_ref(),
                )
                .map(|(t, _)| crate::remove_backport::without(t, &left_out))
                .and_then(|t| lz4_ready(t, install_runtime)),
                None if packing => open_for_packing(
                    &reopen_source,
                    reopen_update.as_deref(),
                    reopen_archive.as_ref(),
                )
                .map(|(t, _)| crate::remove_backport::without(t, &left_out)),
                None => open_game(
                    &reopen_source,
                    reopen_update.as_deref(),
                    reopen_archive.as_ref(),
                )
                .map(|(t, _, _)| t)
                // Unpacked the same way: the same emulator, the same index.
                .and_then(|t| match &unpack {
                    Some(open) => open(t),
                    None => Ok(t),
                })
                // Then the same backport libraries left out.
                .map(|t| crate::remove_backport::without(t, &left_out)),
            };
            // zstd packs: the emulator that goes in, fetched here (off the request) and recorded.
            let mut runtime = None;
            let on_server = to_server
                .as_ref()
                .map(|dest| crate::server_out::ServerFolder {
                    dest,
                    name: name.clone(),
                    cancel: cancel.clone(),
                });
            let out = match &on_server {
                Some(s) => folder_build::FolderOut::Server(s),
                None => folder_build::FolderOut::Local(&target),
            };
            let shown = out.describe();
            let outcome = match unpacked {
                Unpacked::Tree(mut tree) => match zstd {
                    None if lz4.is_some() => match &lz4 {
                        Some(packs) => crate::lz4_packs::build_folder_to(
                            packs,
                            &mut *tree,
                            &reopen,
                            &out,
                            &spool,
                            mode,
                            &cancel,
                            &mut report,
                        ),
                        None => Err("no LZ4 packs".into()),
                    },
                    Some((level, choice)) => {
                        crate::zstd_runtime::pack_runtime(choice).and_then(|r| {
                            crate::log_at!(
                                Info,
                                "convert",
                                "zstd packs: {} goes into {shown}",
                                r.describe(),
                            );
                            crate::zstd_packs::build_folder_to(
                                level,
                                zstd_rules.as_deref(),
                                runtime.insert(r),
                                &mut *tree,
                                &reopen,
                                &out,
                                mode,
                                &cancel,
                                &mut report,
                            )
                        })
                    }
                    None => folder_build::build_to(
                        &mut *tree,
                        &reopen,
                        &out,
                        mode,
                        &cancel,
                        &mut report,
                    ),
                },
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
            drop(spooling);
            if outcome.is_err() && sweep {
                sweep_output_leftovers(&out_dir);
            }
            let completed_at_ms = now_ms();
            drop(ticker);
            match outcome {
                Ok(built) => {
                    // The trees that read the source are gone by now (the build took them).
                    let removal = removal.as_ref().map(|plan| {
                        let notes = std::mem::take(
                            &mut *source_notes.lock().unwrap_or_else(|e| e.into_inner()),
                        );
                        remove_source(plan, &target, &state_for_job, job_id, "folder", &notes)
                    });
                    crate::engine_log::record(
                        "info",
                        format!(
                            "folder: {} -> {shown} ({}, read back){}",
                            source.display(),
                            built.detail,
                            runtime
                                .as_ref()
                                .map(|r| format!("; zstd packs, emulator {}", r.describe()))
                                .unwrap_or_default()
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
                            dest: shown.clone(),
                            files_sent: built.files,
                            skipped_files: 0,
                            skipped_bytes: 0,
                            commit_ack: crate::source_removal::ack(
                                runtime.as_ref().map(runtime_ack),
                                removal.as_ref(),
                            ),
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
                            error_space: None,
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
    };
    answer_or_hand_off(&handoff_state, job_id, rest).await
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

/// The unpack folder a build reads its game from, removed when the build is over however it ends,
/// so nothing depends on the page that started it still being open (#14 of the audit: a reload
/// used to leave it behind for good).
struct Scaffold(Option<PathBuf>);

impl Scaffold {
    fn of(source: &Path) -> Self {
        Scaffold(extract_root_of(source))
    }
}

impl Drop for Scaffold {
    fn drop(&mut self) {
        if let Some(root) = self.0.take() {
            let _ = ps5upload_core::archive_extract::remove_path_retrying(&root);
            active_extracts()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&root);
            crate::workshop_api::forget_unpack(&root);
        }
    }
}

pub(crate) use ps5upload_convert_core::writing::Writing;
use ps5upload_convert_core::writing::{
    active_partials, claimed_elsewhere, has_download_sidecar, has_fresh_checkpoint, is_our_partial,
    sweep_dead_claims,
};

/// [`Writing::claim`] on a blocking thread: clearing a leftover can mean removing a whole pack
/// spool.
pub(crate) async fn claim_off_thread(path: PathBuf) -> Option<Writing> {
    tokio::task::spawn_blocking(move || Writing::claim(path))
        .await
        .ok()
        .flatten()
}

/// The 409 for a build whose output another running build is writing.
fn already_building(path: &Path) -> axum::response::Response {
    json_err(
        StatusCode::CONFLICT,
        format!("{} is already being built", path.display()),
    )
    .into_response()
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
    crate::remote::fetch::sweep_copies_in_output(out);
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
                        is_our_partial(&name)
                            && !has_download_sidecar(&e.path())
                            && !has_fresh_checkpoint(&e.path())
                    }
                    // An LZ4 build's pack volumes, `<name>.lz4spool/`, and a folder build's
                    // unfinished folder (marked, so a user's own is kept).
                    Ok(t) if t.is_dir() => {
                        is_our_lz4_spool(&e.path())
                            || crate::folder_build::is_our_partial_dir(&e.path())
                    }
                    _ => false,
                }
            })
            .map(|e| e.path())
            // A finished output whose rename failed stays for the user to rename.
            .filter(|p| !ps5upload_fpkg::finish::is_kept(p))
            .filter(|p| !active.contains(p) && !claimed_elsewhere(p))
            .partition(|p| p.is_file())
    };
    for p in stale {
        remove_file_retrying(&p);
    }
    for p in spools {
        let _ = std::fs::remove_dir_all(&p);
    }
    sweep_dead_claims(out);
}

/// An LZ4 build's spool, `<image or folder>.lz4spool/`: named so, and holding nothing but pack
/// volumes (`*.pak`), so a user's own folder that happens to be named like one is kept.
fn is_our_lz4_spool(dir: &Path) -> bool {
    let named = dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".lz4spool"));
    named
        && std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().all(|e| {
                e.file_type().is_ok_and(|t| t.is_file())
                    && e.file_name()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                        .ends_with(".pak")
            })
        })
}

/// Prefix of the folders archives are unpacked into, inside the output folder.
pub(crate) const EXTRACT_PREFIX: &str = ".ps5upload-extract-";

/// Unpack folders this engine is still using (a build reads from one until it is cleaned).
fn active_extracts() -> &'static std::sync::Mutex<ActiveExtracts> {
    static SET: std::sync::OnceLock<std::sync::Mutex<ActiveExtracts>> = std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// The unpack folders in use, each with its claim file held, so another process's sweep (the
/// `convert` command, a second engine) leaves them alone too.
#[derive(Default)]
struct ActiveExtracts(
    std::collections::HashMap<PathBuf, Option<ps5upload_convert_core::writing::ClaimLock>>,
);

impl ActiveExtracts {
    fn insert(&mut self, path: PathBuf) {
        let lock = ps5upload_convert_core::writing::ClaimLock::take(&path)
            .ok()
            .flatten();
        self.0.insert(path, lock);
    }
    fn remove(&mut self, path: &Path) {
        self.0.remove(path);
    }
    fn contains(&self, path: &Path) -> bool {
        self.0.contains_key(path)
    }
}

/// Unpacking needs room for the files and, after them, for a package about as large on the
/// same drive; the 1% is the margin the build's own check keeps.
fn extract_shortfall(free: u64, unpacked: u64) -> Option<String> {
    let needed = unpacked.saturating_mul(2).saturating_add(unpacked / 100);
    (free < needed).then(|| {
        format!(
            "only {:.1} GiB free in the output folder; unpacking this archive and building from it needs about {:.1} GiB",
            free as f64 / (1u64 << 30) as f64,
            needed as f64 / (1u64 << 30) as f64,
        )
    })
}

/// The unpack folder `path` is in (it or an ancestor named `.ps5upload-extract-*`), or None:
/// cleanup never deletes anything else.
pub(crate) fn extract_root_of(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|a| {
            a.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(EXTRACT_PREFIX) && n.len() > EXTRACT_PREFIX.len())
        })
        .map(Path::to_path_buf)
}

/// The bytes of the partial output file a stopped build left at `partial` (0 when there is
/// none, or it is a folder): room the next build of it has, since it resumes or clears it.
fn leftover_bytes(partial: &Path) -> u64 {
    std::fs::symlink_metadata(partial)
        .ok()
        .filter(|m| m.is_file())
        .map_or(0, |m| m.len())
}

/// Unpack folders left in `out` by an earlier run (a crash, a closed app), not in use now.
fn remove_stale_extracts(out: &Path) {
    let Ok(entries) = std::fs::read_dir(out) else {
        return;
    };
    // Found with the list of folders in use held, removed with it let go: removing a whole
    // unpacked game takes minutes, and every request that looks at the list (an unpack
    // starting, one being cleaned up) waited behind it. A new unpack folder has a new name.
    let stale: Vec<PathBuf> = {
        let active = active_extracts().lock().unwrap_or_else(|e| e.into_inner());
        entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(EXTRACT_PREFIX))
                    && e.file_type().is_ok_and(|t| t.is_dir())
            })
            .map(|e| e.path())
            .filter(|p| !active.contains(p) && !claimed_elsewhere(p))
            .collect()
    };
    for p in stale {
        let _ = ps5upload_core::archive_extract::remove_path_retrying(&p);
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
    /// DESTROYS THE ARCHIVE: its own space is given back while it unpacks, so the job fits on
    /// a drive with no room for both. A `.zip` is cut short behind each file as it is
    /// unpacked, a `.7z` of several blocks behind each block, and a multi-part `.rar` loses
    /// each part as it is finished; anything else is deleted the moment the unpack has
    /// finished and been checked. Ask `/api/fpkg/extract/free-check` first. Off unless set.
    #[serde(default)]
    free_archive: bool,
}

/// POST /api/fpkg/extract — unpack a game archive into the output folder, as a job. Done's
/// `dest` is the game found inside (a folder or an image), which the build then reads; the
/// unpack folder is removed by `/api/fpkg/extract/cleanup`, on failure, or by a later run.
///
/// With `free_archive` the archive is deleted as it is unpacked (`archive_free`): everything
/// that can be checked is checked first and refused with nothing changed (400, or 409 while
/// something else uses the archive). The game is unpacked into `<archive>.unpacked` in the
/// output folder, which nothing removes but `/api/fpkg/extract/cleanup` with `kept`. A job
/// that fails with part of the archive gone has `error_reason: "archive_partly_deleted"` and
/// the folder holding what was unpacked in `error_detail`.
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
    if let Err(e) = tokio::fs::create_dir_all(&out).await {
        return json_err(StatusCode::BAD_REQUEST, format!("{}: {e}", out.display()))
            .into_response();
    }
    let password = req.password.filter(|p| !p.is_empty());
    // The archive given up as it unpacks: checked and claimed whole before anything starts.
    let freeing = if req.free_archive {
        let (source, password, out) = (source.clone(), password.clone(), out.clone());
        let prepared = tokio::task::spawn_blocking(move || {
            sweep_output_leftovers(&out);
            crate::archive_free::Freeing::prepare(&source, password.as_deref(), &out)
        })
        .await;
        match prepared {
            Ok(Ok(freeing)) => Some(freeing),
            Ok(Err((status, why))) => return json_err(status, why).into_response(),
            Err(join) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("the archive check failed: {join}"),
                )
                .into_response()
            }
        }
    } else {
        None
    };
    // Read until the unpack ends: nothing may start freeing it (or editing it) meanwhile.
    let reading = if freeing.is_none() {
        match crate::image_edit_in_place::Reading::take(&source) {
            Ok(r) => Some(r),
            Err(why) => return json_err(StatusCode::CONFLICT, why).into_response(),
        }
    } else {
        None
    };
    let total = if let Some(freeing) = &freeing {
        freeing.total()
    } else {
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
            Ok(Err(e)) => {
                return json_err(StatusCode::BAD_REQUEST, format!("{e:#}")).into_response()
            }
            Err(join) => {
                return json_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("the archive listing failed: {join}"),
                )
                .into_response()
            }
        };
        if let Some(message) =
            build::free_bytes(&out).and_then(|free| extract_shortfall(free, total))
        {
            return json_err(StatusCode::BAD_REQUEST, message).into_response();
        }
        total
    };

    let job_id = Uuid::new_v4();
    let dest = match &freeing {
        // A folder that stays: once the archive is gone it holds the only copy of the game.
        Some(freeing) => freeing.dest_in(&out),
        None => {
            let dest = out.join(format!("{EXTRACT_PREFIX}{job_id}"));
            active_extracts()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(dest.clone());
            dest
        }
    };
    crate::workshop_api::note_run(job_id, "extract", &source);
    crate::workshop_api::note_unpack(&dest, &source);
    let cancel = register_workshop_job(job_id);
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
                    let state = crate::event_bus::tick_wanted(&events_tx)
                        .then(|| g.get(&job_id).map(|st| serde_json::json!(st)))
                        .flatten();
                    drop(g);
                    if let Some(state) = state {
                        let msg = serde_json::json!({ "job_id": job_id.to_string(), "job": crate::with_live_notes(job_id, state) });
                        let _ = events_tx.send(msg.to_string());
                    }
                }
                _ => break,
            }
        }
    });

    let state_for_job = state.clone();
    let ticker = JobTicker(ticker.abort_handle());
    spawn_workshop_job(move || {
        // A panic in the work below must not leave the job "running" forever with its
        // ticker looping: both guards act on any exit, `fail` unless a result was recorded.
        let mut fail = JobFailOnDropGuard::new(
            state_for_job.jobs.clone(),
            state_for_job.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        let _reading = reading;
        // Until the game is found, the folder is this job's to remove (a panic included). Not
        // one unpacked with the archive given up: what is in it may be all there is.
        let mut unpacking = Unpacking(freeing.is_none().then(|| dest.clone()));
        let mut progress = |done: u64, _total: u64| {
            bytes.store(done, Ordering::Relaxed);
            // Called every MiB on the unpacking thread: a pause holds it here, and a RAR's
            // helper process, its pipe full, waits with it (#631). A cancel is seen just after.
            ps5upload_fpkg::workshop::checkpoint(&cancel);
        };
        let outcome = match &freeing {
            Some(freeing) => {
                crate::engine_log::record(
                    "warn",
                    format!(
                        "fpkg: unpacking {} with its space given back ({}): the archive is deleted",
                        source.display(),
                        freeing.mode()
                    ),
                );
                freeing.run(&dest, password.as_deref(), &mut progress, &cancel)
            }
            None => archive_extract::extract(
                &source,
                &dest,
                password.as_deref(),
                &mut progress,
                &cancel,
            )
            .and_then(|()| archive_extract::find_game(&dest))
            .map_err(|error| crate::archive_free::Failure {
                error: format!("{error:#}"),
                reason: None,
                detail: None,
            }),
        };
        // The claims on the archive end with the unpack.
        drop(freeing);
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
            Err(failure) => {
                drop(unpacking);
                crate::engine_log::record(
                    "warn",
                    format!("fpkg: unpack failed: {}", failure.error),
                );
                set_job(
                    &state_for_job.jobs,
                    &state_for_job.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms: completed_at_ms.saturating_sub(started_at_ms),
                        error: failure.error,
                        error_reason: failure.reason,
                        error_detail: failure.detail,
                        error_console: None,
                        error_space: None,
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
    /// The folder is one an unpack with `free_archive` kept (`<archive>.unpacked`): removed
    /// only when asked for by this, once what was made from the game in it is finished.
    #[serde(default)]
    kept: bool,
}

/// POST /api/fpkg/extract/cleanup — remove an unpack folder once its build is over. A folder
/// an unpack with `free_archive` made is removed only with `kept`, and only one this engine
/// made since it started; any other path is refused.
pub(crate) async fn fpkg_extract_cleanup_handler(
    Json(req): Json<ExtractCleanupReq>,
) -> impl IntoResponse {
    let path = resolve_engine_path(&req.path);
    if req.kept {
        let Some(root) = crate::archive_free::kept_root_of(&path) else {
            return json_err(
                StatusCode::BAD_REQUEST,
                format!("{} is not a kept unpack folder", path.display()),
            )
            .into_response();
        };
        return match tokio::task::spawn_blocking(move || {
            crate::archive_free::release(&root).map_err(|e| format!("{}: {e}", root.display()))
        })
        .await
        {
            Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
            Ok(Err(e)) => json_err(StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
            Err(join) => {
                json_err(StatusCode::INTERNAL_SERVER_ERROR, join.to_string()).into_response()
            }
        };
    }
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

    /// The partial image a stopped build left counts as room for the next build of it; a
    /// folder of that name, or nothing there, counts for none.
    #[test]
    fn a_stopped_builds_partial_counts_as_room() {
        let base = std::env::temp_dir().join(format!("fpkg-leftover-{}", Uuid::new_v4()));
        std::fs::create_dir_all(base.join("dir.exfat.partial")).unwrap();
        std::fs::write(base.join("game.exfat.partial"), vec![0u8; 4096]).unwrap();
        assert_eq!(leftover_bytes(&base.join("game.exfat.partial")), 4096);
        assert_eq!(leftover_bytes(&base.join("dir.exfat.partial")), 0);
        assert_eq!(leftover_bytes(&base.join("none.exfat.partial")), 0);
        let _ = std::fs::remove_dir_all(&base);
    }

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

    /// The unpack folder a build read from goes with the build, page or no page; a game from
    /// anywhere else is never touched.
    #[test]
    fn a_build_removes_the_unpack_folder_it_read_from() {
        let base = std::env::temp_dir().join(format!("ps5upload-scaffold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join(format!("{EXTRACT_PREFIX}run"));
        let game = root.join("Game");
        std::fs::create_dir_all(&game).unwrap();
        active_extracts().lock().unwrap().insert(root.clone());
        drop(Scaffold::of(&game));
        assert!(!root.exists());
        assert!(!active_extracts().lock().unwrap().contains(&root));
        let own = base.join("Own");
        std::fs::create_dir_all(&own).unwrap();
        drop(Scaffold::of(&own));
        assert!(own.exists());
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
        // Anything not ours is never cleared, whatever path is handed in.
        let game = out.join("Game.exfat");
        std::fs::write(&game, b"x").unwrap();
        let w = Writing::claim(game.clone()).unwrap();
        assert!(game.is_file());
        drop(w);
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
            [
                ".D.ffpkg.partial.ps5upload-claim",
                "D.ffpkg.partial",
                "Game.ffpfsc",
                "movie.mkv.partial"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        );
        drop(writing);
        sweep_output_leftovers(&out);
        assert!(!out.join("D.ffpkg.partial").exists());
        assert!(!out.join(".D.ffpkg.partial.ps5upload-claim").exists());

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

    /// Another process's build (the `convert` command beside the app, or a second engine) is seen
    /// through its claim file: its unfinished output and its unpack folder are not swept, and the
    /// same output is refused here. A claim file nobody holds (a killed process's) goes.
    #[test]
    fn another_process_s_unfinished_output_is_never_swept() {
        use ps5upload_convert_core::writing::ClaimLock;
        let out =
            std::env::temp_dir().join(format!("ps5upload-other-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let theirs = out.join("A.exfat.partial");
        std::fs::write(&theirs, b"x").unwrap();
        let unpack = out.join(format!("{EXTRACT_PREFIX}theirs"));
        std::fs::create_dir_all(&unpack).unwrap();
        // Held as another process holds it: a lock on its claim file, not in this engine's sets.
        let held = ClaimLock::take(&theirs).unwrap().unwrap();
        let held_unpack = ClaimLock::take(&unpack).unwrap().unwrap();
        sweep_output_leftovers(&out);
        assert!(theirs.exists() && unpack.exists());
        assert!(Writing::claim(theirs.clone()).is_none());
        assert!(theirs.exists(), "a refused claim clears nothing");
        drop((held, held_unpack));
        sweep_output_leftovers(&out);
        assert!(!theirs.exists() && !unpack.exists());
        // A dead process's claim file, nobody holding it: gone, with what it claimed.
        std::fs::write(&theirs, b"x").unwrap();
        std::fs::write(out.join(".A.exfat.partial.ps5upload-claim"), b"1\n").unwrap();
        std::fs::write(out.join(".Gone.exfat.partial.ps5upload-claim"), b"1\n").unwrap();
        sweep_output_leftovers(&out);
        let left: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
        assert!(left.is_empty(), "{left:?}");
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn a_job_s_bytes_go_through_every_stage_once() {
        // Three stages of 1000: each moves the job through its share, never back.
        assert_eq!(overall_progress(3000, 0, 3, 0, 500), 0);
        assert_eq!(overall_progress(3000, 0, 3, 500, 500), 1000);
        assert_eq!(overall_progress(3000, 1, 3, 0, 0), 1000);
        assert_eq!(overall_progress(3000, 1, 3, 50, 100), 1500);
        assert_eq!(overall_progress(3000, 2, 3, 10, 10), 3000);
        // A stage that counts past its own total, or a stage past the list, stays inside.
        assert_eq!(overall_progress(3000, 2, 3, 20, 10), 3000);
        assert_eq!(overall_progress(3000, 7, 3, 0, 0), 3000);
        assert_eq!(overall_progress(3000, 0, 0, 1, 1), 0);
    }

    /// Straight to the console: .exfat, .ffpkg and .ffpfs, plain or in a .ffpfsc, packed (LZ4
    /// or zstd) or not. An edited copy is refused with the reason, before a job exists.
    #[test]
    fn only_the_formats_that_stream_go_straight_to_the_console() {
        assert_eq!(console_refusal(false), None);
        assert!(console_refusal(true).unwrap().contains("edited copy"));
    }

    /// The request's `destination`, as the client and the API send it.
    #[test]
    fn the_destination_reads_as_local_or_a_console() {
        let req: ExfatBuildReq = serde_json::from_value(serde_json::json!({
            "source": "/g",
            "destination": {"kind": "console", "addr": "10.0.0.2", "dir": "/data/homebrew"}
        }))
        .unwrap();
        assert!(matches!(
            req.destination,
            Some(ImageDestinationReq::Console { ref addr, ref dir })
                if addr == "10.0.0.2" && dir == "/data/homebrew"
        ));
        let req: ExfatBuildReq = serde_json::from_value(
            serde_json::json!({"source": "/g", "destination": {"kind": "local"}}),
        )
        .unwrap();
        assert!(matches!(req.destination, Some(ImageDestinationReq::Local)));
        let req: ExfatBuildReq =
            serde_json::from_value(serde_json::json!({"source": "/g"})).unwrap();
        assert!(req.destination.is_none());
        let req: FolderBuildReq = serde_json::from_value(serde_json::json!({
            "source": "/g.exfat",
            "destination": {"kind": "server", "source_id": "nas-1", "dir": "/games"}
        }))
        .unwrap();
        assert!(matches!(
            req.destination,
            Some(ImageDestinationReq::Server { ref source_id, ref dir })
                if source_id == "nas-1" && dir == "/games"
        ));
    }

    /// The same images go straight to a server as to the console, each refusal in its words.
    #[test]
    fn only_the_formats_that_stream_go_straight_to_a_server() {
        for edited in [false, true] {
            assert_eq!(
                server_refusal(edited).is_some(),
                console_refusal(edited).is_some(),
                "{edited}"
            );
            if let Some(why) = server_refusal(edited) {
                assert!(why.contains("a server"), "{why}");
            }
        }
    }

    /// A server destination names a saved connection; one that is not there is refused before a
    /// job starts.
    #[tokio::test]
    async fn an_unknown_server_or_no_folder_is_refused() {
        crate::remote::pool::testing::install_global(&[]);
        assert!(server_dest("no-such-connection", "/games")
            .err()
            .unwrap()
            .contains("no longer exists"));
        assert!(server_dest("x", "  ").err().unwrap().contains("folder"));
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

#[cfg(test)]
mod title_param_tests {
    use super::*;

    /// A folder converts only with its title's parameters (#596): PS5 param.json or PS4
    /// param.sfo in sce_sys.
    #[tokio::test]
    async fn a_folder_needs_param_json_or_param_sfo() {
        let dir = std::env::temp_dir().join(format!("fpkg-param-{}", std::process::id()));
        let sys = dir.join("sce_sys");
        std::fs::create_dir_all(&sys).unwrap();
        std::fs::write(dir.join("eboot.bin"), b"x").unwrap();
        assert!(!has_title_param(&dir).await);
        std::fs::write(sys.join("param.sfo"), b"x").unwrap();
        assert!(has_title_param(&dir).await);
        std::fs::remove_file(sys.join("param.sfo")).unwrap();
        std::fs::write(sys.join("param.json"), b"{}").unwrap();
        assert!(has_title_param(&dir).await);
        let _ = std::fs::remove_dir_all(&dir);

        let msg = missing_param_message(Path::new("/games/Foo"), true);
        assert!(msg.contains("sce_sys/param.json") && msg.contains("eboot.bin"));
        assert!(missing_param_message(Path::new("/x"), false).contains("param.sfo"));
    }
}

#[cfg(test)]
mod hand_off_tests {
    use super::*;

    fn state() -> AppState {
        let (events_tx, _rx) = tokio::sync::broadcast::channel(16);
        AppState {
            jobs: Arc::default(),
            default_ps5_addr: "127.0.0.1".to_string(),
            events_tx,
        }
    }

    fn refusal() -> axum::response::Response {
        json_err(StatusCode::BAD_REQUEST, "no game in it").into_response()
    }

    async fn status_of(state: &AppState, id: Uuid) -> Option<JobState> {
        for _ in 0..200 {
            let st = state.jobs.lock().unwrap().get(&id).cloned();
            if matches!(st, Some(JobState::Failed { .. })) {
                return st;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        state.jobs.lock().unwrap().get(&id).cloned()
    }

    /// A scan that ends in time answers as it always did: a refusal, and no job.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_quick_refusal_is_the_answer_and_starts_no_job() {
        let state = state();
        let id = Uuid::new_v4();
        let resp =
            answer_or_hand_off_within(&state, id, async { refusal() }, Duration::from_secs(5))
                .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(state.jobs.lock().unwrap().is_empty());
    }

    /// A long scan answers with the job at once, listed at its check stage; its refusal then
    /// ends the job in the same words.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_long_scan_becomes_the_job_and_its_refusal_the_job_failure() {
        let state = state();
        let id = Uuid::new_v4();
        let (go, wait) = tokio::sync::oneshot::channel::<()>();
        let resp = answer_or_hand_off_within(
            &state,
            id,
            async move {
                let _ = wait.await;
                refusal()
            },
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains(&id.to_string()));
        let st = state.jobs.lock().unwrap().get(&id).cloned();
        match st {
            Some(JobState::Running { stage, .. }) => {
                assert_eq!(stage.as_ref().map(|s| s.id.as_str()), Some("check"))
            }
            other => panic!("{other:?}"),
        }
        go.send(()).unwrap();
        match status_of(&state, id).await {
            Some(JobState::Failed { error, .. }) => assert_eq!(error, "no game in it"),
            other => panic!("{other:?}"),
        }
    }

    /// A job the scan listed itself is never overwritten by the placeholder.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_job_already_listed_keeps_its_own_state() {
        let state = state();
        let id = Uuid::new_v4();
        let jobs = state.jobs.clone();
        let resp = answer_or_hand_off_within(
            &state,
            id,
            async move {
                jobs.lock().unwrap().insert(
                    id,
                    JobState::Running {
                        stage: None,
                        started_at_ms: 1,
                        bytes_sent: 0,
                        total_bytes: 42,
                        files: Default::default(),
                        skipped_files: 0,
                        skipped_bytes: 0,
                        files_processing: 0,
                        files_finalized: 0,
                        files_finalizing_total: 0,
                        bytes_finalized: 0,
                    },
                );
                tokio::time::sleep(Duration::from_millis(200)).await;
                (StatusCode::ACCEPTED, "{}").into_response()
            },
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let st = state.jobs.lock().unwrap().get(&id).cloned();
        match st {
            Some(JobState::Running { total_bytes, .. }) => assert_eq!(total_bytes, 42),
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod source_option_tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    type Jobs = Arc<Mutex<HashMap<Uuid, JobState>>>;

    fn state() -> (AppState, Jobs) {
        let jobs: Jobs = Arc::default();
        let (events_tx, _rx) = tokio::sync::broadcast::channel(16);
        let state = AppState {
            jobs: jobs.clone(),
            default_ps5_addr: "127.0.0.1".to_string(),
            events_tx,
        };
        (state, jobs)
    }

    /// A game folder `<root>/PPSA01234` and a small exFAT image of it beside it.
    fn game(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("fpkg-srcopt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("PPSA01234");
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(dir.join("eboot.bin"), b"eboot").unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"titleId":"PPSA01234"}"#,
        )
        .unwrap();
        let image = root.join("PPSA01234.exfat");
        let mut tree = ps5upload_fpkg::source::open(&dir).unwrap();
        crate::image_build::build(
            crate::image_build::ImageFormat::Exfat,
            None,
            &mut *tree,
            &image,
            ps5upload_fpkg::sample::VerifyMode::Full,
            8 << 20,
            &std::sync::atomic::AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        (root, dir, image)
    }

    async fn image(body: serde_json::Value) -> (StatusCode, String, usize) {
        let (state, jobs) = state();
        let req: ExfatBuildReq = serde_json::from_value(body).unwrap();
        let resp = exfat_build_handler(State(state), Json(req))
            .await
            .into_response();
        answer(resp, &jobs).await
    }

    async fn folder(body: serde_json::Value) -> (StatusCode, String, usize) {
        let (state, jobs) = state();
        let req: FolderBuildReq = serde_json::from_value(body).unwrap();
        let resp = folder_build_handler(State(state), Json(req))
            .await
            .into_response();
        answer(resp, &jobs).await
    }

    /// The status, the body, and how many jobs the request started.
    async fn answer(resp: axum::response::Response, jobs: &Jobs) -> (StatusCode, String, usize) {
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let started = jobs.lock().unwrap().len();
        (status, String::from_utf8_lossy(&body).into_owned(), started)
    }

    /// Every request that asks for the source to go where it never does is refused before a
    /// job exists, and says why. Nothing here reaches a Trash: a request that could be
    /// honoured is not sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn delete_source_is_refused_where_the_source_never_goes() {
        let (root, dir, img) = game("delete");
        let out = root.join("out");
        let (dir_s, img_s, out_s) = (
            dir.display().to_string(),
            img.display().to_string(),
            out.display().to_string(),
        );
        let refused = |(status, body, started): (StatusCode, String, usize), want: &str| {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(
                body.contains("the source can't be moved to the Trash"),
                "{body}"
            );
            assert!(body.contains(want), "{want}: {body}");
            assert_eq!(started, 0, "no job may exist: {body}");
        };

        // An image that goes straight to a PS5 (the address is never dialled: refused first).
        refused(
            image(serde_json::json!({
                "source": dir_s, "output_dir": out_s, "delete_source": true,
                "destination": { "kind": "console", "addr": "192.0.2.9", "dir": "/data/homebrew" },
            }))
            .await,
            "straight to a PS5 or a server",
        );
        // An edited copy.
        refused(
            image(serde_json::json!({
                "source": img_s, "delete_source": true,
                "edits": [{ "op": "mkdir", "path": "mods" }],
            }))
            .await,
            "an edited copy never removes",
        );
        // An update merged in: two sources.
        refused(
            image(serde_json::json!({
                "source": dir_s, "output_dir": out_s, "delete_source": true,
                "update": root.join("update.pkg").display().to_string(),
            }))
            .await,
            "two sources",
        );
        // A source on a server or a PS5, for an image and for a folder.
        for source in [
            "remote://nas1/games/PPSA01234.exfat",
            "ps5://192.0.2.9/data/G.exfat",
        ] {
            let body = serde_json::json!({
                "source": source, "output_dir": out_s, "delete_source": true,
            });
            refused(image(body.clone()).await, "on a PS5 or a server");
            refused(folder(body).await, "on a PS5 or a server");
        }
        // The output inside the source folder. (On a machine with no Trash that is the reason
        // given instead; refused either way.)
        refused(
            image(serde_json::json!({
                "source": dir_s, "delete_source": true,
                "output_dir": dir.join("made").display().to_string(),
            }))
            .await,
            "",
        );
        // One file or folder of an image.
        refused(
            folder(serde_json::json!({
                "source": img_s, "output_dir": out_s, "delete_source": true, "entry": "sce_sys",
            }))
            .await,
            "one file or folder of an image",
        );
        // With an update merged in, a folder too.
        refused(
            folder(serde_json::json!({
                "source": img_s, "output_dir": out_s, "delete_source": true,
                "update": root.join("update.pkg").display().to_string(),
            }))
            .await,
            "two sources",
        );

        // Nothing was written and both sources are where they were.
        assert!(dir.join("eboot.bin").is_file() && img.is_file());
        assert!(!out.join("PPSA01234.exfat").exists() && !out.join("PPSA01234").exists());
        assert!(!dir.join("made/PPSA01234.exfat").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The backport is left out of a whole conversion only: an edited copy and a part of an
    /// image refuse the flag.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_backport_is_refused_for_an_edited_copy_and_a_part_of_an_image() {
        let (root, _, img) = game("backport");
        let img_s = img.display().to_string();
        let (status, body, started) = image(serde_json::json!({
            "source": img_s, "remove_backport": true,
            "edits": [{ "op": "mkdir", "path": "mods" }],
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("does not remove the backport"), "{body}");
        assert_eq!(started, 0);
        let (status, body, started) = folder(serde_json::json!({
            "source": img_s, "remove_backport": true, "entry": "sce_sys",
            "output_dir": root.join("out").display().to_string(),
        }))
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("from a whole game"), "{body}");
        assert_eq!(started, 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The job's end: `Ok(dest)` or `Err(error)`, waited for (two minutes at most).
    async fn ended(jobs: &Jobs) -> Result<String, String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let end = jobs.lock().unwrap().values().find_map(|j| match j {
                JobState::Done { dest, .. } => Some(Ok(dest.clone())),
                JobState::Failed { error, .. } => Some(Err(error.clone())),
                _ => None,
            });
            if let Some(end) = end {
                return end;
            }
            assert!(std::time::Instant::now() < deadline, "the job did not end");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// An image being edited in place refuses every build of it before the build reads it; an
    /// image a build reads refuses an in-place edit. The build's registration and the edit's
    /// claim are one decision (`image_edit_in_place::Reading` and `Claim`).
    #[tokio::test(flavor = "multi_thread")]
    async fn an_edit_and_a_build_of_one_image_never_run_together() {
        let (root, _, img) = game("edit-vs-build");
        let (img_s, out_s) = (
            img.display().to_string(),
            root.join("out").display().to_string(),
        );
        let claim = crate::image_edit_in_place::Claim::take(&img).unwrap();
        let body = serde_json::json!({ "source": img_s, "output_dir": out_s });
        for (status, body, started) in [image(body.clone()).await, folder(body.clone()).await] {
            assert_eq!(status, StatusCode::CONFLICT, "{body}");
            assert!(body.contains("edited in place"), "{body}");
            assert_eq!(started, 0);
        }
        let (st, jobs) = state();
        let req: BuildReq = serde_json::from_value(body).unwrap();
        let resp = fpkg_build_handler(State(st), Json(req))
            .await
            .into_response();
        let (status, text, started) = answer(resp, &jobs).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert_eq!(started, 0);
        drop(claim);

        // A build reading it: the edit is refused, the image untouched.
        let before = std::fs::read(&img).unwrap();
        let reading = crate::image_edit_in_place::Reading::take(&img).unwrap();
        let (st, jobs) = state();
        let resp = crate::image_edit_in_place::start(
            st,
            img.clone(),
            vec![ps5upload_fpkg::browse::Edit::Mkdir {
                path: "mods".into(),
            }],
            false,
        )
        .await;
        let (status, text, started) = answer(resp, &jobs).await;
        assert_eq!(status, StatusCode::CONFLICT, "{text}");
        assert!(text.contains("reading"), "{text}");
        assert_eq!(started, 0);
        assert!(std::fs::read(&img).unwrap() == before);
        drop(reading);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder build with `delete_source`, run to its end: the job's end and its
    /// `commit_ack`.
    async fn folder_removing(
        source: &Path,
        out: &Path,
    ) -> (Result<String, String>, Option<serde_json::Value>) {
        let (st, jobs) = state();
        let req: FolderBuildReq = serde_json::from_value(serde_json::json!({
            "source": source.display().to_string(),
            "output_dir": out.display().to_string(),
            "delete_source": true,
        }))
        .unwrap();
        let resp = folder_build_handler(State(st), Json(req))
            .await
            .into_response();
        let (status, body, _) = answer(resp, &jobs).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let end = ended(&jobs).await;
        let ack = jobs.lock().unwrap().values().find_map(|j| match j {
            JobState::Done { commit_ack, .. } => commit_ack.clone(),
            _ => None,
        });
        (end, ack)
    }

    /// The source was kept, and the job says it was for what the conversion left out.
    fn kept_for_left_out(source: &Path, ack: Option<serde_json::Value>, want: &str) {
        let ack = ack.expect("a commit_ack");
        assert_eq!(ack["source_removed"], false, "{ack}");
        let note = ack["source_note"].as_str().unwrap_or_default();
        assert!(note.contains("warning") && note.contains(want), "{note}");
        assert!(!crate::source_removal::trashed_in_tests(source), "{note}");
        assert!(source.exists());
    }

    /// Whatever the conversion leaves out keeps the source a folder build would move to the
    /// Trash: an archive's files beside the game folder, a patch package's files left to its
    /// base game. (A package's protected entries it can't decrypt are the same kind of note,
    /// `SourceTree::notes`, from the same reader: `pkg_source`'s own test covers them.) The
    /// same archive with the game alone does go.
    #[tokio::test(flavor = "multi_thread")]
    async fn delete_source_keeps_a_source_the_conversion_left_something_of() {
        let root = std::env::temp_dir().join(format!("fpkg-leftout-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let game: Vec<(&str, &[u8])> = vec![
            ("sce_sys/param.json", br#"{"titleId":"PPSA01234"}"#),
            ("eboot.bin", b"eboot"),
        ];
        let zip = |path: &Path, extra: Option<&str>| {
            use std::io::Write;
            let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            for (p, d) in &game {
                zw.start_file(format!("Game/PPSA01234/{p}"), opts).unwrap();
                zw.write_all(d).unwrap();
            }
            if let Some(extra) = extra {
                zw.start_file(extra, opts).unwrap();
                zw.write_all(b"not part of the game").unwrap();
            }
            zw.finish().unwrap();
        };
        // An archive with an update folder beside the game: kept.
        let extra = root.join("Extra.zip");
        zip(&extra, Some("Game/Update/sce_sys/param.json.bak"));
        let (end, ack) = folder_removing(&extra, &root.join("out1")).await;
        end.unwrap();
        kept_for_left_out(&extra, ack, "Game/Update/sce_sys/param.json.bak");
        // The game alone: it would go (tests record it instead of using the Trash).
        let alone = root.join("Alone.zip");
        zip(&alone, None);
        let (end, ack) = folder_removing(&alone, &root.join("out2")).await;
        end.unwrap();
        assert!(
            crate::source_removal::trashed_in_tests(&alone),
            "{:?}",
            ack.map(|a| a["source_note"].clone())
        );

        // A PS5 patch package (`\x7FLIH`) carrying every block (as `inspect`'s test makes one).
        let dir = root.join("patch-game");
        std::fs::create_dir_all(dir.join("sce_sys")).unwrap();
        std::fs::write(dir.join("eboot.bin"), [7u8; 3000]).unwrap();
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA04321_00-PATCHTEST0000000","contentVersion":"01.002.000","titleId":"PPSA04321","applicationCategoryType":0}"#,
        )
        .unwrap();
        let pkg_out = root.join("pkg-out");
        std::fs::create_dir_all(&pkg_out).unwrap();
        let built = ps5upload_fpkg::build::build(
            &ps5upload_fpkg::build::BuildRequest::new(&dir, &pkg_out),
            &mut |_| {},
        )
        .unwrap();
        let pkg = std::fs::read(&built.path).unwrap();
        const B: usize = 0x10000;
        let le64 = |at: usize| u64::from_le_bytes(pkg[at..at + 8].try_into().unwrap()) as usize;
        let (pfs_offset, pfs_size, cnt_offset) = (le64(0x10), le64(0x18), le64(0x58));
        let cnt_end = if cnt_offset < pfs_offset {
            pfs_offset
        } else {
            pkg.len()
        };
        let mut patch = vec![0u8; B];
        patch.extend_from_slice(&pkg[..B]);
        patch.extend_from_slice(&pkg[pfs_offset..pfs_offset + pfs_size]);
        let cnt_at = patch.len();
        patch.extend_from_slice(&pkg[cnt_offset..cnt_end]);
        patch[..4].copy_from_slice(b"\x7FLIH");
        for (at, v) in [
            (0x18, patch.len()),
            (0x20, B),
            (0x28, B),
            (0x30, cnt_at),
            (0x38, cnt_end - cnt_offset),
        ] {
            patch[at..at + 8].copy_from_slice(&(v as u64).to_le_bytes());
        }
        let update = root.join("update.pkg");
        std::fs::write(&update, &patch).unwrap();
        let (end, ack) = folder_removing(&update, &root.join("out3")).await;
        end.unwrap();
        kept_for_left_out(&update, ack, "patch package");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A game image whose in-place edit was cut off before its undo journal went (the image
    /// holds `extra.bin`, the journal beside it): `(root, image)`.
    fn interrupted_image(tag: &str) -> (PathBuf, PathBuf) {
        let (root, _, img) = game(tag);
        let host = root.join("host-extra.bin");
        std::fs::write(&host, b"added by an edit that never finished").unwrap();
        crate::image_edit_in_place::crash_before_commit(
            &img,
            &[ps5upload_fpkg::browse::Edit::Put {
                path: "extra.bin".into(),
                from: host,
            }],
        );
        (root, img)
    }

    /// A folder build and a package build of an image an in-place edit was interrupted in
    /// restore it from its journal first: what they write is the image as it was.
    #[tokio::test(flavor = "multi_thread")]
    async fn builds_settle_an_interrupted_in_place_edit_first() {
        let (root, img) = interrupted_image("settle-folder");
        let out = root.join("out");
        let (st, jobs) = state();
        let req: FolderBuildReq = serde_json::from_value(serde_json::json!({
            "source": img.display().to_string(),
            "output_dir": out.display().to_string(),
        }))
        .unwrap();
        let resp = folder_build_handler(State(st), Json(req))
            .await
            .into_response();
        let (status, body, _) = answer(resp, &jobs).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let folder = PathBuf::from(ended(&jobs).await.unwrap());
        assert!(folder.join("eboot.bin").is_file());
        assert!(
            !folder.join("extra.bin").exists(),
            "the folder holds the part-edited image's file"
        );
        assert!(!ps5_dump_forge_patch::journal_path(&img).exists());
        let _ = std::fs::remove_dir_all(&root);

        // A package: the image of a game with everything a package needs.
        let (root, img) = interrupted_image("settle-pkg");
        let dir = root.join("PPSA01234");
        std::fs::write(
            dir.join("sce_sys/param.json"),
            br#"{"contentId":"UP0000-PPSA01234_00-SETTLETEST000000","contentVersion":"01.000.000","titleId":"PPSA01234","applicationCategoryType":0}"#,
        )
        .unwrap();
        // The image again from the full param.json, then cut off mid-edit as above.
        std::fs::remove_file(&img).unwrap();
        std::fs::remove_file(ps5_dump_forge_patch::journal_path(&img)).unwrap();
        let mut tree = ps5upload_fpkg::source::open(&dir).unwrap();
        crate::image_build::build(
            crate::image_build::ImageFormat::Exfat,
            None,
            &mut *tree,
            &img,
            ps5upload_fpkg::sample::VerifyMode::Full,
            8 << 20,
            &std::sync::atomic::AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        drop(tree);
        crate::image_edit_in_place::crash_before_commit(
            &img,
            &[ps5upload_fpkg::browse::Edit::Put {
                path: "extra.bin".into(),
                from: root.join("host-extra.bin"),
            }],
        );
        let out = root.join("out");
        let (st, jobs) = state();
        let req: BuildReq = serde_json::from_value(serde_json::json!({
            "source": img.display().to_string(),
            "output_dir": out.display().to_string(),
        }))
        .unwrap();
        let resp = fpkg_build_handler(State(st), Json(req))
            .await
            .into_response();
        let (status, body, _) = answer(resp, &jobs).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let pkg = PathBuf::from(ended(&jobs).await.unwrap());
        let tree = ps5upload_fpkg::source::open(&pkg).unwrap();
        assert!(tree.files().iter().any(|f| f.path == "eboot.bin"));
        assert!(
            !tree.files().iter().any(|f| f.path == "extra.bin"),
            "the package holds the part-edited image's file"
        );
        assert!(!ps5_dump_forge_patch::journal_path(&img).exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
