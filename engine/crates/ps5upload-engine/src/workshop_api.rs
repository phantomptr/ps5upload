//! Pausing a Workshop job (#631) and "use less of this computer" (#632), over HTTP.
//!
//!   POST /api/jobs/{id}/pause      → hold a running conversion or extraction where it is
//!   POST /api/jobs/{id}/resume     → let it go on
//!   GET  /api/workshop/settings    → `{"gentle": bool, "threads": n, "cores": n}`
//!   POST /api/workshop/settings    → `{"gentle": bool}`: set it, for running jobs too
//!
//! A paused job's snapshot says `"paused": true` and `"paused_ms"` (all the time it has spent
//! paused), so a client can leave paused time out of its speed and time left. The work itself
//! waits in `ps5upload_fpkg::workshop`. The setting is the engine's, kept in its data folder
//! (`workshop.json`), so it holds for every window and browser that uses this engine, Docker
//! included, and across restarts.

use std::path::PathBuf;

use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ps5upload_fpkg::workshop;

use crate::json_err;

const FILE: &str = "workshop.json";

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
    let found = crate::set_job_paused(uuid, paused);
    crate::log_info!(
        "{}_job: job={uuid} found={found}",
        if paused { "pause" } else { "resume" }
    );
    if !found {
        // Over already, or not a job that can wait (an upload has its own pause).
        return json_err(
            StatusCode::CONFLICT,
            "this job is not running, or cannot be paused",
        )
        .into_response();
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({ "paused": paused })),
    )
        .into_response()
}

/// POST /api/jobs/{id}/pause — the job's workers stop at their next chunk and hold, using no
/// processor or disk, until resumed or cancelled. 409 for a job that is over or not pausable.
pub(crate) async fn pause_job(Path(id): Path<String>) -> Response {
    pause_or_resume(&id, true)
}

/// POST /api/jobs/{id}/resume
pub(crate) async fn resume_job(Path(id): Path<String>) -> Response {
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
    fn only_a_registered_running_job_pauses() {
        let id = Uuid::new_v4();
        assert!(!crate::set_job_paused(id, true), "unknown job");
        let plain = crate::register_transfer_cancel(id);
        assert!(
            !crate::set_job_paused(id, true),
            "an upload is not pausable here"
        );
        drop(plain);
        let flag = crate::register_workshop_job(id);
        assert!(crate::set_job_paused(id, true));
        let v = crate::with_live_notes(id, serde_json::json!({ "status": "running" }));
        assert_eq!(v["paused"], true, "{v}");
        assert_eq!(v["pausable"], true, "{v}");
        assert!(crate::set_job_paused(id, false));
        let v = crate::with_live_notes(id, serde_json::json!({ "status": "running" }));
        assert_eq!(v["paused"], false, "{v}");
        drop(flag);
        assert!(!crate::set_job_paused(id, true), "a finished job");
    }
}
