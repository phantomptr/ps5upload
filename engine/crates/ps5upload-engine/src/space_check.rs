//! Space no file uses (#979): on an M.2 or USB drive, what the file system counts as used set
//! against what the files on it take, so space held by a deleted file shows up as a number.
//!
//! * `POST /api/ps5/space-check` `{addr, path}` -> [`SpaceReport`]
//!
//! `path` is the top folder of an M.2 or USB drive as the Volumes list names it (`/mnt/ext1`,
//! `/mnt/usb0`). Internal storage is refused (`space_check_internal`): the console keeps folders
//! there the helper cannot read, so its files never add up to the drive.
//!
//! The drive's used space is `total - free` from `fs.volumes`, read before and after the helper
//! counts the drive's files (`JOB_OP_TREE_SIZE`: names and metadata only, every entry's allocated
//! blocks, folders included). These drives hold no file-system reserve (free for everyone equals
//! free), so what is used and not in a file is space no file holds. Measured on a 2 TB M.2
//! (`bfs`, FW 13.60): 5 MiB with nothing held, the deleted package's size while it was.
//!
//! A file deleted while a program still has it open keeps its space until the program lets go
//! (the installer daemon held an installed package for ten minutes, or until the next install,
//! before 1.6.3). One check at a time per console; a second gets 409 `space_check_busy`.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::{console_addr_or_default, json_err, AppState};

/// Unaccounted space below this is the file system's own bookkeeping (5 MiB measured on 2 TB).
pub(crate) const HELD_MIN: u64 = 1 << 30;
/// How much the used space may move during the count before it is not a fair comparison.
pub(crate) const SETTLE_SLACK: u64 = 64 << 20;
const DEADLINE: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Deserialize)]
pub(crate) struct CheckReq {
    #[serde(default, alias = "console", alias = "host")]
    addr: Option<String>,
    path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpaceVerdict {
    /// The drive's used space is what its files take.
    Clear,
    /// More is used than the files take, and every folder was counted.
    Held,
    /// More is used than the files take, but some folders could not be counted.
    Partial,
    /// The used space changed while the files were counted: something wrote or deleted.
    Busy,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SpaceReport {
    pub path: String,
    pub fs_type: String,
    pub total_bytes: u64,
    /// `total - free` after the count.
    pub used_bytes: u64,
    /// What the files and folders on the drive take (allocated blocks).
    pub files_bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Folders not counted (unreadable, another drive mounted inside).
    pub skipped: u64,
    /// `used_bytes - files_bytes`, never below 0.
    pub unaccounted_bytes: u64,
    pub verdict: SpaceVerdict,
    pub elapsed_ms: u64,
}

/// The verdict for a count, from the used space before and after it.
pub(crate) fn assess(
    used_before: u64,
    used_after: u64,
    files_bytes: u64,
    skipped: u64,
) -> (u64, SpaceVerdict) {
    let unaccounted = used_after.saturating_sub(files_bytes);
    let verdict = if used_before.abs_diff(used_after) > SETTLE_SLACK {
        SpaceVerdict::Busy
    } else if unaccounted < HELD_MIN {
        SpaceVerdict::Clear
    } else if skipped > 0 {
        SpaceVerdict::Partial
    } else {
        SpaceVerdict::Held
    };
    (unaccounted, verdict)
}

/// The drives a check is for: an M.2 or USB drive's own top folder, never internal storage.
pub(crate) fn checkable(path: &str, fs_type: &str) -> Result<(), &'static str> {
    let external = ["/mnt/ext", "/mnt/usb"].iter().any(|p| {
        path.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    });
    if !external {
        return Err("space_check_internal");
    }
    if fs_type == "nullfs" {
        return Err("space_check_not_a_drive");
    }
    Ok(())
}

fn running() -> &'static Mutex<HashSet<String>> {
    static R: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashSet::new()))
}

struct Slot(String);
impl Drop for Slot {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

fn used_of(addr: &str, path: &str) -> anyhow::Result<(u64, u64, String)> {
    let list = ps5upload_core::volumes::list_volumes(addr)?;
    let v = list
        .volumes
        .into_iter()
        .find(|v| v.path == path)
        .ok_or_else(|| anyhow::anyhow!("space_check_no_such_drive"))?;
    Ok((
        v.total_bytes,
        v.total_bytes.saturating_sub(v.free_bytes),
        v.fs_type,
    ))
}

fn check(addr: &str, path: &str) -> Result<SpaceReport, (StatusCode, String)> {
    let bad_gateway = |e: anyhow::Error| (StatusCode::BAD_GATEWAY, format!("{e:#}"));
    let started = std::time::Instant::now();
    let (_, used_before, fs_type) = used_of(addr, path).map_err(|e| {
        if format!("{e}") == "space_check_no_such_drive" {
            (
                StatusCode::NOT_FOUND,
                "space_check_no_such_drive".to_string(),
            )
        } else {
            bad_gateway(e)
        }
    })?;
    checkable(path, &fs_type).map_err(|why| (StatusCode::BAD_REQUEST, why.to_string()))?;
    static OP_IDS: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0xF6_0000_0000_0001);
    let op_id = OP_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = ps5upload_core::fs_ops::tree_size(addr, path, op_id, DEADLINE).map_err(bad_gateway)?;
    let (total, used_after, _) = used_of(addr, path).map_err(bad_gateway)?;
    let (unaccounted, verdict) = assess(used_before, used_after, t.alloc, t.skipped);
    let report = SpaceReport {
        path: path.to_string(),
        fs_type,
        total_bytes: total,
        used_bytes: used_after,
        files_bytes: t.alloc,
        files: t.files,
        dirs: t.dirs,
        skipped: t.skipped,
        unaccounted_bytes: unaccounted,
        verdict,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    crate::log_at!(
        Info,
        "files",
        "space_check {addr} {path} ({}): used {} (before {used_before}), files {} in {} files, \
         {} skipped, unaccounted {unaccounted}: {verdict:?}",
        report.fs_type,
        report.used_bytes,
        report.files_bytes,
        report.files,
        report.skipped
    );
    Ok(report)
}

/// `POST /api/ps5/space-check`: see the module docs.
pub(crate) async fn handler(State(state): State<AppState>, Json(req): Json<CheckReq>) -> Response {
    let addr = console_addr_or_default(req.addr, &state.default_ps5_addr);
    let path = req.path.trim().trim_end_matches('/').to_string();
    if path.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "path is required").into_response();
    }
    if let Err(why) = checkable(&path, "") {
        return json_err(StatusCode::BAD_REQUEST, why).into_response();
    }
    if !running()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(addr.clone())
    {
        return json_err(StatusCode::CONFLICT, "space_check_busy").into_response();
    }
    let slot = Slot(addr.clone());
    let r = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        check(&addr, &path)
    })
    .await;
    match r {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err((code, msg))) => json_err(code, msg).into_response(),
        Err(join) => json_err(StatusCode::INTERNAL_SERVER_ERROR, join.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    /// The Pro's M.2 on 2026-11-27: a 4.3 GB package deleted while the installer still had it open.
    #[test]
    fn a_deleted_package_still_held_open_is_space_no_file_uses() {
        let (u, v) = assess(18_194_825_216, 18_194_825_216, 13_882_425_344, 0);
        assert_eq!(u, 4_312_399_872);
        assert_eq!(v, SpaceVerdict::Held);
    }

    #[test]
    fn the_file_systems_own_bookkeeping_is_not_held_space() {
        // Nothing held: 5 MiB more used than the files take.
        let (u, v) = assess(13_887_668_224, 13_887_668_224, 13_882_425_344, 0);
        assert_eq!(u, 5 << 20);
        assert_eq!(v, SpaceVerdict::Clear);
    }

    #[test]
    fn files_taking_more_than_used_is_nothing_unaccounted() {
        assert_eq!(
            assess(10 * GIB, 10 * GIB, 11 * GIB, 0),
            (0, SpaceVerdict::Clear)
        );
    }

    #[test]
    fn a_drive_written_to_during_the_count_is_not_judged() {
        let (_, v) = assess(10 * GIB, 12 * GIB, 5 * GIB, 0);
        assert_eq!(v, SpaceVerdict::Busy);
        // A little movement (a log line) is fine.
        let (_, v) = assess(10 * GIB, 10 * GIB + (1 << 20), 5 * GIB, 0);
        assert_eq!(v, SpaceVerdict::Held);
    }

    #[test]
    fn a_count_that_skipped_folders_cannot_say_the_space_is_held() {
        assert_eq!(
            assess(10 * GIB, 10 * GIB, 5 * GIB, 2).1,
            SpaceVerdict::Partial
        );
        // ...but a drive that adds up is clear either way.
        assert_eq!(
            assess(10 * GIB, 10 * GIB, 10 * GIB, 2).1,
            SpaceVerdict::Clear
        );
    }

    #[test]
    fn only_an_m2_or_usb_drives_top_folder_is_checked() {
        assert_eq!(checkable("/mnt/ext1", "bfs"), Ok(()));
        assert_eq!(checkable("/mnt/usb0", "exfatfs"), Ok(()));
        assert_eq!(checkable("/data", "nullfs"), Err("space_check_internal"));
        assert_eq!(checkable("/user", "bfs"), Err("space_check_internal"));
        assert_eq!(
            checkable("/mnt/ext1/user", "bfs"),
            Err("space_check_internal")
        );
        assert_eq!(checkable("/mnt/ext", "bfs"), Err("space_check_internal"));
        assert_eq!(
            checkable("/mnt/original_m2", "nullfs"),
            Err("space_check_internal")
        );
        assert_eq!(
            checkable("/mnt/ext2", "nullfs"),
            Err("space_check_not_a_drive")
        );
    }
}
