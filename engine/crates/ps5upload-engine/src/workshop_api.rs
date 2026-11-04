//! Pausing a Workshop job (#631) and "use less of this computer" (#632), over HTTP.
//!
//!   POST /api/jobs/{id}/pause      → hold a running conversion or extraction where it is
//!   POST /api/jobs/{id}/resume     → let it go on
//!   GET  /api/workshop/jobs        → the Workshop jobs running now, with what each is making, so
//!                                    a reloaded page or another window can follow them
//!   GET  /api/workshop/settings    → `{"gentle": bool, "threads": n, "cores": n}`
//!   POST /api/workshop/settings    → `{"gentle": bool}`: set it, for running jobs too
//!
//! A paused job's snapshot says `"paused": true` and `"paused_ms"` (all the time it has spent
//! paused), so a client can leave paused time out of its speed and time left. The work itself
//! waits in `ps5upload_fpkg::workshop`. The setting is the engine's, kept in its data folder
//! (`workshop.json`), so it holds for every window and browser that uses this engine, Docker
//! included, and across restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use axum::{
    extract::{Path as UrlPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ps5upload_fpkg::workshop;

use crate::{json_err, AppState};

const FILE: &str = "workshop.json";

/// What a running Workshop job is making, for a client that did not start it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Run {
    pub job_id: String,
    /// `package`, `image`, `folder` or `extract`.
    pub kind: &'static str,
    /// What the user chose: the archive, for a game unpacked from one.
    pub source: String,
    pub started_at_ms: u64,
}

fn runs() -> &'static Mutex<HashMap<Uuid, Run>> {
    static RUNS: OnceLock<Mutex<HashMap<Uuid, Run>>> = OnceLock::new();
    RUNS.get_or_init(Default::default)
}

/// The archive each unpack folder came from.
fn unpacks() -> &'static Mutex<HashMap<PathBuf, String>> {
    static UNPACKS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    UNPACKS.get_or_init(Default::default)
}

/// Notes that the job `job_id` is running. A game read from an unpack folder is reported as the
/// archive it came from, which is what the user picked.
pub(crate) fn note_run(job_id: Uuid, kind: &'static str, source: &Path) {
    let archive = source.ancestors().find_map(|a| {
        unpacks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(a)
            .cloned()
    });
    let run = Run {
        job_id: job_id.to_string(),
        kind,
        source: archive.unwrap_or_else(|| source.to_string_lossy().into_owned()),
        started_at_ms: crate::now_ms(),
    };
    runs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(job_id, run);
}

/// Notes that the unpack folder `root` holds the archive `archive`.
pub(crate) fn note_unpack(root: &Path, archive: &Path) {
    unpacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(root.to_path_buf(), archive.to_string_lossy().into_owned());
}

pub(crate) fn forget_unpack(root: &Path) {
    unpacks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(root);
}

/// The noted runs still going, by `running`; the others are forgotten. Oldest first.
pub(crate) fn live_runs(running: impl Fn(Uuid) -> bool) -> Vec<Run> {
    let mut g = runs().lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|id, _| running(*id));
    let mut out: Vec<Run> = g.values().cloned().collect();
    out.sort_by_key(|r| r.started_at_ms);
    out
}

/// `GET /api/workshop/jobs`.
pub(crate) async fn get_jobs(State(state): State<AppState>) -> Response {
    let list = {
        let jobs = state.jobs.lock().unwrap_or_else(|e| e.into_inner());
        live_runs(|id| matches!(jobs.get(&id), Some(crate::JobState::Running { .. })))
    };
    Json(list).into_response()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    gentle: bool,
}

fn settings_path() -> Option<PathBuf> {
    ps5upload_ava1::pool::data_dir().map(|d| d.join(FILE))
}

fn read_from(path: &std::path::Path) -> Stored {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_to(path: &std::path::Path, s: Stored) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| crate::state_io::describe("the Workshop setting", dir, &e))?;
    }
    // Written aside and renamed over, so a crash mid-write never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_vec_pretty(&s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, body)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| crate::state_io::describe("the Workshop setting", path, &e))
}

/// Apply the saved setting; called once as the engine starts.
pub(crate) fn load() {
    if let Some(path) = settings_path() {
        let s = read_from(&path);
        workshop::set_gentle(s.gentle);
        if s.gentle {
            crate::engine_log::record(
                "info",
                "workshop: use less of this computer is on (half the cores, background priority)"
                    .to_string(),
            );
        }
    }
}

fn settings_json() -> serde_json::Value {
    serde_json::json!({
        "gentle": workshop::gentle(),
        "threads": workshop::workshop_threads(),
        "cores": std::thread::available_parallelism().map_or(4, |n| n.get()),
    })
}

/// GET /api/workshop/settings
pub(crate) async fn get_settings() -> Json<serde_json::Value> {
    Json(settings_json())
}

#[derive(Deserialize)]
pub(crate) struct SettingsReq {
    gentle: bool,
}

/// POST /api/workshop/settings — takes effect at once: running jobs' threads follow it at their
/// next chunk, and jobs started from now on take the thread count it gives.
pub(crate) async fn set_settings(Json(req): Json<SettingsReq>) -> Response {
    workshop::set_gentle(req.gentle);
    crate::engine_log::record(
        "info",
        format!(
            "workshop: use less of this computer {}",
            if req.gentle { "on" } else { "off" }
        ),
    );
    let saved = match settings_path() {
        Some(path) => {
            let s = Stored { gentle: req.gentle };
            tokio::task::spawn_blocking(move || write_to(&path, s))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
        }
        None => Err("no data folder to keep the setting in".into()),
    };
    match saved {
        Ok(()) => (StatusCode::OK, Json(settings_json())).into_response(),
        // Applied for this run all the same; say why it will not last.
        Err(why) => {
            crate::engine_log::record("warn", format!("workshop: setting not saved: {why}"));
            let mut v = settings_json();
            v["saved"] = false.into();
            v["error"] = why.into();
            (StatusCode::OK, Json(v)).into_response()
        }
    }
}

fn pause_or_resume(id: &str, paused: bool) -> Response {
    let Ok(uuid) = id.parse::<Uuid>() else {
        return json_err(StatusCode::BAD_REQUEST, "invalid job id").into_response();
    };
    let outcome = crate::set_job_paused(uuid, paused);
    crate::log_info!(
        "{}_job: job={uuid} outcome={outcome:?}",
        if paused { "pause" } else { "resume" }
    );
    match outcome {
        crate::JobPause::Done => {}
        crate::JobPause::NotRunning => {
            return json_err(StatusCode::CONFLICT, "this job is not running").into_response();
        }
        // An upload has its own pause.
        crate::JobPause::NotPausable => {
            return json_err(StatusCode::CONFLICT, "this job cannot be paused").into_response();
        }
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "paused": paused })),
    )
        .into_response()
}

/// POST /api/jobs/{id}/pause — the job's workers stop at their next chunk and hold, using no
/// processor or disk, until resumed or cancelled. 409 for a job that is over or not pausable.
pub(crate) async fn pause_job(UrlPath(id): UrlPath<String>) -> Response {
    pause_or_resume(&id, true)
}

/// POST /api/jobs/{id}/resume
pub(crate) async fn resume_job(UrlPath(id): UrlPath<String>) -> Response {
    pause_or_resume(&id, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_round_trips_and_a_missing_file_is_off() {
        let dir = std::env::temp_dir().join(format!("ps5u-workshop-{}", std::process::id()));
        let path = dir.join(FILE);
        let _ = std::fs::remove_file(&path);
        assert_eq!(read_from(&path), Stored { gentle: false });
        write_to(&path, Stored { gentle: true }).unwrap();
        assert_eq!(read_from(&path), Stored { gentle: true });
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(read_from(&path), Stored::default());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_run_is_listed_while_it_goes_and_names_what_the_user_chose() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let root = PathBuf::from("/out/.ps5upload-extract-run1");
        note_unpack(&root, Path::new("/dl/Game.rar"));
        note_run(a, "image", &root.join("Game"));
        note_run(b, "package", Path::new("/games/Other"));
        let (id_a, id_b) = (a.to_string(), b.to_string());
        let mine = |r: &Run| r.job_id == id_a || r.job_id == id_b;
        let listed: Vec<Run> = live_runs(|id| id == a || id == b)
            .into_iter()
            .filter(mine)
            .collect();
        assert_eq!(listed.len(), 2);
        let image = listed.iter().find(|r| r.job_id == id_a).unwrap();
        assert_eq!(
            (image.kind, image.source.as_str()),
            ("image", "/dl/Game.rar")
        );
        let pkg = listed.iter().find(|r| r.job_id == id_b).unwrap();
        assert_eq!(pkg.source, "/games/Other");
        // Once it ends it is gone, and the folder's note with its cleanup.
        assert!(live_runs(|id| id == b).iter().all(|r| r.job_id != id_a));
        forget_unpack(&root);
        note_run(a, "image", &root.join("Game"));
        assert!(live_runs(|id| id == a)
            .iter()
            .any(|r| r.source.ends_with("Game")));
        assert!(live_runs(|_| false).is_empty());
    }

    #[test]
    fn only_a_registered_running_job_pauses() {
        use crate::JobPause::{Done, NotPausable, NotRunning};
        let id = Uuid::new_v4();
        assert_eq!(crate::set_job_paused(id, true), NotRunning, "unknown job");
        let plain = crate::register_transfer_cancel(id);
        assert_eq!(
            crate::set_job_paused(id, true),
            NotPausable,
            "an upload is not pausable here"
        );
        drop(plain);
        let flag = crate::register_workshop_job(id);
        assert_eq!(crate::set_job_paused(id, true), Done);
        let v = crate::with_live_notes(id, serde_json::json!({ "status": "running" }));
        assert_eq!(v["paused"], true, "{v}");
        assert_eq!(v["pausable"], true, "{v}");
        assert_eq!(crate::set_job_paused(id, false), Done);
        let v = crate::with_live_notes(id, serde_json::json!({ "status": "running" }));
        assert_eq!(v["paused"], false, "{v}");
        drop(flag);
        assert_eq!(
            crate::set_job_paused(id, true),
            NotRunning,
            "a finished job"
        );
    }
}
