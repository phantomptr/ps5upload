//! Which outputs a build in this process is writing, and which leftovers beside an output are
//! a build's own: what lets two builds of one output refuse each other, and a sweep tell an
//! unfinished build of ours from a user's file.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Package files being written by a build in this engine (`<stem>.pkg.partial`).
pub fn active_partials() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Marks a file as being written by a running build, for as long as it lives: the sweep below
/// leaves it alone, and a second build of the same output is refused. The mark is seen by other
/// processes too (the `convert` command, a second engine, the app): see [`ClaimLock`].
pub struct Writing {
    path: PathBuf,
    _lock: Option<ClaimLock>,
}

/// A claim on a path every process honours: a small file beside it, `.<name>.ps5upload-claim`,
/// held under an OS file lock for as long as the claim lives. The OS drops the lock when the
/// process ends however it ends (a crash, a kill, a power cut), so a claim never outlives its
/// owner the way a pid written to a file would. The file also names the owning process's pid,
/// for whoever looks.
pub struct ClaimLock {
    file_path: PathBuf,
    _file: std::fs::File,
}

/// The claim file of `path`, or `None` for a path that is not a plain absolute one (a console
/// or server destination is claimed in this process only).
pub fn claim_file_of(path: &Path) -> Option<PathBuf> {
    let path = clean_absolute(path)?;
    let name = path.file_name()?;
    let mut claim = std::ffi::OsString::from(".");
    claim.push(name);
    claim.push(CLAIM_SUFFIX);
    Some(path.with_file_name(claim))
}

const CLAIM_SUFFIX: &str = ".ps5upload-claim";

/// Whether `file` is still the file at `path` (it was not removed and made anew between the open
/// and the lock). Off Unix there is no cheap file identity; Windows refuses to open a file that
/// is being removed, which covers the same race.
fn same_file(file: &std::fs::File, path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (file.metadata(), std::fs::metadata(path)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (file, path);
        true
    }
}

/// Whether a failed open of the claim file is worth another try. On Windows "access denied"
/// has two causes: the file is being removed by its owner right now (over in an instant, as
/// the owner closes it straight after), or the folder cannot be written at all. Neither is "a
/// build holds it": the first passes on a retry, and the second stays, so the caller then
/// goes on without a claim file and the build itself reports the folder it cannot write,
/// where this used to say the output was already being built.
fn retry_open(kind: std::io::ErrorKind, windows: bool) -> bool {
    windows && kind == std::io::ErrorKind::PermissionDenied
}

impl ClaimLock {
    /// Takes the claim on `path`. `Err(())` when another process holds it. `Ok(None)` when no
    /// claim file can be made there (a read-only folder, a file system without locks, a path
    /// that is not a local one): the claim is then this process's alone.
    #[allow(clippy::result_unit_err)]
    pub fn take(path: &Path) -> Result<Option<ClaimLock>, ()> {
        let Some(file_path) = claim_file_of(path) else {
            return Ok(None);
        };
        if let Some(parent) = file_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        for _ in 0..3 {
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&file_path)
            {
                Ok(f) => f,
                Err(e) if retry_open(e.kind(), cfg!(windows)) => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                Err(_) => return Ok(None),
            };
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Err(()),
                Err(std::fs::TryLockError::Error(_)) => return Ok(None),
            }
            if !same_file(&file, &file_path) {
                // Its owner removed it as we opened it: take the new one.
                continue;
            }
            let _ = file.set_len(0);
            let _ = (&file).write_all(format!("{}\n", std::process::id()).as_bytes());
            return Ok(Some(ClaimLock {
                file_path,
                _file: file,
            }));
        }
        Ok(None)
    }
}

impl Drop for ClaimLock {
    fn drop(&mut self) {
        // Removed while still locked, then unlocked as the file closes: whoever opened it in
        // between finds it gone after the lock (`same_file`) and starts over.
        let _ = std::fs::remove_file(&self.file_path);
    }
}

/// Whether another process holds a claim on `path` (a build writing it, an unpack in use). A
/// claim file nobody holds any more is a dead process's and is removed here.
pub fn claimed_elsewhere(path: &Path) -> bool {
    let Some(file_path) = claim_file_of(path) else {
        return false;
    };
    let file = match std::fs::OpenOptions::new().read(true).open(&file_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        // Windows: being removed by its owner, which still holds it.
        Err(_) => return cfg!(windows),
    };
    match file.try_lock() {
        Ok(()) => {
            if same_file(&file, &file_path) {
                let _ = std::fs::remove_file(&file_path);
            }
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// Claim files in `dir` that nobody holds and whose path is gone: what a killed process left.
pub fn sweep_dead_claims(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(target) = name
            .strip_suffix(CLAIM_SUFFIX)
            .and_then(|n| n.strip_prefix('.'))
            .filter(|n| !n.is_empty())
        else {
            continue;
        };
        let target = dir.join(target);
        if !target.exists() && e.file_type().is_ok_and(|t| t.is_file()) {
            // Removes it when nobody holds it.
            let _ = claimed_elsewhere(&target);
        }
    }
}

/// A path this engine writes and may clear: a `.partial` output or an `.lz4spool` beside one.
fn is_own_leftover(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase())
        .is_some_and(|n| n.ends_with(".partial") || n.ends_with(".lz4spool"))
}

fn is_partial(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".partial"))
}

/// Why a build can't write its `partial`, or `None`: a folder is there that no folder build of
/// ours left (it lacks the marker), which [`Writing::claim`] never removes. The build refuses
/// rather than write into it or fail on it later with a bare "File exists".
pub fn partial_in_the_way(partial: &Path) -> Option<String> {
    (partial.is_dir() && !crate::folder_build::is_our_partial_dir(partial)).then(|| {
        format!(
            "{} is a folder that is not an unfinished build of PS5Upload's; it is left as it \
             is. Move or rename it, then try again",
            partial.display()
        )
    })
}

/// `path` as a leftover `claim` may look at and clear: absolute, with no `..` (or other
/// non-normal part) after its root, and named like one of our own leftovers. `None` for
/// anything else, which `claim` then never touches on disk.
fn clearable_leftover(path: &std::path::Path) -> Option<PathBuf> {
    if !is_own_leftover(path) {
        return None;
    }
    clean_absolute(path)
}

/// `path` absolute with no `..` (or other non-normal part) after its root, or `None`.
fn clean_absolute(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => clean.push(part),
            Component::CurDir | Component::ParentDir => return None,
        }
    }
    Some(clean)
}

impl Writing {
    /// Claims `path` for one build. `None` when another running build already writes it: two
    /// builds of one output would wreck each other's file. A leftover at `path` from a build
    /// that is not running any more (the app closed, the engine died) is removed, so the
    /// rebuild the queue starts is not refused as "output already exists".
    ///
    /// Also `None` for a finished output kept under this name because its rename failed
    /// ([`ps5upload_fpkg::finish::is_kept`]): it is no leftover, and a retry of the same build
    /// used to delete it here, hours of writing with it.
    pub fn claim(path: PathBuf) -> Option<Writing> {
        // Still there, that is: once the user renamed or moved it the name is free again.
        if ps5upload_fpkg::finish::is_kept(&path) && path.symlink_metadata().is_ok() {
            return None;
        }
        let mut active = active_partials().lock().unwrap_or_else(|e| e.into_inner());
        if active.contains(&path) {
            return None;
        }
        // Another process (the `convert` command, a second engine) writing it refuses this one
        // too, and its file is not cleared from under it.
        let lock = ClaimLock::take(&path).ok()?;
        // Claimed from here, and the list let go before the leftover is cleared: removing an
        // unfinished folder build or a pack spool takes minutes, and every other build's end
        // and every sweep of leftovers waited behind it.
        active.insert(path.clone());
        drop(active);
        // Only ever our own leftovers (a `.partial` output or an `.lz4spool`) at an absolute
        // path with no `..`, whatever path a caller hands in; anything else is never touched.
        if let Some(leftover) = clearable_leftover(&path) {
            // A leftover with a checkpoint is what the build goes on from (`image_build`).
            let checkpointed = crate::image_build::checkpoint_path(&leftover).is_file();
            if leftover.is_dir() {
                // A `.partial` folder only when a folder build of ours left it (it carries the
                // marker); anything else there is someone's folder, kept, and the build refuses
                // it ([`partial_in_the_way`]). A pack spool is always ours.
                if !is_partial(&leftover) || crate::folder_build::is_our_partial_dir(&leftover) {
                    let _ = std::fs::remove_dir_all(&leftover);
                } else {
                    // An empty one (a build that died before its marker) holds nothing to lose.
                    let _ = std::fs::remove_dir(&leftover);
                }
            } else if leftover.is_file() && !checkpointed {
                let _ = std::fs::remove_file(&leftover);
            }
        }
        Some(Writing { path, _lock: lock })
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        active_partials()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.path);
    }
}

/// The in-progress files our builds write: `<name>.<kind>.partial`. Nothing else is touched,
/// since a compression writes next to the user's own image (a browser's `.partial` stays).
pub fn is_our_partial(name: &str) -> bool {
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
pub fn has_download_sidecar(partial: &Path) -> bool {
    let mut name = partial.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    partial.with_file_name(name).is_file()
}

/// A crashed image build's partial file, kept for a while for the same build to go on from. An old
/// one goes, with its checkpoint, or a game never built again would hold its space for good.
pub fn has_fresh_checkpoint(partial: &Path) -> bool {
    let checkpoint = crate::image_build::checkpoint_path(partial);
    let Ok(modified) = std::fs::metadata(&checkpoint).and_then(|m| m.modified()) else {
        return false;
    };
    let fresh = modified
        .elapsed()
        .is_ok_and(|age| age < crate::image_build::RESUME_KEPT);
    if !fresh {
        crate::image_build::remove_checkpoint(partial);
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.partial` folder is cleared only when it carries a folder build's marker; anyone
    /// else's is kept and named as in the way. A pack spool is ours either way.
    #[test]
    fn a_partial_folder_without_our_marker_is_never_cleared() {
        let root = crate::test_tmp::temp_path(format!("ps5upload-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let theirs = root.join("Game.partial");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("save.dat"), b"keep").unwrap();
        let w = Writing::claim(theirs.clone()).unwrap();
        drop(w);
        assert_eq!(std::fs::read(theirs.join("save.dat")).unwrap(), b"keep");
        assert!(partial_in_the_way(&theirs).is_some());

        let ours = root.join("Ours.partial");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::write(ours.join(crate::folder_build::MARKER), b"").unwrap();
        assert!(partial_in_the_way(&ours).is_none());
        drop(Writing::claim(ours.clone()).unwrap());
        assert!(!ours.exists());

        let empty = root.join("Empty.partial");
        std::fs::create_dir_all(&empty).unwrap();
        drop(Writing::claim(empty.clone()).unwrap());
        assert!(!empty.exists());

        let spool = root.join("Game.exfat.lz4spool");
        std::fs::create_dir_all(&spool).unwrap();
        drop(Writing::claim(spool.clone()).unwrap());
        assert!(!spool.exists());
        assert!(partial_in_the_way(&root.join("missing.partial")).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A finished output kept under its `.partial` name (its rename failed) is not a leftover:
    /// the next build of the same output is refused, and the file stays. The claim used to
    /// delete it.
    #[test]
    fn a_kept_finished_output_is_not_cleared_by_the_next_claim() {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-keptclaim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let partial = root.join("Game.pkg.partial");
        std::fs::write(&partial, b"a finished package").unwrap();
        ps5upload_fpkg::finish::kept_message(&partial, &root.join("Game.pkg"), &"denied");
        assert!(Writing::claim(partial.clone()).is_none());
        assert_eq!(std::fs::read(&partial).unwrap(), b"a finished package");
        // Renamed by hand, as the message asks: the name can be built again.
        std::fs::rename(&partial, root.join("Game.pkg")).unwrap();
        drop(Writing::claim(partial.clone()).unwrap());
        // Another output's leftover is still cleared.
        let other = root.join("Other.pkg.partial");
        std::fs::write(&other, b"cut short").unwrap();
        drop(Writing::claim(other.clone()).unwrap());
        assert!(!other.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A claim is seen through its file by any process (here: another handle, which the OS lock
    /// treats the same), refuses a second claim, and leaves nothing behind once dropped.
    #[test]
    fn a_claim_is_seen_by_other_processes_and_cleared_when_dropped() {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-claimlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let partial = root.join("Game.exfat.partial");
        let file = claim_file_of(&partial).unwrap();
        assert_eq!(file, root.join(".Game.exfat.partial.ps5upload-claim"));
        let w = Writing::claim(partial.clone()).unwrap();
        assert!(file.is_file());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap().trim(),
            std::process::id().to_string()
        );
        assert!(claimed_elsewhere(&partial));
        assert!(ClaimLock::take(&partial).is_err());
        drop(w);
        assert!(!file.exists());
        assert!(!claimed_elsewhere(&partial));
        // A claim file its process left when it was killed is nobody's: free, and removed.
        std::fs::write(&file, b"1\n").unwrap();
        assert!(!claimed_elsewhere(&partial));
        assert!(!file.exists());
        // A path that is no local one (a console or a server destination) has no claim file.
        assert!(claim_file_of(Path::new("ps5://1.2.3.4/data/g.exfat")).is_none());
        assert!(matches!(
            ClaimLock::take(Path::new("ps5://1.2.3.4/data/g.exfat")),
            Ok(None)
        ));
        sweep_dead_claims(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_no_claim_file_can_be_made_in_is_not_a_running_build() {
        use std::io::ErrorKind;
        // Windows "access denied" is retried, never read as "held by another build".
        assert!(retry_open(ErrorKind::PermissionDenied, true));
        assert!(!retry_open(ErrorKind::PermissionDenied, false));
        assert!(!retry_open(ErrorKind::NotFound, true));
        // A parent that is a file: no claim file can exist there on any OS.
        let root = crate::test_tmp::temp_path(format!("ps5upload-noclaim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("not-a-folder");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(matches!(
            ClaimLock::take(&blocker.join("Game.exfat.partial")),
            Ok(None)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_our_own_leftovers_at_a_plain_absolute_path_are_cleared() {
        let root = std::env::temp_dir();
        let ok = root.join("out").join("Game.exfat.partial");
        assert_eq!(clearable_leftover(&ok), Some(ok.clone()));
        assert!(clearable_leftover(&root.join("Game.lz4spool")).is_some());
        // Not ours, not absolute, or climbing out: never touched.
        assert!(clearable_leftover(&root.join("Game.exfat")).is_none());
        assert!(clearable_leftover(Path::new("out/Game.exfat.partial")).is_none());
        assert!(clearable_leftover(&root.join("..").join("Game.exfat.partial")).is_none());
        // A `.` inside a path is dropped by `components()` itself, so it names the same file.
        let dotted = root.join(".").join("Game.exfat.partial");
        assert_eq!(
            clearable_leftover(&dotted),
            Some(root.join("Game.exfat.partial"))
        );
    }
}
