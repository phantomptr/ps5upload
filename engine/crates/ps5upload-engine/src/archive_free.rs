//! Unpacking an archive with its own space given back as it goes (`free_archive` on
//! `POST /api/fpkg/extract`, checked first by `POST /api/fpkg/extract/free-check`): the way to
//! convert a game on a drive with no room for the archive and the unpacked game side by side.
//! It destroys the archive, so it is never the default; what each kind of archive allows, and
//! the rules the unpack keeps, are in `ps5upload_core::archive_extract::free`.
//!
//! What the engine adds here:
//!
//! - Nothing else may be using the archive: every file of it is claimed like an image edited
//!   in place (`image_edit_in_place::Claim`, refused while a build or an upload reads it, and
//!   they are refused while this runs), and across processes (`ClaimLock`).
//! - Room: the output drive has to hold the unpack at its largest and, after it, what is made
//!   from the game, counting the archive's own space only when it is on that drive.
//! - Where it unpacks: a folder with a plain name beside the outputs, `<archive>.unpacked`,
//!   not a `.ps5upload-extract-*` scratch folder. Those are removed when a build fails, by any
//!   later run's sweep and by a closed app; this one holds the only copy of the game once the
//!   archive is gone, so nothing removes it but [`release`], which the app calls once what it
//!   made from the game is finished.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

use ps5upload_convert_core::writing::ClaimLock;
use ps5upload_core::archive_extract::{self, free};
use ps5upload_fpkg::build;

use crate::image_edit_in_place::Claim;
use crate::json_err;

/// `error_reason` of an unpack that stopped with part of the archive gone.
pub(crate) const PARTLY_DELETED: &str = "archive_partly_deleted";

/// The sentence that follows the cause in such a failure's `error` (the app shows the cause
/// and says the rest in the user's language).
pub(crate) const PARTLY_DELETED_TEXT: &str =
    "The archive is partly deleted and can't be used again.";

/// Folders unpacked into with the archive given up, by this engine since it started.
fn kept() -> &'static Mutex<HashSet<PathBuf>> {
    static SET: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    SET.get_or_init(Default::default)
}

fn keep(root: &Path) {
    kept()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(root.to_path_buf());
}

/// The kept unpack folder `path` is (or is in), if this engine made one there.
pub(crate) fn kept_root_of(path: &Path) -> Option<PathBuf> {
    let set = kept().lock().unwrap_or_else(|e| e.into_inner());
    path.ancestors()
        .find(|a| set.contains(*a))
        .map(Path::to_path_buf)
}

/// Removes a kept unpack folder: what was made from the game in it is finished.
pub(crate) fn release(root: &Path) -> std::io::Result<()> {
    kept()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(root);
    crate::workshop_api::forget_unpack(root);
    match archive_extract::remove_path_retrying(root) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A folder for the unpacked game in `out` that nothing has yet: `<archive's name>.unpacked`.
fn kept_dest(out: &Path, source: &Path) -> PathBuf {
    let stem = crate::archive_tree::stem(source);
    let mut dest = out.join(format!("{stem}.unpacked"));
    let mut n = 2;
    while dest.symlink_metadata().is_ok() {
        dest = out.join(format!("{stem}.unpacked-{n}"));
        n += 1;
    }
    dest
}

/// The room the output drive needs: for the unpack at its largest (with the usual 1% over),
/// and, after it, for what is made from the game, about as large again. The archive's own
/// space counts as room only when it is on that drive.
fn room_needed(plan: &free::FreePlan, same_drive: bool) -> u64 {
    let margin = plan.unpacked / 100;
    let unpacking = plan.room_needed(same_drive).saturating_add(margin);
    let credit = if same_drive { plan.archive_bytes } else { 0 };
    let building = (plan.unpacked.saturating_mul(2))
        .saturating_add(margin)
        .saturating_sub(credit);
    unpacking.max(building)
}

fn shortfall(free: u64, needed: u64) -> Option<String> {
    let gib = |n: u64| n as f64 / (1u64 << 30) as f64;
    (free < needed).then(|| {
        format!(
            "only {:.1} GiB free in the output folder; unpacking this archive while freeing \
             it, and building from it, needs about {:.1} GiB",
            gib(free),
            gib(needed)
        )
    })
}

/// An archive checked for this, with what the check found.
struct Checked {
    plan: free::FreePlan,
    same_drive: bool,
    needed: u64,
    free: Option<u64>,
}

/// Everything that can be checked before anything is unpacked: the archive (`free::plan`) and
/// the room in `out`. `Err` is the plain reason it can't be done; nothing was changed.
fn check(source: &Path, password: Option<&str>, out: &Path) -> Result<Checked, String> {
    let plan = free::plan(source, password).map_err(|e| format!("{e:#}"))?;
    // The folder may not exist yet: the drive it will be on is its nearest folder's.
    let on = out.ancestors().find(|a| a.exists()).unwrap_or(out);
    let same_drive = free::same_drive(&plan.parts[0], on);
    let needed = room_needed(&plan, same_drive);
    let free = build::free_bytes(out);
    if let Some(why) = free.and_then(|free| shortfall(free, needed)) {
        return Err(why);
    }
    Ok(Checked {
        plan,
        same_drive,
        needed,
        free,
    })
}

#[derive(Deserialize)]
pub(crate) struct FreeCheckReq {
    /// A `.zip`, `.7z`, `.rar` (the first part of a multi-part set), tar or `.zst` on this
    /// computer.
    source: String,
    #[serde(default)]
    output_dir: Option<String>,
    /// RAR only; never logged.
    #[serde(default)]
    password: Option<String>,
}

/// POST /api/fpkg/extract/free-check — whether an archive can be unpacked with its space given
/// back. Asks before `free_archive` on `/api/fpkg/extract`, and says how it would go: `mode` is
/// `entries` (a `.zip`: freed file by file), `blocks` (a `.7z` of more than one block: block by
/// block), `parts` (a multi-part `.rar`: part by part) or `end` (anything else: deleted once the
/// unpack is finished and checked). `parts` counts the blocks or parts (1 otherwise); `game` is
/// `folder` or `image`. `archive_bytes` is the archive's size (every part), `unpacked` what it
/// unpacks to, `same_drive` whether it sits on the output folder's drive (only then does its
/// freed space count), `needed` the free bytes the unpack and the build after it need, and
/// `free` the output folder's free bytes (null when they could not be read). Changes nothing.
/// 400 with the plain reason when it can't be done, including too little room.
pub(crate) async fn free_check_handler(Json(req): Json<FreeCheckReq>) -> impl IntoResponse {
    let source = crate::fpkg_api::resolve_engine_path(&req.source);
    if archive_extract::archive_kind(&source).is_none() || !source.is_file() {
        return json_err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a .zip, .7z or .rar file", source.display()),
        )
        .into_response();
    }
    let out = crate::fpkg_api::output_dir(req.output_dir.as_deref());
    let password = req.password.filter(|p| !p.is_empty());
    let checked =
        tokio::task::spawn_blocking(move || check(&source, password.as_deref(), &out)).await;
    match checked {
        Ok(Ok(c)) => Json(serde_json::json!({
            "mode": c.plan.mode.name(),
            "game": c.plan.game.name(),
            "parts": c.plan.pieces,
            "archive_bytes": c.plan.archive_bytes,
            "unpacked": c.plan.unpacked,
            "same_drive": c.same_drive,
            "needed": c.needed,
            "free": c.free,
        }))
        .into_response(),
        Ok(Err(why)) => json_err(StatusCode::BAD_REQUEST, why).into_response(),
        Err(join) => json_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("the archive check failed: {join}"),
        )
        .into_response(),
    }
}

/// An archive about to be unpacked with its space given back: checked, and claimed so that
/// nothing else reads it until this is dropped.
pub(crate) struct Freeing {
    plan: free::FreePlan,
    _claims: Vec<Claim>,
    _locks: Vec<ClaimLock>,
}

/// Why an unpack stopped.
pub(crate) struct Failure {
    pub(crate) error: String,
    /// [`PARTLY_DELETED`] when part of the archive is gone.
    pub(crate) reason: Option<String>,
    /// The folder the unpacked files were kept in, then.
    pub(crate) detail: Option<String>,
}

impl Freeing {
    /// Checks `source` and the room in `out`, and claims every file of the archive. `Err` is
    /// the answer to give (400 with the reason, or 409 when something is using it); nothing
    /// was changed.
    pub(crate) fn prepare(
        source: &Path,
        password: Option<&str>,
        out: &Path,
    ) -> Result<Freeing, (StatusCode, String)> {
        let checked = check(source, password, out).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        let mut claims = Vec::new();
        let mut locks = Vec::new();
        for part in &checked.plan.parts {
            claims.push(Claim::take(part).map_err(|e| (StatusCode::CONFLICT, e))?);
            match ClaimLock::take(part) {
                Ok(lock) => locks.extend(lock),
                Err(()) => {
                    return Err((
                        StatusCode::CONFLICT,
                        format!("{} is in use by another PS5Upload", part.display()),
                    ))
                }
            }
        }
        Ok(Freeing {
            plan: checked.plan,
            _claims: claims,
            _locks: locks,
        })
    }

    /// What the job's progress runs to (an archive freed as it goes is read through once
    /// first).
    pub(crate) fn total(&self) -> u64 {
        match self.plan.mode {
            free::FreeMode::End => self.plan.unpacked,
            _ => self.plan.unpacked.saturating_mul(2),
        }
    }

    pub(crate) fn mode(&self) -> &'static str {
        self.plan.mode.name()
    }

    /// The folder this unpacks into, in `out`: made known as one to keep.
    pub(crate) fn dest_in(&self, out: &Path) -> PathBuf {
        let dest = kept_dest(out, &self.plan.parts[0]);
        keep(&dest);
        dest
    }

    /// Unpacks into `dest` and returns the game found there. A failure with the archive still
    /// whole removes `dest`; one with part of it gone keeps everything unpacked so far and
    /// says where it is.
    pub(crate) fn run(
        &self,
        dest: &Path,
        password: Option<&str>,
        progress: &mut dyn FnMut(u64, u64),
        cancel: &AtomicBool,
    ) -> Result<PathBuf, Failure> {
        let mut game = None;
        let unpacked = free::consume(&self.plan, dest, password, progress, cancel, &mut || {
            game = Some(archive_extract::find_game(dest)?);
            Ok(())
        });
        match (unpacked, game) {
            (Ok(out), Some(game)) => {
                for p in &out.not_removed {
                    crate::engine_log::record(
                        "warn",
                        format!(
                            "fpkg: unpacked, but {} could not be deleted (something holds it)",
                            p.display()
                        ),
                    );
                }
                Ok(game)
            }
            (Ok(_), None) => Err(failure(
                free::ConsumeError {
                    error: anyhow::anyhow!("the unpack ended without looking for the game"),
                    left: Some(Vec::new()),
                },
                dest,
            )),
            (Err(e), _) => Err(failure(e, dest)),
        }
    }
}

/// The failure as the job reports it. With the archive whole, the unpack folder goes (as any
/// failed unpack's does); with part of it gone, the folder stays and the message says so.
fn failure(e: free::ConsumeError, dest: &Path) -> Failure {
    let cause = format!("{:#}", e.error);
    let Some(left) = e.left else {
        let _ = release(dest);
        return Failure {
            error: cause,
            reason: None,
            detail: None,
        };
    };
    Failure {
        error: partly_deleted_message(&cause, dest, &left),
        reason: Some(PARTLY_DELETED.to_string()),
        detail: Some(dest.display().to_string()),
    }
}

fn partly_deleted_message(cause: &str, dest: &Path, left: &[PathBuf]) -> String {
    let left = if left.is_empty() {
        "Nothing is left of the archive.".to_string()
    } else {
        format!(
            "What is left of the archive: {}.",
            left.iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "{}. {PARTLY_DELETED_TEXT} Everything unpacked before it stopped is kept in {}. {left}",
        cause.trim_end_matches('.'),
        dest.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_of(mode: free::FreeMode, archive: u64, unpacked: u64, peak: u64) -> free::FreePlan {
        // A real plan of a small zip, with the figures under test put in.
        let dir = crate::test_tmp::temp_path(format!(
            "ps5u-free-room-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let z = dir.join("g.zip");
        {
            use std::io::Write;
            let mut zw = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
            zw.start_file(
                "g/sce_sys/param.json",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(b"{}").unwrap();
            zw.finish().unwrap();
        }
        let plan = free::plan(&z, None);
        let _ = std::fs::remove_dir_all(&dir);
        let mut plan = plan.unwrap();
        plan.mode = mode;
        plan.archive_bytes = archive;
        plan.unpacked = unpacked;
        plan.peak_beside = peak;
        plan
    }

    /// On the archive's own drive the job fits where the archive's space covers part of it;
    /// on another drive it needs what any unpack and build need.
    #[test]
    fn the_archives_own_space_counts_only_on_its_own_drive() {
        let gib = 1u64 << 30;
        // A 40 GiB zip of a 50 GiB game, freed as it goes: 10 GiB over the archive at most.
        let zip = plan_of(free::FreeMode::Entries, 40 * gib, 50 * gib, 11 * gib);
        let same = room_needed(&zip, true);
        // 100.5 for unpack and build, less the archive's 40.
        assert_eq!(same, 100 * gib + gib / 2 - 40 * gib);
        let other = room_needed(&zip, false);
        assert_eq!(other, 100 * gib + gib / 2);
        assert!(same < other);
        // The usual way needs the archive too: 40 more than this on the same drive.
        // Freed only at the end, the unpack itself needs all 50 beside the archive.
        let end = plan_of(free::FreeMode::End, 40 * gib, 50 * gib, 50 * gib);
        assert_eq!(room_needed(&end, true), 60 * gib + gib / 2);
        // A game that packs to almost nothing: the unpack is the larger need.
        let tight = plan_of(free::FreeMode::End, 49 * gib, 50 * gib, 50 * gib);
        assert_eq!(room_needed(&tight, true), 51 * gib + gib / 2);
        let tight = plan_of(free::FreeMode::End, 99 * gib, 50 * gib, 50 * gib);
        assert_eq!(room_needed(&tight, true), 50 * gib + gib / 2);

        assert!(shortfall(60 * gib, 60 * gib).is_none());
        let why = shortfall(10 * gib, 60 * gib).unwrap();
        assert!(
            why.contains("10.0 GiB free") && why.contains("60.0 GiB"),
            "{why}"
        );
    }

    #[test]
    fn a_failure_with_part_of_the_archive_gone_says_where_everything_is() {
        let dest = Path::new("/out/Game.unpacked");
        let f = failure(
            free::ConsumeError {
                error: anyhow::anyhow!("cancelled"),
                left: Some(vec![PathBuf::from("/dl/Game.zip.partial-deleted")]),
            },
            dest,
        );
        assert_eq!(f.reason.as_deref(), Some(PARTLY_DELETED));
        assert_eq!(f.detail.as_deref(), Some("/out/Game.unpacked"));
        assert_eq!(
            f.error,
            "cancelled. The archive is partly deleted and can't be used again. Everything \
             unpacked before it stopped is kept in /out/Game.unpacked. What is left of the \
             archive: /dl/Game.zip.partial-deleted."
        );
        let gone = partly_deleted_message("no game in the archive.", dest, &[]);
        assert!(gone.starts_with("no game in the archive. The archive is partly deleted"));
        assert!(gone.ends_with("Nothing is left of the archive."), "{gone}");
    }

    #[test]
    fn a_kept_folder_is_known_by_any_path_in_it_and_gets_a_name_of_its_own() {
        let base = crate::test_tmp::temp_path(format!("ps5u-kept-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let first = kept_dest(&base, Path::new("/dl/Game.part1.rar"));
        assert_eq!(first, base.join("Game.unpacked"));
        std::fs::create_dir_all(first.join("Game/sce_sys")).unwrap();
        assert_eq!(
            kept_dest(&base, Path::new("/dl/Game.zip")),
            base.join("Game.unpacked-2")
        );
        // Not ours until it is noted: nothing can ask for a user's folder to be removed.
        assert_eq!(kept_root_of(&first.join("Game")), None);
        keep(&first);
        assert_eq!(
            kept_root_of(&first.join("Game/sce_sys")),
            Some(first.clone())
        );
        assert_eq!(kept_root_of(&base), None);
        release(&first).unwrap();
        assert!(!first.exists());
        assert_eq!(kept_root_of(&first), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
