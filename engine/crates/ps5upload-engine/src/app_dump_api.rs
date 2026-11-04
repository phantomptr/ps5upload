//! Dump an installed game to a drive, inside ps5upload (8.10).
//!
//! `POST /api/ps5/dump {addr, title_id, dest, decrypt?, fself?, dlc?, op_id}`: the helper copies
//! the title's mounted files (`/mnt/sandbox/pfsmnt/<ID>-app0`, its sce_sys extras and DLC) to
//! `<dest>/<ID>-app`, decrypting its executables when asked (the payload's port of
//! ps5-app-dumper; see payload/src/app_dump.c). The files only exist while the game runs, so a
//! title that is not mounted is started first and given a moment to settle. Progress and cancel
//! go through `op-status` / `op-cancel` with `op_id`, like an archive extract. 200 with the
//! result; 409 cancelled; 502 with the helper's cause (`dump_exists`, `dump_no_space`,
//! `dump_not_running`, …).

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::{helper_stop, json_err, missing_console_addr, required_console_addr};

/// How long a started game gets to mount its files, and to settle once they are there (the
/// original dumper waits 30 s; reading while the game is still loading competes with it).
const MOUNT_WAIT: Duration = Duration::from_secs(120);
const RELAUNCH_AFTER: Duration = Duration::from_secs(45);
const SETTLE: Duration = Duration::from_secs(15);
/// A large game copies for a long time; the job is followed, not timed out.
const DUMP_DEADLINE: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Deserialize)]
pub struct DumpReq {
    #[serde(alias = "console", alias = "host")]
    addr: Option<String>,
    title_id: String,
    dest: String,
    #[serde(default)]
    decrypt: bool,
    #[serde(default)]
    fself: bool,
    #[serde(default = "yes")]
    dlc: bool,
    #[serde(default)]
    op_id: u64,
}

fn yes() -> bool {
    true
}

/// A title id as the console names folders: four letters, five digits.
fn valid_title_id(id: &str) -> bool {
    id.len() == 9
        && id[..4].bytes().all(|b| b.is_ascii_uppercase())
        && id[4..].bytes().all(|b| b.is_ascii_digit())
}

fn mount_path(title_id: &str) -> String {
    format!("/mnt/sandbox/pfsmnt/{title_id}-app0")
}

/// Start the game if its files are not mounted, and wait until they are.
fn ensure_running(addr: &str, title_id: &str) -> anyhow::Result<()> {
    let mount = mount_path(title_id);
    let mounted = || ps5upload_core::fs_ops::fs_stat(addr, &mount).is_ok_and(|s| s.kind == "dir");
    if mounted() {
        return Ok(());
    }
    crate::log_info!("app_dump: {title_id} not running; starting it");
    // A launch reports failure now and then and the game starts anyway (and the reverse): the
    // mount is what counts, and one more try is made if it does not come.
    let _ = ps5upload_core::fs_ops::app_launch(addr, title_id);
    let t0 = Instant::now();
    let mut relaunched = false;
    while t0.elapsed() < MOUNT_WAIT {
        std::thread::sleep(Duration::from_secs(2));
        if mounted() {
            std::thread::sleep(SETTLE);
            return Ok(());
        }
        if !relaunched && t0.elapsed() > RELAUNCH_AFTER {
            relaunched = true;
            let _ = ps5upload_core::fs_ops::app_launch(addr, title_id);
        }
    }
    anyhow::bail!("dump_not_running: the game did not start, so its files could not be read")
}

pub async fn dump(Json(req): Json<DumpReq>) -> Response {
    let Some(addr) = required_console_addr(req.addr) else {
        return missing_console_addr();
    };
    if !valid_title_id(&req.title_id) {
        return json_err(StatusCode::BAD_REQUEST, "dump_bad_title_id").into_response();
    }
    if !req.dest.starts_with('/') || req.dest.contains("..") {
        return json_err(StatusCode::BAD_REQUEST, "dump_bad_dest").into_response();
    }
    let x = ps5upload_core::fs_ops::AppDumpRequest {
        title_id: req.title_id.clone(),
        dest: req.dest,
        decrypt: req.decrypt,
        fself: req.fself && req.decrypt,
        dlc: req.dlc,
    };
    let op_id = req.op_id;
    let started = Instant::now();
    crate::log_info!(
        "app_dump: addr={addr} title={} dest={} decrypt={} fself={} dlc={} op_id={op_id}",
        x.title_id,
        x.dest,
        x.decrypt,
        x.fself,
        x.dlc
    );
    let title = x.title_id.clone();
    match tokio::task::spawn_blocking(move || {
        // Marks the console busy for the job: a helper stop is refused meanwhile.
        let _busy = helper_stop::enter(&addr);
        ensure_running(&addr, &x.title_id)?;
        // It was mounted a moment ago: a refusal now means the game closed (some apps exit
        // on their own) rather than that it never started.
        ps5upload_core::fs_ops::app_dump(&addr, &x, op_id, DUMP_DEADLINE).map_err(|e| {
            if format!("{e:#}").contains("dump_not_running") {
                anyhow::anyhow!("dump_game_closed: the game closed before its files were copied")
            } else {
                e
            }
        })
    })
    .await
    .map_err(anyhow::Error::from)
    .and_then(|r| r)
    {
        Ok(done) => {
            crate::log_info!(
                "app_dump ok: {title} -> {} ({} files, {} bytes, {} decrypted, {} failed) in {} s",
                done.path,
                done.files,
                done.bytes,
                done.decrypted,
                done.decrypt_failed,
                started.elapsed().as_secs()
            );
            let mut v = serde_json::to_value(&done).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.insert("ok".into(), true.into());
            }
            Json(v).into_response()
        }
        Err(e) if e.to_string() == "cancelled" => {
            json_err(StatusCode::CONFLICT, "cancelled").into_response()
        }
        Err(e) => {
            crate::log_warn!("app_dump failed: {title}: {e:#}");
            json_err(StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_ids() {
        assert!(valid_title_id("PPSA01234"));
        assert!(valid_title_id("CUSA00900"));
        assert!(!valid_title_id("PPSA0123"));
        assert!(!valid_title_id("ppsa01234"));
        assert!(!valid_title_id("PPSA0123/"));
        assert_eq!(
            mount_path("PPSA01234"),
            "/mnt/sandbox/pfsmnt/PPSA01234-app0"
        );
    }
}
