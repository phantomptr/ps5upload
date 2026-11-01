//! Workshop's "Rebuild AMPR index" (#621): a libSceAmpr game's `ampr_emu.index` regenerated
//! from its files where it stands, without converting it.
//!
//! `ampr_emu` trusts the index's sizes, so one left stale by a trimmed, patched or re-dumped
//! folder breaks the game. A package or game image always carries a fresh one (the build
//! generates it); this is for a game played as it is:
//!
//! - a **folder on this computer** or **on the PS5** (`ps5://`): the index is regenerated from
//!   the folder's files (the same listing a game image's build makes, see
//!   [`ps5upload_fpkg::ampr_index::rebuild`]) and written at the game's root, the game's own
//!   copy kept once as `ampr_emu.index.bak`. A console folder is read and written through the
//!   helper, like a console dump Convert reads in place.
//! - an **uncompressed image** on this computer is never changed: Workshop writes an edited
//!   copy (`/api/exfat/build` with `rebuild_ampr_index`, see [`open_for_index`]), whose build
//!   regenerates the index as every image build does.
//! - a **.ffpfsc** is compressed and read-only: converting it again makes a fresh index.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use ps5upload_fpkg::ampr_index::{self, Rebuilt, BACKUP, NAME};

use crate::fpkg_api::resolve_engine_path;
use crate::{
    json_err, now_ms, register_transfer_cancel, set_job, AppState, JobCreated, JobFailOnDropGuard,
    JobStage, JobState,
};

/// The most a console write takes in one go (`fs_write_bytes_large`); an index this big lists
/// well over a hundred thousand files.
const CONSOLE_WRITE_MAX: usize = 16 << 20;

/// What the job ends with, in its `commit_ack`.
#[derive(serde::Serialize, Debug, PartialEq)]
pub(crate) struct Outcome {
    /// Files the new index lists.
    pub files: usize,
    /// It lists other files or sizes than the game's own did (or the game had none).
    pub differed: bool,
    /// The game had an index before.
    pub had_index: bool,
    /// Where the game's own index was kept, when this rebuild kept it.
    pub backup: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AmprIndexReq {
    /// A game folder on this computer, or on a console (`ps5://<ip>/<path>`).
    source: String,
}

/// Where the game is, and so how its index is written.
#[derive(Debug, PartialEq)]
enum Target {
    Local(PathBuf),
    /// The console's address and the folder's path on it.
    Console {
        addr: String,
        path: String,
    },
}

/// Why `source` cannot have its index rebuilt in place, or where it is.
fn target_of(source: &str) -> Result<Target, String> {
    let path = resolve_engine_path(source);
    let image = crate::image_build::has_image_extension(&path);
    if source.starts_with("ps5://") {
        if image {
            return Err(
                "this image is on the PS5: an edited copy is written from an image on this \
                 computer, so copy it here first"
                    .into(),
            );
        }
        let (host, path) = crate::convert_source::console_target(source)?;
        if in_image_mount_root(&path) {
            return Err(MOUNTED_IMAGE.into());
        }
        return Ok(Target::Console {
            addr: crate::console_addr(&host),
            path,
        });
    }
    if ps5upload_fpkg::remote_source::is_remote(&path) {
        return Err(
            "this game is on a saved server: copy it to this computer or the PS5 first".into(),
        );
    }
    if image {
        return Err(
            "a game image is never changed here: Workshop writes an edited copy with the new \
             index"
                .into(),
        );
    }
    if !path.is_dir() {
        return Err(format!("{} is not a game folder", path.display()));
    }
    Ok(Target::Local(path))
}

/// Why a console folder that is a mounted image is refused: an image is mounted read-write by
/// default, so the index (and its backup) would be written into the user's image.
const MOUNTED_IMAGE: &str = "This folder is a mounted image; rebuild the index on the image's \
                             file instead (copy it to this computer)";

/// Where images are mounted on a console: ps5upload's own mounts, ShadowMount+'s, a running
/// game's sandbox (its own image) and a disc.
const CONSOLE_IMAGE_MOUNT_ROOTS: &[&str] = &[
    "/mnt/ps5upload",
    "/mnt/shadowmnt",
    "/mnt/sandbox",
    "/mnt/disc",
];

/// `path` is one of [`CONSOLE_IMAGE_MOUNT_ROOTS`] or inside one.
fn in_image_mount_root(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    CONSOLE_IMAGE_MOUNT_ROOTS
        .iter()
        .any(|root| path == *root || path.starts_with(&format!("{root}/")))
}

/// The console's mount holding `path` is an image: one the payload mounted (it records the
/// image, and may mount it anywhere, e.g. `/data/homebrew/<title>`), a disk-image device
/// (`/dev/lvd*`, `/dev/md*`), or a mount from or at an image mount root (a nullfs of one).
fn on_mounted_image(vols: &ps5upload_core::volumes::VolumeList, path: &str) -> bool {
    vols.find_for_path(path.trim_end_matches('/'))
        .is_some_and(|v| {
            !v.source_image.is_empty()
                || v.mount_from.starts_with("/dev/lvd")
                || v.mount_from.starts_with("/dev/md")
                || in_image_mount_root(&v.path)
                || in_image_mount_root(&v.mount_from)
        })
}

/// A console folder is written only when the console's mount list says it is not inside a
/// mounted image; when the list cannot be read, it is not written either.
fn check_console_folder(addr: &str, path: &str) -> Result<(), String> {
    let vols = ps5upload_core::volumes::list_volumes(addr).map_err(|e| {
        format!("could not tell whether this folder is a mounted image (the PS5's drives: {e:#})")
    })?;
    if on_mounted_image(&vols, path) {
        return Err(MOUNTED_IMAGE.into());
    }
    Ok(())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Where [`write_local`] writes the new index before renaming it over the old one.
const PARTIAL: &str = ".ampr_emu.index.partial";

/// Writes a rebuilt index into a folder on this computer: the game's own kept once as
/// [`BACKUP`] (never over one already there), then the new one put in place whole (written
/// beside it and renamed over it, so the game never sees half an index).
pub(crate) fn write_local(root: &Path, r: &Rebuilt) -> Result<Outcome, String> {
    use std::io::Write;
    let backup = if r.save_backup {
        let (_, old) = r.old.as_ref().expect("a backup is of an old index");
        let at = root.join(BACKUP);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&at)
            .map_err(|e| format!("{}: {e}", at.display()))?;
        f.write_all(old)
            .and_then(|()| f.sync_all())
            .map_err(|e| format!("{}: {e}", at.display()))?;
        Some(at.display().to_string())
    } else {
        None
    };
    // The game's own spelling of the name, when it had one.
    let name = r.old.as_ref().map_or(NAME, |(p, _)| p.as_str());
    let target = root.join(name);
    let partial = root.join(PARTIAL);
    let written = std::fs::File::create(&partial)
        .and_then(|mut f| f.write_all(&r.index).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&partial, &target));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(format!("{}: {e}", target.display()));
    }
    Ok(Outcome {
        files: r.files,
        differed: r.differs,
        had_index: r.old.is_some(),
        backup,
    })
}

/// Writes a rebuilt index into a game folder on a console, through the helper.
fn write_console(addr: &str, dir: &str, r: &Rebuilt) -> Result<Outcome, String> {
    let dir = dir.trim_end_matches('/');
    if r.index.len() > CONSOLE_WRITE_MAX {
        return Err(format!(
            "the new index is {} MiB, more than the {} MiB a console write takes; copy the \
             folder here, rebuild it, and upload it",
            r.index.len() >> 20,
            CONSOLE_WRITE_MAX >> 20
        ));
    }
    let backup = if r.save_backup {
        let (_, old) = r.old.as_ref().expect("a backup is of an old index");
        let at = format!("{dir}/{BACKUP}");
        ps5upload_core::diagnostics::fs_write_bytes_large(addr, &at, old, true)
            .map_err(|e| format!("{at}: {e:#}"))?;
        Some(at)
    } else {
        None
    };
    let name = r.old.as_ref().map_or(NAME, |(p, _)| p.as_str());
    let at = format!("{dir}/{name}");
    ps5upload_core::diagnostics::fs_write_bytes_large(addr, &at, &r.index, false)
        .map_err(|e| format!("{at}: {e:#}"))?;
    Ok(Outcome {
        files: r.files,
        differed: r.differs,
        had_index: r.old.is_some(),
        backup,
    })
}

/// Rebuilds the index of the game at `target`, reporting each stage (`list`, `check`,
/// `write`) and stopping between them when `cancel` is set. Returns the outcome and the
/// index's path.
fn run(
    target: &Target,
    source: &str,
    cancel: &AtomicBool,
    stage: &mut dyn FnMut(&str, u32, u64),
) -> Result<(Outcome, String), String> {
    let stopped = || cancel.load(Ordering::Relaxed);
    stage("list", 0, 0);
    let open_at = match target {
        Target::Local(p) => {
            // A rebuild's own temp file left by an engine that was killed mid-write: never
            // part of the game, so never listed in its index.
            let _ = std::fs::remove_file(p.join(PARTIAL));
            p.clone()
        }
        Target::Console { .. } => PathBuf::from(source),
    };
    let mut tree = ps5upload_fpkg::source::open(&open_at).map_err(|e| e.to_string())?;
    if stopped() {
        return Err("cancelled".into());
    }
    stage("check", 1, tree.files().len() as u64);
    let rebuilt = ampr_index::rebuild(tree.as_mut(), now_secs())?;
    drop(tree);
    if stopped() {
        return Err("cancelled".into());
    }
    stage("write", 2, rebuilt.files as u64);
    match target {
        Target::Local(root) => {
            let name = rebuilt.old.as_ref().map_or(NAME, |(p, _)| p.as_str());
            let at = root.join(name).display().to_string();
            write_local(root, &rebuilt).map(|o| (o, at))
        }
        Target::Console { addr, path } => {
            let name = rebuilt.old.as_ref().map_or(NAME, |(p, _)| p.as_str());
            let at = format!("{}/{name}", path.trim_end_matches('/'));
            write_console(addr, path, &rebuilt).map(|o| (o, at))
        }
    }
}

fn running(started_at_ms: u64, stage: Option<JobStage>) -> JobState {
    JobState::Running {
        stage,
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
    }
}

/// POST /api/fpkg/ampr-index — rebuild a game folder's `ampr_emu.index` where it is (this
/// computer or a console), as a job: its stage is `list`, `check` or `write`, its cancel stops
/// it before anything is written, and when done `dest` is the index and `commit_ack` the
/// [`Outcome`]. A game whose eboot.bin does not import libSceAmpr fails, said so.
pub(crate) async fn ampr_index_handler(
    State(state): State<AppState>,
    Json(req): Json<AmprIndexReq>,
) -> impl IntoResponse {
    let target = match target_of(req.source.trim()) {
        Ok(t) => t,
        Err(e) => return json_err(StatusCode::BAD_REQUEST, e).into_response(),
    };
    if let Target::Console { addr, path } = &target {
        let (addr, path) = (addr.clone(), path.clone());
        let checked = tokio::task::spawn_blocking(move || check_console_folder(&addr, &path))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(e) = checked {
            return json_err(StatusCode::BAD_REQUEST, e).into_response();
        }
    }
    let job_id = Uuid::new_v4();
    let cancel = register_transfer_cancel(job_id);
    let started_at_ms = now_ms();
    set_job(
        &state.jobs,
        &state.events_tx,
        job_id,
        running(started_at_ms, None),
    );
    let source = req.source.trim().to_string();
    tokio::task::spawn_blocking(move || {
        let mut fail = JobFailOnDropGuard::new(
            state.jobs.clone(),
            state.events_tx.clone(),
            job_id,
            started_at_ms,
        );
        let mut stage = |id: &str, index: u32, total: u64| {
            set_job(
                &state.jobs,
                &state.events_tx,
                job_id,
                running(
                    started_at_ms,
                    Some(JobStage {
                        id: id.to_string(),
                        index,
                        count: 3,
                        done: 0,
                        total,
                    }),
                ),
            );
        };
        let outcome = run(&target, &source, &cancel, &mut stage);
        let completed_at_ms = now_ms();
        let elapsed_ms = completed_at_ms.saturating_sub(started_at_ms);
        match outcome {
            Ok((outcome, dest)) => {
                crate::engine_log::record(
                    "info",
                    format!(
                        "ampr index: rebuilt {dest} ({} files, differed={}, backup={:?})",
                        outcome.files, outcome.differed, outcome.backup
                    ),
                );
                set_job(
                    &state.jobs,
                    &state.events_tx,
                    job_id,
                    JobState::Done {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms,
                        tx_id_hex: String::new(),
                        bytes_sent: 0,
                        dest,
                        files_sent: outcome.files as u64,
                        skipped_files: 0,
                        skipped_bytes: 0,
                        commit_ack: Some(serde_json::json!(outcome)),
                    },
                );
            }
            Err(error) => {
                crate::engine_log::record("warn", format!("ampr index: {source}: {error}"));
                set_job(
                    &state.jobs,
                    &state.events_tx,
                    job_id,
                    JobState::Failed {
                        started_at_ms,
                        completed_at_ms,
                        elapsed_ms,
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

/// The image at `source` opened for an edited copy whose only change is a regenerated
/// `ampr_emu.index` (`/api/exfat/build` with `rebuild_ampr_index`): the copy's build
/// regenerates it, as every image build of a libSceAmpr game does. Refused for a compressed
/// image (converting it again makes the fresh index) and for a game that does not read
/// through AMPR. The image itself is only read.
pub(crate) fn open_for_index(source: &Path) -> Result<crate::fpkg_api::EditedImage, String> {
    let mut edited = crate::fpkg_api::open_edited(source, &[])?;
    if edited.compress {
        return Err(COMPRESSED.into());
    }
    if !ps5upload_fpkg::source::imports_ampr(edited.tree.as_mut(), "eboot.bin") {
        return Err(ampr_index::NOT_AMPR.into());
    }
    Ok(edited)
}

/// Why a `.ffpfsc` keeps its index.
pub(crate) const COMPRESSED: &str =
    "this image is compressed and read-only: rebuild the index by converting it again \
     (Workshop › Convert)";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_build::{self, AmprIndexed, ImageFormat};
    use ps5upload_fpkg::sample::VerifyMode;
    use ps5upload_fpkg::source::FolderSource;

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ps5upload-ampridx-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// A small game whose eboot.bin imports libSceAmpr (`ampr`), or one that does not.
    fn game(root: &Path, ampr: bool) -> PathBuf {
        let src = root.join("PPSA99999");
        std::fs::create_dir_all(src.join("sce_sys")).unwrap();
        std::fs::create_dir_all(src.join("data/sub")).unwrap();
        let mut eboot: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let import: &[u8] = if ampr {
            b"libSceAmpr.prx"
        } else {
            b"libSceGnmx.prx"
        };
        eboot[1000..1000 + import.len()].copy_from_slice(import);
        std::fs::write(src.join("eboot.bin"), eboot).unwrap();
        std::fs::write(
            src.join("sce_sys/param.json"),
            b"{\"titleId\":\"PPSA99999\"}",
        )
        .unwrap();
        std::fs::write(src.join("data/sub/a.bin"), vec![3u8; 5000]).unwrap();
        std::fs::write(src.join("data/b.txt"), b"hello").unwrap();
        src
    }

    fn rebuild_folder(dir: &Path, mtime: i64) -> Result<(Rebuilt, Outcome), String> {
        let mut tree = FolderSource::open(dir).map_err(|e| e.to_string())?;
        let r = ampr_index::rebuild(&mut tree, mtime)?;
        let o = write_local(dir, &r)?;
        Ok((r, o))
    }

    fn listed(index: &[u8]) -> Vec<(String, u64)> {
        let mut v: Vec<_> = ampr_index::parse(index)
            .expect("an AMPRIDX3 index")
            .into_iter()
            .map(|e| (e.path, e.size))
            .collect();
        v.sort();
        v
    }

    /// A folder's rebuilt index is byte for byte the one a game image of that folder carries.
    #[test]
    fn a_rebuilt_index_is_the_one_the_build_makes() {
        let root = scratch("same");
        let src = game(&root, true);
        let mut tree = FolderSource::open(&src).unwrap();
        let built = AmprIndexed::wrap_at(&mut tree, 1_700_000_000)
            .unwrap()
            .expect("an AMPR game")
            .index()
            .to_vec();
        let (r, outcome) = rebuild_folder(&src, 1_700_000_000).unwrap();
        assert_eq!(r.index, built);
        assert_eq!(std::fs::read(src.join(NAME)).unwrap(), built);
        assert_eq!(
            outcome,
            Outcome {
                files: 4,
                differed: true,
                had_index: false,
                backup: None,
            }
        );
        // Nothing was there to keep, and nothing is left half-written.
        assert!(!src.join(BACKUP).exists());
        assert!(!src.join(PARTIAL).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The game's own index is kept as .bak the first time, and only then; the new one lists
    /// the folder (backup included), and the folder's check passes afterwards.
    #[test]
    fn the_old_index_is_kept_once() {
        let root = scratch("bak");
        let src = game(&root, true);
        // A stale index: eboot.bin listed at a size it no longer has, a file that is gone.
        let stale = ampr_index::build(
            &[
                ("eboot.bin".into(), 1),
                ("gone.bin".into(), 9),
                ("data/b.txt".into(), 5),
            ],
            0,
        )
        .unwrap();
        std::fs::write(src.join(NAME), &stale).unwrap();

        let (_, first) = rebuild_folder(&src, 1).unwrap();
        assert!(first.differed && first.had_index);
        assert_eq!(
            first.backup.as_deref(),
            Some(src.join(BACKUP).display().to_string().as_str())
        );
        assert_eq!(std::fs::read(src.join(BACKUP)).unwrap(), stale);
        let now = std::fs::read(src.join(NAME)).unwrap();
        assert_eq!(
            listed(&now),
            [
                (BACKUP.to_string(), stale.len() as u64),
                ("data/b.txt".into(), 5),
                ("data/sub/a.bin".into(), 5000),
                ("eboot.bin".into(), 70_000),
                ("sce_sys/param.json".into(), 23),
            ]
        );
        let check = |dir: &Path| {
            let mut tree = ps5upload_fpkg::source::open(dir).unwrap();
            ps5upload_fpkg::source::readiness(tree.as_mut())
                .checks
                .into_iter()
                .find(|c| c.name == "ampr_emu.index for a libSceAmpr title")
                .expect("the ampr check")
        };
        let c = check(&src);
        assert!(c.ok, "{}", c.detail);

        // Again: the backup stays the game's own, and an index that already describes the
        // folder is said not to differ.
        let (_, second) = rebuild_folder(&src, 2).unwrap();
        assert_eq!(second.backup, None);
        assert!(!second.differed);
        assert_eq!(std::fs::read(src.join(BACKUP)).unwrap(), stale);
        assert_eq!(
            listed(&std::fs::read(src.join(NAME)).unwrap()),
            listed(&now)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A game that does not read through libSceAmpr is refused, and nothing is written.
    #[test]
    fn a_game_without_ampr_is_refused() {
        let root = scratch("noampr");
        let src = game(&root, false);
        let err = rebuild_folder(&src, 0).unwrap_err();
        assert_eq!(err, ampr_index::NOT_AMPR);
        assert!(!src.join(NAME).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The job's own steps on a folder: stages in order, the outcome, and Cancel before
    /// anything is written.
    #[test]
    fn the_job_reports_stages_and_cancel_writes_nothing() {
        let root = scratch("job");
        let src = game(&root, true);
        let target = target_of(&src.display().to_string()).unwrap();
        let mut stages = Vec::new();
        let (outcome, dest) = run(
            &target,
            &src.display().to_string(),
            &AtomicBool::new(false),
            &mut |s, _, _| stages.push(s.to_string()),
        )
        .unwrap();
        assert_eq!(stages, ["list", "check", "write"]);
        assert_eq!(dest, src.join(NAME).display().to_string());
        assert_eq!(outcome.files, 4);
        std::fs::remove_file(src.join(NAME)).unwrap();
        let err = run(
            &target,
            &src.display().to_string(),
            &AtomicBool::new(true),
            &mut |_, _, _| {},
        )
        .unwrap_err();
        assert_eq!(err, "cancelled");
        assert!(!src.join(NAME).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A killed engine's temp file is not listed by the next rebuild (nor left behind).
    #[test]
    fn a_leftover_partial_is_not_listed() {
        let root = scratch("partial");
        let src = game(&root, true);
        std::fs::write(src.join(PARTIAL), b"half an index").unwrap();
        let target = target_of(&src.display().to_string()).unwrap();
        let (outcome, dest) = run(
            &target,
            &src.display().to_string(),
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(outcome.files, 4);
        let now = listed(&std::fs::read(dest).unwrap());
        assert!(now.iter().all(|(p, _)| p != PARTIAL), "{now:?}");
        assert!(!src.join(PARTIAL).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A console folder inside a mounted image is refused: the rebuild would write into the
    /// user's image.
    #[test]
    fn a_mounted_image_on_the_console_is_refused() {
        for source in [
            "ps5://192.168.1.5/mnt/ps5upload/PPSA01234",
            "ps5://192.168.1.5/mnt/shadowmnt/Game_ca51a0d7/",
            "ps5://192.168.1.5/mnt/sandbox/pfsmnt/PPSA01234-app0",
        ] {
            assert_eq!(target_of(source).unwrap_err(), MOUNTED_IMAGE, "{source}");
        }
        // Not a prefix by name alone.
        assert!(target_of("ps5://192.168.1.5/mnt/ps5uploads/g").is_ok());

        let vols: ps5upload_core::volumes::VolumeList = serde_json::from_str(
            r#"{"volumes":[
            {"path":"/data","mount_from":"/dev/ssd0.user","fs_type":"ufs","total_bytes":1,"free_bytes":1,"writable":true},
            {"path":"/mnt/ext0","mount_from":"/dev/nvme1","fs_type":"exfatfs","total_bytes":1,"free_bytes":1,"writable":true},
            {"path":"/data/homebrew/PPSA17599","mount_from":"/dev/lvd0","fs_type":"ufs","total_bytes":1,"free_bytes":0,"writable":true,"source_image":"/data/homebrew/PPSA17599.ffpkg"},
            {"path":"/data/img","mount_from":"/dev/md3","fs_type":"exfatfs","total_bytes":1,"free_bytes":1,"writable":true},
            {"path":"/system_ex/app/PPSA02222","mount_from":"/mnt/shadowmnt/G_1","fs_type":"nullfs","total_bytes":1,"free_bytes":1,"writable":true},
            {"path":"/system_ex/app/PPSA03333","mount_from":"/data/homebrew/PPSA03333","fs_type":"nullfs","total_bytes":1,"free_bytes":1,"writable":true}
            ]}"#,
        )
        .unwrap();
        for path in [
            "/data/homebrew/PPSA17599",
            "/data/homebrew/PPSA17599/",
            "/data/homebrew/PPSA17599/sub",
            "/data/img/G",
            "/system_ex/app/PPSA02222",
        ] {
            assert!(on_mounted_image(&vols, path), "{path}");
        }
        for path in [
            "/data/homebrew/PPSA01234",
            "/data/homebrew/PPSA175990",
            "/mnt/ext0/games/G",
            "/system_ex/app/PPSA03333",
        ] {
            assert!(!on_mounted_image(&vols, path), "{path}");
        }
    }

    #[test]
    fn where_an_index_is_rebuilt() {
        assert_eq!(
            target_of("ps5://192.168.1.5/data/homebrew/PPSA01234").unwrap(),
            Target::Console {
                addr: crate::console_addr("192.168.1.5"),
                path: "/data/homebrew/PPSA01234".into(),
            }
        );
        for (source, says) in [
            ("ps5://192.168.1.5/mnt/ext0/g.exfat", "on the PS5"),
            ("/games/g.exfat", "edited copy"),
            ("/games/g.ffpfsc", "edited copy"),
            ("remote://nas/games/g", "saved server"),
            ("/no/such/folder", "not a game folder"),
        ] {
            let err = target_of(source).unwrap_err();
            assert!(err.contains(says), "{source}: {err}");
        }
    }

    /// An exFAT image's edited copy carries the regenerated index (the folder's own, rebuilt),
    /// and the image itself is byte for byte as it was. A compressed image is refused.
    #[test]
    fn an_images_edited_copy_carries_the_new_index() {
        let root = scratch("image");
        let src = game(&root, true);
        // The image carries a stale index, as a dump made into an image without one would.
        std::fs::write(src.join(NAME), b"AMPRIDX3 stale").unwrap();
        let image = root.join("PPSA99999.exfat");
        let mut tree = FolderSource::open(&src).unwrap();
        image_build::build(
            ImageFormat::Exfat,
            None,
            &mut tree,
            &image,
            VerifyMode::Full,
            image_build::DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        let before = std::fs::read(&image).unwrap();

        let mut edited = open_for_index(&image).unwrap();
        assert_eq!(
            (edited.format, edited.compress),
            (ImageFormat::Exfat, false)
        );
        let mut indexed = AmprIndexed::wrap_at(&mut *edited.tree, 7)
            .unwrap()
            .expect("an AMPR game");
        let copy = root.join("PPSA99999-edited.exfat");
        image_build::build(
            ImageFormat::Exfat,
            None,
            &mut indexed,
            &copy,
            VerifyMode::Full,
            image_build::DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(std::fs::read(&image).unwrap(), before, "the image changed");
        let mut out = ps5upload_fpkg::source::open(&copy).unwrap();
        let carried = out.read(NAME).unwrap();
        std::fs::remove_file(src.join(NAME)).unwrap();
        let mut folder = FolderSource::open(&src).unwrap();
        assert_eq!(carried, ampr_index::rebuild(&mut folder, 7).unwrap().index);
        assert_eq!(listed(&carried).len(), 4);

        // A .ffpfsc keeps its index: converting it again makes a fresh one.
        let packed = root.join("PPSA99999.ffpfsc");
        let mut tree = FolderSource::open(&src).unwrap();
        let (_, inner) = image_build::output_names(&src, ImageFormat::Exfat, true);
        image_build::build(
            ImageFormat::Exfat,
            inner.as_deref(),
            &mut tree,
            &packed,
            VerifyMode::Full,
            image_build::DEFAULT_VERIFY_CHUNK,
            &AtomicBool::new(false),
            &mut |_, _, _| {},
        )
        .unwrap();
        assert_eq!(open_for_index(&packed).err().unwrap(), COMPRESSED);
        let _ = std::fs::remove_dir_all(&root);
    }
}
