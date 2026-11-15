//! Convert's "Move the source to the Trash once the output is verified".
//!
//! The idea is ps5-dump-forge's `--delete-source` (`crates/ps5-dump-forge-core/src/convert.rs`,
//! `jobs.rs` and `delete.rs`, GPLv3 as this is): the source goes only after the job is done,
//! with its output read back and given its name, and only while it is still what the job
//! read. The job stays done either way and says which happened.
//!
//! Stricter than Forge in three ways: the source is moved to the system Trash or Recycle Bin
//! and never deleted for good (where there is no Trash the option is refused); only a file or
//! folder on this computer goes, never one on a PS5 or a server; and the output has to be on
//! this computer too, so the copy that replaces the source was read back by this engine.

use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// What a job saw of its source before it read it, to tell afterwards whether the source is
/// still that: a file's size and time, or every file (size, time) and folder of a folder.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    File(u64, Option<SystemTime>),
    /// By path under the folder, sorted; a folder ends in `/`, a link in `@`.
    Folder(Vec<(String, u64, Option<SystemTime>)>),
}

fn walk(
    dir: &Path,
    under: &str,
    out: &mut Vec<(String, u64, Option<SystemTime>)>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if under.is_empty() {
            name
        } else {
            format!("{under}/{name}")
        };
        // A link is listed, never followed: what it points at is not part of the source.
        let meta = entry.metadata()?;
        if meta.file_type().is_symlink() {
            out.push((format!("{rel}@"), 0, None));
        } else if meta.is_dir() {
            walk(&entry.path(), &rel, out)?;
            out.push((format!("{rel}/"), 0, None));
        } else {
            out.push((rel, meta.len(), meta.modified().ok()));
        }
    }
    Ok(())
}

fn see(path: &Path) -> std::io::Result<Seen> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut all = Vec::new();
        walk(path, "", &mut all)?;
        all.sort();
        Ok(Seen::Folder(all))
    } else {
        Ok(Seen::File(meta.len(), meta.modified().ok()))
    }
}

/// `path` with links resolved, for one that may not exist yet: its nearest existing parent
/// resolved, the rest as given.
fn resolved(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return Some(rest.iter().rev().fold(real, |p, c| p.join(c)));
        }
        rest.push(at.file_name()?.to_os_string());
        at = at.parent()?;
    }
}

/// A folder another filesystem is mounted on (a drive's own top folder): never a game's.
#[cfg(unix)]
fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Some(parent), Ok(here)) = (path.parent(), std::fs::metadata(path)) else {
        return true;
    };
    std::fs::metadata(parent).map_or(true, |up| up.dev() != here.dev())
}

#[cfg(not(unix))]
fn is_mount_point(path: &Path) -> bool {
    // `D:\` has no parent; a folder mounted as a drive is rare enough to leave to the Recycle
    // Bin's own refusal.
    path.parent().is_none()
}

/// Why `source` can never be moved to the Trash by a conversion, whatever it makes; `None`
/// when it can. Said by the inspection, so the option can explain itself before a run.
pub(crate) fn source_refusal(source: &Path) -> Option<String> {
    source_refusal_with(
        source,
        crate::collection_tidy_api::trash_available(),
        &crate::fpkg_api::user_home(),
    )
}

fn source_refusal_with(source: &Path, trash: bool, home: &Path) -> Option<String> {
    if ps5upload_fpkg::remote_source::is_remote(source) {
        return Some(
            "the source is on a PS5 or a server: only a file or folder on this computer is \
             moved to the Trash"
                .into(),
        );
    }
    if crate::fpkg_api::extract_root_of(source).is_some() {
        return Some(
            "the game was unpacked from an archive, and that scratch copy is removed anyway: \
             the archive itself is kept"
                .into(),
        );
    }
    if !trash {
        return Some(
            "this engine has no Trash or Recycle Bin to move it to (it runs in Docker, or on a \
             Linux system with no desktop session), and a source is never deleted for good"
                .into(),
        );
    }
    let meta = match std::fs::symlink_metadata(source) {
        Ok(m) => m,
        Err(e) => return Some(format!("{}: {e}", source.display())),
    };
    if meta.file_type().is_symlink() {
        return Some(format!(
            "{} is a link, and what it points to would stay: remove it yourself",
            source.display()
        ));
    }
    if !meta.is_dir() && !meta.is_file() {
        return Some(format!("{} is not a file or a folder", source.display()));
    }
    let Ok(real) = std::fs::canonicalize(source) else {
        return Some(format!("{} could not be resolved", source.display()));
    };
    // A whole drive, or a folder the user's own files live under, is not a game to throw away
    // whatever is in it.
    let normal = real
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if normal == 0 || (meta.is_dir() && (is_mount_point(&real) || home.starts_with(&real))) {
        return Some(format!(
            "{} is a whole drive or holds your home folder: it is never moved to the Trash",
            real.display()
        ));
    }
    None
}

/// Why `source` cannot go once `output` is made: one is inside the other (or they are the
/// same), so removing the source would take the output with it, or the output's folder away.
fn nesting_refusal(source: &Path, output: &Path) -> Option<String> {
    let (Some(src), Some(out)) = (resolved(source), resolved(output)) else {
        return Some("the source's or the output's real path could not be worked out".into());
    };
    if out.starts_with(&src) {
        return Some(format!(
            "the output {} is inside the source {}: pick another output folder",
            out.display(),
            src.display()
        ));
    }
    if src.starts_with(&out) {
        return Some(format!(
            "the source {} is inside the output {}",
            src.display(),
            out.display()
        ));
    }
    None
}

/// A source a job may move to the Trash when it is done: where it really is, and what it was
/// before the job read it.
#[derive(Debug)]
pub(crate) struct Plan {
    source: PathBuf,
    seen: Seen,
}

impl Plan {
    /// The source, with its links resolved.
    pub(crate) fn source(&self) -> &Path {
        &self.source
    }
}

/// Checks that `source` may go once `output` (the image, or the folder, about to be made on
/// this computer) is verified, and notes what the source is now. An error is the reason the
/// request is refused: nothing has been written yet.
pub(crate) fn plan(source: &Path, output: &Path) -> Result<Plan, String> {
    // Tests run where there may be no Trash (a headless Linux): they never reach one anyway
    // (see `finish`).
    let trash = cfg!(test) || crate::collection_tidy_api::trash_available();
    plan_with(source, output, trash, &crate::fpkg_api::user_home())
}

fn plan_with(source: &Path, output: &Path, trash: bool, home: &Path) -> Result<Plan, String> {
    let refused = |why: String| format!("the source can't be moved to the Trash: {why}");
    if let Some(why) = source_refusal_with(source, trash, home) {
        return Err(refused(why));
    }
    if let Some(why) = nesting_refusal(source, output) {
        return Err(refused(why));
    }
    let real =
        std::fs::canonicalize(source).map_err(|e| refused(format!("{}: {e}", source.display())))?;
    let seen = see(&real).map_err(|e| refused(format!("{}: {e}", real.display())))?;
    Ok(Plan { source: real, seen })
}

/// How a planned removal ended. The job is done either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub(crate) removed: bool,
    /// One line for the log and the screen: `source moved to the Trash: <path>` or
    /// `source kept: <reason>`.
    pub(crate) note: String,
}

impl Outcome {
    fn kept(why: impl std::fmt::Display) -> Self {
        Self {
            removed: false,
            note: format!("source kept: {why}"),
        }
    }
}

/// The job is done and `output` verified and in place: moves the planned source to the Trash,
/// unless something says to keep it. `warnings` are the run's (any keeps the source: a
/// conversion that had something to say is looked at before its source goes); `busy` is
/// whether another running job reads the source.
pub(crate) fn finish(plan: &Plan, output: &Path, warnings: &[String], busy: bool) -> Outcome {
    finish_with(plan, output, warnings, busy, &mut |path| trash(path))
}

#[cfg(not(test))]
fn trash(path: &Path) -> Result<(), String> {
    crate::collection_tidy_api::move_to_trash(path)
}

/// Tests never move anything to the real Trash: what would have gone is recorded instead
/// ([`trashed_in_tests`]), and the source stays.
#[cfg(test)]
fn trash(path: &Path) -> Result<(), String> {
    trashed()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(path.to_path_buf());
    Err("tests never use the Trash".into())
}

#[cfg(test)]
fn trashed() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static TRASHED: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> =
        std::sync::OnceLock::new();
    TRASHED.get_or_init(Default::default)
}

/// Tests: whether a build would have moved `path` to the Trash.
#[cfg(test)]
pub(crate) fn trashed_in_tests(path: &Path) -> bool {
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    (trashed().lock().unwrap_or_else(|e| e.into_inner()).iter()).any(|p| *p == real || p == path)
}

fn finish_with(
    plan: &Plan,
    output: &Path,
    warnings: &[String],
    busy: bool,
    trash: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Outcome {
    if let Some(first) = warnings.first() {
        return Outcome::kept(format!(
            "the conversion had {} warning(s), the first: {first}",
            warnings.len()
        ));
    }
    if busy {
        return Outcome::kept("another job is reading it");
    }
    if std::fs::symlink_metadata(output).is_err() {
        return Outcome::kept(format!("the output {} is not there", output.display()));
    }
    // Again, now that the output exists and its links resolve.
    if let Some(why) = nesting_refusal(&plan.source, output) {
        return Outcome::kept(why);
    }
    match see(&plan.source) {
        Ok(now) if now == plan.seen => {}
        Ok(_) => {
            return Outcome::kept(format!(
                "{} changed while it was being converted",
                plan.source.display()
            ))
        }
        Err(e) => return Outcome::kept(format!("{}: {e}", plan.source.display())),
    }
    match trash(&plan.source) {
        Ok(()) => Outcome {
            removed: true,
            note: format!("source moved to the Trash: {}", plan.source.display()),
        },
        Err(e) => Outcome::kept(format!(
            "{} could not be moved to the Trash: {e}",
            plan.source.display()
        )),
    }
}

/// A finished job's `commit_ack` with the removal's outcome in it (`source_removed`,
/// `source_note`); as it was when no removal was asked for.
pub(crate) fn ack(
    ack: Option<serde_json::Value>,
    outcome: Option<&Outcome>,
) -> Option<serde_json::Value> {
    let Some(outcome) = outcome else {
        return ack;
    };
    let mut ack = match ack {
        Some(serde_json::Value::Object(o)) => o,
        _ => serde_json::Map::new(),
    };
    ack.insert("source_removed".into(), outcome.removed.into());
    ack.insert("source_note".into(), outcome.note.clone().into());
    Some(serde_json::Value::Object(ack))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("ps5upload-srcrm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::canonicalize(&root).unwrap()
    }

    /// A game folder `<root>/game` and an image `<root>/game.exfat`.
    fn sources(root: &Path) -> (PathBuf, PathBuf) {
        let game = root.join("game");
        std::fs::create_dir_all(game.join("sce_sys")).unwrap();
        std::fs::create_dir_all(game.join("data/empty")).unwrap();
        std::fs::write(game.join("eboot.bin"), b"EBOOT").unwrap();
        std::fs::write(game.join("sce_sys/param.json"), b"{}").unwrap();
        let image = root.join("game.exfat");
        std::fs::write(&image, b"an image").unwrap();
        (game, image)
    }

    /// A home that is nowhere near the scratch folders.
    fn home() -> PathBuf {
        PathBuf::from("/nonexistent-home-for-tests")
    }

    fn planned(source: &Path, output: &Path) -> Result<Plan, String> {
        plan_with(source, output, true, &home())
    }

    /// A stand-in Trash: it only notes what it was handed, and nothing leaves the disk.
    fn trashed(
        plan: &Plan,
        output: &Path,
        warnings: &[String],
        busy: bool,
    ) -> (Outcome, Vec<PathBuf>) {
        let mut moved = Vec::new();
        let outcome = finish_with(plan, output, warnings, busy, &mut |p| {
            moved.push(p.to_path_buf());
            Ok(())
        });
        (outcome, moved)
    }

    #[test]
    fn a_source_on_this_computer_goes_once_the_output_is_there() {
        let root = scratch("ok");
        let (game, image) = sources(&root);
        for (source, output) in [
            (&game, root.join("out/game.ffpkg")),
            (&image, root.join("out/game")),
        ] {
            let plan = planned(source, &output).unwrap();
            // Before the output exists, nothing goes.
            let (o, moved) = trashed(&plan, &output, &[], false);
            assert!(!o.removed && o.note.contains("is not there"), "{}", o.note);
            assert!(moved.is_empty());
            std::fs::create_dir_all(output.parent().unwrap()).unwrap();
            std::fs::write(&output, b"made").unwrap();
            let (o, moved) = trashed(&plan, &output, &[], false);
            assert!(o.removed, "{}", o.note);
            assert_eq!(
                o.note,
                format!("source moved to the Trash: {}", source.display())
            );
            assert_eq!(moved, std::slice::from_ref(source));
            std::fs::remove_file(&output).unwrap();
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_source_on_a_ps5_or_a_server_is_refused() {
        let out = Path::new("/out/game.exfat");
        for source in [
            "ps5://192.0.2.1/data/homebrew/game",
            "remote://nas/games/game.exfat",
        ] {
            let err = planned(Path::new(source), out).unwrap_err();
            assert!(err.contains("on a PS5 or a server"), "{source}: {err}");
            assert!(source_refusal_with(Path::new(source), true, &home()).is_some());
        }
    }

    #[test]
    fn no_trash_refuses_the_option_instead_of_deleting_for_good() {
        let root = scratch("notrash");
        let (game, image) = sources(&root);
        for source in [&game, &image] {
            let err = plan_with(source, &root.join("out/x.exfat"), false, &home()).unwrap_err();
            assert!(err.contains("no Trash or Recycle Bin"), "{err}");
            assert!(err.contains("never deleted for good"), "{err}");
        }
        assert!(game.exists() && image.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn output_inside_the_source_or_the_source_inside_the_output_is_refused() {
        let root = scratch("nest");
        let (game, image) = sources(&root);
        // The output inside the source folder, at any depth, existing yet or not.
        for output in [game.join("game.exfat"), game.join("out/deep/game.exfat")] {
            let err = planned(&game, &output).unwrap_err();
            assert!(err.contains("is inside the source"), "{err}");
        }
        // The output is the source itself.
        let err = planned(&image, &image).unwrap_err();
        assert!(err.contains("is inside the source"), "{err}");
        // The source inside the output: a folder made where the image sits.
        let err = planned(&image, &root).unwrap_err();
        assert!(err.contains("is inside the output"), "{err}");
        let err = planned(&game.join("eboot.bin"), &game).unwrap_err();
        assert!(err.contains("is inside the output"), "{err}");
        // Beside each other is fine, a name that only starts the same included.
        assert!(planned(&game, &root.join("game.exfat")).is_ok());
        assert!(planned(&game, &root.join("game-out/game.exfat")).is_ok());
        // `..` in the output is not reasoned about: through a folder that exists it resolves
        // (and lands in the source), through one that does not it is refused as it stands.
        let err = planned(&game, &root.join("game/../game/x.exfat")).unwrap_err();
        assert!(err.contains("is inside the source"), "{err}");
        assert!(planned(&game, &root.join("other/../game/x.exfat")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A link into the source folder is the source folder: the output through it is inside.
    #[cfg(unix)]
    #[test]
    fn a_link_does_not_hide_the_nesting_and_a_linked_source_is_refused() {
        let root = scratch("link");
        let (game, _) = sources(&root);
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&game, &alias).unwrap();
        let err = planned(&game, &alias.join("game.exfat")).unwrap_err();
        assert!(err.contains("is inside the source"), "{err}");
        let err = planned(&alias, &root.join("game.exfat")).unwrap_err();
        assert!(err.contains("is a link"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_run_with_warnings_keeps_its_source() {
        let root = scratch("warn");
        let (game, _) = sources(&root);
        let output = root.join("game.exfat");
        let plan = planned(&game, &output).unwrap();
        std::fs::write(&output, b"made").unwrap();
        let warnings = ["3 files were renamed".to_string(), "another".to_string()];
        let (o, moved) = trashed(&plan, &output, &warnings, false);
        assert!(!o.removed);
        assert_eq!(
            o.note,
            "source kept: the conversion had 2 warning(s), the first: 3 files were renamed"
        );
        assert!(moved.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_source_another_job_reads_is_kept() {
        let root = scratch("busy");
        let (game, _) = sources(&root);
        let output = root.join("game.exfat");
        let plan = planned(&game, &output).unwrap();
        std::fs::write(&output, b"made").unwrap();
        let (o, moved) = trashed(&plan, &output, &[], true);
        assert!(!o.removed && o.note.contains("another job"), "{}", o.note);
        assert!(moved.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_source_that_changed_during_the_run_is_kept() {
        let root = scratch("changed");
        let (game, image) = sources(&root);
        let output = root.join("out.exfat");
        std::fs::write(&output, b"made").unwrap();

        // A file added, a file grown, a folder added, a folder removed: each keeps the folder.
        let changes: [&dyn Fn(); 4] = [
            &|| std::fs::write(game.join("save.dat"), b"new").unwrap(),
            &|| std::fs::write(game.join("eboot.bin"), b"EBOOT, patched").unwrap(),
            &|| std::fs::create_dir(game.join("mods")).unwrap(),
            &|| std::fs::remove_dir(game.join("data/empty")).unwrap(),
        ];
        for change in changes {
            let plan = planned(&game, &output).unwrap();
            change();
            let (o, moved) = trashed(&plan, &output, &[], false);
            assert!(!o.removed && o.note.contains("changed while"), "{}", o.note);
            assert!(moved.is_empty());
        }
        // Untouched since it was noted, it goes.
        let plan = planned(&game, &output).unwrap();
        assert!(trashed(&plan, &output, &[], false).0.removed);

        // An image that grew, and one that is gone.
        let plan = planned(&image, &output).unwrap();
        std::fs::write(&image, b"an image, longer").unwrap();
        let (o, _) = trashed(&plan, &output, &[], false);
        assert!(!o.removed && o.note.contains("changed while"), "{}", o.note);
        let plan = planned(&image, &output).unwrap();
        std::fs::remove_file(&image).unwrap();
        let (o, moved) = trashed(&plan, &output, &[], false);
        assert!(!o.removed && moved.is_empty(), "{}", o.note);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_trash_that_refuses_keeps_the_source_and_says_why() {
        let root = scratch("refuse");
        let (game, _) = sources(&root);
        let output = root.join("game.exfat");
        let plan = planned(&game, &output).unwrap();
        std::fs::write(&output, b"made").unwrap();
        let o = finish_with(&plan, &output, &[], false, &mut |_| {
            Err("the volume has no trash".into())
        });
        assert!(!o.removed);
        assert!(
            o.note
                .ends_with("could not be moved to the Trash: the volume has no trash"),
            "{}",
            o.note
        );
        assert!(game.join("eboot.bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_archive_scratch_copy_a_missing_source_and_a_home_folder_are_refused() {
        let root = scratch("misc");
        let (game, _) = sources(&root);
        let out = root.join("out/x.exfat");
        // The unpack folder of an archive: the engine removes it itself.
        let unpacked = root
            .join(format!("{}abc", crate::fpkg_api::EXTRACT_PREFIX))
            .join("Game");
        std::fs::create_dir_all(&unpacked).unwrap();
        let err = planned(&unpacked, &out).unwrap_err();
        assert!(err.contains("unpacked from an archive"), "{err}");
        // Nothing there.
        assert!(planned(&root.join("gone"), &out).is_err());
        // The folder the home folder is in (or the home folder itself) is not a game.
        let err = plan_with(&root, &out, true, &game).unwrap_err();
        assert!(err.contains("holds your home folder"), "{err}");
        let err = plan_with(&game, &out, true, &game).unwrap_err();
        assert!(err.contains("holds your home folder"), "{err}");
        // A file is never taken for a drive.
        assert!(plan_with(&game.join("eboot.bin"), &out, true, &game).is_ok());
        // The root of the filesystem.
        let top = root.ancestors().last().unwrap();
        assert!(source_refusal_with(top, true, &home())
            .unwrap()
            .contains("whole drive"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_outcome_joins_the_jobs_ack() {
        let moved = Outcome {
            removed: true,
            note: "source moved to the Trash: /g".into(),
        };
        assert_eq!(ack(None, None), None);
        let kept = serde_json::json!({ "ampr_runtime": "x" });
        assert_eq!(ack(Some(kept.clone()), None), Some(kept.clone()));
        assert_eq!(
            ack(Some(kept), Some(&moved)),
            Some(serde_json::json!({
                "ampr_runtime": "x",
                "source_removed": true,
                "source_note": "source moved to the Trash: /g",
            }))
        );
        assert_eq!(
            ack(None, Some(&Outcome::kept("it changed"))),
            Some(serde_json::json!({
                "source_removed": false,
                "source_note": "source kept: it changed",
            }))
        );
    }
}
