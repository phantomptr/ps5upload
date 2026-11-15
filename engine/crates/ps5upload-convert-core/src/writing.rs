//! Which outputs a build in this process is writing, and which leftovers beside an output are
//! a build's own: what lets two builds of one output refuse each other, and a sweep tell an
//! unfinished build of ours from a user's file.

use std::path::{Path, PathBuf};

/// Package files being written by a build in this engine (`<stem>.pkg.partial`).
pub fn active_partials() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static SET: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    SET.get_or_init(Default::default)
}

/// Marks a file as being written by a running build, for as long as it lives: the sweep below
/// leaves it alone, and a second build of the same output is refused.
pub struct Writing(PathBuf);

/// A path this engine writes and may clear: a `.partial` output or an `.lz4spool` beside one.
fn is_own_leftover(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase())
        .is_some_and(|n| n.ends_with(".partial") || n.ends_with(".lz4spool"))
}

/// `path` as a leftover `claim` may look at and clear: absolute, with no `..` (or other
/// non-normal part) after its root, and named like one of our own leftovers. `None` for
/// anything else, which `claim` then never touches on disk.
fn clearable_leftover(path: &std::path::Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() || !is_own_leftover(path) {
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
    pub fn claim(path: PathBuf) -> Option<Writing> {
        let mut active = active_partials().lock().unwrap_or_else(|e| e.into_inner());
        if active.contains(&path) {
            return None;
        }
        // Only ever our own leftovers (a `.partial` output or an `.lz4spool`) at an absolute
        // path with no `..`, whatever path a caller hands in; anything else is never touched.
        if let Some(leftover) = clearable_leftover(&path) {
            // A leftover with a checkpoint is what the build goes on from (`image_build`).
            let checkpointed = crate::image_build::checkpoint_path(&leftover).is_file();
            if leftover.is_dir() {
                let _ = std::fs::remove_dir_all(&leftover);
            } else if leftover.is_file() && !checkpointed {
                let _ = std::fs::remove_file(&leftover);
            }
        }
        active.insert(path.clone());
        Some(Writing(path))
    }
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
